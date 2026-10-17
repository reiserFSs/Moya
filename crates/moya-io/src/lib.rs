//! Image file I/O, preserving stored color values and ICC profiles without color conversion.
use std::{fs::File, io::{BufReader, BufWriter, Write}, path::Path};
use anyhow::{Context, Result, bail, ensure};
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder};

mod psd;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleDepth { U8, U16, F32 }

/// Straight-alpha RGBA f32. U8/U16 sources: encoded values 0..1 as stored. F32 (EXR, 32-bit TIFF/PSD): linear, unclamped.
pub struct Picture { pub width: u32, pub height: u32, pub depth: SampleDepth, pub rgba: Vec<f32>, pub icc_profile: Option<Vec<u8>> }

/// Bounds checked from the header before allocating decoded pixels.
#[derive(Clone, Copy, Debug)]
pub struct LoadLimits { pub max_pixels: u64, pub max_bytes: u64 }

impl Default for LoadLimits {
    fn default() -> Self { Self { max_pixels: 16384 * 16384, max_bytes: 4 * 1024 * 1024 * 1024 } }
}

impl LoadLimits {
    pub(crate) fn check(self, width: u32, height: u32) -> Result<()> {
        let pixels = u64::from(width) * u64::from(height);
        ensure!(width > 0 && height > 0 && width <= 16384 && height <= 16384,
            "image dimensions {width}x{height} exceed the 16384-per-side decode limit");
        ensure!(pixels <= self.max_pixels && pixels.checked_mul(16).is_some_and(|n| n <= self.max_bytes),
            "image {width}x{height} exceeds decoded pixel/byte limits");
        Ok(())
    }
}

/// PNG, JPEG, TIFF (8/16/32), EXR, PSD/PSB (flattened composite; 8/16/32-bit).
/// ICC bytes are retained, not interpreted. EXR RGB values are read as stored (linear).
/// PSD/PSB supports RGB/grayscale raw, PackBits, ZIP and ZIP prediction; layers
/// are skipped. Decoded storage is limited by [`LoadLimits`].
pub fn load(path: &Path) -> Result<Picture> {
    load_with_limits(path, LoadLimits::default())
}

/// Loads using caller-supplied pixel and decoded-byte budgets, with a 16384-per-side ceiling.
pub fn load_with_limits(path: &Path, limits: LoadLimits) -> Result<Picture> {
    if matches!(extension(path).as_str(), "psd" | "psb") {
        return psd::load(path, limits).with_context(|| format!("loading {}", path.display()));
    }
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder_limits = image::Limits::default();
    decoder_limits.max_image_width = Some(16384);
    decoder_limits.max_image_height = Some(16384);
    decoder_limits.max_alloc = Some(limits.max_bytes);
    reader.limits(decoder_limits);
    let format = reader.format();
    let mut decoder = reader.into_decoder().with_context(|| format!("opening {}", path.display()))?;
    let (width, height) = decoder.dimensions();
    limits.check(width, height)?;
    ensure!(decoder.total_bytes() <= limits.max_bytes, "decoder storage exceeds byte limit");
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
/// Encodes into a sibling temporary file; failures preserve the destination.
pub fn save(path: &Path, pic: &Picture, depth: SampleDepth) -> Result<()> {
    let count = (pic.width as usize).checked_mul(pic.height as usize).and_then(|n| n.checked_mul(4));
    ensure!(pic.width > 0 && pic.height > 0 && count == Some(pic.rgba.len()), "invalid RGBA image dimensions or sample count");
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
            let bytes_per_sample = match depth { SampleDepth::U8 => 1, SampleDepth::U16 => 2, SampleDepth::F32 => 4 };
            if needs_big_tiff(pic.rgba.len() as u64 * bytes_per_sample, pic.icc_profile.as_ref().map_or(0, |p| p.len() as u64)) {
                let mut encoder = tiff::encoder::TiffEncoder::new_big(&mut writer)?;
                encode_tiff(&mut encoder, pic, depth)?;
            } else {
                let mut encoder = tiff::encoder::TiffEncoder::new(&mut writer)?;
                encode_tiff(&mut encoder, pic, depth)?;
            }
        }
        "exr" => image::codecs::openexr::OpenExrEncoder::new(&mut writer).write_image(bytemuck::cast_slice(&pic.rgba), pic.width, pic.height, ExtendedColorType::Rgba32F)?,
        _ => unreachable!(),
    }
    writer.flush().with_context(|| format!("writing {}", path.display()))?;
    drop(writer);
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
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

