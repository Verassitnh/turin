//! Port of upstream/apps/web/src/components/chat/ExpandedImagePreview.tsx,
//! ZoomableImage.tsx and ExpandedImageDialog.tsx.
//!
//! Blob URLs are an in-process table (`blob:turin/…`) so a revoked thumbnail
//! stops resolving, the same way `URL.revokeObjectURL` makes `fetch` fail.
//! Video bytes have no player here: the dialog shows the unavailable frame.

use std::sync::{Arc, LazyLock, Mutex};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Entity, Global, Image, ImageFormat, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, ScrollDelta, ScrollWheelEvent,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, img, px, rems,
};

use crate::composer::attachment_files::is_video_attachment;
use crate::state::helpers::media_reference::MediaReference;
use crate::state::helpers::media_source::{
    MediaAccess, MediaKind, ResolveMediaSourceInput, resolve_media_source,
};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Icon, IconName};

pub const MAX_ZOOM: f64 = 8.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentAsset {
    pub environment_id: String,
    pub attachment_id: String,
    pub file_name: String,
    pub mime_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaActionSource {
    pub kind: String,
    pub name: String,
    pub src: Option<String>,
    pub reference_url: Option<String>,
    pub asset: Option<AttachmentAsset>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExpandedImageItem {
    pub src: Option<String>,
    pub name: String,
    pub video: bool,
    pub original_url: Option<String>,
    pub bytes: Option<Arc<Vec<u8>>>,
    pub mime_type: Option<String>,
    pub actions_source: Option<MediaActionSource>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExpandedImagePreview {
    pub images: Vec<ExpandedImageItem>,
    pub index: usize,
}

/// `wrapExpandedImageIndex`.
pub fn wrap_expanded_image_index(index: i64, image_count: i64) -> i64 {
    if image_count > 0 {
        ((index % image_count) + image_count) % image_count
    } else {
        0
    }
}

pub fn expanded_image_key(preview: &ExpandedImagePreview) -> String {
    let item = preview.images.get(preview.index);
    let asset = item.and_then(|item| item.actions_source.as_ref()?.asset.as_ref());
    let head = item
        .and_then(|item| item.src.clone())
        .or_else(|| {
            asset.map(|asset| {
                format!(
                    "{}:{}:{}",
                    asset.environment_id, asset.attachment_id, asset.file_name
                )
            })
        })
        .unwrap_or_else(|| "image".into());
    format!("{head}:{}", preview.index)
}

/// `resolveProtocolRelativeMediaUrl`. `t3code:` and anything other than `http:`
/// become `https:`.
pub fn resolve_protocol_relative_media_url(src: &str, page_protocol: &str) -> String {
    if !src.starts_with("//") {
        return src.to_owned();
    }
    let protocol = if page_protocol == "http:" {
        "http:"
    } else {
        "https:"
    };
    format!("{protocol}{src}")
}

fn external_host(href: &str) -> Option<String> {
    let href = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_owned()
    };
    let rest = href
        .strip_prefix("https://")
        .or_else(|| href.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?.split('@').next_back()?;
    let host = host.split(':').next()?.to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

pub struct MarkdownMediaPreviewInput<'a> {
    pub source: &'a str,
    pub page_protocol: &'a str,
}

/// Direct media only. An environment asset still has to be signed by the host;
/// this returns the error upstream throws when the environment is disconnected.
pub fn resolve_markdown_media_preview(
    input: MarkdownMediaPreviewInput<'_>,
) -> Result<ExpandedImagePreview, String> {
    let media = resolve_media_source(input.source, &ResolveMediaSourceInput::default())
        .ok_or_else(|| "Reconnect to this environment and open the media again.".to_owned())?;
    let MediaAccess::Direct { uri } = &media.access else {
        return Err("Reconnect to this environment and open the media again.".into());
    };
    let src = resolve_protocol_relative_media_url(uri, input.page_protocol);
    let original_url = external_host(uri).map(|_| uri.clone());
    let reference_url = match &media.reference {
        Some(MediaReference::Url(url)) => Some(url.url.clone()),
        _ => None,
    };
    Ok(ExpandedImagePreview {
        images: vec![ExpandedImageItem {
            src: Some(src.clone()),
            name: media.name.clone(),
            video: media.kind == MediaKind::Video,
            original_url,
            bytes: None,
            mime_type: Some(media.mime_type),
            actions_source: Some(MediaActionSource {
                kind: media.kind.as_str().into(),
                name: media.name,
                src: Some(src),
                reference_url,
                asset: None,
            }),
        }],
        index: 0,
    })
}

#[derive(Clone, Debug)]
pub struct PersistedVideoAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub preview_url: Option<String>,
    pub downloadable: bool,
}

/// `buildAttachmentVideoPreview`.
pub fn build_attachment_video_preview(
    environment_id: &str,
    attachment: &PersistedVideoAttachment,
) -> Option<ExpandedImagePreview> {
    if !is_video_attachment(&attachment.name, &attachment.mime_type) {
        return None;
    }
    let asset = attachment.downloadable.then(|| AttachmentAsset {
        environment_id: environment_id.to_owned(),
        attachment_id: attachment.id.clone(),
        file_name: attachment.name.clone(),
        mime_type: attachment.mime_type.clone(),
    });
    if attachment.preview_url.is_none() && asset.is_none() {
        return None;
    }
    Some(ExpandedImagePreview {
        images: vec![ExpandedImageItem {
            src: attachment.preview_url.clone(),
            name: attachment.name.clone(),
            video: true,
            original_url: None,
            bytes: None,
            mime_type: Some(attachment.mime_type.clone()),
            actions_source: Some(MediaActionSource {
                kind: "video".into(),
                name: attachment.name.clone(),
                src: attachment.preview_url.clone(),
                reference_url: None,
                asset,
            }),
        }],
        index: 0,
    })
}

#[derive(Clone, Debug)]
pub struct PreviewAttachment {
    pub id: String,
    pub name: String,
    /// `"image"` or `"file"`.
    pub kind: String,
    pub mime_type: String,
    pub preview_url: Option<String>,
    pub bytes: Option<Vec<u8>>,
}

/// `buildExpandedImagePreview`. A local video becomes one `blob:` item.
pub fn build_expanded_image_preview(
    images: &[PreviewAttachment],
    selected_id: &str,
) -> Option<ExpandedImagePreview> {
    let selected = images.iter().find(|image| image.id == selected_id)?;
    if selected.kind == "file"
        && selected.bytes.is_some()
        && is_video_attachment(&selected.name, &selected.mime_type)
    {
        let bytes = selected.bytes.clone().unwrap_or_default();
        return Some(ExpandedImagePreview {
            images: vec![ExpandedImageItem {
                src: Some(create_object_url(&bytes)),
                name: selected.name.clone(),
                video: true,
                original_url: None,
                bytes: Some(Arc::new(bytes)),
                mime_type: Some(selected.mime_type.clone()),
                actions_source: None,
            }],
            index: 0,
        });
    }
    let previewable: Vec<_> = images
        .iter()
        .filter(|image| image.kind == "image" && image.preview_url.is_some())
        .collect();
    if previewable.is_empty() {
        return None;
    }
    let index = previewable
        .iter()
        .position(|image| image.id == selected_id)?;
    Some(ExpandedImagePreview {
        images: previewable
            .into_iter()
            .map(|image| ExpandedImageItem {
                src: image.preview_url.clone(),
                name: image.name.clone(),
                video: false,
                original_url: None,
                bytes: image.bytes.clone().map(Arc::new),
                mime_type: Some(image.mime_type.clone()),
                actions_source: None,
            })
            .collect(),
        index,
    })
}

struct BlobStore {
    next: u64,
    live: std::collections::HashMap<String, Vec<u8>>,
}

fn blobs() -> &'static Mutex<BlobStore> {
    static BLOBS: LazyLock<Mutex<BlobStore>> = LazyLock::new(|| {
        Mutex::new(BlobStore {
            next: 0,
            live: std::collections::HashMap::new(),
        })
    });
    &BLOBS
}

pub fn create_object_url(bytes: &[u8]) -> String {
    let mut store = blobs().lock().expect("blob store");
    store.next += 1;
    let url = format!("blob:turin/{}", store.next);
    store.live.insert(url.clone(), bytes.to_vec());
    url
}

pub fn revoke_object_url(url: &str) {
    if let Ok(mut store) = blobs().lock() {
        store.live.remove(url);
    }
}

pub fn object_url_bytes(url: &str) -> Option<Vec<u8>> {
    blobs()
        .lock()
        .ok()
        .and_then(|store| store.live.get(url).cloned())
}

pub struct VideoElement {
    pub src: String,
}

/// `attachVideoThumbnail`.
pub fn attach_video_thumbnail(video: &mut VideoElement, bytes: &[u8]) -> impl FnOnce() + use<> {
    let url = create_object_url(bytes);
    video.src = url.clone();
    move || revoke_object_url(&url)
}

/// Width and height caps for the image stage (`92vw` minus the dialog gutter, `86vh` / `100vh - 10rem`).
pub fn stage_limits(column_w: f64, column_h: f64) -> (f64, f64) {
    let cap_h = (column_h * 0.86).min(column_h - 160.0).max(1.0);
    let cap_w = (column_w * 0.92 - if column_w >= 640.0 { 96.0 } else { 0.0 }).max(1.0);
    (cap_w, cap_h)
}

pub fn fit_scale(window_w: f64, window_h: f64, natural_w: f64, natural_h: f64) -> f64 {
    let (cap_w, cap_h) = stage_limits(window_w, window_h);
    1.0_f64
        .min(cap_w / natural_w.max(1.0))
        .min(cap_h / natural_h.max(1.0))
}

/// The stage is the fitted image, then capped by the column. At zoom 1 the view
/// matches the image so a centered frame pins the close button to that image.
pub struct StageBox {
    pub view_w: f64,
    pub view_h: f64,
    pub content_w: f64,
    pub content_h: f64,
    pub fit: f64,
    pub cap_w: f64,
    pub cap_h: f64,
}

pub fn stage_box(
    column_w: f64,
    column_h: f64,
    natural_w: f64,
    natural_h: f64,
    zoom: f64,
) -> StageBox {
    let (cap_w, cap_h) = stage_limits(column_w, column_h);
    let fit = fit_scale(column_w, column_h, natural_w, natural_h);
    let zoom = clamp_zoom(zoom);
    let content_w = natural_w * fit * zoom;
    let content_h = natural_h * fit * zoom;
    StageBox {
        view_w: content_w.min(cap_w).max(1.0),
        view_h: content_h.min(cap_h).max(1.0),
        content_w,
        content_h,
        fit,
        cap_w,
        cap_h,
    }
}

/// Scroll that keeps one image point under a window coordinate when the stage recenters.
pub fn anchored_scroll(
    scroll: f64,
    cursor: f64,
    origin: f64,
    previous_zoom: f64,
    next_zoom: f64,
    new_origin: f64,
    max_scroll: f64,
) -> f64 {
    let local = cursor - origin;
    let image = (scroll + local) / previous_zoom.max(f64::EPSILON);
    let next_local = cursor - new_origin;
    (image * next_zoom - next_local).clamp(0.0, max_scroll.max(0.0))
}

/// Arrow keys pan when the axis overflows. Otherwise a horizontal key steps a gallery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowAction {
    Pan,
    Step,
    Pass,
}

