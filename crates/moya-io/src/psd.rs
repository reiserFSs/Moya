//! Flattened RGB/grayscale PSD and PSB composites; layers are not rendered.
use anyhow::{bail, ensure, Context, Result};
use flate2::{Decompress, FlushDecompress, Status};
use std::{fs::File, io::{BufReader, Read, Seek, SeekFrom}, path::Path};
#[cfg(test)]
use std::io::Cursor;

use super::{LoadLimits, Picture, SampleDepth};

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
}

struct Stream<R> {
    source: R,
    end: u64,
}

impl<R: Read + Seek> Stream<R> {
    fn new(mut source: R) -> Result<Self> {
        let end = source.seek(SeekFrom::End(0))?;
        source.seek(SeekFrom::Start(0))?;
        Ok(Self { source, end })
    }
    fn position(&mut self) -> Result<u64> { Ok(self.source.stream_position()?) }
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0; N];
        self.source.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    fn u16(&mut self) -> Result<u16> { Ok(u16::from_be_bytes(self.bytes()?)) }
    fn u32(&mut self) -> Result<u32> { Ok(u32::from_be_bytes(self.bytes()?)) }
    fn length(&mut self, large: bool) -> Result<u64> {
        Ok(if large { u64::from_be_bytes(self.bytes()?) } else { self.u32()? as u64 })
    }
    fn section_end(&mut self, length: u64, parent: u64) -> Result<u64> {
        let end = self.position()?.checked_add(length).context("PSD section length overflow")?;
        ensure!(end <= parent && end <= self.end, "truncated PSD section");
        Ok(end)
    }
    fn skip_to(&mut self, end: u64) -> Result<()> {
        ensure!(end >= self.position()? && end <= self.end, "invalid PSD section boundary");
        self.source.seek(SeekFrom::Start(end))?;
        Ok(())
    }
    fn retained(&mut self, length: u64, budget: u64) -> Result<Vec<u8>> {
        ensure!(length <= budget, "PSD data exceeds memory limit");
        let length = usize::try_from(length).context("PSD data exceeds address space")?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).context("allocating PSD data")?;
        bytes.resize(length, 0);
        self.source.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

pub(super) fn load(path: &Path, limits: LoadLimits) -> Result<Picture> {
    let file = File::open(path).with_context(|| format!("reading {}", path.display()))?;
    decode_reader(BufReader::new(file), limits)
        .with_context(|| format!("decoding PSD {}", path.display()))
}

#[cfg(test)]
fn decode(bytes: &[u8]) -> Result<Picture> {
    decode_reader(Cursor::new(bytes), LoadLimits { max_pixels: 268_435_456, max_bytes: 4 * 1024 * 1024 * 1024 })
}

