//! Port of upstream/apps/web/src/lib/imageCompression.ts (+ test).
//!
//! Downscale + re-encode for image attachments that are too big for where
//! they're headed. Two consumers share the same pipeline:
//!
//! - The prompt stash persists images as base64, so [`compress_image_for_stash`]
//!   targets a per-image character budget.
//! - The composer accepts pasted/dropped images larger than the provider's
//!   `PROVIDER_SEND_TURN_MAX_IMAGE_BYTES` wire cap and shrinks them to fit via
//!   [`compress_image_to_byte_limit`] instead of rejecting the paste.
//!
//! Supported images already within budget pass through untouched, except that
//! a PNG or JPEG longer than `IMAGE_ATTACHMENT_MAX_EDGE` on a side is scaled
//! down to it in its own format before upload. HEIC/HEIF photos are converted
//! to JPEG first because providers cannot consume them.
//!
//! Upstream drives the browser's `createImageBitmap` and canvas. The size and
//! MIME decisions, the quality ladder, the fallback scale passes and the HEIC
//! metadata check are pure and live here; decoding, drawing and encoding sit
//! behind [`ImageCodec`] (the `image` crate in [`ImageCrateCodec`]), and the
//! HEIC decode behind [`HeicConverter`] (macOS `sips`). Everything here is CPU
//! work or a subprocess: callers run it on the background executor
//! (the `*_blocking` wrappers use `spawn_blocking`).
//!
//! See PORTING.md Deviations for the WebP, thumbnail-cache and file-read
//! differences.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use turin_shared::image_dimensions::{IMAGE_DIMENSIONS_HEADER_BYTES, read_image_dimensions};

use super::image_attachment_size::{
    IMAGE_ATTACHMENT_MAX_EDGE, ImageSize, downscalable_image_mime_type, fit_image_attachment_size,
};
use crate::composer::attachment_files::{PickedFile, is_heic_image_file};

/// Longest edge kept when an image has to be re-encoded.
const MAX_DIMENSION: u32 = IMAGE_ATTACHMENT_MAX_EDGE;
/// Base64 budget for a single stashed image (~975KB of binary).
pub const MAX_STASH_IMAGE_DATA_URL_CHARS: usize = 1_300_000;
/// Ceiling on the *source* file handed to the re-encoder. File size is a
/// proxy for pixel count, and decoding hundreds of megapixels can exhaust
/// memory — beyond this we refuse rather than risk it.
pub const MAX_COMPRESSIBLE_SOURCE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_HEIC_DECODE_PIXELS: u64 = 64_000_000;
const MAX_HEIC_METADATA_BYTES: usize = 1024 * 1024;
/// Quality ladder tried in order until the encoded image fits the budget.
/// The floor stays high enough to avoid visible blocking on UI screenshots;
/// if even that overflows we drop resolution instead of quality.
const QUALITY_STEPS: [f64; 4] = [0.92, 0.85, 0.78, 0.68];
/// Extra downscale passes applied when even the lowest quality overflows.
const FALLBACK_SCALE_STEPS: [f64; 2] = [0.75, 0.55];

const IMAGE_WEBP: &str = "image/webp";
const IMAGE_JPEG: &str = "image/jpeg";
const IMAGE_PNG: &str = "image/png";

/// A `File`: its name, MIME type and bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageFile {
    pub name: String,
    pub mime_type: String,
    pub bytes: Arc<Vec<u8>>,
    pub last_modified: Option<i64>,
}

impl ImageFile {
    pub fn new(name: &str, mime_type: &str, bytes: Vec<u8>) -> Self {
        Self {
            name: name.to_owned(),
            mime_type: mime_type.to_owned(),
            bytes: Arc::new(bytes),
            last_modified: None,
        }
    }

    pub fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn picked(&self) -> PickedFile {
        PickedFile::new(&self.name, &self.mime_type, self.size())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompressedStashImage {
    pub image_size: Option<ImageSize>,
    pub data_url: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// True when the payload was re-encoded rather than stored verbatim.
    pub recompressed: bool,
}

/// Why an image could not be compressed. Callers report these differently:
/// "too large" is a budget outcome, "unreadable" is a decode failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageCompressionFailureReason {
    TooLarge,
    Unreadable,
}

pub type CompressStashImageResult = Result<CompressedStashImage, ImageCompressionFailureReason>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressedImageFile {
    pub file: ImageFile,
    pub recompressed: bool,
    pub image_size: Option<ImageSize>,
}

pub type CompressImageFileResult = Result<CompressedImageFile, ImageCompressionFailureReason>;

/// A decode, draw or encode failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CodecError(pub String);

/// The canvas pipeline: `createImageBitmap`, `createCanvas`, `drawImage`,
/// `convertToBlob`. A `Bitmap` is released when dropped (upstream's `close`).
pub trait ImageCodec {
    type Bitmap;
    type Surface;

    /// `canRecompress`: whether this codec can decode and encode at all.
    fn can_recompress(&self) -> bool;

    /// `createImageBitmap`; `apply_orientation` is `imageOrientation: "from-image"`,
    /// which is also the spec default for callers that pass no options.
    fn decode(&self, file: &ImageFile, apply_orientation: bool)
    -> Result<Self::Bitmap, CodecError>;

    fn bitmap_size(&self, bitmap: &Self::Bitmap) -> ImageSize;

    /// `createCanvas`: `Ok(None)` when no 2D surface is available.
    fn create_surface(&self, size: ImageSize) -> Result<Option<Self::Surface>, CodecError>;

    /// Paint the surface white (the matte behind a format without alpha).
    fn fill_white(&self, surface: &mut Self::Surface);

    /// `drawImage(bitmap, sx, sy, sw, sh, 0, 0, dw, dh)` with high-quality smoothing.
    fn draw(
        &self,
        surface: &mut Self::Surface,
        bitmap: &Self::Bitmap,
        source: DrawSource,
        destination: ImageSize,
    ) -> Result<(), CodecError>;