pub fn arrow_action(can_pan: bool, image_count: usize, horizontal: bool) -> ArrowAction {
    if can_pan {
        ArrowAction::Pan
    } else if horizontal && image_count > 1 {
        ArrowAction::Step
    } else {
        ArrowAction::Pass
    }
}

pub fn clamp_zoom(next: f64) -> f64 {
    next.clamp(1.0, MAX_ZOOM)
}

/// `deltaMode`: 0 pixels, 1 lines (`* 16`), 2 pages (`* clientHeight`).
pub fn wheel_zoom(zoom: f64, delta_y: f64, delta_mode: u8, client_height: f64, ctrl: bool) -> f64 {
    if delta_y == 0.0 {
        return zoom;
    }
    let scaled = delta_y
        * match delta_mode {
            1 => 16.0,
            2 => client_height,
            _ => 1.0,
        };
    clamp_zoom(zoom * (-scaled * if ctrl { 0.01 } else { 0.002 }).exp())
}

pub fn toggle_zoom(zoom: f64) -> f64 {
    if zoom > 1.0 { 1.0 } else { 2.0 }
}

/// Arrow pan of 40px. `None` when the axis does not overflow, so gallery keys still move.
pub fn pan_arrow(
    zoom: f64,
    content: f64,
    viewport: f64,
    scroll: f64,
    backward: bool,
) -> Option<f64> {
    if zoom <= 1.0 || content <= viewport {
        return None;
    }
    let delta = if backward { -40.0 } else { 40.0 };
    Some((scroll + delta).clamp(0.0, (content - viewport).max(0.0)))
}

