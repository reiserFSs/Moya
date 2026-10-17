use std::{borrow::Cow, io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write}};
use anyhow::{ensure, Result};
use super::SampleDepth;

/// Match image 0.25.10's PNG defaults, including its incompressible-data fallback.
/// Compressed bytes are spooled because the fallback decision and IDAT lengths
/// precede their payloads; neither pixels nor compressed output grow in RAM.
pub(super) fn png(
    mut writer: impl Write,
    width: u32,
    height: u32,
    depth: SampleDepth,
    icc: Option<&[u8]>,
    fill: &mut impl FnMut(u32, u32, &mut [f32]) -> Result<()>,
) -> Result<()> {
    ensure!(width > 0 && height > 0, "invalid PNG dimensions");
    ensure!(matches!(depth, SampleDepth::U8 | SampleDepth::U16), "unsupported PNG sample depth");
    let sample_bytes = if depth == SampleDepth::U8 { 1 } else { 2 };
    let samples = (width as usize).checked_mul(4).ok_or_else(|| anyhow::anyhow!("PNG row too large"))?;
    let row_bytes = samples.checked_mul(sample_bytes).ok_or_else(|| anyhow::anyhow!("PNG row too large"))?;
    let filtered_len = (row_bytes as u64 + 1).checked_mul(u64::from(height)).ok_or_else(|| anyhow::anyhow!("PNG data too large"))?;
    // Match fdeflate 0.3.7's compressed_size exactly, even its threshold at
    // exact multiples of 65535 (rather than substituting the actual size).
    let stored_len = (filtered_len.saturating_sub(1) / 65535) * (65535 + 5)
        + (filtered_len % 65535 + 5) + 6;
    let band_rows = height.min(64) as usize;
    let mut rgba = vec![0.0; samples.checked_mul(band_rows).ok_or_else(|| anyhow::anyhow!("PNG band too large"))?];
    let mut row = vec![0; row_bytes];
    let mut previous = vec![0; row_bytes];
    let mut filtered = vec![0; row_bytes + 1];
    let mut scratch = vec![0; row_bytes];
    let mut raw = BufWriter::new(tempfile::tempfile()?);
    let mut compressed = fdeflate::Compressor::new(BufWriter::new(tempfile::tempfile()?))?;
    for y in 0..height {
        if y % 64 == 0 {
            let rows = (height - y).min(64);
            fill(y, rows, &mut rgba[..samples * rows as usize])?;
        }
        let start = (y % 64) as usize * samples;
        let rgba_row = &rgba[start..start + samples];
        if sample_bytes == 1 {
            for (out, value) in row.iter_mut().zip(rgba_row) {
                *out = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        } else {
            for (out, value) in row.chunks_exact_mut(2).zip(rgba_row) {
                out.copy_from_slice(&((value.clamp(0.0, 1.0) * 65535.0).round() as u16).to_be_bytes());
            }
        }
        raw.write_all(&[0])?;
        raw.write_all(&row)?;
        filtered[0] = adaptive_filter(sample_bytes * 4, &previous, &row, &mut filtered[1..], &mut scratch);
        // A single write per filtered row is significant: fdeflate does not
        // preserve zero runs across calls. image uses precisely this boundary.
        compressed.write_data(&filtered)?;
        std::mem::swap(&mut row, &mut previous);
    }
    let mut compressed = compressed.finish()?;
    let compressed_len = compressed.stream_position()?;
    if compressed_len > stored_len {
        compressed.seek(SeekFrom::Start(0))?;
        compressed.get_mut().set_len(0)?;
        raw.seek(SeekFrom::Start(0))?;
        let mut raw = BufReader::new(raw.into_inner().map_err(|e| e.into_error())?);
        compressed.write_all(&[0x78, 0x01])?;
        let mut checksum = simd_adler32::Adler32::new();
        let mut buffer = [0; 65535];
        let mut left = filtered_len;
        while left > 0 {
            let n = left.min(buffer.len() as u64) as usize;
            raw.read_exact(&mut buffer[..n])?;
            let length = n as u16;
            compressed.write_all(&[u8::from(left == n as u64)])?;
            compressed.write_all(&length.to_le_bytes())?;
            compressed.write_all(&(!length).to_le_bytes())?;
            compressed.write_all(&buffer[..n])?;
            checksum.write(&buffer[..n]);
            left -= n as u64;
        }
        // fdeflate's StoredOnlyCompressor::finish unwraps this write; keep
        // spool I/O failures fallible instead of panicking on a full disk.
        compressed.write_all(&checksum.finish().to_be_bytes())?;
    }
    let mut remaining = compressed.stream_position()?;
    compressed.seek(SeekFrom::Start(0))?;
    let mut compressed = BufReader::new(compressed.into_inner().map_err(|e| e.into_error())?);

    // Let the same png dependency construct all metadata (notably iCCP).
    // Dropping its header writer appends IEND; defer that chunk until after IDAT.
    let mut header = Vec::new();
    let mut info = ::png::Info::with_size(width, height);
    if let Some(icc) = icc.filter(|profile| !profile.is_empty()) {
        info.icc_profile = Some(Cow::Borrowed(icc));
    }
    let mut encoder = ::png::Encoder::with_info(&mut header, info)?;
    encoder.set_color(::png::ColorType::Rgba);
    encoder.set_depth(if sample_bytes == 1 { ::png::BitDepth::Eight } else { ::png::BitDepth::Sixteen });
    encoder.set_compression(::png::Compression::Fast);
    encoder.set_filter(::png::Filter::Adaptive);
    encoder.write_header()?.finish()?;
    let end = header.len() - 12;
    writer.write_all(&header[..end])?;
    let mut buffer = [0; 64 * 1024];
    while remaining > 0 {
        let chunk_len = remaining.min(u64::from(u32::MAX >> 1));
        writer.write_all(&(chunk_len as u32).to_be_bytes())?;
        writer.write_all(b"IDAT")?;
        let mut crc = crc32fast::Hasher::new();
        crc.update(b"IDAT");
        let mut left = chunk_len;
        while left > 0 {
            let n = left.min(buffer.len() as u64) as usize;
            compressed.read_exact(&mut buffer[..n])?;
            writer.write_all(&buffer[..n])?;
            crc.update(&buffer[..n]);
            left -= n as u64;
        }
        writer.write_all(&crc.finalize().to_be_bytes())?;
        remaining -= chunk_len;
    }
    writer.write_all(&header[end..])?;
    Ok(())
}

// png 0.18.1 adaptive filtering: Up, Sub, Avg, Paeth, last wins ties,
// except that a zero score immediately wins. NoFilter is not a candidate.
fn adaptive_filter(bpp: usize, previous: &[u8], row: &[u8], output: &mut [u8], scratch: &mut [u8]) -> u8 {
    let mut best = u64::MAX;
    let mut choice = 0;
    for method in [2, 1, 3, 4] {
        let mut score = 0u64;
        for i in 0..row.len() {
            let a = if i >= bpp { row[i - bpp] } else { 0 };
            let b = previous[i];
            let c = if i >= bpp { previous[i - bpp] } else { 0 };
            let predictor = match method {
                2 => b,
                1 => a,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                _ => {
                    let p = i16::from(a) + i16::from(b) - i16::from(c);
                    let pa = (p - i16::from(a)).abs();
                    let pb = (p - i16::from(b)).abs();
                    let pc = (p - i16::from(c)).abs();
                    if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c }
                }
            };
            scratch[i] = row[i].wrapping_sub(predictor);
            score = score.saturating_add(u64::from((scratch[i] as i8).unsigned_abs()));
        }
        if score <= best {
            best = score;
            choice = method;
            output.copy_from_slice(scratch);
            if score == 0 { break; }
        }
    }
    choice
}