    /// `encodeCanvas`: `Ok(None)` when the codec cannot produce `mime_type`
    /// (upstream: the blob comes back with a different type).
    fn encode(
        &self,
        surface: &Self::Surface,
        quality: f64,
        mime_type: &str,
    ) -> Result<Option<Vec<u8>>, CodecError>;
}

/// The source rectangle of a `drawImage`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawSource {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// `heicTo({ blob, type: "image/jpeg", quality })`.
pub trait HeicConverter {
    fn convert_to_jpeg(&self, file: &ImageFile, quality: f64) -> Result<Vec<u8>, CodecError>;
}

/// Data URL length, `data:<type>;base64,` plus the padded base64 payload.
fn data_url_length(mime_type: &str, size: u64) -> usize {
    let mime = if mime_type.is_empty() {
        "application/octet-stream"
    } else {
        mime_type
    };
    format!("data:{mime};base64,").len() + 4 * (size as usize).div_ceil(3)
}

/// `readFileAsDataUrl`.
pub fn read_file_as_data_url(mime_type: &str, bytes: &[u8]) -> String {
    let mime = if mime_type.is_empty() {
        "application/octet-stream"
    } else {
        mime_type
    };
    format!("data:{mime};base64,{}", BASE64.encode(bytes))
}

/// `atob` rejected the payload.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid base64 image payload")]
pub struct DataUrlError;

/// Base64 payload of a data URL decoded back into a file.
pub fn data_url_to_file(
    data_url: &str,
    name: &str,
    mime_type: &str,
) -> Result<ImageFile, DataUrlError> {
    // `slice(indexOf(",") + 1)`: without a comma the whole string is the payload.
    let payload = data_url
        .find(',')
        .map_or(data_url, |index| &data_url[index + 1..]);
    let bytes = BASE64.decode(payload).map_err(|_| DataUrlError)?;
    Ok(ImageFile::new(name, mime_type, bytes))
}

/// Re-encoding changes the container, so a name like `shot.png` would lie
/// about its contents. Swap the extension to match the encoded mime type.
fn file_name_for_mime_type(name: &str, mime_type: &str) -> String {
    let extension = if mime_type == IMAGE_WEBP {
        ".webp"
    } else {
        ".jpg"
    };
    let base = match name.rfind('.') {
        Some(dot_index) if dot_index > 0 => &name[..dot_index],
        _ => name,
    };
    format!("{base}{extension}")
}

// --- HEIC metadata -------------------------------------------------------

struct HeicMetadataBox {
    payload_offset: usize,
    end_offset: usize,
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let slice = bytes.get(offset..offset + 8)?;
    Some(u64::from_be_bytes(slice.try_into().ok()?))
}

fn find_heic_metadata_box(
    view: &[u8],
    start_offset: usize,
    end_offset: usize,
    box_type: u32,
) -> Option<HeicMetadataBox> {
    let mut offset = start_offset;
    while offset + 8 <= end_offset {
        let mut size = read_u32(view, offset)? as usize;
        let mut header_size = 8;
        if size == 1 {
            if offset + 16 > end_offset {
                return None;
            }
            let extended_size = read_u64(view, offset + 8)?;
            if extended_size > (1u64 << 53) - 1 {
                return None;
            }
            size = extended_size as usize;
            header_size = 16;
        } else if size == 0 {
            size = end_offset - offset;
        }
        if size < header_size || size > end_offset - offset {
            return None;
        }

        let next_offset = offset + size;
        if read_u32(view, offset + 4)? == box_type {
            return Some(HeicMetadataBox {
                payload_offset: offset + header_size,
                end_offset: next_offset,
            });
        }
        offset = next_offset;
    }
    None
}

const BOX_META: u32 = 0x6d65_7461;
const BOX_IPRP: u32 = 0x6970_7270;
const BOX_IPCO: u32 = 0x6970_636f;
const BOX_ISPE: u32 = 0x6973_7065;

/// Read HEIC image dimensions before the decoder allocates full RGBA buffers.
/// `bytes` is the file's first [`MAX_HEIC_METADATA_BYTES`].
fn validate_heic_image_dimensions(file: &ImageFile) -> Option<ImageCompressionFailureReason> {
    use ImageCompressionFailureReason::{TooLarge, Unreadable};
    let head = &file.bytes[..file.bytes.len().min(MAX_HEIC_METADATA_BYTES)];
    let Some(meta) = find_heic_metadata_box(head, 0, head.len(), BOX_META) else {
        return Some(Unreadable);
    };
    if meta.payload_offset + 4 > meta.end_offset {
        return Some(Unreadable);
    }
    let Some(properties) =
        find_heic_metadata_box(head, meta.payload_offset + 4, meta.end_offset, BOX_IPRP)
    else {
        return Some(Unreadable);
    };
    let Some(containers) = find_heic_metadata_box(
        head,
        properties.payload_offset,
        properties.end_offset,
        BOX_IPCO,
    ) else {
        return Some(Unreadable);
    };

    let mut offset = containers.payload_offset;
    let mut found_image_dimensions = false;
    while offset < containers.end_offset {
        let Some(image) = find_heic_metadata_box(head, offset, containers.end_offset, BOX_ISPE)
        else {
            break;
        };
        if image.payload_offset + 12 > image.end_offset {
            return Some(Unreadable);
        }
        let (Some(width), Some(height)) = (
            read_u32(head, image.payload_offset + 4),
            read_u32(head, image.payload_offset + 8),
        ) else {
            return Some(Unreadable);
        };
        if width == 0 || height == 0 {
            return Some(Unreadable);
        }
        if u64::from(width) > MAX_HEIC_DECODE_PIXELS / u64::from(height) {
            return Some(TooLarge);
        }
        found_image_dimensions = true;
        offset = image.end_offset;
    }
    if found_image_dimensions {
        None
    } else {
        Some(Unreadable)
    }
}

// --- Encoding ------------------------------------------------------------

/// `encodeCanvas` against a codec error: an encoder that throws is a failed
/// attempt, not a crash.
type Encoded = (Vec<u8>, String, ImageSize);

/// Draws `bitmap` scaled to fit `max_dimension` and encodes it, stepping
/// quality down until the data URL fits `budget_chars`.
fn encode_within_budget<C: ImageCodec>(
    codec: &C,
    bitmap: &C::Bitmap,
    max_dimension: u32,
    budget_chars: usize,
    preferred_mime_type: Option<&str>,
) -> Result<Option<Encoded>, CodecError> {
    let source = codec.bitmap_size(bitmap);
    let longest = f64::from(source.width.max(source.height));
    let scale = (f64::from(max_dimension) / longest).min(1.0);
    let width = ((f64::from(source.width) * scale).round() as u32).max(1);
    let height = ((f64::from(source.height) * scale).round() as u32).max(1);
    let size = ImageSize { width, height };
    let Some(mut target) = codec.create_surface(size)? else {
        return Ok(None);
    };

    // Probe WebP once; JPEG (no alpha) needs a white matte, so the fill has to
    // happen before drawing and depends on which codec we end up using.
    let mime_type = match preferred_mime_type {
        Some(mime_type) => mime_type.to_owned(),
        None => {
            if codec
                .encode(&target, QUALITY_STEPS[0], IMAGE_WEBP)?
                .is_some()
            {
                IMAGE_WEBP.to_owned()
            } else {
                IMAGE_JPEG.to_owned()
            }
        }
    };

    if mime_type == IMAGE_JPEG {
        codec.fill_white(&mut target);
    }
    codec.draw(
        &mut target,
        bitmap,
        DrawSource {
            x: 0.0,
            y: 0.0,
            width: f64::from(source.width),
            height: f64::from(source.height),
        },
        size,
    )?;

    for quality in QUALITY_STEPS {
        let Some(encoded) = codec.encode(&target, quality, &mime_type)? else {
            break;
        };
        if data_url_length(&mime_type, encoded.len() as u64) <= budget_chars {
            return Ok(Some((encoded, mime_type, size)));
        }
    }
    Ok(None)
}

struct Reencoded {
    bytes: Vec<u8>,
    mime_type: String,
    image_size: ImageSize,
}

/// Shared re-encode loop: decodes `file`, then walks the quality ladder and
/// fallback downscale passes until an encoding fits `budget_chars`.
fn reencode_within_budget<C: ImageCodec>(
    codec: &C,
    file: &ImageFile,
    budget_chars: usize,
    preferred_mime_type: Option<&str>,
) -> Result<Reencoded, ImageCompressionFailureReason> {
    use ImageCompressionFailureReason::{TooLarge, Unreadable};
    if !codec.can_recompress() {
        return Err(TooLarge);
    }

    let Ok(bitmap) = codec.decode(file, true) else {
        return Err(Unreadable);
    };

    // Each pass shrinks relative to the *previous target*, capped by
    // MAX_DIMENSION. Scaling a fixed ceiling instead would be a no-op for
    // images already smaller than that ceiling — the fallback passes would
    // all resolve to the source size and never actually reduce resolution.
    let source = codec.bitmap_size(&bitmap);
    let base_dimension = MAX_DIMENSION.min(source.width.max(source.height));
    // Tracks whether the *last* attempt failed, so a run of encoder failures
    // is reported as unreadable while a run of merely-too-big results is
    // reported as too-large.
    let mut encode_failed = false;
    for dimension_scale in std::iter::once(1.0).chain(FALLBACK_SCALE_STEPS) {
        let target_dimension =
            ((f64::from(base_dimension) * dimension_scale).round() as u32).max(1);
        let encoded = match encode_within_budget(
            codec,
            &bitmap,
            target_dimension,
            budget_chars,
            preferred_mime_type,
        ) {
            Ok(encoded) => encoded,
            Err(_) => {
                // Canvas allocation, drawing, or the codec itself can fail —
                // often precisely *because* the target is too big. Keep trying
                // the smaller fallback scales rather than giving up: a reduced
                // pass may well succeed.
                encode_failed = true;
                continue;
            }
        };
        encode_failed = false;
        if let Some((bytes, mime_type, image_size)) = encoded {
            return Ok(Reencoded {
                bytes,
                mime_type,
                image_size,
            });
        }
    }
    Err(if encode_failed { Unreadable } else { TooLarge })
}

/// A centered square crop, at most 256px, for the composer's `object-cover`
/// image tiles, as a PNG data URL.
pub fn create_composer_image_thumbnail<C: ImageCodec>(
    codec: &C,
    file: &ImageFile,
) -> Option<String> {
    if !codec.can_recompress() {
        return None;
    }
    let bitmap = codec.decode(file, true).ok()?;
    let size = codec.bitmap_size(&bitmap);
    let side = size.width.min(size.height);
    if side == 0 {
        return None;
    }
    let dimension = 256.min(side);
    let target_size = ImageSize {
        width: dimension,
        height: dimension,
    };
    let mut surface = codec.create_surface(target_size).ok()??;
    codec
        .draw(
            &mut surface,
            &bitmap,
            DrawSource {
                x: f64::from(size.width - side) / 2.0,
                y: f64::from(size.height - side) / 2.0,
                width: f64::from(side),
                height: f64::from(side),
            },
            target_size,
        )
        .ok()?;
    let encoded = codec.encode(&surface, 1.0, IMAGE_PNG).ok()??;
    Some(read_file_as_data_url(IMAGE_PNG, &encoded))
}

/// Produces the payload to persist for a stashed image.
///
/// Small images are stored verbatim (preserving PNG transparency and exact
/// pixels). Anything over budget is downscaled and re-encoded; if it still
/// doesn't fit after the fallback passes, reports a failure so the caller
/// can record it as dropped.
pub fn compress_image_for_stash<C: ImageCodec>(
    codec: &C,
    file: &ImageFile,
    budget_chars: usize,
) -> CompressStashImageResult {
    if data_url_length(&file.mime_type, file.size()) <= budget_chars {
        return Ok(CompressedStashImage {
            data_url: read_file_as_data_url(&file.mime_type, &file.bytes),
            mime_type: file.mime_type.clone(),
            size_bytes: file.size(),
            recompressed: false,
            image_size: None,
        });
    }
    let reencoded = reencode_within_budget(codec, file, budget_chars, None)?;
    Ok(CompressedStashImage {
        data_url: read_file_as_data_url(&reencoded.mime_type, &reencoded.bytes),
        size_bytes: reencoded.bytes.len() as u64,
        mime_type: reencoded.mime_type,
        recompressed: true,
        image_size: Some(reencoded.image_size),
    })
}

/// `compressImageForStash` with its default budget.
pub fn compress_image_for_stash_default<C: ImageCodec>(
    codec: &C,
    file: &ImageFile,
) -> CompressStashImageResult {
    compress_image_for_stash(codec, file, MAX_STASH_IMAGE_DATA_URL_CHARS)
}

/// Scales a PNG or JPEG whose long edge is over `IMAGE_ATTACHMENT_MAX_EDGE`
/// down to that edge in its own format, so a PNG keeps its transparency.
/// Returns `None` when the image already fits, is another format (GIF, WebP,
/// SVG), or cannot be decoded or encoded here; the caller keeps the original.
fn fit_image_to_max_edge<C: ImageCodec>(
    codec: &C,
    file: &ImageFile,
) -> Option<(ImageFile, ImageSize)> {
    let mime_type = downscalable_image_mime_type(&file.mime_type)?;
    if !codec.can_recompress() || file.size() > MAX_COMPRESSIBLE_SOURCE_BYTES {
        return None;
    }
    // The header answers "does it fit?" without decoding every attachment.
    let header =
        read_image_dimensions(&file.bytes[..file.bytes.len().min(IMAGE_DIMENSIONS_HEADER_BYTES)])?;
    fit_image_attachment_size(ImageSize {
        width: header.width,
        height: header.height,
    })?;
    let bitmap = codec.decode(file, true).ok()?;
    let bitmap_size = codec.bitmap_size(&bitmap);
    let image_size = fit_image_attachment_size(bitmap_size)?;
    let mut target = codec.create_surface(image_size).ok()??;
    codec
        .draw(
            &mut target,
            &bitmap,
            DrawSource {
                x: 0.0,
                y: 0.0,
                width: f64::from(bitmap_size.width),
                height: f64::from(bitmap_size.height),
            },
            image_size,
        )
        .ok()?;
    let blob = codec
        .encode(&target, QUALITY_STEPS[0], mime_type.as_str())
        .ok()??;
    Some((
        ImageFile {
            name: file.name.clone(),
            mime_type: mime_type.as_str().to_owned(),
            bytes: Arc::new(blob),
            last_modified: file.last_modified,
        },
        image_size,
    ))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CompressToByteLimitOptions<'a> {
    pub preferred_mime_type: Option<&'a str>,
    pub source_size_bytes: Option<u64>,
}

/// Shrinks `file` until its binary size fits `max_bytes`, returning a new
/// file (WebP or JPEG). A PNG or JPEG over `IMAGE_ATTACHMENT_MAX_EDGE` is
/// first scaled to that edge in its own format. Other files already within the
/// limit pass through untouched, preserving their exact bytes and format.
/// Sources above [`MAX_COMPRESSIBLE_SOURCE_BYTES`] are refused outright —
/// decoding them is the risk, so no amount of output budget makes them safe. An
/// internally converted image can provide its original source size when the
/// intermediate format expands beyond that ceiling.
pub fn compress_image_to_byte_limit<C: ImageCodec>(
    codec: &C,
    file: &ImageFile,
    max_bytes: u64,
    options: CompressToByteLimitOptions<'_>,
) -> CompressImageFileResult {
    let fitted = fit_image_to_max_edge(codec, file);
    if let Some((fitted_file, image_size)) = &fitted
        && fitted_file.size() <= max_bytes
    {
        return Ok(CompressedImageFile {
            file: fitted_file.clone(),
            recompressed: true,
            image_size: Some(*image_size),
        });
    }
    if fitted.is_none() && file.size() <= max_bytes {
        return Ok(CompressedImageFile {
            file: file.clone(),
            recompressed: false,
            image_size: None,
        });
    }
    if options.source_size_bytes.unwrap_or(file.size()) > MAX_COMPRESSIBLE_SOURCE_BYTES {
        return Err(ImageCompressionFailureReason::TooLarge);
    }
    // The re-encode loop budgets in data-URL characters. Base64 turns 3 bytes
    // into 4 chars; flooring keeps the budget a hair conservative instead of
    // admitting an encoding right at the byte cap.
    let budget_chars = (max_bytes / 3 * 4) as usize;
    let source = fitted.as_ref().map_or(file, |(fitted_file, _)| fitted_file);
    let reencoded =
        reencode_within_budget(codec, source, budget_chars, options.preferred_mime_type)?;
    Ok(CompressedImageFile {
        file: ImageFile {
            name: file_name_for_mime_type(
                if file.name.is_empty() {
                    "image"
                } else {
                    &file.name
                },
                &reencoded.mime_type,
            ),
            mime_type: reencoded.mime_type,
            bytes: Arc::new(reencoded.bytes),
            last_modified: None,
        },
        recompressed: true,
        image_size: Some(reencoded.image_size),
    })
}

/// Converts HEIC/HEIF photos to provider-compatible JPEG before applying the
/// attachment size limit.
pub fn prepare_image_for_attachment<C: ImageCodec, H: HeicConverter>(
    codec: &C,
    heic: &H,
    file: &ImageFile,
    max_bytes: u64,
) -> CompressImageFileResult {
    use ImageCompressionFailureReason::{TooLarge, Unreadable};
    if !is_heic_image_file(&file.picked()) {
        return compress_image_to_byte_limit(codec, file, max_bytes, Default::default());
    }

    if file.size() > MAX_COMPRESSIBLE_SOURCE_BYTES {
        return Err(TooLarge);
    }

    if let Some(dimension_error) = validate_heic_image_dimensions(file) {
        return Err(dimension_error);
    }
    let Ok(converted) = heic.convert_to_jpeg(file, QUALITY_STEPS[0]) else {
        return Err(Unreadable);
    };

    let jpeg = ImageFile {
        name: file_name_for_mime_type(
            if file.name.is_empty() {
                "image"
            } else {
                &file.name
            },
            IMAGE_JPEG,
        ),
        mime_type: IMAGE_JPEG.to_owned(),
        bytes: Arc::new(converted),
        last_modified: file.last_modified,
    };
    let result = compress_image_to_byte_limit(
        codec,
        &jpeg,
        max_bytes,
        CompressToByteLimitOptions {
            preferred_mime_type: Some(IMAGE_JPEG),
            source_size_bytes: Some(file.size()),
        },
    )?;
    Ok(CompressedImageFile {
        recompressed: true,
        ..result
    })
}

// --- Async entry points ----------------------------------------------------

/// `prepareImageForAttachment` on the blocking pool: decode, resize and
/// encode are CPU work and must not run on the UI thread.
pub async fn prepare_image_for_attachment_blocking(
    file: ImageFile,
    max_bytes: u64,
) -> CompressImageFileResult {
    tokio::task::spawn_blocking(move || {
        prepare_image_for_attachment(&ImageCrateCodec, &SipsHeicConverter, &file, max_bytes)
    })
    .await
    .unwrap_or(Err(ImageCompressionFailureReason::Unreadable))
}

/// `compressImageForStash` on the blocking pool.
pub async fn compress_image_for_stash_blocking(
    file: ImageFile,
    budget_chars: usize,
) -> CompressStashImageResult {
    tokio::task::spawn_blocking(move || {
        compress_image_for_stash(&ImageCrateCodec, &file, budget_chars)
    })
    .await
    .unwrap_or(Err(ImageCompressionFailureReason::Unreadable))
}

/// `createComposerImageThumbnail` on the blocking pool.
pub async fn create_composer_image_thumbnail_blocking(file: ImageFile) -> Option<String> {
    tokio::task::spawn_blocking(move || create_composer_image_thumbnail(&ImageCrateCodec, &file))
        .await
        .ok()
        .flatten()
}

// --- The `image` crate codec -------------------------------------------------

/// The canvas stand-in: `image` decodes, resizes with Lanczos (high-quality
/// smoothing) and encodes PNG and JPEG. It has no lossy WebP encoder, so a
/// WebP request answers `None` and the pipeline takes upstream's JPEG
/// fallback, with the white matte.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageCrateCodec;

impl ImageCodec for ImageCrateCodec {
    type Bitmap = image::DynamicImage;
    type Surface = image::RgbaImage;