pub fn png_pixel_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    positive_size(
        u32::from_be_bytes(bytes[16..20].try_into().ok()?),
        u32::from_be_bytes(bytes[20..24].try_into().ok()?),
    )
}

fn positive_size(width: u32, height: u32) -> Option<(u32, u32)> {
    (width > 0 && height > 0).then_some((width, height))
}

/// Intrinsic pixel size from the image header. PNG, JPEG, GIF, WebP, and SVG.
pub fn image_pixel_size(bytes: &[u8]) -> Option<(u32, u32)> {
    png_pixel_size(bytes)
        .or_else(|| gif_pixel_size(bytes))
        .or_else(|| jpeg_pixel_size(bytes))
        .or_else(|| webp_pixel_size(bytes))
        .or_else(|| svg_pixel_size(bytes))
}

fn gif_pixel_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 10 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return None;
    }
    positive_size(
        u16::from_le_bytes([bytes[6], bytes[7]]) as u32,
        u16::from_le_bytes([bytes[8], bytes[9]]) as u32,
    )
}

fn jpeg_pixel_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut index = 2usize;
    while index + 1 < bytes.len() {
        if bytes[index] != 0xFF {
            return None;
        }
        while index < bytes.len() && bytes[index] == 0xFF {
            index += 1;
        }
        if index >= bytes.len() {
            return None;
        }
        let marker = bytes[index];
        index += 1;
        // Standalone markers: SOI, EOI, RSTn.
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if index + 1 >= bytes.len() {
            return None;
        }
        let len = u16::from_be_bytes([bytes[index], bytes[index + 1]]) as usize;
        if len < 2 || index + len > bytes.len() {
            return None;
        }
        let sof = matches!(
            marker,
            0xC0 | 0xC1
                | 0xC2
                | 0xC3
                | 0xC5
                | 0xC6
                | 0xC7
                | 0xC9
                | 0xCA
                | 0xCB
                | 0xCD
                | 0xCE
                | 0xCF
        );
        if sof && len >= 7 {
            return positive_size(
                u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]) as u32,
                u16::from_be_bytes([bytes[index + 3], bytes[index + 4]]) as u32,
            );
        }
        if marker == 0xDA {
            return None;
        }
        index += len;
    }
    None
}

