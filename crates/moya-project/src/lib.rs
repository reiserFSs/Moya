//! Versioned `.moya` ZIP projects: JSON settings, lossless 16-bit PNG masks and optional source.
use std::{collections::HashSet, fs::File, io::{Cursor, Read, Seek, Write}, path::{Path, PathBuf}};
use anyhow::{Context, Result, ensure};
use image::{ImageDecoder, ImageEncoder};
use moya_core::LayerSettings;
use serde::{Deserialize, Serialize};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const VERSION: u32 = 1;
const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_JSON: u64 = 4 * 1024 * 1024;
const MAX_SIDE: u32 = 16384;

#[derive(Clone, Debug, PartialEq)]
pub struct MaskData { pub width: u32, pub height: u32, pub data: Vec<u16>, pub enabled: bool }
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectLayer { pub settings: LayerSettings, pub mask: Option<MaskData> }
#[derive(Clone, Debug, PartialEq)]
pub enum SourceRef { Path(PathBuf), Embedded { file_name: String, bytes: Vec<u8> } }
#[derive(Clone, Debug, PartialEq)]
pub struct Project { pub version: u32, pub source: SourceRef, pub layers: Vec<ProjectLayer>, pub selected: Option<u64> }

#[derive(Serialize, Deserialize)]
struct Manifest { version: u32, source: ManifestSource, layers: Vec<ManifestLayer>, selected: Option<u64> }
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ManifestSource { Path { path: PathBuf }, Embedded { file_name: String } }
#[derive(Serialize, Deserialize)]
struct ManifestLayer {
    settings: LayerSettings,
    #[serde(default)] mask: Option<String>,
    #[serde(default = "default_enabled")] mask_enabled: bool,
}
fn default_enabled() -> bool { true }

fn safe_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\\', ':', '\0']) && !name.starts_with('/')
        && name.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}
fn source_name(name: &str) -> bool { safe_name(name) && !name.contains('/') }
fn mask_size(width: u32, height: u32) -> Result<u64> {
    ensure!(width > 0 && height > 0 && width <= MAX_SIDE && height <= MAX_SIDE,
        "mask dimensions {width}x{height} exceed the {MAX_SIDE}-per-side source limit");
    Ok(u64::from(width) * u64::from(height) * 2)
}
fn validate(p: &Project) -> Result<()> {
    ensure!(p.version == VERSION, "unsupported Moya project version {}", p.version);
    ensure!(p.layers.len() + 2 <= MAX_ENTRIES, "too many project layers");
    let mut ids = HashSet::new();
    let mut decoded = 0u64;
    for layer in &p.layers {
        ensure!(ids.insert(layer.settings.id), "duplicate layer id {}", layer.settings.id);
        if let Some(mask) = &layer.mask {
            let bytes = mask_size(mask.width, mask.height)?;
            ensure!(bytes / 2 == mask.data.len() as u64, "invalid mask sample count");
            decoded += bytes;
        }
    }
    ensure!(decoded <= MAX_BYTES, "decoded masks exceed project size limit");
    ensure!(p.selected.is_none_or(|id| ids.contains(&id)), "selected layer is missing");
    match &p.source {
        SourceRef::Path(path) => ensure!(!path.as_os_str().is_empty(), "source path is empty"),
        SourceRef::Embedded { file_name, bytes } => {
            ensure!(source_name(file_name), "invalid embedded source filename");
            ensure!(!bytes.is_empty() && bytes.len() as u64 + decoded <= MAX_BYTES, "embedded source exceeds project size limit or is empty");
        }
    }
    Ok(())
}

/// Writes beside the destination, then atomically replaces it; failures leave the old file intact.
pub fn save(path: &Path, p: &Project) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    write_archive(temp.as_file_mut(), p)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error).with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

/// Relative source references are resolved against the project folder, never the process cwd.
pub fn load(path: &Path) -> Result<Project> {
    let mut p = read_archive(File::open(path).with_context(|| format!("opening {}", path.display()))?)?;
    if let SourceRef::Path(source) = &mut p.source {
        if source.is_relative() {
            let absolute = std::path::absolute(path)?;
            *source = absolute.parent().unwrap().join(&*source);
        }
        ensure!(source.is_file(), "Moya project source image is missing: {}", source.display());
    }
    Ok(p)
}

/// In-memory loading preserves path references without requiring the source file to exist.
pub fn to_bytes(p: &Project) -> Result<Vec<u8>> { Ok(write_archive(Cursor::new(Vec::new()), p)?.into_inner()) }
pub fn from_bytes(b: &[u8]) -> Result<Project> {
    ensure!(b.len() as u64 <= MAX_BYTES, "project archive exceeds size limit");
    read_archive(Cursor::new(b))
}