// Reserve room for IFDs, strip offsets and profile metadata near the classic TIFF ceiling.
fn needs_big_tiff(pixel_bytes: u64, profile_bytes: u64) -> bool {
    pixel_bytes.saturating_add(profile_bytes).saturating_add(16 * 1024 * 1024) >= u32::MAX as u64
}

fn encode_tiff<W: Write + std::io::Seek, K: tiff::encoder::TiffKind>(encoder: &mut tiff::encoder::TiffEncoder<W, K>, pic: &Picture, depth: SampleDepth) -> Result<()> {
    match depth {
        SampleDepth::U8 => write_tiff::<_, _, tiff::encoder::colortype::RGBA8>(encoder, pic, &quantize8(&pic.rgba)),
        SampleDepth::U16 => write_tiff::<_, _, tiff::encoder::colortype::RGBA16>(encoder, pic, &quantize16(&pic.rgba)),
        SampleDepth::F32 => write_tiff::<_, _, tiff::encoder::colortype::RGBA32Float>(encoder, pic, &pic.rgba),
    }
}

fn write_tiff<W: Write + std::io::Seek, K: tiff::encoder::TiffKind, C: tiff::encoder::colortype::ColorType>(encoder: &mut tiff::encoder::TiffEncoder<W, K>, pic: &Picture, data: &[C::Inner]) -> Result<()>
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
    fn decoder_headers_obey_injected_limits() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let pic = Picture { width: 2, height: 2, depth: SampleDepth::F32, rgba: vec![0.5; 16], icc_profile: None };
        for (ext, depth) in [("png", SampleDepth::U8), ("tiff", SampleDepth::F32), ("exr", SampleDepth::F32)] {
            let path = dir.path().join(format!("limit.{ext}"));
            save(&path, &pic, depth)?;
            if ext == "exr" {
                let mut header = std::fs::read(&path)?;
                for name in [b"dataWindow\0box2i\0".as_slice(), b"displayWindow\0box2i\0".as_slice()] {
                    let offset = header.windows(name.len()).position(|w| w == name).unwrap() + name.len() + 4;
                    header[offset + 8..offset + 12].copy_from_slice(&999_999i32.to_le_bytes());
                    header[offset + 12..offset + 16].copy_from_slice(&999_999i32.to_le_bytes());
                }
                let huge = dir.path().join("huge.exr");
                std::fs::write(&huge, header)?;
                assert!(load_with_limits(&huge, LoadLimits { max_pixels: 1, max_bytes: 1024 }).is_err());
            }
            assert!(load_with_limits(&path, LoadLimits { max_pixels: 1, max_bytes: 1024 }).is_err(), "{ext} pixel limit");
            assert!(load_with_limits(&path, LoadLimits { max_pixels: 100, max_bytes: 16 }).is_err(), "{ext} byte limit");
        }
        assert!(LoadLimits::default().check(1_000_000, 1_000_000).is_err());
        Ok(())
    }

    #[test]
    fn encoder_failure_preserves_destination_and_removes_temp() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("victim.jpg");
        std::fs::write(&path, b"ORIGINAL CONTENT")?;
        let pic = Picture { width: 70000, height: 1, depth: SampleDepth::U8, rgba: vec![0.5; 70000 * 4], icc_profile: None };
        assert!(save(&path, &pic, SampleDepth::U8).is_err());
        assert_eq!(std::fs::read(&path)?, b"ORIGINAL CONTENT");
        assert_eq!(std::fs::read_dir(dir.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn big_tiff_threshold_and_encoder() -> Result<()> {
        assert!(!needs_big_tiff(1024, 0));
        assert!(needs_big_tiff(u32::MAX as u64 - 1024, 0));
        assert!(needs_big_tiff(1, u32::MAX as u64));
        let pic = Picture { width: 1, height: 1, depth: SampleDepth::F32, rgba: vec![0.5; 4], icc_profile: None };
        let mut bytes = std::io::Cursor::new(Vec::new());
        encode_tiff(&mut tiff::encoder::TiffEncoder::new_big(&mut bytes)?, &pic, SampleDepth::F32)?;
        assert_eq!(&bytes.get_ref()[..4], b"II\x2b\x00");
        let mut decoder = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes.into_inner()))?;
        assert_eq!(decoder.dimensions()?, (1, 1));
        assert!(matches!(decoder.read_image()?, tiff::decoder::DecodingResult::F32(data) if data == pic.rgba));
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