fn webp_pixel_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 16 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let chunk = &bytes[12..16];
    if chunk == b"VP8X" && bytes.len() >= 30 {
        let width =
            1 + u32::from(bytes[24]) + (u32::from(bytes[25]) << 8) + (u32::from(bytes[26]) << 16);
        let height =
            1 + u32::from(bytes[27]) + (u32::from(bytes[28]) << 8) + (u32::from(bytes[29]) << 16);
        return positive_size(width, height);
    }
    if chunk == b"VP8 " && bytes.len() >= 30 && &bytes[23..26] == b"\x9d\x01\x2a" {
        let width = u16::from_le_bytes([bytes[26], bytes[27]]) as u32 & 0x3fff;
        let height = u16::from_le_bytes([bytes[28], bytes[29]]) as u32 & 0x3fff;
        return positive_size(width, height);
    }
    if chunk == b"VP8L" && bytes.len() >= 25 && bytes[20] == 0x2f {
        let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
        return positive_size((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1);
    }
    None
}

fn svg_pixel_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(bytes).ok()?.to_ascii_lowercase();
    if !text.contains("<svg") {
        return None;
    }
    if let Some(size) = svg_view_box(&text) {
        return Some(size);
    }
    let width = svg_attr(&text, "width").and_then(svg_number)?;
    let height = svg_attr(&text, "height").and_then(svg_number)?;
    positive_size(width, height)
}

fn svg_view_box(text: &str) -> Option<(u32, u32)> {
    let (start, quote) = text
        .find("viewbox=\"")
        .map(|index| (index + "viewbox=\"".len(), '"'))
        .or_else(|| {
            text.find("viewbox='")
                .map(|index| (index + "viewbox='".len(), '\''))
        })?;
    let end = start + text[start..].find(quote)?;
    let mut parts = text[start..end].split_whitespace();
    let _x = parts.next()?;
    let _y = parts.next()?;
    positive_size(svg_number(parts.next()?)?, svg_number(parts.next()?)?)
}

fn svg_attr<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let pattern = format!("{name}=\"");
    let mut rest = text;
    while let Some(index) = rest.find(&pattern) {
        let boundary = index == 0 || rest.as_bytes()[index - 1].is_ascii_whitespace();
        if boundary {
            let value = &rest[index + pattern.len()..];
            let end = value.find('"')?;
            return Some(&value[..end]);
        }
        rest = &rest[index + pattern.len()..];
    }
    None
}

fn svg_number(value: &str) -> Option<u32> {
    let number = value.trim_end_matches("px");
    if number.contains('%') {
        return None;
    }
    let parsed: f64 = number.parse().ok()?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return None;
    }
    Some(parsed.round() as u32)
}

fn image_format(mime: &str) -> Option<ImageFormat> {
    Some(match mime {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::Webp,
        "image/gif" => ImageFormat::Gif,
        "image/svg+xml" => ImageFormat::Svg,
        _ => return None,
    })
}

struct ZoomState {
    zoom: f64,
    scroll_x: f64,
    scroll_y: f64,
    drag: Option<(f64, f64, f64, f64)>,
    moved: f64,
    image_index: Option<usize>,
    origin_x: f64,
    origin_y: f64,
    base_w: f64,
    base_h: f64,
    cap_w: f64,
    cap_h: f64,
    content_w: f64,
    content_h: f64,
    view_w: f64,
    view_h: f64,
}

impl Default for ZoomState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            drag: None,
            moved: 0.0,
            image_index: None,
            origin_x: 0.0,
            origin_y: 0.0,
            base_w: 1.0,
            base_h: 1.0,
            cap_w: 1.0,
            cap_h: 1.0,
            content_w: 1.0,
            content_h: 1.0,
            view_w: 1.0,
            view_h: 1.0,
        }
    }
}

#[derive(Clone)]
pub struct ZoomHandle {
    state: Entity<ZoomState>,
}

impl ZoomHandle {
    pub fn pan(&self, key: &str, cx: &mut App) -> bool {
        let mut panned = false;
        self.state.update(cx, |state, cx| {
            let vertical = key == "up" || key == "down";
            let backward = key == "left" || key == "up";
            let (content, view, scroll) = if vertical {
                (state.content_h, state.view_h, state.scroll_y)
            } else {
                (state.content_w, state.view_w, state.scroll_x)
            };
            if let Some(next) = pan_arrow(state.zoom, content, view, scroll, backward) {
                if vertical {
                    state.scroll_y = next;
                } else {
                    state.scroll_x = next;
                }
                panned = true;
                cx.notify();
            }
        });
        panned
    }
}

#[derive(Default)]
struct ImageOverlayState {
    column: Option<(f64, f64)>,
    zoom: Option<ZoomHandle>,
}

impl Global for ImageOverlayState {}

pub fn pan_open_zoom(key: &str, cx: &mut App) -> bool {
    let Some(handle) = cx.default_global::<ImageOverlayState>().zoom.clone() else {
        return false;
    };
    handle.pan(key, cx)
}

pub fn clear_open_zoom(cx: &mut App) {
    cx.default_global::<ImageOverlayState>().zoom = None;
}

fn column_size(window: &Window, cx: &mut App) -> (f64, f64) {
    if let Some(size) = cx.default_global::<ImageOverlayState>().column {
        return size;
    }
    let viewport = window.viewport_size();
    (f64::from(viewport.width), f64::from(viewport.height))
}

