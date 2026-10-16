//! Port of upstream/apps/web/src/components/chat/ComposerImageThumbnail.tsx
//!
//! Keeps full-resolution image decoding out of composer rerenders: the tile
//! shows a centered square crop of at most 256px
//! (`createComposerImageThumbnail`, ported in `state::helpers::image_compression`),
//! produced once on the background executor, and the fallback until it is
//! ready or when the image cannot be decoded.

use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, AppContext as _, Image, ImageFormat, IntoElement, ObjectFit, RenderOnce,
    SharedString, Styled as _, StyledImage as _, Window, img,
};

use crate::state::helpers::image_compression::{
    ImageCrateCodec, ImageFile, create_composer_image_thumbnail, data_url_to_file,
};

/// The GPUI image format of a mime type.
pub fn gpui_image_format(mime: &str) -> Option<ImageFormat> {
    Some(match mime {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::Webp,
        "image/gif" => ImageFormat::Gif,
        "image/svg+xml" => ImageFormat::Svg,
        "image/bmp" => ImageFormat::Bmp,
        "image/tiff" => ImageFormat::Tiff,
        _ => return None,
    })
}

/// The thumbnail of `file` as PNG bytes: `createComposerImageThumbnail`,
/// unwrapped from its data URL.
pub fn thumbnail_png_bytes(file: &ImageFile) -> Option<Vec<u8>> {
    let data_url = create_composer_image_thumbnail(&ImageCrateCodec, file)?;
    data_url_to_file(&data_url, "thumbnail.png", "image/png")
        .ok()
        .map(|file| file.bytes.as_ref().clone())
}

enum Thumbnail {
    Pending,
    /// `src: null` when the crop could not be made.
    Ready(Option<Arc<Image>>),
}

#[derive(IntoElement)]
pub struct ComposerImageThumbnail {
    /// Identifies the attachment; a new file under the same id is a new tile.
    pub id: SharedString,
    pub file: ImageFile,
    pub fallback: AnyElement,
}

impl RenderOnce for ComposerImageThumbnail {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(
            SharedString::from(format!("thumbnail-{}-{}", self.id, self.file.bytes.len())),
            cx,
            |_, _| Thumbnail::Pending,
        );
        if matches!(state.read(cx), Thumbnail::Pending) {
            let file = self.file.clone();
            let weak = state.downgrade();
            // Marked below so a second render does not start the crop again.
            state.update(cx, |state, _| *state = Thumbnail::Ready(None));
            cx.spawn(async move |cx| {
                let png = cx
                    .background_spawn(async move { thumbnail_png_bytes(&file) })
                    .await;
                weak.update(cx, |state, cx| {
                    *state = Thumbnail::Ready(
                        png.map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Png, bytes))),
                    );
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        match state.read(cx) {
            Thumbnail::Ready(Some(image)) => img(image.clone())
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            _ => self.fallback,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode");
        out
    }

    #[test]
    fn crops_a_wide_image_to_a_square_no_larger_than_256() {
        let file = ImageFile::new("wide.png", "image/png", png(600, 300));
        let bytes = thumbnail_png_bytes(&file).expect("thumbnail");
        let decoded = image::load_from_memory(&bytes).expect("png");
        assert_eq!((decoded.width(), decoded.height()), (256, 256));
    }

    #[test]
    fn keeps_a_small_image_at_its_short_side() {
        let file = ImageFile::new("small.png", "image/png", png(80, 40));
        let bytes = thumbnail_png_bytes(&file).expect("thumbnail");
        let decoded = image::load_from_memory(&bytes).expect("png");
        assert_eq!((decoded.width(), decoded.height()), (40, 40));
    }

    #[test]
    fn has_no_thumbnail_for_bytes_that_are_not_an_image() {
        let file = ImageFile::new("notes.png", "image/png", b"nope".to_vec());
        assert!(thumbnail_png_bytes(&file).is_none());
    }

    #[test]
    fn maps_mime_types_to_gpui_formats() {
        assert!(matches!(
            gpui_image_format("image/jpg"),
            Some(ImageFormat::Jpeg)
        ));
        assert!(gpui_image_format("application/pdf").is_none());
    }
}
