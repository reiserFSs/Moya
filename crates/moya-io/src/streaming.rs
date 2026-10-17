use std::{io::{BufWriter, Seek, Write}, path::Path};
use anyhow::{Context, Result, bail, ensure};
use super::{SampleDepth, extension, needs_big_tiff};

/// Encodes bounded RGBA f32 bands into an atomically replaced file.
/// Integer outputs use the same stored-value quantization as [`super::save`].
/// PNG/TIFF/EXR retain only rows or strips; JPEG buffers RGB8 because its encoder
/// requires a complete image. ICC is preserved except for EXR, matching `save`.
/// `fill` may be called more than once for a row (EXR block ordering).
pub fn save_streaming(path: &Path, width: u32, height: u32, depth: SampleDepth, icc: Option<&[u8]>, mut fill: impl FnMut(u32, u32, &mut [f32]) -> Result<()>) -> Result<()> {
    let count = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4));
    ensure!(width > 0 && height > 0 && count.is_some(), "invalid RGBA image dimensions or sample count");
    let ext = extension(path);
    match (ext.as_str(), depth) {
        ("png", SampleDepth::U8 | SampleDepth::U16) | ("jpg" | "jpeg", SampleDepth::U8)
        | ("tif" | "tiff", _) | ("exr", SampleDepth::F32) => {},
        _ => bail!("unsupported output format/depth: {ext} / {depth:?}"),
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut writer = BufWriter::new(temporary.as_file_mut());
    match ext.as_str() {
        "png" => super::stream_codecs::png(&mut writer, width, height, depth, icc, &mut fill)?,
        "exr" => super::stream_codecs::exr(&mut writer, width, height, &mut fill)?,
        "tif" | "tiff" => {
            let bytes = match depth { SampleDepth::U8 => 1, SampleDepth::U16 => 2, SampleDepth::F32 => 4 };
            if needs_big_tiff(count.unwrap() as u64 * bytes, icc.map_or(0, |p| p.len() as u64)) {
                tiff_bands(&mut tiff::encoder::TiffEncoder::new_big(&mut writer)?, width, height, depth, icc, &mut fill)?;
            } else {
                tiff_bands(&mut tiff::encoder::TiffEncoder::new(&mut writer)?, width, height, depth, icc, &mut fill)?;
            }
        }
        "jpg" | "jpeg" => {
            use image::ImageEncoder;
            let mut rgb = vec![0u8; count.unwrap() / 4 * 3];
            let mut band = vec![0.0; width as usize * height.min(64) as usize * 4];
            for y in (0..height).step_by(64) {
                let rows = (height - y).min(64);
                let samples = &mut band[..width as usize * rows as usize * 4];
                fill(y, rows, samples)?;
                let offset = y as usize * width as usize * 3;
                for (dst, src) in rgb[offset..offset + samples.len() / 4 * 3].chunks_exact_mut(3).zip(samples.chunks_exact(4)) {
                    for c in 0..3 { dst[c] = (src[c].clamp(0.0, 1.0) * 255.0).round() as u8; }
                }
            }
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 95);
            if let Some(icc) = icc { encoder.set_icc_profile(icc.to_vec())?; }
            encoder.write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)?;
        }
        _ => unreachable!(),
    }
    writer.flush().with_context(|| format!("writing {}", path.display()))?;
    drop(writer);
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn tiff_bands<W: Write + Seek, K: tiff::encoder::TiffKind>(encoder: &mut tiff::encoder::TiffEncoder<W, K>, width: u32, height: u32, depth: SampleDepth, icc: Option<&[u8]>, fill: &mut impl FnMut(u32, u32, &mut [f32]) -> Result<()>) -> Result<()> {
    macro_rules! strips {
        ($color:ty, $convert:expr) => {{
            let mut image = encoder.new_image::<$color>(width, height)?;
            image.encoder().write_tag(tiff::tags::Tag::ExtraSamples, &[2u16][..])?;
            if let Some(icc) = icc { image.encoder().write_tag(tiff::tags::Tag::IccProfile, icc)?; }
            let row_samples = width as usize * 4;
            let strip_rows = image.next_strip_sample_count() as usize / row_samples;
            // Keep the on-disk strips identical while amortizing GPU readback
            // submission/map overhead across several small strips.
            let band_rows = strip_rows * (64 / strip_rows).max(1);
            let mut band = vec![0.0; band_rows * row_samples];
            let mut converted = Vec::new();
            let mut y = 0;
            while y < height {
                let rows = (height - y).min(band_rows as u32);
                let samples = rows as usize * row_samples;
                fill(y, rows, &mut band[..samples])?;
                let mut offset = 0;
                while offset < samples {
                    let count = image.next_strip_sample_count() as usize;
                    ($convert)(&mut image, &band[offset..offset + count], &mut converted)?;
                    offset += count;
                }
                y += rows;
            }
            image.finish()?;
        }};
    }
    macro_rules! integer {
        ($color:ty, $ty:ty, $scale:expr) => {
            strips!($color, |image: &mut tiff::encoder::ImageEncoder<'_, W, $color, K>, band: &[f32], converted: &mut Vec<$ty>| -> Result<()> {
                converted.clear();
                converted.extend(band.iter().map(|v| (v.clamp(0.0, 1.0) * $scale).round() as $ty));
                image.write_strip(converted)?;
                Ok(())
            });
        };
    }
    match depth {
        SampleDepth::U8 => { integer!(tiff::encoder::colortype::RGBA8, u8, 255.0); }
        SampleDepth::U16 => { integer!(tiff::encoder::colortype::RGBA16, u16, 65535.0); }
        SampleDepth::F32 => {
            strips!(tiff::encoder::colortype::RGBA32Float, |image: &mut tiff::encoder::ImageEncoder<'_, W, tiff::encoder::colortype::RGBA32Float, K>, band: &[f32], _: &mut Vec<f32>| -> Result<()> {
                image.write_strip(band)?;
                Ok(())
            });
        }
    }
    Ok(())
}