fn remember_column(bounds: gpui_kit::Bounds<gpui_kit::Pixels>, cx: &mut App) {
    let next = (f64::from(bounds.size.width), f64::from(bounds.size.height));
    if next.0 < 1.0 || next.1 < 1.0 {
        return;
    }
    let changed = cx
        .default_global::<ImageOverlayState>()
        .column
        .map(|current| (current.0 - next.0).abs() > 0.5 || (current.1 - next.1).abs() > 0.5)
        .unwrap_or(true);
    if changed {
        cx.default_global::<ImageOverlayState>().column = Some(next);
        cx.refresh_windows();
    }
}

fn apply_anchored_zoom(state: &mut ZoomState, next: f64, cursor: Option<(f64, f64)>) {
    let previous = state.zoom.max(f64::EPSILON);
    let next = clamp_zoom(next);
    if (previous - next).abs() < 1e-12 {
        return;
    }
    let (cursor_x, cursor_y) = cursor.unwrap_or((
        state.origin_x + state.view_w / 2.0,
        state.origin_y + state.view_h / 2.0,
    ));
    let new_view_w = (state.base_w * next).min(state.cap_w).max(1.0);
    let new_view_h = (state.base_h * next).min(state.cap_h).max(1.0);
    let new_origin_x = state.origin_x + (state.view_w - new_view_w) / 2.0;
    let new_origin_y = state.origin_y + (state.view_h - new_view_h) / 2.0;
    let max_x = (state.base_w * next - new_view_w).max(0.0);
    let max_y = (state.base_h * next - new_view_h).max(0.0);
    state.scroll_x = anchored_scroll(
        state.scroll_x,
        cursor_x,
        state.origin_x,
        previous,
        next,
        new_origin_x,
        max_x,
    );
    state.scroll_y = anchored_scroll(
        state.scroll_y,
        cursor_y,
        state.origin_y,
        previous,
        next,
        new_origin_y,
        max_y,
    );
    state.zoom = next;
    state.view_w = new_view_w;
    state.view_h = new_view_h;
    state.content_w = state.base_w * next;
    state.content_h = state.base_h * next;
    state.origin_x = new_origin_x;
    state.origin_y = new_origin_y;
}

