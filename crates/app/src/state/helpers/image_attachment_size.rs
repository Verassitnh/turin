//! Port of upstream/packages/client-runtime/src/imageAttachmentSize.ts (upstream has no tests for
//! it; the ones here cover its arithmetic).

/// Longest edge, in pixels, a PNG or JPEG attachment keeps. Anthropic rejects an image over
/// 2000px on a side once a request holds more than 20 images, and Claude Code forwards images
/// sent into a running turn unchanged, so one large paste breaks every later request in that
/// thread. Attachments are scaled down to this edge when they are attached.
pub const IMAGE_ATTACHMENT_MAX_EDGE: u32 = 2000;

/// Formats a client re-encodes in place. GIF, WebP, and SVG are left as attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownscalableImageMimeType {
    Png,
    Jpeg,
}

impl DownscalableImageMimeType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

pub fn downscalable_image_mime_type(mime_type: &str) -> Option<DownscalableImageMimeType> {
    match mime_type.to_lowercase().as_str() {
        "image/png" => Some(DownscalableImageMimeType::Png),
        "image/jpeg" | "image/jpg" => Some(DownscalableImageMimeType::Jpeg),
        _ => None,
    }
}

/// `{ width, height }` in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageSize {
    pub width: u32,
    pub height: u32,
}

/// The size an image should be scaled to so its long edge fits [`IMAGE_ATTACHMENT_MAX_EDGE`],
/// keeping its aspect ratio. `None` when it already fits.
pub fn fit_image_attachment_size(size: ImageSize) -> Option<ImageSize> {
    let longest_edge = size.width.max(size.height);
    if longest_edge <= IMAGE_ATTACHMENT_MAX_EDGE {
        return None;
    }
    let scale = f64::from(IMAGE_ATTACHMENT_MAX_EDGE) / f64::from(longest_edge);
    // `Math.max(1, Math.min(MAX, Math.round(edge * scale)))`; both sides are positive, so
    // `f64::round` (half away from zero) matches `Math.round` (half toward +∞).
    let scaled = |edge: u32| {
        let rounded = (f64::from(edge) * scale).round() as u32;
        rounded.clamp(1, IMAGE_ATTACHMENT_MAX_EDGE)
    };
    Some(ImageSize {
        width: scaled(size.width),
        height: scaled(size.height),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: u32, height: u32) -> ImageSize {
        ImageSize { width, height }
    }

    #[test]
    fn leaves_an_image_that_fits_untouched() {
        assert_eq!(fit_image_attachment_size(size(2000, 1000)), None);
        assert_eq!(fit_image_attachment_size(size(1999, 2000)), None);
    }

    #[test]
    fn scales_the_long_edge_to_the_cap_and_keeps_the_aspect_ratio() {
        assert_eq!(
            fit_image_attachment_size(size(4000, 2000)),
            Some(size(2000, 1000))
        );
        assert_eq!(
            fit_image_attachment_size(size(3000, 4500)),
            Some(size(1333, 2000))
        );
    }

    #[test]
    fn never_scales_an_edge_below_one_pixel() {
        assert_eq!(
            fit_image_attachment_size(size(10_000, 1)),
            Some(size(2000, 1))
        );
    }

    #[test]
    fn re_encodes_only_png_and_jpeg() {
        assert_eq!(
            downscalable_image_mime_type("IMAGE/PNG"),
            Some(DownscalableImageMimeType::Png)
        );
        assert_eq!(
            downscalable_image_mime_type("image/jpg"),
            Some(DownscalableImageMimeType::Jpeg)
        );
        assert_eq!(DownscalableImageMimeType::Jpeg.as_str(), "image/jpeg");
        assert_eq!(downscalable_image_mime_type("image/gif"), None);
        assert_eq!(downscalable_image_mime_type("image/webp"), None);
        assert_eq!(downscalable_image_mime_type("image/svg+xml"), None);
    }
}
