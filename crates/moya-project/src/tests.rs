use super::*;
use moya_core::{LayerBlend, LayerInput};

fn project() -> Project {
    Project { version: VERSION, source: SourceRef::Embedded { file_name: "original.png".into(), bytes: b"source bytes preserved verbatim".to_vec() }, selected: Some(2), layers: (1..=3).map(|id| ProjectLayer {
        settings: LayerSettings { id, name: format!("Glow {id}"), opacity: 0.25 * id as f32, blend: if id == 2 { LayerBlend::Lighten } else { LayerBlend::Auto }, input: if id == 3 { LayerInput::Below } else { LayerInput::Original }, ..Default::default() },
        mask: Some(MaskData { width: 3, height: 2, data: vec![0, 1, 255, 256, 32768, 65535], enabled: id != 2 }),
    }).collect() }
}
fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(*name, SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
#[test]
fn three_layer_round_trip_memory_and_atomic_file() -> Result<()> {
    let p = project();
    let bytes = to_bytes(&p)?;
    assert_eq!(from_bytes(&bytes)?, p);
    let mut zip = ZipArchive::new(Cursor::new(&bytes))?;
    assert_eq!(zip.len(), 5);
    assert_eq!(zip.by_name("masks/2.png")?.compression(), zip::CompressionMethod::Deflated);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.moya");
    save(&path, &p)?;
    assert_eq!(load(&path)?, p);
    let mut invalid = p.clone();
    invalid.layers[0].mask.as_mut().unwrap().data.clear();
    assert!(save(&path, &invalid).is_err());
    assert_eq!(load(&path)?, p);
    assert_eq!(std::fs::read_dir(dir.path())?.count(), 1);
    Ok(())
}
#[test]
fn paths_resolve_at_project_and_missing_source_is_clear() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.moya");
    let mut p = project();
    p.source = SourceRef::Path("source.tif".into());
    save(&path, &p)?;
    let error = load(&path).unwrap_err().to_string();
    assert!(error.contains("source image is missing"), "{error}");
    assert!(error.contains("source.tif"));
    assert_eq!(from_bytes(&to_bytes(&p)?)?, p); // memory transport needs no local source
    std::fs::write(dir.path().join("source.tif"), b"present")?;
    let loaded = load(&path)?;
    assert_eq!(loaded.source, SourceRef::Path(dir.path().join("source.tif")));
    p.source = loaded.source;
    save(&path, &p)?;
    assert_eq!(load(&path)?, p);
    Ok(())
}
#[test]
fn malformed_archives_and_unsafe_names_are_rejected() {
    assert!(from_bytes(b"not a zip").is_err());
    for name in ["../escape", "/absolute", "a/../../escape", "C:/escape", "a\\escape", "a/./x"] {
        assert!(from_bytes(&archive(&[(name, b"bad")])).is_err(), "{name}");
    }
    let mut bomb = archive(&[("project.json", b"{}")]);
    let central = bomb.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
    bomb[central + 24..central + 28].copy_from_slice(&((MAX_BYTES + 1) as u32).to_le_bytes());
    assert!(from_bytes(&bomb).unwrap_err().to_string().contains("size limit"));
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..=MAX_ENTRIES { zip.start_file(format!("entry{i}"), SimpleFileOptions::default()).unwrap(); }
    assert!(from_bytes(&zip.finish().unwrap().into_inner()).unwrap_err().to_string().contains("too many"));
}
#[test]
fn versions_unknown_fields_ids_and_mask_headers() -> Result<()> {
    let p = project();
    let bytes = to_bytes(&p)?;
    let mut zip = ZipArchive::new(Cursor::new(bytes))?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..zip.len() { let mut e = zip.by_index(i)?; let mut b = Vec::new(); e.read_to_end(&mut b)?; entries.push((e.name().to_owned(), b)); }
    let mut json: serde_json::Value = serde_json::from_slice(&entries[0].1)?;
    json["future"] = serde_json::json!({"ignored": true});
    entries[0].1 = serde_json::to_vec(&json)?;
    let encode = |entries: &[(String, Vec<u8>)]| archive(&entries.iter().map(|(n,b)| (n.as_str(), b.as_slice())).collect::<Vec<_>>());
    assert_eq!(from_bytes(&encode(&entries))?, p);
    for layer in json["layers"].as_array_mut().unwrap() { layer.as_object_mut().unwrap().remove("mask_enabled"); }
    entries[0].1 = serde_json::to_vec(&json)?;
    assert!(from_bytes(&encode(&entries))?.layers.iter().all(|l| l.mask.as_ref().unwrap().enabled));
    json["version"] = 999.into();
    entries[0].1 = serde_json::to_vec(&json)?;
    assert!(from_bytes(&encode(&entries)).unwrap_err().to_string().contains("version"));
    json["version"] = VERSION.into();
    json["layers"][1]["settings"]["id"] = 1.into();
    json["layers"][1]["mask"] = "masks/1.png".into();
    entries[0].1 = serde_json::to_vec(&json)?;
    assert!(from_bytes(&encode(&entries)).unwrap_err().to_string().contains("duplicate layer"));
    entries[0].1 = read_entry(&mut zip, "project.json", MAX_JSON)?;
    let mut huge = Vec::new();
    image::codecs::png::PngEncoder::new(&mut huge).write_image(&vec![0; (MAX_SIDE as usize + 1) * 2], MAX_SIDE + 1, 1, image::ExtendedColorType::L16)?;
    entries[1].1 = huge;
    assert!(from_bytes(&encode(&entries)).is_err());
    entries[1].1 = b"corrupt PNG".to_vec();
    assert!(from_bytes(&encode(&entries)).is_err());
    Ok(())
}