fn decode_reader<R: Read + Seek>(source: R, limits: LoadLimits) -> Result<Picture> {
    let mut input = Stream::new(source)?;
    ensure!(input.bytes::<4>()? == *b"8BPS", "invalid PSD signature");
    let version = input.u16()?;
    ensure!(matches!(version, 1 | 2), "unsupported PSD version {version}");
    let large = version == 2;
    ensure!(input.bytes::<6>()? == [0; 6], "invalid PSD reserved header bytes");
    let channels = input.u16()? as usize;
    ensure!((1..=56).contains(&channels), "invalid PSD channel count");
    let height = input.u32()?;
    let width = input.u32()?;
    let limit = if large { 300_000 } else { 30_000 };
    ensure!(width > 0 && height > 0 && width <= limit && height <= limit, "invalid PSD dimensions");
    limits.check(width, height)?;
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
    let color_end = input.section_end(color_length, input.end)?;
    input.skip_to(color_end)?;
    let resource_length = input.length(false)?;
    let resource_end = input.section_end(resource_length, input.end)?;
    let mut icc_profile = None;
    let mut alpha_ids = None;
    let mut retained = 0u64;
    while input.position()? < resource_end {
        ensure!(input.bytes::<4>()? == *b"8BIM", "invalid PSD image resource signature");
        let id = input.u16()?;
        let name_length = input.bytes::<1>()?[0] as u64;
        let name_end = input.section_end(((name_length + 2) & !1) - 1, resource_end)?;
        input.skip_to(name_end)?;
        let n = input.length(false)?;
        let data_end = input.section_end(n, resource_end)?;
        if id == 1039 || id == 1053 {
            if id == 1053 {
                ensure!(n % 4 == 0 && n / 4 <= channels as u64, "invalid PSD alpha identifiers");
            }
            let data = input.retained(n, limits.max_bytes.saturating_sub(retained))?;
            let target = if id == 1039 { &mut icc_profile } else { &mut alpha_ids };
            retained -= target.as_ref().map_or(0, |v: &Vec<u8>| v.len() as u64);
            retained += n;
            *target = Some(data);
        } else {
            input.skip_to(data_end)?;
        }
        let padded_end = input.section_end(n % 2, resource_end)?;
        input.skip_to(padded_end)?;
    }
    ensure!(input.position()? == resource_end, "invalid PSD resource section");
    let layer_length = input.length(large)?;
    let layer_end = input.section_end(layer_length, input.end)?;
    let mut layer_count = 0i16;
    if layer_length > 0 {
        let n = input.length(large)?;
        let info_end = input.section_end(n, layer_end)?;
        if n > 0 {
            ensure!(n >= 2, "truncated PSD layer count");
            layer_count = input.u16()? as i16;
        }
        input.skip_to(info_end)?;
        if n == 0 && input.position()? < layer_end {
            let mask_length = input.length(false)?;
            let mask_end = input.section_end(mask_length, layer_end)?;
            input.skip_to(mask_end)?;
            while input.position()? < layer_end {
                let signature = input.bytes::<4>()?;
                ensure!(signature == *b"8BIM" || signature == *b"8B64", "invalid PSD layer block signature");
                let key = input.bytes::<4>()?;
                let wide = large && matches!(&key, b"LMsk" | b"Lr16" | b"Lr32" | b"Layr" | b"Mt16" | b"Mt32" | b"Mtrn" | b"Alph" | b"FMsk" | b"lnk2" | b"FEid" | b"FXid" | b"PxSD");
                let length = input.length(wide)?;
                let end = input.section_end(length, layer_end)?;
                if key == *b"Lr16" || key == *b"Lr32" {
                    ensure!(length >= 2, "truncated PSD layer count");
                    layer_count = input.u16()? as i16;
                }
                input.skip_to(end)?;
                let padding = (4 - length % 4) % 4;
                let padded_end = input.section_end(padding, layer_end)?;
                input.skip_to(padded_end)?;
            }
        }
        input.skip_to(layer_end)?;
    }
    // A negative layer count declares merged transparency. Positive alpha IDs
    // describe saved selection channels, not transparency.
    let alpha = if channels == colors {
        None
    } else if layer_count < 0 {
        Some(colors)
    } else if let Some(ids) = alpha_ids.as_deref() {
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
    let allocation = (rgba_length as u64).checked_mul(4)
        .and_then(|n| n.checked_add(decoded_length as u64 + 1))
        .and_then(|n| n.checked_add(retained))
        .and_then(|n| n.checked_add(row_bytes as u64))
        .context("PSD allocation size overflow")?;
    ensure!(allocation <= limits.max_bytes, "PSD decoded image exceeds memory limit");
    let compression = input.u16()?;
    let encoded_length = input.end - input.position()?;
    let encoded = input.retained(encoded_length, limits.max_bytes - allocation)?;
    let mut input = Reader { bytes: &encoded };
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
    fn global_high_depth_layers_declare_merged_alpha_with_selection_ids() {
        for version in [1u16, 2] {
            for bits in [16u16, 32] {
                let mut file = b"8BPS".to_vec();
                file.extend(version.to_be_bytes());
                file.extend([0; 6]);
                file.extend(3u16.to_be_bytes());
                file.extend(1u32.to_be_bytes());
                file.extend(1u32.to_be_bytes());
                file.extend(bits.to_be_bytes());
                file.extend(1u16.to_be_bytes());
                file.extend(0u32.to_be_bytes());
                let mut resource = b"8BIM".to_vec();
                resource.extend(1053u16.to_be_bytes());
                resource.extend([0, 0]);
                resource.extend(8u32.to_be_bytes());
                resource.extend(7u32.to_be_bytes());
                resource.extend(9u32.to_be_bytes());
                file.extend((resource.len() as u32).to_be_bytes());
                file.extend(resource);
                let mut layers = if version == 2 { 0u64.to_be_bytes().to_vec() } else { 0u32.to_be_bytes().to_vec() };
                layers.extend(0u32.to_be_bytes());
                layers.extend(b"8BIM");
                layers.extend(if bits == 16 { b"Lr16" } else { b"Lr32" });
                if version == 2 { layers.extend(4u64.to_be_bytes()); }
                else { layers.extend(4u32.to_be_bytes()); }
                layers.extend((-1i16).to_be_bytes());
                layers.extend([0; 2]);
                if version == 2 { file.extend((layers.len() as u64).to_be_bytes()); }
                else { file.extend((layers.len() as u32).to_be_bytes()); }
                file.extend(layers);
                file.extend(0u16.to_be_bytes());
                for value in [0.75f32, 0.5, 1.0] {
                    if bits == 16 { file.extend(((value * 65535.0).round() as u16).to_be_bytes()); }
                    else { file.extend(value.to_be_bytes()); }
                }
                let picture = decode(&file).unwrap();
                assert!((picture.rgba[0] - 0.5).abs() < 0.0001);
                assert!((picture.rgba[3] - 0.5).abs() < 0.0001);
                assert!(picture.rgba.iter().all(|v| v.is_finite()));
            }
        }
    }

    #[test]
    fn sparse_huge_layer_section_is_skipped_and_budgets_reject_allocations() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("moya-sparse-psb-{}-{nonce}.psb", std::process::id()));
        let fixture = fixture(2, 8, 0, false);
        let layer_length = 1u64 << 33;
        let mut file = File::create(&path).unwrap();
        file.write_all(&fixture[..50]).unwrap();
        file.write_all(&layer_length.to_be_bytes()).unwrap();
        file.write_all(&(layer_length - 8).to_be_bytes()).unwrap();
        file.write_all(&1i16.to_be_bytes()).unwrap();
        file.seek(SeekFrom::Start(58 + layer_length)).unwrap();
        file.write_all(&fixture[58..]).unwrap();
        drop(file);
        let limits = LoadLimits { max_pixels: 100, max_bytes: 1024 };
        let picture = load(&path, limits);
        let rejected = load(&path, LoadLimits { max_bytes: 32, ..limits });
        std::fs::remove_file(&path).unwrap();
        assert_eq!(picture.unwrap().rgba.len(), 24);
        assert!(rejected.is_err());
        let mut encoded = fixture.clone();
        encoded.extend([0; 1024]);
        assert!(decode_reader(Cursor::new(encoded), limits).is_err());
        let mut resources = fixture;
        resources[30..34].copy_from_slice(&2060u32.to_be_bytes());
        resources[42..46].copy_from_slice(&2048u32.to_be_bytes());
        resources.splice(46..50, [0; 2048]);
        assert!(decode_reader(Cursor::new(resources), limits).is_err());
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
        let picture = load(&path, LoadLimits { max_pixels: 268_435_456, max_bytes: 4 * 1024 * 1024 * 1024 });
        std::fs::remove_file(&path).unwrap();
        let picture = picture.unwrap();
        assert_eq!((picture.width, picture.height), (3, 2));
        assert_eq!(picture.depth, SampleDepth::U8);
        assert_eq!(picture.rgba.len(), 3 * 2 * 4);
        assert!(picture.rgba.iter().all(|value| value.is_finite() && (0.0..=1.0).contains(value)));
    }
}