    fn can_recompress(&self) -> bool {
        true
    }

    fn decode(
        &self,
        file: &ImageFile,
        apply_orientation: bool,
    ) -> Result<Self::Bitmap, CodecError> {
        use image::ImageDecoder as _;
        let reader = image::ImageReader::new(std::io::Cursor::new(file.bytes.as_slice()))
            .with_guessed_format()
            .map_err(|error| CodecError(error.to_string()))?;
        let mut decoder = reader
            .into_decoder()
            .map_err(|error| CodecError(error.to_string()))?;
        let orientation = decoder.orientation().ok();
        let mut bitmap = image::DynamicImage::from_decoder(decoder)
            .map_err(|error| CodecError(error.to_string()))?;
        if apply_orientation && let Some(orientation) = orientation {
            bitmap.apply_orientation(orientation);
        }
        Ok(bitmap)
    }

    fn bitmap_size(&self, bitmap: &Self::Bitmap) -> ImageSize {
        ImageSize {
            width: image::GenericImageView::width(bitmap),
            height: image::GenericImageView::height(bitmap),
        }
    }

    fn create_surface(&self, size: ImageSize) -> Result<Option<Self::Surface>, CodecError> {
        Ok(Some(image::RgbaImage::new(size.width, size.height)))
    }

    fn fill_white(&self, surface: &mut Self::Surface) {
        for pixel in surface.pixels_mut() {
            *pixel = image::Rgba([255, 255, 255, 255]);
        }
    }

