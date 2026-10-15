//! Port of upstream/packages/shared/src/imageDimensions.ts

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDimensions {
    pub width: u32,
    pub height: u32,
}

/// Enough for every supported header. A JPEG's frame header can sit behind
/// several 64 KiB metadata segments (EXIF, an ICC profile, XMP), so allow a
/// few of them before giving up.
pub const IMAGE_DIMENSIONS_HEADER_BYTES: usize = 256 * 1024;

pub fn read_image_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    let dimensions = read_png(bytes)
        .or_else(|| read_gif(bytes))
        .or_else(|| read_webp(bytes))
        .or_else(|| read_jpeg(bytes))?;
    if dimensions.width > 0 && dimensions.height > 0 {
        Some(dimensions)
    } else {
        None
    }
}

fn u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn u16_be(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn u16_at(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(if little_endian {
        u16::from_le_bytes([slice[0], slice[1]])
    } else {
        u16::from_be_bytes([slice[0], slice[1]])
    })
}

fn u32_at(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(if little_endian {
        u32::from_le_bytes(slice.try_into().ok()?)
    } else {
        u32::from_be_bytes(slice.try_into().ok()?)
    })
}

fn read_png(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 24 {
        return None;
    }
    if bytes[0] != 0x89
        || bytes[1] != 0x50
        || bytes[2] != 0x4e
        || bytes[3] != 0x47
        || bytes[12] != 0x49
        || bytes[13] != 0x48
        || bytes[14] != 0x44
        || bytes[15] != 0x52
    {
        return None;
    }
    Some(ImageDimensions {
        width: u32_be(bytes, 16)?,
        height: u32_be(bytes, 20)?,
    })
}

fn read_gif(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 10
        || bytes[0] != 0x47
        || bytes[1] != 0x49
        || bytes[2] != 0x46
        || bytes[3] != 0x38
    {
        return None;
    }
    Some(ImageDimensions {
        width: u32::from(u16_at(bytes, 6, true)?),
        height: u32::from(u16_at(bytes, 8, true)?),
    })
}

fn read_webp(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 16 {
        return None;
    }
    if bytes[0] != 0x52
        || bytes[1] != 0x49
        || bytes[2] != 0x46
        || bytes[3] != 0x46
        || bytes[8] != 0x57
        || bytes[9] != 0x45
        || bytes[10] != 0x42
        || bytes[11] != 0x50
    {
        return None;
    }
    let chunk = [bytes[12], bytes[13], bytes[14], bytes[15]];
    match &chunk {
        b"VP8 " => {
            if bytes.len() < 30 {
                return None;
            }
            Some(ImageDimensions {
                width: u32::from(u16_at(bytes, 26, true)? & 0x3fff),
                height: u32::from(u16_at(bytes, 28, true)? & 0x3fff),
            })
        }
        b"VP8L" => {
            if bytes.len() < 25 {
                return None;
            }
            let packed = u32_at(bytes, 21, true)?;
            Some(ImageDimensions {
                width: (packed & 0x3fff) + 1,
                height: ((packed >> 14) & 0x3fff) + 1,
            })
        }
        b"VP8X" => {
            if bytes.len() < 30 {
                return None;
            }
            Some(ImageDimensions {
                width: (u32::from(bytes[24])
                    | (u32::from(bytes[25]) << 8)
                    | (u32::from(bytes[26]) << 16))
                    + 1,
                height: (u32::from(bytes[27])
                    | (u32::from(bytes[28]) << 8)
                    | (u32::from(bytes[29]) << 16))
                    + 1,
            })
        }
        _ => None,
    }
}

fn read_jpeg(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }
    let mut offset = 2usize;
    let mut rotated = false;
    while offset + 9 <= bytes.len() {
        if bytes[offset] != 0xff {
            return None;
        }
        let marker = bytes[offset + 1];
        if marker == 0xff {
            offset += 1;
            continue;
        }
        if (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc {
            let height = u32::from(u16_be(bytes, offset + 5)?);
            let width = u32::from(u16_be(bytes, offset + 7)?);
            return Some(if rotated {
                ImageDimensions {
                    width: height,
                    height: width,
                }
            } else {
                ImageDimensions { width, height }
            });
        }
        if marker == 0xd9 || marker == 0xda {
            return None;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            offset += 2;
            continue;
        }
        let length = usize::from(u16_be(bytes, offset + 2)?);
        if marker == 0xe1 && !rotated {
            let end = (offset + 2 + length).min(bytes.len());
            rotated = exif_orientation_swaps_axes(bytes, offset + 4, end);
        }
        offset += 2 + length;
    }
    None
}

fn exif_orientation_swaps_axes(bytes: &[u8], start: usize, mut end: usize) -> bool {
    end = end.min(bytes.len());
    if end.saturating_sub(start) < 14 || bytes.get(start..start + 4) != Some(b"Exif") {
        return false;
    }
    let tiff = start + 6;
    let little_endian = bytes.get(tiff) == Some(&0x49) && bytes.get(tiff + 1) == Some(&0x49);
    if !little_endian && !(bytes.get(tiff) == Some(&0x4d) && bytes.get(tiff + 1) == Some(&0x4d)) {
        return false;
    }
    let ifd =
        tiff + usize::try_from(u32_at(bytes, tiff + 4, little_endian).unwrap_or(0)).unwrap_or(0);
    if ifd + 2 > end {
        return false;
    }
    let entries = usize::from(u16_at(bytes, ifd, little_endian).unwrap_or(0));
    for index in 0..entries {
        let entry = ifd + 2 + index * 12;
        if entry + 12 > end {
            return false;
        }
        if u16_at(bytes, entry, little_endian) == Some(0x0112) {
            let orientation = u16_at(bytes, entry + 8, little_endian).unwrap_or(0);
            return (5..=8).contains(&orientation);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_bytes(n: u32) -> [u8; 4] {
        n.to_be_bytes()
    }

    fn u16_bytes(n: u16) -> [u8; 2] {
        n.to_be_bytes()
    }

    fn u16_le(n: u16) -> [u8; 2] {
        n.to_le_bytes()
    }

    #[test]
    fn reads_a_png_ihdr() {
        let mut png = vec![0x89];
        png.extend_from_slice(b"PNG");
        png.extend_from_slice(&[0x0d, 0x0a, 0x1a, 0x0a]);
        png.extend_from_slice(&u32_bytes(13));
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&u32_bytes(1600));
        png.extend_from_slice(&u32_bytes(900));
        assert_eq!(
            read_image_dimensions(&png),
            Some(ImageDimensions {
                width: 1600,
                height: 900
            })
        );
    }

    #[test]
    fn reads_a_gif_logical_screen() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&u16_le(320));
        gif.extend_from_slice(&u16_le(240));
        assert_eq!(
            read_image_dimensions(&gif),
            Some(ImageDimensions {
                width: 320,
                height: 240
            })
        );
    }

    #[test]
    fn reads_a_jpeg_start_of_frame_after_an_app_segment() {
        let mut app1 = vec![0xff, 0xe1];
        app1.extend_from_slice(&u16_bytes(2 + 6));
        app1.extend_from_slice(b"Exif\0\0");
        let mut sof0 = vec![0xff, 0xc0];
        sof0.extend_from_slice(&u16_bytes(17));
        sof0.push(8);
        sof0.extend_from_slice(&u16_bytes(1400));
        sof0.extend_from_slice(&u16_bytes(720));
        let mut jpeg = vec![0xff, 0xd8];
        jpeg.extend_from_slice(&app1);
        jpeg.extend_from_slice(&sof0);
        assert_eq!(
            read_image_dimensions(&jpeg),
            Some(ImageDimensions {
                width: 720,
                height: 1400
            })
        );
    }

    #[test]
    fn swaps_axes_for_a_rotated_jpeg() {
        let tiff = [
            0x4d, 0x4d, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x08, 0x00, 0x01, 0x01, 0x12, 0x00, 0x03,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut app1 = vec![0xff, 0xe1];
        app1.extend_from_slice(&u16_bytes(2 + 6 + tiff.len() as u16));
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);
        let mut sof0 = vec![0xff, 0xc0];
        sof0.extend_from_slice(&u16_bytes(17));
        sof0.push(8);
        sof0.extend_from_slice(&u16_bytes(3024));
        sof0.extend_from_slice(&u16_bytes(4032));
        let mut jpeg = vec![0xff, 0xd8];
        jpeg.extend_from_slice(&app1);
        jpeg.extend_from_slice(&sof0);
        assert_eq!(
            read_image_dimensions(&jpeg),
            Some(ImageDimensions {
                width: 3024,
                height: 4032
            })
        );
    }

    #[test]
    fn returns_null_for_unsupported_or_zero_sized_input() {
        assert_eq!(read_image_dimensions(b"<svg/>"), None);
        assert_eq!(read_image_dimensions(&[0x89, b'P', b'N', b'G']), None);
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&u16_le(0));
        gif.extend_from_slice(&u16_le(240));
        assert_eq!(read_image_dimensions(&gif), None);
        assert_eq!(read_image_dimensions(&[0xff, 0xd8, 0xff, 0xd9]), None);
        assert_eq!(read_image_dimensions(&[]), None);
    }
}
