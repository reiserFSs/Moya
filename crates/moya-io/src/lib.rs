//! Image file I/O, preserving stored color values and ICC profiles without color conversion.
use std::{fs::File, io::{BufReader, BufWriter, Write}, path::Path};
use anyhow::{Context, Result, bail, ensure};
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder};

mod psd;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleDepth { U8, U16, F32 }

/// Straight-alpha RGBA f32. U8/U16 sources: encoded values 0..1 as stored. F32 (EXR, 32-bit TIFF/PSD): linear, unclamped.
pub struct Picture { pub width: u32, pub height: u32, pub depth: SampleDepth, pub rgba: Vec<f32>, pub icc_profile: Option<Vec<u8>> }

/// PNG, JPEG, TIFF (8/16/32), EXR, PSD/PSB (flattened composite; 8/16/32-bit).
/// ICC bytes are retained, not interpreted. EXR RGB values are read as stored (linear).
/// PSD/PSB supports RGB/grayscale raw, PackBits, ZIP and ZIP prediction; layers
/// are skipped and composites exceeding 1 GiB decoded working storage are rejected.
pub fn load(path: &Path) -> Result<Picture> {
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    if matches!(extension(path).as_str(), "psd" | "psb") {
        return psd::load(path).with_context(|| format!("loading {}", path.display()));
    }
    let format = reader.format();
    let mut decoder = reader.into_decoder().with_context(|| format!("opening {}", path.display()))?;
    let mut icc_profile = decoder.icc_profile()?;
    let depth = match decoder.color_type() {
        image::ColorType::L16 | image::ColorType::La16 | image::ColorType::Rgb16 | image::ColorType::Rgba16 => SampleDepth::U16,
        image::ColorType::Rgb32F | image::ColorType::Rgba32F => SampleDepth::F32,
        _ => SampleDepth::U8,
    };
    let image = DynamicImage::from_decoder(decoder)?.into_rgba32f();
    let (width, height) = image.dimensions();
    let mut rgba = image.into_raw();
    if format == Some(image::ImageFormat::Tiff) {
        let mut tags = tiff::decoder::Decoder::new(BufReader::new(File::open(path)?))?;
        icc_profile = tags.find_tag(tiff::tags::Tag::IccProfile)?
            .map(|value| value.into_u8_vec()).transpose()?;
        if let Some(extra) = tags.find_tag_unsigned_vec::<u16>(tiff::tags::Tag::ExtraSamples)? {
            if extra.first() == Some(&1) {
                for pixel in rgba.chunks_exact_mut(4) {
                    if pixel[3] != 0.0 {
                        let alpha = pixel[3];
                        for channel in &mut pixel[..3] { *channel /= alpha; }
                    } else { pixel[..3].fill(0.0); }
                }
            }
        }
    }
    Ok(Picture { width, height, depth, rgba, icc_profile })
}