    fn draw(
        &self,
        surface: &mut Self::Surface,
        bitmap: &Self::Bitmap,
        source: DrawSource,
        destination: ImageSize,
    ) -> Result<(), CodecError> {
        let cropped = bitmap.crop_imm(
            source.x.round() as u32,
            source.y.round() as u32,
            source.width.round() as u32,
            source.height.round() as u32,
        );
        let resized = cropped.resize_exact(
            destination.width,
            destination.height,
            image::imageops::FilterType::Lanczos3,
        );
        image::imageops::overlay(surface, &resized.to_rgba8(), 0, 0);
        Ok(())
    }

    fn encode(
        &self,
        surface: &Self::Surface,
        quality: f64,
        mime_type: &str,
    ) -> Result<Option<Vec<u8>>, CodecError> {
        let mut out = Vec::new();
        match mime_type {
            IMAGE_PNG => {
                surface
                    .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                    .map_err(|error| CodecError(error.to_string()))?;
            }
            IMAGE_JPEG => {
                // JPEG has no alpha: the pipeline matted the surface white first.
                let rgb = image::DynamicImage::ImageRgba8(surface.clone()).to_rgb8();
                let quality = (quality * 100.0).round().clamp(1.0, 100.0) as u8;
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
                    .encode_image(&rgb)
                    .map_err(|error| CodecError(error.to_string()))?;
            }
            _ => return Ok(None),
        }
        Ok(Some(out))
    }
}

/// HEIC to JPEG with macOS's `sips`, which decodes HEIC natively (upstream
/// loads the `heic-to` WASM decoder on demand). Blocking: run it on the
/// background executor.
#[derive(Debug, Clone, Copy, Default)]
pub struct SipsHeicConverter;

impl SipsHeicConverter {
    /// `sips -s format jpeg -s formatOptions <0-100> <in> --out <out>`.
    fn arguments(quality: f64, input: &std::path::Path, output: &std::path::Path) -> Vec<String> {
        vec![
            "-s".to_owned(),
            "format".to_owned(),
            "jpeg".to_owned(),
            "-s".to_owned(),
            "formatOptions".to_owned(),
            ((quality * 100.0).round() as u32).to_string(),
            input.display().to_string(),
            "--out".to_owned(),
            output.display().to_string(),
        ]
    }
}

impl HeicConverter for SipsHeicConverter {
    fn convert_to_jpeg(&self, file: &ImageFile, quality: f64) -> Result<Vec<u8>, CodecError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "turin-heic-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).map_err(|error| CodecError(error.to_string()))?;
        let result = (|| {
            let input = directory.join("input.heic");
            let output = directory.join("output.jpg");
            std::fs::write(&input, file.bytes.as_slice())
                .map_err(|error| CodecError(error.to_string()))?;
            let status = std::process::Command::new("/usr/bin/sips")
                .args(Self::arguments(quality, &input, &output))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|error| CodecError(error.to_string()))?;
            if !status.success() {
                return Err(CodecError("Invalid HEIC image".to_owned()));
            }
            std::fs::read(&output).map_err(|error| CodecError(error.to_string()))
        })();
        let _ = std::fs::remove_dir_all(&directory);
        result
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;

    fn make_file(size_bytes: usize, mime_type: &str) -> ImageFile {
        ImageFile::new("shot.png", mime_type, vec![7; size_bytes])
    }

    fn png() -> ImageFile {
        make_file(0, IMAGE_PNG)
    }

    fn size(width: u32, height: u32) -> ImageSize {
        ImageSize { width, height }
    }

    /// Records what the pipeline asked of the canvas.
    #[derive(Default)]
    struct Log {
        decodes: Cell<usize>,
        decode_orientations: RefCell<Vec<bool>>,
        surfaces: RefCell<Vec<ImageSize>>,
        encodes: RefCell<Vec<(String, f64)>>,
        fills: Cell<usize>,
        draws: RefCell<Vec<(DrawSource, ImageSize)>>,
        closed: Cell<usize>,
    }

    struct FakeBitmap {
        size: ImageSize,
        log: Rc<Log>,
    }

    impl Drop for FakeBitmap {
        fn drop(&mut self) {
            self.log.closed.set(self.log.closed.get() + 1);
        }
    }

    struct FakeSurface {
        size: ImageSize,
    }

    /// Stubs `createImageBitmap` + `OffscreenCanvas`. The encoder returns a
    /// payload whose size scales with quality, mirroring how a real encoder
    /// shrinks as quality drops, and answers `None` for WebP when
    /// `supports_webp` is false (the blob comes back with another type).
    struct FakeCodec {
        log: Rc<Log>,
        bitmap_size: ImageSize,
        supports_webp: bool,
        can_recompress: bool,
        decode_fails: bool,
        encode_fails: bool,
        size_for: Box<dyn Fn(f64, ImageSize) -> usize>,
    }

    impl FakeCodec {
        fn new(size_for: impl Fn(f64, ImageSize) -> usize + 'static) -> Self {
            Self {
                log: Rc::default(),
                bitmap_size: size(4000, 3000),
                supports_webp: true,
                can_recompress: true,
                decode_fails: false,
                encode_fails: false,
                size_for: Box::new(size_for),
            }
        }

        fn flat(bytes: usize) -> Self {
            Self::new(move |_, _| bytes)
        }
    }

    impl ImageCodec for FakeCodec {
        type Bitmap = FakeBitmap;
        type Surface = FakeSurface;

        fn can_recompress(&self) -> bool {
            self.can_recompress
        }