/// `ZoomableImage`. Returns the stage element and the view box the frame should match.
pub fn zoomable_image(
    id: impl Into<gpui_kit::ElementId>,
    item: &ExpandedImageItem,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) -> (AnyElement, f64, f64) {
    let id = id.into();
    let state = window.use_keyed_state(id.clone(), cx, |_, _| ZoomState::default());
    let theme = cx.app_theme().clone();
    let (column_w, column_h) = column_size(window, cx);
    let (natural_w, natural_h) = item
        .bytes
        .as_ref()
        .and_then(|bytes| image_pixel_size(bytes))
        .map(|(w, h)| (w as f64, h as f64))
        .unwrap_or((640.0, 480.0));
    let mut zoom = state.read(cx).zoom;
    let mut scroll_x = state.read(cx).scroll_x;
    let mut scroll_y = state.read(cx).scroll_y;
    if state.read(cx).image_index != Some(index) {
        zoom = 1.0;
        scroll_x = 0.0;
        scroll_y = 0.0;
    }
    let stage = stage_box(column_w, column_h, natural_w, natural_h, zoom);
    state.update(cx, |state, _| {
        if state.image_index != Some(index) {
            state.drag = None;
            state.moved = 0.0;
        }
        state.image_index = Some(index);
        state.zoom = zoom;
        state.scroll_x = scroll_x;
        state.scroll_y = scroll_y;
        state.base_w = natural_w * stage.fit;
        state.base_h = natural_h * stage.fit;
        state.cap_w = stage.cap_w;
        state.cap_h = stage.cap_h;
        state.content_w = stage.content_w;
        state.content_h = stage.content_h;
        state.view_w = stage.view_w;
        state.view_h = stage.view_h;
    });
    cx.default_global::<ImageOverlayState>().zoom = Some(ZoomHandle {
        state: state.clone(),
    });
    let view_w = stage.view_w;
    let view_h = stage.view_h;
    let width = stage.content_w;
    let height = stage.content_h;
    let dragging = state.read(cx).drag.is_some();
    let picture = item.bytes.as_ref().and_then(|bytes| {
        image_format(item.mime_type.as_deref().unwrap_or("image/png"))
            .map(|format| Arc::new(Image::from_bytes(format, bytes.as_ref().clone())))
    });
    let state_wheel = state.clone();
    let state_click = state.clone();
    let state_down = state.clone();
    let state_move = state.clone();
    let state_up = state.clone();
    let state_key = state.clone();
    let state_bounds = state.clone();
    let element = div()
        .id(id)
        .relative()
        .w(px(view_w as f32))
        .h(px(view_h as f32))
        .overflow_hidden()
        .rounded(RADIUS_LG)
        .bg(theme.background)
        .border_1()
        .border_color(with_alpha(theme.border, 0.7))
        .cursor(if zoom > 1.0 {
            if dragging {
                gpui_kit::CursorStyle::ClosedHand
            } else {
                gpui_kit::CursorStyle::OpenHand
            }
        } else {
            gpui_kit::CursorStyle::PointingHand
        })
        .child(
            canvas(
                move |bounds, _, cx| {
                    state_bounds.update(cx, |state, _| {
                        state.origin_x = f64::from(bounds.origin.x);
                        state.origin_y = f64::from(bounds.origin.y);
                    });
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .on_scroll_wheel(move |event: &ScrollWheelEvent, _, cx| {
            let (delta_y, mode) = match event.delta {
                ScrollDelta::Pixels(point) => (f64::from(point.y), 0u8),
                ScrollDelta::Lines(point) => (f64::from(point.y), 1u8),
            };
            let cursor = (f64::from(event.position.x), f64::from(event.position.y));
            let ctrl = event.modifiers.control;
            state_wheel.update(cx, |state, cx| {
                let next = wheel_zoom(state.zoom, delta_y, mode, state.view_h, ctrl);
                apply_anchored_zoom(state, next, Some(cursor));
                cx.notify();
            });
            cx.stop_propagation();
        })
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            state_down.update(cx, |state, _| {
                state.drag = Some((
                    f64::from(event.position.x),
                    f64::from(event.position.y),
                    state.scroll_x,
                    state.scroll_y,
                ));
                state.moved = 0.0;
            });
        })
        .on_mouse_move(move |event, _, cx| {
            state_move.update(cx, |state, cx| {
                let Some((x, y, left, top)) = state.drag else {
                    return;
                };
                if state.zoom <= 1.0 {
                    return;
                }
                let dx = f64::from(event.position.x) - x;
                let dy = f64::from(event.position.y) - y;
                state.moved = state.moved.max((dx * dx + dy * dy).sqrt());
                state.scroll_x = (left - dx).clamp(0.0, (state.content_w - state.view_w).max(0.0));
                state.scroll_y = (top - dy).clamp(0.0, (state.content_h - state.view_h).max(0.0));
                cx.notify();
            });
        })
        .on_mouse_up(MouseButton::Left, move |event, _, cx| {
            let cursor = (f64::from(event.position.x), f64::from(event.position.y));
            state_up.update(cx, |state, cx| {
                let suppress = state.moved > 4.0;
                state.drag = None;
                if !suppress {
                    apply_anchored_zoom(state, toggle_zoom(state.zoom), Some(cursor));
                }
                state.moved = 0.0;
                cx.notify();
            });
        })
        .on_click(move |_, _, cx| {
            let _ = &state_click;
            cx.stop_propagation();
        })
        .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
            if event.keystroke.modifiers.control
                || event.keystroke.modifiers.platform
                || event.keystroke.modifiers.alt
            {
                return;
            }
            let key = event.keystroke.key.as_str();
            state_key.update(cx, |state, cx| {
                match key {
                    "enter" | "space" => {
                        apply_anchored_zoom(state, toggle_zoom(state.zoom), None);
                    }
                    "+" | "=" => apply_anchored_zoom(state, state.zoom * 1.5, None),
                    "-" => apply_anchored_zoom(state, state.zoom / 1.5, None),
                    "0" => apply_anchored_zoom(state, 1.0, None),
                    _ => return,
                }
                cx.notify();
            });
        })
        .child(
            div()
                .ml(px(-scroll_x as f32))
                .mt(px(-scroll_y as f32))
                .child(match picture {
                    Some(picture) => img(picture)
                        .w(px(width as f32))
                        .h(px(height as f32))
                        .into_any_element(),
                    None => div()
                        .w(px(width as f32))
                        .h(px(height as f32))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(TEXT_SM.size)
                        .text_color(theme.muted_foreground)
                        .child(item.name.clone())
                        .into_any_element(),
                }),
        )
        .into_any_element();
    (element, view_w, view_h)
}