pub(super) fn exr(
    writer: impl Write + Seek,
    width: u32,
    height: u32,
    fill: &mut impl FnMut(u32, u32, &mut [f32]) -> Result<()>,
) -> Result<()> {
    use ::exr::{block::{self, UncompressedBlock, writer::ChunksWriter}, prelude::{Image, SpecificChannels, Vec2, WritableImage}};
    ensure!(width > 0 && height > 0, "invalid EXR dimensions");
    // Infer exactly image 0.25.10's metadata: RGBA f32, RLE, 64x64 tiles.
    let image = Image::from_channels(
        (width as usize, height as usize),
        SpecificChannels::rgba(|_: Vec2<usize>| (0.0f32, 0.0f32, 0.0f32, 0.0f32)),
    );
    let headers = image.write().infer_meta_data();
    let mut source_error = None;
    // image's default parallel compressor emits Unspecified-order tiles in
    // completion order, so its historical multi-tile bytes are nondeterministic.
    // Enumerate the same tiles sequentially for stable bytes and bounded memory.
    let result = block::write(writer, headers, true, |meta, chunks| {
        let mut compressor = chunks.sequential_blocks_compressor(&meta);
        let mut rgba = vec![0.0; width as usize * 4 * 64.min(height as usize)];
        let mut cached_y = usize::MAX;
        for (number, index) in block::enumerate_ordered_header_block_indices(&meta.headers) {
            let y = index.pixel_position.y();
            if cached_y != y {
                let rows = 64.min(height as usize - y);
                if let Err(error) = fill(y as u32, rows as u32, &mut rgba[..width as usize * rows * 4]) {
                    source_error = Some(error);
                    return Err(::exr::error::Error::Aborted);
                }
                cached_y = y;
            }
            let mut data = Vec::with_capacity(index.pixel_size.area() * 16);
            // EXR's channel list is alphabetically sorted: A, B, G, R.
            for row in 0..index.pixel_size.y() {
                for channel in [3, 2, 1, 0] {
                    for x in index.pixel_position.x()..index.pixel_position.x() + index.pixel_size.x() {
                        let value = rgba[(row * width as usize + x) * 4 + channel];
                        data.extend_from_slice(&value.to_ne_bytes());
                    }
                }
            }
            compressor.compress_block(number, UncompressedBlock { index, data })?;
        }
        Ok(())
    });
    if let Some(error) = source_error { return Err(error); }
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{Picture, save, save_streaming};

    #[test]
    fn tiny_and_flat_png_match_image() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let reference = directory.path().join("reference.png");
        let streamed = directory.path().join("streamed.png");
        for (width, height) in [(1, 1), (1, 65), (129, 130)] {
            for value in [0.0, 0.5, 1.0] {
                for depth in [SampleDepth::U8, SampleDepth::U16] {
                    for icc in [None, Some(b"opaque test ICC profile".as_slice())] {
                        let rgba = vec![value; width as usize * height as usize * 4];
                        let picture = Picture { width, height, depth, rgba, icc_profile: icc.map(<[u8]>::to_vec) };
                        save(&reference, &picture, depth)?;
                        save_streaming(&streamed, width, height, depth, icc, |y, rows, out| {
                            assert!(rows <= 64);
                            let start = y as usize * width as usize * 4;
                            out.copy_from_slice(&picture.rgba[start..start + out.len()]);
                            Ok(())
                        })?;
                        assert_eq!(std::fs::read(&streamed)?, std::fs::read(&reference)?, "{width}x{height} {value} {depth:?}");
                    }
                }
            }
        }
        Ok(())
    }
}