        fn decode(&self, _file: &ImageFile, orient: bool) -> Result<FakeBitmap, CodecError> {
            self.log.decodes.set(self.log.decodes.get() + 1);
            self.log.decode_orientations.borrow_mut().push(orient);
            if self.decode_fails {
                return Err(CodecError("corrupt image".to_owned()));
            }
            Ok(FakeBitmap {
                size: self.bitmap_size,
                log: self.log.clone(),
            })
        }

        fn bitmap_size(&self, bitmap: &FakeBitmap) -> ImageSize {
            bitmap.size
        }

        fn create_surface(&self, size: ImageSize) -> Result<Option<FakeSurface>, CodecError> {
            self.log.surfaces.borrow_mut().push(size);
            Ok(Some(FakeSurface { size }))
        }

        fn fill_white(&self, _surface: &mut FakeSurface) {
            self.log.fills.set(self.log.fills.get() + 1);
        }

        fn draw(
            &self,
            surface: &mut FakeSurface,
            _bitmap: &FakeBitmap,
            source: DrawSource,
            destination: ImageSize,
        ) -> Result<(), CodecError> {
            self.log.draws.borrow_mut().push((source, destination));
            assert_eq!(surface.size, destination);
            Ok(())
        }

        fn encode(
            &self,
            surface: &FakeSurface,
            quality: f64,
            mime_type: &str,
        ) -> Result<Option<Vec<u8>>, CodecError> {
            self.log
                .encodes
                .borrow_mut()
                .push((mime_type.to_owned(), quality));
            if self.encode_fails {
                return Err(CodecError("encoder unavailable".to_owned()));
            }
            if mime_type == IMAGE_WEBP && !self.supports_webp {
                return Ok(None);
            }
            Ok(Some(vec![0; (self.size_for)(quality, surface.size)]))
        }
    }

    struct FakeHeic {
        result: Result<Vec<u8>, CodecError>,
        calls: Cell<usize>,
        quality: Cell<f64>,
    }

    impl FakeHeic {
        fn returning(bytes: Vec<u8>) -> Self {
            Self {
                result: Ok(bytes),
                calls: Cell::new(0),
                quality: Cell::new(0.0),
            }
        }

        fn failing() -> Self {
            Self {
                result: Err(CodecError("Invalid HEIC image".to_owned())),
                calls: Cell::new(0),
                quality: Cell::new(0.0),
            }
        }
    }

    impl HeicConverter for FakeHeic {
        fn convert_to_jpeg(&self, _file: &ImageFile, quality: f64) -> Result<Vec<u8>, CodecError> {
            self.calls.set(self.calls.get() + 1);
            self.quality.set(quality);
            self.result.clone()
        }
    }

