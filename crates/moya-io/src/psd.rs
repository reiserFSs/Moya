//! Flattened RGB/grayscale PSD and PSB composites; layers are not rendered.
use anyhow::{bail, ensure, Context, Result};
use flate2::{Decompress, FlushDecompress, Status};
use std::path::Path;

use super::{Picture, SampleDepth};

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(n <= self.bytes.len(), "truncated PSD section");
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn length(&mut self, large: bool) -> Result<usize> {
        let n = if large {
            u64::from_be_bytes(self.take(8)?.try_into()?)
        } else {
            self.u32()? as u64
        };
        usize::try_from(n).context("PSD section length exceeds address space")
    }
}

pub(super) fn load(path: &Path) -> Result<Picture> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    decode(&bytes).with_context(|| format!("decoding PSD {}", path.display()))
}

fn decode(bytes: &[u8]) -> Result<Picture> {
    let mut input = Reader { bytes };
    ensure!(input.take(4)? == b"8BPS", "invalid PSD signature");
    let version = input.u16()?;
    ensure!(matches!(version, 1 | 2), "unsupported PSD version {version}");
    let large = version == 2;
    ensure!(input.take(6)? == [0; 6], "invalid PSD reserved header bytes");
    let channels = input.u16()? as usize;
    ensure!((1..=56).contains(&channels), "invalid PSD channel count");
    let height = input.u32()?;
    let width = input.u32()?;
    let limit = if large { 300_000 } else { 30_000 };
    ensure!(width > 0 && height > 0 && width <= limit && height <= limit, "invalid PSD dimensions");
    let bits = input.u16()?;
    let depth = match bits {
        8 => SampleDepth::U8,
        16 => SampleDepth::U16,
        32 => SampleDepth::F32,
        _ => bail!("unsupported PSD sample depth {bits}; expected 8, 16 or 32"),
    };
    let mode = input.u16()?;
    let colors = match mode {
        1 => 1,
        3 => 3,
        _ => bail!("unsupported PSD color mode {mode}; expected grayscale or RGB"),
    };
    ensure!(channels >= colors, "PSD lacks required color channels");
    let color_length = input.length(false)?;
    input.take(color_length)?;
    let resource_length = input.length(false)?;
    let mut resources = Reader { bytes: input.take(resource_length)? };
    let mut icc_profile = None;
    let mut alpha_ids = None;
    while !resources.bytes.is_empty() {
        ensure!(resources.take(4)? == b"8BIM", "invalid PSD image resource signature");
        let id = resources.u16()?;
        let name_length = resources.take(1)?[0] as usize;
        resources.take(name_length)?;
        if (name_length + 1) % 2 != 0 {
            resources.take(1)?;
        }
        let n = resources.length(false)?;
        let data = resources.take(n)?;
        if id == 1039 {
            icc_profile = Some(data.to_vec());
        } else if id == 1053 {
            ensure!(n % 4 == 0 && n / 4 <= channels, "invalid PSD alpha identifiers");
            alpha_ids = Some(data);
        }
        if n % 2 != 0 {
            resources.take(1)?;
        }
    }
    let layer_length = input.length(large)?;
    let mut layers = Reader { bytes: input.take(layer_length)? };
    let mut layer_count = 0i16;
    if !layers.bytes.is_empty() {
        let n = layers.length(large)?;
        let mut info = Reader { bytes: layers.take(n)? };
        if n > 0 {
            layer_count = info.u16()? as i16;
        }
    }
    // A negative layer count declares merged transparency. Positive alpha IDs
    // describe saved selection channels, not transparency.
    let alpha = if channels == colors {
        None
    } else if layer_count < 0 {
        Some(colors)
    } else if let Some(ids) = alpha_ids {
        ids.chunks_exact(4).position(|id| id == [0; 4])
            .map(|i| channels - ids.len() / 4 + i)
            .filter(|&i| i >= colors)
    } else if layer_count == 0 {
        Some(colors)
    } else {
        None
    };
    let sample_bytes = bits as usize / 8;
    let pixels = (width as usize).checked_mul(height as usize).context("PSD pixel count overflow")?;
    let row_bytes = (width as usize).checked_mul(sample_bytes).context("PSD row size overflow")?;
    let rows = (height as usize).checked_mul(channels).context("PSD row count overflow")?;
    let decoded_length = row_bytes.checked_mul(rows).context("PSD composite size overflow")?;
    let rgba_length = pixels.checked_mul(4).context("PSD RGBA size overflow")?;
    let allocation = rgba_length.checked_mul(4).and_then(|n| n.checked_add(decoded_length))
        .context("PSD allocation size overflow")?;
    // ponytail: bound decoder working storage to 1 GiB; streaming channels can
    // lift this ceiling when larger PSB composites are needed.
    ensure!(allocation <= 1024 * 1024 * 1024, "PSD decoded image exceeds 1 GiB memory limit");
    let compression = input.u16()?;
    let mut owned;
    let raw = match compression {
        0 => input.take(decoded_length)?,
        1 => {
            let count_bytes = if large { 4 } else { 2 };
            let table_length = rows.checked_mul(count_bytes).context("PSD RLE table overflow")?;
            let mut table = Reader { bytes: input.take(table_length)? };
            owned = Vec::new();
            owned.try_reserve_exact(decoded_length).context("allocating PSD RLE output")?;
            for _ in 0..rows {
                let n = if large { table.u32()? as usize } else { table.u16()? as usize };
                unpack_row(input.take(n)?, row_bytes, &mut owned)?;
            }
            &owned
        }
        2 | 3 => {
            owned = Vec::new();
            owned.try_reserve_exact(decoded_length + 1).context("allocating PSD ZIP output")?;
            owned.resize(decoded_length + 1, 0);
            let mut zip = Decompress::new(true);
            let status = zip.decompress(input.bytes, &mut owned, FlushDecompress::Finish)
                .context("invalid PSD ZIP stream")?;
            ensure!(status == Status::StreamEnd && zip.total_out() == decoded_length as u64,
                "PSD ZIP output length mismatch or truncated stream");
            owned.truncate(decoded_length);
            if compression == 3 {
                undo_prediction(&mut owned, width as usize, sample_bytes)?;
            }
            &owned
        }
        _ => bail!("unsupported PSD compression {compression}"),
    };
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(rgba_length).context("allocating PSD RGBA output")?;
    let plane_bytes = pixels * sample_bytes;
    let sample = |channel: usize, pixel: usize| -> f32 {
        let offset = channel * plane_bytes + pixel * sample_bytes;
        match sample_bytes {
            1 => raw[offset] as f32 / 255.0,
            2 => u16::from_be_bytes([raw[offset], raw[offset + 1]]) as f32 / 65535.0,
            _ => f32::from_be_bytes(raw[offset..offset + 4].try_into().unwrap()),
        }
    };
    for p in 0..pixels {
        let a = alpha.map_or(1.0, |channel| sample(channel, p));
        for c in 0..3 {
            let value = sample(if colors == 1 { 0 } else { c }, p);
            // Photoshop's merged preview is composited over white. Undo its
            // matte to return straight alpha; transparent RGB has no meaning.
            let straight = if alpha.is_some() {
                if a > 0.0 { (value + a - 1.0) / a } else { 0.0 }
            } else { value };
            rgba.push(if bits == 32 { straight } else { straight.clamp(0.0, 1.0) });
        }
        rgba.push(a);
    }
    Ok(Picture { width, height, depth, rgba, icc_profile })
}