fn write_archive<W: Write + Seek>(writer: W, p: &Project) -> Result<W> {
    validate(p)?;
    let manifest = Manifest {
        version: p.version,
        source: match &p.source {
            SourceRef::Path(path) => ManifestSource::Path { path: path.clone() },
            SourceRef::Embedded { file_name, .. } => ManifestSource::Embedded { file_name: file_name.clone() },
        },
        layers: p.layers.iter().map(|l| ManifestLayer { settings: l.settings.clone(), mask: l.mask.as_ref().map(|_| format!("masks/{}.png", l.settings.id)), mask_enabled: l.mask.as_ref().is_none_or(|m| m.enabled) }).collect(),
        selected: p.selected,
    };
    let json = serde_json::to_vec(&manifest)?;
    ensure!(json.len() as u64 <= MAX_JSON, "project JSON exceeds size limit");
    let mut zip = ZipWriter::new(writer);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("project.json", options)?;
    zip.write_all(&json)?;
    for layer in &p.layers {
        if let Some(mask) = &layer.mask {
            zip.start_file(format!("masks/{}.png", layer.settings.id), options)?;
            image::codecs::png::PngEncoder::new(&mut zip).write_image(bytemuck::cast_slice(&mask.data), mask.width, mask.height, image::ExtendedColorType::L16)?;
        }
    }
    if let SourceRef::Embedded { file_name, bytes } = &p.source {
        zip.start_file(format!("source/{file_name}"), options)?;
        zip.write_all(bytes)?;
    }
    Ok(zip.finish()?)
}

fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str, limit: u64) -> Result<Vec<u8>> {
    let mut entry = zip.by_name(name).with_context(|| format!("missing project entry {name}"))?;
    ensure!(entry.size() <= limit, "project entry {name} exceeds size limit");
    let mut bytes = Vec::new();
    (&mut entry).take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "project entry {name} exceeds size limit");
    Ok(bytes)
}
fn read_archive<R: Read + Seek>(reader: R) -> Result<Project> {
    let mut zip = ZipArchive::new(reader).context("invalid Moya ZIP archive")?;
    ensure!(zip.len() <= MAX_ENTRIES, "too many project archive entries");
    let mut total = 0u64;
    let mut names = HashSet::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        ensure!(safe_name(entry.name()), "unsafe project entry name: {}", entry.name());
        ensure!(names.insert(entry.name().to_owned()), "duplicate project entry name");
        ensure!(entry.unix_mode().is_none_or(|m| m & 0o170000 != 0o120000), "project symlinks are not allowed");
        total = total.checked_add(entry.size()).context("project size overflow")?;
        ensure!(total <= MAX_BYTES, "uncompressed project exceeds size limit");
    }
    let manifest: Manifest = serde_json::from_slice(&read_entry(&mut zip, "project.json", MAX_JSON)?)?;
    ensure!(manifest.version == VERSION, "unsupported Moya project version {}", manifest.version);
    ensure!(manifest.layers.len() + 2 <= MAX_ENTRIES, "too many project layers");
    let source = match manifest.source {
        ManifestSource::Path { path } => SourceRef::Path(path),
        ManifestSource::Embedded { file_name } => {
            ensure!(source_name(&file_name), "invalid embedded source filename");
            let bytes = read_entry(&mut zip, &format!("source/{file_name}"), MAX_BYTES)?;
            SourceRef::Embedded { file_name, bytes }
        }
    };
    let mut layers = Vec::with_capacity(manifest.layers.len());
    let mut decoded = match &source { SourceRef::Embedded { bytes, .. } => bytes.len() as u64, SourceRef::Path(_) => 0 };
    for mut layer in manifest.layers {
        let mask = if let Some(name) = layer.mask {
            ensure!(name == format!("masks/{}.png", layer.settings.id), "invalid layer mask entry");
            let bytes = read_entry(&mut zip, &name, MAX_BYTES)?;
            let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes))?;
            let (width, height) = decoder.dimensions();
            decoded = decoded.checked_add(mask_size(width, height)?).context("decoded mask size overflow")?;
            ensure!(decoded <= MAX_BYTES, "decoded masks exceed project size limit");
            ensure!(decoder.color_type() == image::ColorType::L16, "project mask must be a 16-bit grayscale PNG");
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(MAX_SIDE);
            limits.max_image_height = Some(MAX_SIDE);
            limits.max_alloc = Some(MAX_BYTES);
            decoder.set_limits(limits)?;
            let data = image::DynamicImage::from_decoder(decoder)?.into_luma16().into_raw();
            Some(MaskData { width, height, data, enabled: layer.mask_enabled })
        } else { None };
        layer.settings.sanitize();
        layers.push(ProjectLayer { settings: layer.settings, mask });
    }
    let p = Project { version: manifest.version, source, layers, selected: manifest.selected };
    validate(&p)?;
    Ok(p)
}

#[cfg(test)]
mod tests;