/// Format from extension: png (8/16), tif/tiff (8/16/32), exr (f32), jpg (8).
/// Integer outputs clamp and round stored values; no implicit gamma/color transform occurs.
/// JPEG drops alpha. TIFF explicitly tags unassociated alpha; EXR stores straight RGB and alpha.
/// Unsupported format/depth combinations fail before opening the destination.
pub fn save(path: &Path, pic: &Picture, depth: SampleDepth) -> Result<()> {
    let count = (pic.width as usize).checked_mul(pic.height as usize).and_then(|n| n.checked_mul(4));
    ensure!(pic.width > 0 && pic.height > 0 && count == Some(pic.rgba.len()), "invalid RGBA image dimensions or sample count");
    let ext = extension(path);
    match (ext.as_str(), depth) {
        ("png", SampleDepth::U8 | SampleDepth::U16) | ("jpg" | "jpeg", SampleDepth::U8)
        | ("tif" | "tiff", _) | ("exr", SampleDepth::F32) => {},
        _ => bail!("unsupported output format/depth: {ext} / {depth:?}"),
    }
    let mut writer = BufWriter::new(File::create(path)?);
    match ext.as_str() {
        "png" => {
            let mut encoder = image::codecs::png::PngEncoder::new(&mut writer);
            if let Some(icc) = &pic.icc_profile { encoder.set_icc_profile(icc.clone())?; }
            match depth {
                SampleDepth::U8 => encoder.write_image(&quantize8(&pic.rgba), pic.width, pic.height, ExtendedColorType::Rgba8)?,
                SampleDepth::U16 => encoder.write_image(bytemuck::cast_slice(&quantize16(&pic.rgba)), pic.width, pic.height, ExtendedColorType::Rgba16)?,
                _ => unreachable!(),
            }
        }
        "jpg" | "jpeg" => {
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 95);
            if let Some(icc) = &pic.icc_profile { encoder.set_icc_profile(icc.clone())?; }
            let rgb: Vec<u8> = pic.rgba.chunks_exact(4).flat_map(|p| p[..3].iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect();
            encoder.write_image(&rgb, pic.width, pic.height, ExtendedColorType::Rgb8)?;
        }
        "tif" | "tiff" => {
            let mut encoder = tiff::encoder::TiffEncoder::new(&mut writer)?;
            match depth {
                SampleDepth::U8 => write_tiff::<_, tiff::encoder::colortype::RGBA8>(&mut encoder, pic, &quantize8(&pic.rgba))?,
                SampleDepth::U16 => write_tiff::<_, tiff::encoder::colortype::RGBA16>(&mut encoder, pic, &quantize16(&pic.rgba))?,
                SampleDepth::F32 => write_tiff::<_, tiff::encoder::colortype::RGBA32Float>(&mut encoder, pic, &pic.rgba)?,
            }
        }
        "exr" => image::codecs::openexr::OpenExrEncoder::new(&mut writer).write_image(bytemuck::cast_slice(&pic.rgba), pic.width, pic.height, ExtendedColorType::Rgba32F)?,
        _ => unreachable!(),
    }
    writer.flush().with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Writes an 8-bit grayscale PNG (one byte per pixel, row-major). Validates before creating the file.
pub fn save_gray_png(path: &Path, width: u32, height: u32, gray: &[u8]) -> Result<()> {
    ensure!(width > 0 && height > 0 && (width as usize).checked_mul(height as usize) == Some(gray.len()), "invalid grayscale image dimensions or sample count");
    let mut writer = BufWriter::new(File::create(path)?);
    image::codecs::png::PngEncoder::new(&mut writer).write_image(gray, width, height, ExtendedColorType::L8)?;
    writer.flush().with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn write_tiff<W: Write + std::io::Seek, C: tiff::encoder::colortype::ColorType>(encoder: &mut tiff::encoder::TiffEncoder<W>, pic: &Picture, data: &[C::Inner]) -> Result<()>
where [C::Inner]: tiff::encoder::TiffValue {
    let mut image = encoder.new_image::<C>(pic.width, pic.height)?;
    image.encoder().write_tag(tiff::tags::Tag::ExtraSamples, &[2u16][..])?;
    if let Some(icc) = &pic.icc_profile { image.encoder().write_tag(tiff::tags::Tag::IccProfile, icc.as_slice())?; }
    image.write_data(data)?;
    Ok(())
}

fn extension(path: &Path) -> String { path.extension().unwrap_or_default().to_string_lossy().to_ascii_lowercase() }
fn quantize8(data: &[f32]) -> Vec<u8> { data.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect() }
fn quantize16(data: &[f32]) -> Vec<u16> { data.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16).collect() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_depth_roundtrips_and_icc() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut pic = Picture { width: 7, height: 5, depth: SampleDepth::F32, rgba: Vec::new(), icc_profile: Some(b"test opaque ICC profile payload".to_vec()) };
        for _ in 0..35 { pic.rgba.extend_from_slice(&[0.25, 0.5, 0.75, 0.4]); }
        for (ext, depth, tolerance) in [("png", SampleDepth::U8, 1.0/255.0), ("png", SampleDepth::U16, 1.0/65535.0), ("jpg", SampleDepth::U8, 0.025), ("tif", SampleDepth::U8, 1.0/255.0), ("tiff", SampleDepth::U16, 1.0/65535.0), ("tiff", SampleDepth::F32, 0.0), ("exr", SampleDepth::F32, 0.0)] {
            let path = dir.path().join(format!("image-{depth:?}.{ext}"));
            save(&path, &pic, depth)?;
            let actual = load(&path)?;
            assert_eq!((actual.width, actual.height, actual.depth), (7, 5, depth));
            if ext != "exr" { assert_eq!(actual.icc_profile, pic.icc_profile, "{ext} ICC"); }
            for (i, (a, b)) in actual.rgba.iter().zip(&pic.rgba).enumerate() {
                if ext == "jpg" && i % 4 == 3 { assert_eq!(*a, 1.0); }
                else { assert!((a-b).abs() <= tolerance, "{ext} {depth:?} sample {i}: {a} != {b}"); }
            }
        }
        Ok(())
    }

    #[test]
    fn float_formats_preserve_hdr_and_straight_alpha() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let pic = Picture { width: 2, height: 1, depth: SampleDepth::F32, rgba: vec![-0.5, 4.0, 0.25, 0.25, 5.0, 0.5, 3.0, 0.0], icc_profile: None };
        for ext in ["tiff", "exr"] {
            let path = dir.path().join(format!("hdr.{ext}"));
            save(&path, &pic, SampleDepth::F32)?;
            assert_eq!(load(&path)?.rgba, pic.rgba);
        }
        Ok(())
    }

    #[test]
    fn associated_tiff_is_converted_to_straight() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("associated.tiff");
        let mut encoder = tiff::encoder::TiffEncoder::new(File::create(&path)?)?;
        let mut image = encoder.new_image::<tiff::encoder::colortype::RGBA32Float>(1, 1)?;
        image.encoder().write_tag(tiff::tags::Tag::ExtraSamples, &[1u16][..])?;
        image.write_data(&[0.125, 0.25, 0.5, 0.25])?;
        drop(encoder);
        assert_eq!(load(&path)?.rgba, [0.5, 1.0, 2.0, 0.25]);
        Ok(())
    }

    #[test]
    fn rejects_invalid_images_without_truncation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("keep.png");
        std::fs::write(&path, b"keep")?;
        let pic = Picture { width: 1, height: 1, depth: SampleDepth::F32, rgba: vec![0.0;4], icc_profile: None };
        assert!(save(&path, &pic, SampleDepth::F32).is_err());
        assert_eq!(std::fs::read(path)?, b"keep");
        Ok(())
    }
}