fn unpack_row(bytes: &[u8], expected: usize, output: &mut Vec<u8>) -> Result<()> {
    let start = output.len();
    let mut input = Reader { bytes };
    while !input.bytes.is_empty() {
        let control = input.take(1)?[0] as i8;
        let count = match control {
            0..=127 => control as usize + 1,
            -127..=-1 => (1 - control as i16) as usize,
            _ => continue, // PackBits -128 is a no-op.
        };
        ensure!(count <= expected - (output.len() - start), "PSD RLE row exceeds declared width");
        if control >= 0 {
            output.extend_from_slice(input.take(count)?);
        } else {
            let value = input.take(1)?[0];
            output.resize(output.len() + count, value);
        }
    }
    ensure!(output.len() - start == expected, "truncated PSD RLE row");
    Ok(())
}

fn undo_prediction(bytes: &mut [u8], width: usize, sample_bytes: usize) -> Result<()> {
    let mut scratch = Vec::new();
    if sample_bytes == 4 {
        scratch.try_reserve_exact(width * 4).context("allocating PSD prediction row")?;
        scratch.resize(width * 4, 0);
    }
    for row in bytes.chunks_exact_mut(width * sample_bytes) {
        if sample_bytes == 2 {
            let mut previous = 0u16;
            for sample in row.chunks_exact_mut(2) {
                previous = previous.wrapping_add(u16::from_be_bytes([sample[0], sample[1]]));
                sample.copy_from_slice(&previous.to_be_bytes());
            }
        } else {
            for i in 1..row.len() {
                row[i] = row[i].wrapping_add(row[i - 1]);
            }
            if sample_bytes == 4 {
                scratch.copy_from_slice(row);
                for x in 0..width {
                    for b in 0..4 {
                        row[x * 4 + b] = scratch[b * width + x];
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    fn fixture(version: u16, bits: u16, compression: u16, gray: bool) -> Vec<u8> {
        let channels = if gray { 1u16 } else { 3 };
        let mut file = b"8BPS".to_vec();
        file.extend(version.to_be_bytes());
        file.extend([0; 6]);
        file.extend(channels.to_be_bytes());
        file.extend(2u32.to_be_bytes());
        file.extend(3u32.to_be_bytes());
        file.extend(bits.to_be_bytes());
        file.extend((if gray { 1u16 } else { 3 }).to_be_bytes());
        file.extend(0u32.to_be_bytes());
        let mut resource = b"8BIM".to_vec();
        resource.extend(1039u16.to_be_bytes());
        resource.extend([0, 0]);
        resource.extend(3u32.to_be_bytes());
        resource.extend([1, 2, 3, 0]);
        file.extend((resource.len() as u32).to_be_bytes());
        file.extend(resource);
        if version == 2 { file.extend(0u64.to_be_bytes()); }
        else { file.extend(0u32.to_be_bytes()); }
        file.extend(compression.to_be_bytes());
        let mut raw = Vec::new();
        for _ in 0..channels {
            for value in [0.0f32, 0.2, 0.7, 1.0, 0.5, 0.1] {
                match bits {
                    8 => raw.push((value * 255.0).round() as u8),
                    16 => raw.extend(((value * 65535.0).round() as u16).to_be_bytes()),
                    _ => raw.extend((value * 4.0 - 1.0).to_be_bytes()),
                }
            }
        }
        let row_length = 3 * bits as usize / 8;
        match compression {
            0 => file.extend(raw),
            1 => {
                for _ in 0..channels as usize * 2 {
                    if version == 2 { file.extend((row_length as u32 + 1).to_be_bytes()); }
                    else { file.extend((row_length as u16 + 1).to_be_bytes()); }
                }
                for row in raw.chunks_exact(row_length) {
                    file.push(row_length as u8 - 1);
                    file.extend(row);
                }
            }
            _ => {
                if compression == 3 {
                    for row in raw.chunks_exact_mut(row_length) {
                        if bits == 16 {
                            let mut previous = 0u16;
                            for sample in row.chunks_exact_mut(2) {
                                let current = u16::from_be_bytes([sample[0], sample[1]]);
                                sample.copy_from_slice(&current.wrapping_sub(previous).to_be_bytes());
                                previous = current;
                            }
                        } else {
                            if bits == 32 {
                                let original = row.to_vec();
                                for b in 0..4 {
                                    for x in 0..3 { row[b * 3 + x] = original[x * 4 + b]; }
                                }
                            }
                            for i in (1..row.len()).rev() { row[i] = row[i].wrapping_sub(row[i - 1]); }
                        }
                    }
                }
                let mut zip = ZlibEncoder::new(Vec::new(), Compression::default());
                zip.write_all(&raw).unwrap();
                file.extend(zip.finish().unwrap());
            }
        }
        file
    }

    #[test]
    fn generated_composites_all_depths_versions_and_compressions() {
        for version in [1, 2] {
            for bits in [8, 16, 32] {
                for compression in 0..=3 {
                    for gray in [false, true] {
                        let picture = decode(&fixture(version, bits, compression, gray)).unwrap();
                        assert_eq!((picture.width, picture.height), (3, 2));
                        assert_eq!(picture.depth, match bits { 8 => SampleDepth::U8, 16 => SampleDepth::U16, _ => SampleDepth::F32 });
                        assert_eq!(picture.icc_profile.as_deref(), Some(&[1, 2, 3][..]));
                        for (pixel, value) in picture.rgba.chunks_exact(4).zip([0.0f32, 0.2, 0.7, 1.0, 0.5, 0.1]) {
                            let expected = if bits == 32 { value * 4.0 - 1.0 } else { value };
                            for &color in &pixel[..3] { assert!((color - expected).abs() <= 0.002); }
                            assert_eq!(pixel[3], 1.0);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rejects_truncation_and_bad_packbits() {
        for compression in 0..=3 {
            let file = fixture(2, 32, compression, false);
            for length in 0..file.len() { assert!(decode(&file[..length]).is_err(), "accepted truncated length {length}, compression {compression}"); }
        }
        assert!(unpack_row(&[255, 42], 1, &mut Vec::new()).is_err());
        assert!(unpack_row(&[0], 1, &mut Vec::new()).is_err());
        let mut out = Vec::new();
        unpack_row(&[128, 254, 42, 128], 3, &mut out).unwrap();
        assert_eq!(out, [42; 3]);
    }
    #[test]
    fn merged_alpha_is_returned_straight() {
        let mut file = fixture(1, 8, 0, true);
        file[12..14].copy_from_slice(&2u16.to_be_bytes());
        let mut layers = 6u32.to_be_bytes().to_vec();
        layers.extend(2u32.to_be_bytes());
        layers.extend((-1i16).to_be_bytes());
        file.splice(50..54, layers);
        // The original second gray sample is 51. Store a white-matted gray
        // of 128 with alpha 128; straight gray is 1/128 after quantization.
        file[63] = 128;
        file.extend([255, 128, 255, 255, 255, 255]);
        let picture = decode(&file).unwrap();
        assert!((picture.rgba[4] - 1.0 / 128.0).abs() < 0.00001);
        assert!((picture.rgba[7] - 128.0 / 255.0).abs() < 0.00001);
    }


    #[test]
    fn loads_generated_odd_width_composite() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("moya-psd-{}-{nonce}.psd", std::process::id()));
        std::fs::write(&path, fixture(1, 8, 0, false)).unwrap();
        let picture = load(&path);
        std::fs::remove_file(&path).unwrap();
        let picture = picture.unwrap();
        assert_eq!((picture.width, picture.height), (3, 2));
        assert_eq!(picture.depth, SampleDepth::U8);
        assert_eq!(picture.rgba.len(), 3 * 2 * 4);
        assert!(picture.rgba.iter().all(|value| value.is_finite() && (0.0..=1.0).contains(value)));
    }
}