    fn box_bytes(name: &str, contents: &[&[u8]]) -> Vec<u8> {
        let length = 8 + contents.iter().map(|content| content.len()).sum::<usize>();
        let mut bytes = (length as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(name.as_bytes());
        for content in contents {
            bytes.extend_from_slice(content);
        }
        bytes
    }

    fn heic_file(name: &str, mime_type: &str, width: u32, height: u32) -> ImageFile {
        let mut dimensions = vec![0u8; 12];
        dimensions[4..8].copy_from_slice(&width.to_be_bytes());
        dimensions[8..12].copy_from_slice(&height.to_be_bytes());
        let properties = box_bytes(
            "iprp",
            &[&box_bytes("ipco", &[&box_bytes("ispe", &[&dimensions])])],
        );
        let mut bytes = box_bytes("ftyp", &[b"heic", &[0, 0, 0, 0]]);
        bytes.extend(box_bytes("meta", &[&[0, 0, 0, 0], &properties]));
        ImageFile::new(name, mime_type, bytes)
    }

    fn heic_default() -> ImageFile {
        heic_file("photo.heic", "image/heic", 4000, 3000)
    }

    // --- thumbnails -------------------------------------------------------

    #[test]
    fn thumbnail_decodes_a_tall_original_once_and_crops_a_bounded_center_square() {
        let mut codec = FakeCodec::new(|_, _| 0);
        codec.bitmap_size = size(2304, 32766);
        // The "thumbnail" payload.
        codec.size_for = Box::new(|_, _| 0);
        struct Thumbnail(FakeCodec);
        impl ImageCodec for Thumbnail {
            type Bitmap = FakeBitmap;
            type Surface = FakeSurface;
            fn can_recompress(&self) -> bool {
                self.0.can_recompress()
            }
            fn decode(&self, file: &ImageFile, o: bool) -> Result<FakeBitmap, CodecError> {
                self.0.decode(file, o)
            }
            fn bitmap_size(&self, b: &FakeBitmap) -> ImageSize {
                self.0.bitmap_size(b)
            }
            fn create_surface(&self, s: ImageSize) -> Result<Option<FakeSurface>, CodecError> {
                self.0.create_surface(s)
            }
            fn fill_white(&self, s: &mut FakeSurface) {
                self.0.fill_white(s)
            }
            fn draw(
                &self,
                s: &mut FakeSurface,
                b: &FakeBitmap,
                src: DrawSource,
                d: ImageSize,
            ) -> Result<(), CodecError> {
                self.0.draw(s, b, src, d)
            }
            fn encode(
                &self,
                _s: &FakeSurface,
                quality: f64,
                mime_type: &str,
            ) -> Result<Option<Vec<u8>>, CodecError> {
                self.0
                    .log
                    .encodes
                    .borrow_mut()
                    .push((mime_type.to_owned(), quality));
                Ok(Some(b"thumbnail".to_vec()))
            }
        }
        let codec = Thumbnail(codec);
        let original = ImageFile::new("tall.png", IMAGE_PNG, b"original bytes".to_vec());

        let thumbnail = create_composer_image_thumbnail(&codec, &original);

        assert_eq!(
            thumbnail.as_deref(),
            Some("data:image/png;base64,dGh1bWJuYWls")
        );
        assert_eq!(codec.0.log.decodes.get(), 1);
        assert_eq!(*codec.0.log.surfaces.borrow(), [size(256, 256)]);
        assert_eq!(
            *codec.0.log.draws.borrow(),
            [(
                DrawSource {
                    x: 0.0,
                    y: 15231.0,
                    width: 2304.0,
                    height: 2304.0
                },
                size(256, 256)
            )]
        );
        assert_eq!(codec.0.log.closed.get(), 1);
        assert_eq!(original.bytes.as_slice(), b"original bytes");
    }

    #[test]
    fn thumbnail_decodes_with_exif_orientation_applied() {
        let mut codec = FakeCodec::flat(1);
        codec.bitmap_size = size(500, 500);

        let _ = create_composer_image_thumbnail(&codec, &make_file(5, IMAGE_PNG));

        assert_eq!(*codec.log.decode_orientations.borrow(), [true]);
    }

    #[test]
    fn thumbnail_releases_the_decoded_image_when_encoding_fails() {
        let mut codec = FakeCodec::flat(1);
        codec.bitmap_size = size(500, 500);
        codec.encode_fails = true;

        assert_eq!(
            create_composer_image_thumbnail(&codec, &make_file(5, IMAGE_PNG)),
            None
        );
        assert_eq!(codec.log.closed.get(), 1);
    }

    // --- data URLs ----------------------------------------------------------

    #[test]
    fn data_url_to_file_decodes_a_captured_image_without_a_fetch_request() {
        let file = data_url_to_file("data:image/png;base64,AAEC/w==", "window.png", IMAGE_PNG)
            .expect("valid payload");
        assert_eq!(file.name, "window.png");
        assert_eq!(file.mime_type, IMAGE_PNG);
        assert_eq!(file.bytes.as_slice(), [0, 1, 2, 255]);
        assert_eq!(
            data_url_to_file("data:image/png;base64,!!", "x.png", IMAGE_PNG),
            Err(DataUrlError)
        );
    }

    // --- compress for stash ---------------------------------------------------

    #[test]
    fn keeps_the_exact_data_url_budget_boundary_and_original_bytes() {
        let codec = FakeCodec::flat(120_000);
        let original = make_file(3, IMAGE_PNG);
        let data_url = "data:image/png;base64,BwcH";

        let result = compress_image_for_stash(&codec, &original, data_url.len());

        let image = result.expect("fits");
        assert_eq!(image.data_url, data_url);
        assert!(!image.recompressed);
        assert_eq!(
            compress_image_for_stash(&codec, &original, data_url.len() - 1),
            Err(ImageCompressionFailureReason::TooLarge)
        );
    }

    #[test]
    fn stores_a_small_image_verbatim_without_re_encoding() {
        let codec = FakeCodec::flat(1);

        let image = compress_image_for_stash_default(&codec, &make_file(1024, IMAGE_PNG))
            .expect("small image");

        assert!(!image.recompressed);
        assert_eq!(image.mime_type, IMAGE_PNG);
        assert!(image.data_url.starts_with("data:image/png"));
        // Untouched payloads must not pay for a decode.
        assert_eq!(codec.log.decodes.get(), 0);
    }

    #[test]
    fn re_encodes_an_oversized_image_to_webp_within_the_budget() {
        // Comfortably under budget at the very first quality step.
        let codec = FakeCodec::flat(120_000);

        let image = compress_image_for_stash_default(&codec, &make_file(4_000_000, IMAGE_PNG))
            .expect("fits");

        assert!(image.recompressed);
        // createImageBitmap(file) defaults to imageOrientation "from-image".
        assert_eq!(*codec.log.decode_orientations.borrow(), [true]);
        assert_eq!(image.mime_type, IMAGE_WEBP);
        assert!(image.data_url.len() <= MAX_STASH_IMAGE_DATA_URL_CHARS);
        // sizeBytes describes the re-encoded payload, not the 4MB original.
        assert!(image.size_bytes < 4_000_000);
        // WebP keeps alpha, so no white matte is painted.
        assert_eq!(codec.log.fills.get(), 0);
        assert!(codec.log.closed.get() >= 1);
    }

    #[test]
    fn falls_back_to_jpeg_with_a_white_matte_when_webp_encoding_is_unavailable() {
        let mut codec = FakeCodec::flat(120_000);
        codec.supports_webp = false;

        let image = compress_image_for_stash_default(&codec, &make_file(4_000_000, IMAGE_PNG))
            .expect("fits");

        assert!(image.recompressed);
        assert_eq!(image.mime_type, IMAGE_JPEG);
        // JPEG has no alpha, so transparent regions must be matted white.
        assert!(codec.log.fills.get() > 0);
    }

    #[test]
    fn steps_quality_down_until_the_encoded_image_fits() {
        // Only the lowest quality step (0.68) lands under the budget.
        let codec = FakeCodec::new(|quality, _| if quality <= 0.68 { 400_000 } else { 3_000_000 });

        let image = compress_image_for_stash_default(&codec, &make_file(9_000_000, IMAGE_PNG))
            .expect("fits at the floor");

        assert!(image.recompressed);
        assert!(image.data_url.len() <= MAX_STASH_IMAGE_DATA_URL_CHARS);
        assert!(codec.log.closed.get() >= 1);
    }

    #[test]
    fn reports_too_large_when_even_the_smallest_encoding_overflows_the_budget() {
        let codec = FakeCodec::flat(8_000_000);

        let result = compress_image_for_stash_default(&codec, &make_file(9_000_000, IMAGE_PNG));

        assert_eq!(result, Err(ImageCompressionFailureReason::TooLarge));
        // The bitmap must still be released on the give-up path.
        assert!(codec.log.closed.get() >= 1);
    }

    #[test]
    fn reports_too_large_for_an_oversized_image_when_the_codec_cannot_re_encode() {
        let mut codec = FakeCodec::flat(1);
        codec.can_recompress = false;

        assert_eq!(
            compress_image_for_stash_default(&codec, &make_file(4_000_000, IMAGE_PNG)),
            Err(ImageCompressionFailureReason::TooLarge)
        );
    }

    #[test]
    fn reports_unreadable_when_the_image_fails_to_decode() {
        let mut codec = FakeCodec::flat(1);
        codec.decode_fails = true;

        assert_eq!(
            compress_image_for_stash_default(&codec, &make_file(4_000_000, IMAGE_PNG)),
            Err(ImageCompressionFailureReason::Unreadable)
        );
    }

    #[test]
    fn reports_unreadable_when_every_attempt_fails_to_encode() {
        let mut codec = FakeCodec::flat(1);
        codec.encode_fails = true;

        assert_eq!(
            compress_image_for_stash_default(&codec, &make_file(4_000_000, IMAGE_PNG)),
            Err(ImageCompressionFailureReason::Unreadable)
        );
    }

    #[test]
    fn shrinks_below_the_source_size_when_the_image_is_already_under_max_dimension() {
        // A small-but-heavy source (e.g. a dense PNG): only a real downscale
        // can get it under budget, since quality alone never suffices.
        let mut codec = FakeCodec::new(|_, surface| {
            if surface.width < 800 {
                100_000
            } else {
                5_000_000
            }
        });
        codec.bitmap_size = size(800, 600);

        let image = compress_image_for_stash_default(&codec, &make_file(4_000_000, IMAGE_PNG))
            .expect("a downscaled pass fits");

        assert!(image.recompressed);
        // Fallback passes must scale off the bitmap, not a fixed 2000px
        // ceiling that would never go below an 800px source.
        let smallest = codec.log.surfaces.borrow().iter().map(|s| s.width).min();
        assert!(smallest < Some(800));
    }

    // --- compress to a byte limit ------------------------------------------------

    #[test]
    fn encodes_with_webp_when_supported_and_jpeg_when_not() {
        for supports_webp in [true, false] {
            let mut codec = FakeCodec::flat(120_000);
            codec.supports_webp = supports_webp;

            let result = compress_image_to_byte_limit(
                &codec,
                &make_file(2_000_000, IMAGE_PNG),
                1_000_000,
                Default::default(),
            )
            .expect("fits");

            assert_eq!(
                result.file.mime_type,
                if supports_webp {
                    IMAGE_WEBP
                } else {
                    IMAGE_JPEG
                }
            );
            assert_eq!(result.file.size(), 120_000);
            assert_eq!(codec.log.closed.get(), 1);
        }
    }

    #[test]
    fn passes_small_files_through_byte_for_byte() {
        let codec = FakeCodec::flat(1);
        let original = make_file(1024, IMAGE_PNG);

        let result =
            compress_image_to_byte_limit(&codec, &original, 10 * 1024 * 1024, Default::default())
                .expect("fits");

        assert!(!result.recompressed);
        assert_eq!(result.file, original);
        assert!(Arc::ptr_eq(&result.file.bytes, &original.bytes));
        assert_eq!(codec.log.decodes.get(), 0);
    }

    #[test]
    fn re_encodes_an_oversized_file_under_the_byte_cap() {
        let codec = FakeCodec::flat(200_000);

        let result = compress_image_to_byte_limit(
            &codec,
            &make_file(2_000_000, IMAGE_PNG),
            1_000_000,
            Default::default(),
        )
        .expect("fits");

        assert!(result.recompressed);
        assert_eq!(result.file.mime_type, IMAGE_WEBP);
        // The re-encoded name must match the new container format.
        assert_eq!(result.file.name, "shot.webp");
        assert!(result.file.size() <= 1_000_000);
    }

    #[test]
    fn refuses_sources_above_the_decode_safety_ceiling() {
        let codec = FakeCodec::flat(1);

        let result = compress_image_to_byte_limit(
            &codec,
            &make_file(MAX_COMPRESSIBLE_SOURCE_BYTES as usize + 1, IMAGE_PNG),
            10 * 1024 * 1024,
            Default::default(),
        );

        assert_eq!(result, Err(ImageCompressionFailureReason::TooLarge));
        // The whole point of the ceiling is to never decode such a file.
        assert_eq!(codec.log.decodes.get(), 0);
    }

    #[test]
    fn reports_too_large_when_no_encoding_fits_the_byte_cap() {
        let codec = FakeCodec::flat(3_000_000);

        let result = compress_image_to_byte_limit(
            &codec,
            &make_file(2_000_000, IMAGE_PNG),
            1_000_000,
            Default::default(),
        );

        assert_eq!(result, Err(ImageCompressionFailureReason::TooLarge));
        assert!(codec.log.closed.get() >= 1);
    }

    #[test]
    fn a_file_without_a_name_is_named_image() {
        let codec = FakeCodec::flat(200_000);
        let mut file = make_file(2_000_000, IMAGE_PNG);
        file.name = String::new();

        let result = compress_image_to_byte_limit(&codec, &file, 1_000_000, Default::default())
            .expect("fits");

        assert_eq!(result.file.name, "image.webp");
    }

    // --- HEIC ----------------------------------------------------------------------

    #[test]
    fn recognizes_heic_and_heif_mime_types_and_case_insensitive_file_extensions() {
        let heic = |name: &str, mime: &str| is_heic_image_file(&PickedFile::new(name, mime, 0));
        assert!(heic("photo.bin", "image/heic"));
        assert!(heic("photo.bin", "image/heif"));
        assert!(!heic("photo.heic", "image/heic-sequence"));
        assert!(!heic("photo.heif", "image/heif-sequence"));
        assert!(heic("IMG_1234.HEIC", ""));
        assert!(heic("photo.heif", "application/octet-stream"));
        assert!(!heic("photo.png", "image/png"));
        assert!(!heic("photo.heic", "image/png"));
        assert!(!heic("photo.heif", "image/jpeg"));
    }

    #[test]
    fn converts_a_heic_photo_with_a_missing_mime_type_into_a_named_jpeg() {
        let codec = FakeCodec::flat(1);
        let heic = FakeHeic::returning(vec![4, 5, 6, 7]);
        let mut original = heic_file("IMG_1234.HEIC", "", 4000, 3000);
        original.last_modified = Some(123);

        let result = prepare_image_for_attachment(&codec, &heic, &original, 1024).expect("jpeg");

        assert_eq!(heic.quality.get(), 0.92);
        assert_eq!(result.file.name, "IMG_1234.jpg");
        assert_eq!(result.file.mime_type, IMAGE_JPEG);
        assert_eq!(result.file.size(), 4);
        assert_eq!(result.file.last_modified, Some(123));
        assert!(result.recompressed);
    }

    #[test]
    fn keeps_oversized_converted_photos_in_jpeg_format_while_shrinking_them() {
        let codec = FakeCodec::flat(200_000);
        let heic = FakeHeic::returning(vec![0; 2_000_000]);
        let original = heic_file("photo.heif", "image/heif", 4000, 3000);

        let result =
            prepare_image_for_attachment(&codec, &heic, &original, 1_000_000).expect("fits");

        assert_eq!(result.file.name, "photo.jpg");
        assert_eq!(result.file.mime_type, IMAGE_JPEG);
        assert!(result.file.size() <= 1_000_000);
        assert!(codec.log.fills.get() > 0);
    }

    #[test]
    fn compresses_jpeg_intermediates_above_the_source_safety_ceiling() {
        let codec = FakeCodec::flat(200_000);
        let heic = FakeHeic::returning(vec![0; MAX_COMPRESSIBLE_SOURCE_BYTES as usize + 1]);
        let original = heic_file("large.heic", "image/heic", 4000, 3000);

        let result =
            prepare_image_for_attachment(&codec, &heic, &original, 1_000_000).expect("fits");

        assert_eq!(result.file.name, "large.jpg");
        assert_eq!(result.file.mime_type, IMAGE_JPEG);
        assert!(result.file.size() <= 1_000_000);
        assert!(codec.log.closed.get() >= 1);
    }

    #[test]
    fn accepts_24_and_48_megapixel_heic_photos() {
        for (width, height) in [(5712, 4284), (8064, 6048)] {
            let codec = FakeCodec::flat(1);
            let heic = FakeHeic::returning(b"jpeg".to_vec());
            let original = heic_file("photo.heic", "image/heic", width, height);

            let result = prepare_image_for_attachment(&codec, &heic, &original, 1024).expect("ok");

            assert_eq!(result.file.mime_type, IMAGE_JPEG);
            assert_eq!(heic.calls.get(), 1);
        }
    }

    #[test]
    fn rejects_oversized_heic_dimensions_before_loading_the_decoder() {
        let codec = FakeCodec::flat(1);
        let heic = FakeHeic::returning(vec![1]);
        let original = heic_file("photo.heic", "image/heic", 16_000, 4001);

        assert_eq!(
            prepare_image_for_attachment(&codec, &heic, &original, 1024),
            Err(ImageCompressionFailureReason::TooLarge)
        );
        assert_eq!(heic.calls.get(), 0);
    }

    #[test]
    fn rejects_invalid_heic_metadata_before_loading_the_decoder() {
        let codec = FakeCodec::flat(1);
        let heic = FakeHeic::returning(vec![1]);
        let original = ImageFile::new("broken.heic", "image/heic", vec![1, 2, 3]);

        assert_eq!(
            prepare_image_for_attachment(&codec, &heic, &original, 1024),
            Err(ImageCompressionFailureReason::Unreadable)
        );
        assert_eq!(heic.calls.get(), 0);
    }

    #[test]
    fn rejects_heic_dimensions_of_zero() {
        let codec = FakeCodec::flat(1);
        let heic = FakeHeic::returning(vec![1]);
        let original = heic_file("photo.heic", "image/heic", 0, 3000);

        assert_eq!(
            prepare_image_for_attachment(&codec, &heic, &original, 1024),
            Err(ImageCompressionFailureReason::Unreadable)
        );
    }

    #[test]
    fn reports_unreadable_when_heic_decoding_fails() {
        let codec = FakeCodec::flat(1);
        let heic = FakeHeic::failing();
        let original = heic_file("broken.heic", "image/heic", 4000, 3000);

        assert_eq!(
            prepare_image_for_attachment(&codec, &heic, &original, 1024),
            Err(ImageCompressionFailureReason::Unreadable)
        );
    }

    #[test]
    fn rejects_unsafe_heic_sources_before_loading_the_decoder() {
        let codec = FakeCodec::flat(1);
        let heic = FakeHeic::returning(vec![1]);
        let original = ImageFile::new(
            "large.heic",
            "image/heic",
            vec![0; MAX_COMPRESSIBLE_SOURCE_BYTES as usize + 1],
        );

        assert_eq!(
            prepare_image_for_attachment(&codec, &heic, &original, 1024),
            Err(ImageCompressionFailureReason::TooLarge)
        );
        assert_eq!(heic.calls.get(), 0);
    }

    #[test]
    fn leaves_supported_images_untouched_without_loading_the_heic_decoder() {
        let codec = FakeCodec::flat(1);
        let heic = FakeHeic::returning(vec![1]);
        let original = make_file(1024, IMAGE_PNG);

        let result = prepare_image_for_attachment(&codec, &heic, &original, 2048).expect("ok");

        assert_eq!(result.file, original);
        assert!(!result.recompressed);
        assert_eq!(heic.calls.get(), 0);
    }

    #[test]
    fn the_heic_metadata_walk_handles_extended_and_open_ended_boxes() {
        // A 64-bit extended-size `meta` box and a size-0 (to end of file) `iprp`.
        let mut dimensions = vec![0u8; 12];
        dimensions[4..8].copy_from_slice(&4000u32.to_be_bytes());
        dimensions[8..12].copy_from_slice(&3000u32.to_be_bytes());
        let ispe = box_bytes("ispe", &[&dimensions]);
        let ipco = box_bytes("ipco", &[&ispe]);
        let mut iprp = vec![0, 0, 0, 0];
        iprp.extend_from_slice(b"iprp");
        iprp.extend_from_slice(&ipco);
        let meta_payload_len = 4 + iprp.len();
        let mut bytes = 1u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"meta");
        bytes.extend_from_slice(&((16 + meta_payload_len) as u64).to_be_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&iprp);
        let file = ImageFile::new("photo.heic", "image/heic", bytes);

        assert_eq!(validate_heic_image_dimensions(&file), None);
    }

