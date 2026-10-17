use anyhow::{Result, bail};
use std::{fs::File, io::{BufReader, BufWriter}, path::Path};
use tiff::{decoder::{Decoder, DecodingResult}, encoder::{TiffEncoder, colortype}, ColorType};

pub fn save(path: &Path, width: u32, height: u32, pixels: &[[f32; 4]], depth: u8) -> Result<()> {
    anyhow::ensure!(pixels.len() == width as usize * height as usize, "pixel dimensions differ");
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let mut enc = TiffEncoder::new(BufWriter::new(File::create(path)?))?;
    match depth {
        8 => enc.write_image::<colortype::RGB8>(width, height, &pixels.iter().flat_map(|p| p[..3].iter().map(|v| (v.clamp(0.,1.) * 255.).round() as u8)).collect::<Vec<_>>())?,
        16 => enc.write_image::<colortype::RGB16>(width, height, &pixels.iter().flat_map(|p| p[..3].iter().map(|v| (v.clamp(0.,1.) * 65535.).round() as u16)).collect::<Vec<_>>())?,
        32 => enc.write_image::<colortype::RGB32Float>(width, height, &pixels.iter().flat_map(|p| p[..3].iter().copied()).collect::<Vec<_>>())?,
        _ => bail!("unsupported TIFF depth {depth}"),
    }
    Ok(())
}

pub fn load(path: &Path) -> Result<(u32, u32, Vec<[f32;4]>)> {
    let mut dec = Decoder::new(BufReader::new(File::open(path)?))?;
    let (w,h) = dec.dimensions()?;
    let channels = match dec.colortype()? { ColorType::RGB(_) => 3, ColorType::RGBA(_) => 4, ColorType::Gray(_) => 1, other => bail!("unsupported TIFF color type {other:?}") };
    let data: Vec<f32> = match dec.read_image()? {
        DecodingResult::U8(v) => v.into_iter().map(|x| x as f32 / 255.).collect(),
        DecodingResult::U16(v) => v.into_iter().map(|x| x as f32 / 65535.).collect(),
        DecodingResult::F32(v) => v,
        other => bail!("unsupported TIFF samples {other:?}"),
    };
    anyhow::ensure!(data.len() == w as usize*h as usize*channels, "TIFF sample count differs");
    let pixels = data.chunks_exact(channels).map(|p| if channels == 1 {[p[0],p[0],p[0],1.]} else {[p[0],p[1],p[2],if channels == 4 {p[3]} else {1.}]}).collect();
    Ok((w,h,pixels))
}
