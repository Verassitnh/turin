//! Port of upstream/apps/server/src/assets/icns.ts.

use turin_shared::image_dimensions::read_image_dimensions;

const ICNS_HEADER_BYTES: usize = 8;
const ICNS_MAGIC: &[u8; 4] = b"icns";
const PNG_MAGIC: &[u8; 4] = b"\x89PNG";

/// Picks the smallest PNG at least `min_size` wide, else the largest. `None`
/// when there is none. Returns a copy so the response does not pin the whole
/// `.icns` buffer.
pub fn extract_icns_png(bytes: &[u8], min_size: u32) -> Option<Vec<u8>> {
    if bytes.len() < ICNS_HEADER_BYTES || &bytes[..4] != ICNS_MAGIC {
        return None;
    }
    let end = usize::try_from(read_u32(bytes, 4)?)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let mut best: Option<(&[u8], u32)> = None;
    let mut offset = ICNS_HEADER_BYTES;
    while offset + ICNS_HEADER_BYTES <= end {
        let Some(length) = read_u32(bytes, offset + 4).and_then(|l| usize::try_from(l).ok()) else {
            break;
        };
        if length < ICNS_HEADER_BYTES || offset + length > end {
            break;
        }
        let entry = &bytes[offset + ICNS_HEADER_BYTES..offset + length];
        let is_png = entry.len() >= 4 && &entry[..4] == PNG_MAGIC;
        offset += length;
        let dimensions = if is_png {
            read_image_dimensions(entry)
        } else {
            None
        };
        let Some(width) = dimensions.map(|dimensions| dimensions.width) else {
            continue;
        };
        let is_better = match best {
            None => true,
            Some((_, best_width)) if best_width < min_size => width > best_width,
            Some((_, best_width)) => width >= min_size && width < best_width,
        };
        if is_better {
            best = Some((entry, width));
        }
    }
    best.map(|(png, _)| png.to_vec())
}

/// Big-endian `u32` at `offset`, `None` when the buffer is too short.
fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let window = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes(window.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_header(size: u32) -> Vec<u8> {
        let mut png = vec![0u8; 24];
        png[..16].copy_from_slice(&[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52,
        ]);
        png[16..20].copy_from_slice(&size.to_be_bytes());
        png[20..24].copy_from_slice(&size.to_be_bytes());
        png
    }

    fn icns(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let length = 8 + entries
            .iter()
            .map(|(_, data)| 8 + data.len())
            .sum::<usize>();
        let mut bytes = Vec::with_capacity(length);
        bytes.extend_from_slice(b"icns");
        bytes.extend_from_slice(&(length as u32).to_be_bytes());
        for (kind, data) in entries {
            bytes.extend_from_slice(kind.as_bytes());
            bytes.extend_from_slice(&((8 + data.len()) as u32).to_be_bytes());
            bytes.extend_from_slice(data);
        }
        bytes
    }

    fn width_of(png: Option<Vec<u8>>) -> Option<u32> {
        png.map(|png| u32::from_be_bytes(png[16..20].try_into().unwrap()))
    }

    #[test]
    fn picks_the_smallest_png_that_covers_the_requested_size() {
        let file = icns(&[
            ("TOC ", vec![0; 16]),
            ("ic09", png_header(512)),
            ("icp5", png_header(32)),
            ("ic07", png_header(128)),
            ("ic08", png_header(256)),
        ]);

        assert_eq!(width_of(extract_icns_png(&file, 96)), Some(128));
    }

    #[test]
    fn falls_back_to_the_largest_png_when_none_covers_the_requested_size() {
        let file = icns(&[("icp4", png_header(16)), ("icp5", png_header(32))]);

        assert_eq!(width_of(extract_icns_png(&file, 96)), Some(32));
    }

    #[test]
    fn returns_none_for_files_without_a_png_entry_or_with_a_foreign_header() {
        assert_eq!(extract_icns_png(&icns(&[("it32", vec![0; 64])]), 96), None);
        assert_eq!(extract_icns_png(&png_header(128), 96), None);
    }

    #[test]
    fn stops_at_an_entry_whose_length_runs_past_the_file() {
        let mut file = icns(&[("ic07", png_header(128)), ("ic08", png_header(256))]);
        // Length field of the second entry: header (8) + first entry (8 + 24).
        let second_length_offset = 8 + 8 + 24 + 4;
        file[second_length_offset..second_length_offset + 4]
            .copy_from_slice(&4096u32.to_be_bytes());

        assert_eq!(width_of(extract_icns_png(&file, 200)), Some(128));
    }
}