    // --- max edge -----------------------------------------------------------------------

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend(13u32.to_be_bytes());
        bytes.extend(b"IHDR");
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 6, 0, 0, 0]);
        bytes
    }

    fn jpeg_header(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xc0];
        bytes.extend(17u16.to_be_bytes());
        bytes.push(8);
        bytes.extend(height.to_be_bytes());
        bytes.extend(width.to_be_bytes());
        bytes
    }

    fn image_with_header(header: Vec<u8>, name: &str, mime_type: &str) -> ImageFile {
        let mut bytes = header;
        bytes.extend(std::iter::repeat_n(7, 2048));
        ImageFile::new(name, mime_type, bytes)
    }

    #[test]
    fn scales_a_large_png_screenshot_under_the_byte_cap_to_2000px_and_keeps_it_a_png() {
        let mut codec = FakeCodec::flat(500_000);
        codec.bitmap_size = size(3024, 1964);
        let heic = FakeHeic::failing();
        let original = image_with_header(png_header(3024, 1964), "shot.png", IMAGE_PNG);

        let result =
            prepare_image_for_attachment(&codec, &heic, &original, 10 * 1024 * 1024).expect("ok");

        assert_eq!(*codec.log.decode_orientations.borrow(), [true]);
        assert_eq!(*codec.log.surfaces.borrow(), [size(2000, 1299)]);
        assert_eq!(*codec.log.encodes.borrow(), [(IMAGE_PNG.to_owned(), 0.92)]);
        // PNG keeps alpha, so nothing paints a matte behind it.
        assert_eq!(codec.log.fills.get(), 0);
        assert!(result.recompressed);
        assert_eq!(result.image_size, Some(size(2000, 1299)));
        assert_eq!(result.file.name, "shot.png");
        assert_eq!(result.file.mime_type, IMAGE_PNG);
    }

    #[test]
    fn scales_a_rotated_jpeg_by_its_displayed_size_and_keeps_it_a_jpeg() {
        // Stored landscape, displayed portrait after the EXIF rotation.
        let mut codec = FakeCodec::flat(500_000);
        codec.bitmap_size = size(3024, 4032);
        let heic = FakeHeic::failing();
        let original = image_with_header(jpeg_header(4032, 3024), "photo.jpg", IMAGE_JPEG);

        let result =
            prepare_image_for_attachment(&codec, &heic, &original, 10 * 1024 * 1024).expect("ok");

        assert_eq!(*codec.log.surfaces.borrow(), [size(1500, 2000)]);
        assert_eq!(*codec.log.encodes.borrow(), [(IMAGE_JPEG.to_owned(), 0.92)]);
        assert_eq!(result.file.mime_type, IMAGE_JPEG);
        assert_eq!(result.image_size, Some(size(1500, 2000)));
    }

    #[test]
    fn passes_images_that_fit_or_are_not_downscalable_through_without_decoding() {
        let gif_header = vec![0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0xb8, 0x0b, 0x10, 0x04];
        for (header, mime_type) in [
            (png_header(2000, 1200), IMAGE_PNG),
            (gif_header, "image/gif"),
        ] {
            let mut codec = FakeCodec::flat(500_000);
            codec.bitmap_size = size(3000, 1040);
            let heic = FakeHeic::failing();
            let original = image_with_header(header, "image", mime_type);

            let result = prepare_image_for_attachment(&codec, &heic, &original, 10 * 1024 * 1024)
                .expect("ok");

            assert_eq!(result.file, original);
            assert!(!result.recompressed);
            assert_eq!(codec.log.decodes.get(), 0);
        }
    }

    #[test]
    fn a_large_image_over_the_byte_cap_is_fitted_first_and_then_re_encoded() {
        // Fitting to 2000px yields 500k, still over the 400k cap: it is
        // re-encoded from the fitted copy, so the result is not a PNG any more.
        let mut codec = FakeCodec::new(|quality, _| if quality > 0.9 { 500_000 } else { 100_000 });
        codec.bitmap_size = size(3024, 1964);
        let original = image_with_header(png_header(3024, 1964), "shot.png", IMAGE_PNG);

        let result = compress_image_to_byte_limit(&codec, &original, 400_000, Default::default())
            .expect("fits");

        assert!(result.recompressed);
        assert!(result.file.size() <= 400_000);
        assert_eq!(result.file.mime_type, IMAGE_WEBP);
        assert_eq!(result.file.name, "shot.webp");
    }

    // --- misc ---------------------------------------------------------------------------------

    #[test]
    fn data_url_length_matches_the_encoded_url() {
        for len in [0usize, 1, 2, 3, 4, 100] {
            let bytes = vec![1u8; len];
            let url = read_file_as_data_url(IMAGE_PNG, &bytes);
            assert_eq!(data_url_length(IMAGE_PNG, len as u64), url.len(), "{len}");
        }
        assert_eq!(
            read_file_as_data_url("", b"x"),
            "data:application/octet-stream;base64,eA=="
        );
    }

    #[test]
    fn file_names_follow_the_encoded_container() {
        assert_eq!(file_name_for_mime_type("shot.png", IMAGE_WEBP), "shot.webp");
        assert_eq!(file_name_for_mime_type("shot.png", IMAGE_JPEG), "shot.jpg");
        assert_eq!(file_name_for_mime_type("noext", IMAGE_JPEG), "noext.jpg");
        assert_eq!(
            file_name_for_mime_type(".hidden", IMAGE_JPEG),
            ".hidden.jpg"
        );
        assert_eq!(png().size(), 0);
    }

    #[test]
    fn sips_arguments_ask_for_a_jpeg_at_the_requested_quality() {
        let args = SipsHeicConverter::arguments(
            0.92,
            std::path::Path::new("/tmp/in.heic"),
            std::path::Path::new("/tmp/out.jpg"),
        );
        assert_eq!(
            args,
            [
                "-s",
                "format",
                "jpeg",
                "-s",
                "formatOptions",
                "92",
                "/tmp/in.heic",
                "--out",
                "/tmp/out.jpg"
            ]
        );
    }

    /// A deterministic noise image: PNG-incompressible, so it needs a re-encode.
    fn noise_png(width: u32, height: u32) -> ImageFile {
        let mut state = 0x2545_f491_u32;
        let source = image::RgbaImage::from_fn(width, height, |_, _| {
            let mut next = || {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            };
            image::Rgba([next(), next(), next(), 255])
        });
        let mut encoded = Vec::new();
        source
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )
            .expect("png encodes");
        ImageFile::new("noise.png", IMAGE_PNG, encoded)
    }

    #[test]
    fn the_image_crate_codec_decodes_gif_bmp_and_webp_so_oversized_ones_can_shrink() {
        let source =
            image::RgbaImage::from_fn(24, 16, |x, y| image::Rgba([x as u8, y as u8, 9, 255]));
        for (format, name, mime) in [
            (image::ImageFormat::Gif, "a.gif", "image/gif"),
            (image::ImageFormat::Bmp, "a.bmp", "image/bmp"),
            (image::ImageFormat::WebP, "a.webp", IMAGE_WEBP),
        ] {
            let mut encoded = Vec::new();
            source
                .write_to(&mut std::io::Cursor::new(&mut encoded), format)
                .expect("encodes");
            let file = ImageFile::new(name, mime, encoded);
            let bitmap = ImageCrateCodec
                .decode(&file, true)
                .unwrap_or_else(|error| panic!("{name} decodes: {error:?}"));
            assert_eq!(ImageCrateCodec.bitmap_size(&bitmap), size(24, 16));
        }
    }

    #[test]
    fn the_image_crate_codec_decodes_and_encodes_png_and_jpeg_but_not_webp() {
        let codec = ImageCrateCodec;
        let file = noise_png(64, 32);

        let bitmap = codec.decode(&file, true).expect("png decodes");
        assert_eq!(codec.bitmap_size(&bitmap), size(64, 32));
        assert!(
            codec
                .decode(&ImageFile::new("x.png", IMAGE_PNG, vec![1, 2, 3]), false)
                .is_err()
        );

        let mut surface = codec
            .create_surface(size(32, 16))
            .expect("ok")
            .expect("surface");
        codec.fill_white(&mut surface);
        codec
            .draw(
                &mut surface,
                &bitmap,
                DrawSource {
                    x: 0.0,
                    y: 0.0,
                    width: 64.0,
                    height: 32.0,
                },
                size(32, 16),
            )
            .expect("draws");
        let jpeg = codec
            .encode(&surface, 0.92, IMAGE_JPEG)
            .expect("ok")
            .expect("jpeg");
        assert_eq!(&jpeg[..2], [0xff, 0xd8]);
        let png = codec
            .encode(&surface, 1.0, IMAGE_PNG)
            .expect("ok")
            .expect("png");
        assert_eq!(&png[..4], [0x89, 0x50, 0x4e, 0x47]);
        // No lossy WebP encoder: the pipeline takes upstream's JPEG fallback.
        assert_eq!(codec.encode(&surface, 0.92, IMAGE_WEBP).expect("ok"), None);
    }

    #[test]
    fn the_image_crate_codec_re_encodes_an_oversized_image_to_a_matted_jpeg_within_budget() {
        let file = noise_png(300, 300);
        let budget = 400_000;
        assert!(data_url_length(&file.mime_type, file.size()) > budget);

        let image = compress_image_for_stash(&ImageCrateCodec, &file, budget).expect("fits");

        assert!(image.recompressed);
        assert_eq!(image.mime_type, IMAGE_JPEG);
        assert!(image.data_url.len() <= budget);
        assert_eq!(image.image_size, Some(size(300, 300)));
    }
}