/// `ExpandedImageDialog`. The media-close and media-navigation buttons are
/// painted here; the shared button table does not include those variants.
pub fn expanded_image_dialog(
    preview: &ExpandedImagePreview,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
    on_step: impl Fn(i64, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.app_theme().clone();
    let item = preview
        .images
        .get(preview.index)
        .cloned()
        .unwrap_or_else(|| ExpandedImageItem {
            src: None,
            name: "Image".into(),
            video: false,
            original_url: None,
            bytes: None,
            mime_type: None,
            actions_source: None,
        });
    let count = preview.images.len();
    let on_close = std::rc::Rc::new(on_close);
    let dismiss = on_close.clone();
    let step = std::rc::Rc::new(on_step);
    let step_back = step.clone();
    let (body, frame_w, frame_h) = if item.video {
        clear_open_zoom(cx);
        (video_unavailable(&theme, &item.name), 480.0, 270.0)
    } else {
        zoomable_image("expanded-zoom", &item, preview.index, window, cx)
    };
    div()
        .id("expanded-image-dialog")
        .relative()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(with_alpha(
            theme.background,
            if theme.is_dark() { 0.64 } else { 0.6 },
        ))
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| dismiss(window, cx))
        .child(
            canvas(|bounds, _, cx| remember_column(bounds, cx), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
        .child(
            div()
                .id("expanded-image-frame")
                .relative()
                .w(px(frame_w as f32))
                .h(px(frame_h as f32))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(body)
                .child(media_close(on_close))
                .when(count > 1, |this| {
                    this.child(media_nav("expanded-prev", true, move |window, cx| {
                        step_back(-1, window, cx)
                    }))
                    .child(media_nav(
                        "expanded-next",
                        false,
                        move |window, cx| step(1, window, cx),
                    ))
                }),
        )
        .into_any_element()
}

fn video_unavailable(_theme: &AppTheme, name: &str) -> AnyElement {
    div()
        .w(px(480.))
        .h(px(270.))
        .rounded(RADIUS_LG)
        .bg(gpui_kit::black())
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(rems(0.5))
        .text_color(gpui_kit::white())
        .child(
            Icon::new(IconName::Play)
                .size(rems(1.5))
                .color(gpui_kit::white()),
        )
        .child(
            div()
                .text_size(TEXT_SM.size)
                .child("Video playback is not available"),
        )
        .child(
            div()
                .text_size(TEXT_XS.size)
                .opacity(0.7)
                .child(name.to_owned()),
        )
        .into_any_element()
}

type MediaAction = std::rc::Rc<dyn Fn(&mut Window, &mut App)>;

fn media_close(on_close: MediaAction) -> AnyElement {
    div()
        .id("expanded-image-close")
        .absolute()
        .top_0()
        .right_0()
        .flex()
        .size(rems(1.5))
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(with_alpha(gpui_kit::black(), 0.65))
        .border_1()
        .border_color(with_alpha(gpui_kit::white(), 0.2))
        .cursor(gpui_kit::CursorStyle::PointingHand)
        .child(
            Icon::new(IconName::X)
                .size(rems(0.75))
                .color(gpui_kit::white()),
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| on_close(window, cx))
        .into_any_element()
}

fn media_nav(
    id: &'static str,
    left: bool,
    on_step: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .absolute()
        .top(rems(0.))
        .bottom(rems(0.))
        .when(left, |this| this.left(rems(0.5)))
        .when(!left, |this| this.right(rems(0.5)))
        .flex()
        .items_center()
        .child(
            div()
                .flex()
                .size(rems(2.))
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(with_alpha(gpui_kit::black(), 0.35))
                .cursor(gpui_kit::CursorStyle::PointingHand)
                .child(
                    Icon::new(if left {
                        IconName::ChevronLeft
                    } else {
                        IconName::ChevronRight
                    })
                    .size(rems(1.))
                    .color(gpui_kit::white()),
                ),
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| on_step(window, cx))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_protocol_relative_media_without_changing_its_source() {
        for (page, media) in [
            ("t3code:", "https:"),
            ("t3code-dev:", "https:"),
            ("http:", "http:"),
            ("https:", "https:"),
        ] {
            let source = "//cdn.example.com/recording.mp4?token=a%2FB&v=1#t=2";
            let preview = resolve_markdown_media_preview(MarkdownMediaPreviewInput {
                source,
                page_protocol: page,
            })
            .unwrap();
            let item = &preview.images[0];
            assert_eq!(
                item.src.as_deref(),
                Some(format!("{media}{source}").as_str())
            );
            assert_eq!(item.original_url.as_deref(), Some(source));
            let actions = item.actions_source.as_ref().unwrap();
            assert_eq!(
                actions.src.as_deref(),
                Some(format!("{media}{source}").as_str())
            );
            assert_eq!(actions.reference_url.as_deref(), Some(source));
        }
    }

    #[test]
    fn builds_a_signed_asset_preview_for_a_persisted_video_attachment() {
        let preview = build_attachment_video_preview(
            "environment-1",
            &PersistedVideoAttachment {
                id: "attachment-video-1".into(),
                name: "demo.mp4".into(),
                mime_type: "video/mp4".into(),
                preview_url: None,
                downloadable: true,
            },
        )
        .unwrap();
        assert_eq!(preview.index, 0);
        let item = &preview.images[0];
        assert!(item.src.is_none());
        assert!(item.video);
        assert_eq!(item.name, "demo.mp4");
        let asset = item
            .actions_source
            .as_ref()
            .unwrap()
            .asset
            .as_ref()
            .unwrap();
        assert_eq!(asset.environment_id, "environment-1");
        assert_eq!(asset.attachment_id, "attachment-video-1");
        assert_eq!(asset.file_name, "demo.mp4");
        assert_eq!(asset.mime_type, "video/mp4");
    }

    #[test]
    fn builds_a_video_preview_for_a_local_video_attachment() {
        let attachment = PreviewAttachment {
            id: "video-1".into(),
            name: "demo.mp4".into(),
            kind: "file".into(),
            mime_type: "video/mp4".into(),
            preview_url: None,
            bytes: Some(vec![1, 2, 3]),
        };
        let preview =
            build_expanded_image_preview(std::slice::from_ref(&attachment), &attachment.id)
                .unwrap();
        assert_eq!(preview.index, 0);
        assert_eq!(preview.images[0].name, "demo.mp4");
        assert!(preview.images[0].video);
        let src = preview.images[0].src.as_deref().unwrap();
        assert!(src.starts_with("blob:"));
        assert!(object_url_bytes(src).is_some());
        revoke_object_url(src);
    }

    #[test]
    fn releases_a_video_thumbnail_url_when_detached() {
        let mut video = VideoElement { src: String::new() };
        let detach = attach_video_thumbnail(&mut video, &[1, 2, 3]);
        let url = video.src.clone();
        assert!(object_url_bytes(&url).is_some());
        detach();
        assert!(object_url_bytes(&url).is_none());
    }

    #[test]
    fn keeps_backward_media_navigation_visible_beyond_a_complete_cycle() {
        let images = ["first", "second"];
        let walked: Vec<_> = (0..7)
            .map(|step| images[wrap_expanded_image_index(-step, images.len() as i64) as usize])
            .collect();
        assert_eq!(
            walked,
            [
                "first", "second", "first", "second", "first", "second", "first"
            ]
        );
        let mut index = 0i64;
        for step in 1..=7 {
            index = wrap_expanded_image_index(index - 1, images.len() as i64);
            assert_eq!(
                images[index as usize],
                if step % 2 == 1 { "second" } else { "first" }
            );
        }
    }

    #[test]
    fn zoom_math_matches_the_wheel_and_fit_rules() {
        let fit = fit_scale(800.0, 600.0, 2000.0, 1000.0);
        assert!(fit < 1.0);
        assert!((wheel_zoom(1.0, 0.0, 0, 100.0, false) - 1.0).abs() < 1e-9);
        let zoomed = wheel_zoom(1.0, -10.0, 1, 100.0, true);
        assert!(zoomed > 1.0);
        assert_eq!(clamp_zoom(100.0), MAX_ZOOM);
        assert_eq!(toggle_zoom(1.0), 2.0);
        assert_eq!(toggle_zoom(2.0), 1.0);
        assert!(pan_arrow(1.0, 800.0, 400.0, 0.0, false).is_none());
        assert_eq!(pan_arrow(2.0, 800.0, 400.0, 0.0, false), Some(40.0));
        assert!(pan_arrow(2.0, 400.0, 400.0, 0.0, false).is_none());
    }

    #[test]
    fn stage_matches_the_fitted_image_inside_the_column() {
        let stage = stage_box(844.0, 700.0, 640.0, 360.0, 1.0);
        assert!((stage.view_w - 640.0).abs() < 1e-6);
        assert!((stage.view_h - 360.0).abs() < 1e-6);
        assert!(stage.view_w < 844.0);
        let zoomed = stage_box(844.0, 700.0, 640.0, 360.0, 4.0);
        assert!(zoomed.content_w > zoomed.view_w);
        assert!(zoomed.view_w <= zoomed.cap_w + 1e-6);
        assert!(zoomed.view_h <= zoomed.cap_h + 1e-6);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_pointer() {
        let scroll = anchored_scroll(0.0, 180.0, 100.0, 1.0, 2.0, 50.0, 10_000.0);
        let before = (0.0 + (180.0 - 100.0)) / 1.0;
        let after = (scroll + (180.0 - 50.0)) / 2.0;
        assert!((before - after).abs() < 1e-9);
        assert!((scroll - 30.0).abs() < 1e-9);
        let unchanged = anchored_scroll(10.0, 40.0, 0.0, 2.0, 2.0, 0.0, 100.0);
        assert!((unchanged - 10.0).abs() < 1e-9);
    }

    #[test]
    fn arrow_keys_pan_before_they_step_the_gallery() {
        assert_eq!(arrow_action(true, 3, true), ArrowAction::Pan);
        assert_eq!(arrow_action(false, 3, true), ArrowAction::Step);
        assert_eq!(arrow_action(false, 1, true), ArrowAction::Pass);
        assert_eq!(arrow_action(false, 3, false), ArrowAction::Pass);
    }

    #[test]
    fn intrinsic_size_comes_from_png_jpeg_gif_webp_and_svg() {
        let mut png = vec![0u8; 24];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[16..20].copy_from_slice(&640u32.to_be_bytes());
        png[20..24].copy_from_slice(&360u32.to_be_bytes());
        assert_eq!(image_pixel_size(&png), Some((640, 360)));

        let mut jpeg = vec![0u8; 21];
        jpeg[..4].copy_from_slice(&[0xFF, 0xD8, 0xFF, 0xC0]);
        jpeg[4..7].copy_from_slice(&[0x00, 0x11, 0x08]);
        jpeg[7..11].copy_from_slice(&[0x01, 0x2C, 0x02, 0x80]);
        assert_eq!(image_pixel_size(&jpeg), Some((640, 300)));

        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&320u16.to_le_bytes());
        gif.extend_from_slice(&200u16.to_le_bytes());
        assert_eq!(image_pixel_size(&gif), Some((320, 200)));

        let mut webp = vec![0u8; 30];
        webp[..4].copy_from_slice(b"RIFF");
        webp[8..12].copy_from_slice(b"WEBP");
        webp[12..16].copy_from_slice(b"VP8X");
        webp[24] = 0x7F;
        webp[25] = 0x02;
        webp[27] = 0x67;
        webp[28] = 0x01;
        assert_eq!(image_pixel_size(&webp), Some((640, 360)));

        let svg = br#"<svg viewBox="0 0 640 360"></svg>"#;
        assert_eq!(image_pixel_size(svg), Some((640, 360)));
        let sized = br#"<svg width="320px" height="180"></svg>"#;
        assert_eq!(image_pixel_size(sized), Some((320, 180)));
        assert_eq!(image_pixel_size(b"not an image"), None);
    }
}
