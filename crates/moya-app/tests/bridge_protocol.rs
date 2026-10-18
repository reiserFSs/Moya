#![cfg(target_os = "macos")]

use anyhow::{Context, Result, bail, ensure};
use moya_core::{LayerBlend, LayerSettings};
use moya_io::{Picture, SampleDepth};
use serde_json::{Value, json};
use std::{
    fs,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tungstenite::{Message, WebSocket};
#[path = "../src/platform_paths.rs"]
mod platform_paths;

const ADDRESS: &str = "127.0.0.1:47811";
const WAIT: Duration = Duration::from_secs(90);

struct TestApp {
    child: Option<Child>,
    discovery: PathBuf,
    temp: PathBuf,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let pid = child.id();
            if matches!(child.try_wait(), Ok(None)) {
                // Target only this child; never quit another running Moya instance.
                let script = format!("tell application \"System Events\"\nset targetProcess to first process whose unix id is {pid}\ntell targetProcess\nset frontmost to true\nkeystroke \"q\" using command down\nend tell\nend tell");
                let mut quit = Command::new("/usr/bin/osascript").args(["-e", &script])
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().ok();
                let deadline = Instant::now() + Duration::from_secs(3);
                while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(50));
                }
                if let Some(quit) = &mut quit {
                    if !matches!(quit.try_wait(), Ok(Some(_))) {
                        let _ = quit.kill();
                    }
                    let _ = quit.wait();
                }
                if !matches!(child.try_wait(), Ok(Some(_))) {
                    let _ = child.kill();
                }
            }
            let _ = child.wait();
            let owns_discovery = fs::read(&self.discovery).ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|value| value["pid"].as_u64() == Some(u64::from(pid)));
            if owns_discovery {
                let _ = fs::remove_file(&self.discovery);
            }
        }
        let _ = fs::remove_dir_all(&self.temp);
    }
}

fn connect() -> Result<WebSocket<TcpStream>> {
    let stream = TcpStream::connect_timeout(&ADDRESS.parse()?, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(WAIT))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let (socket, _) = tungstenite::client(format!("ws://{ADDRESS}"), stream)?;
    Ok(socket)
}

fn send(socket: &mut WebSocket<TcpStream>, value: Value) -> Result<()> {
    socket.send(Message::Text(value.to_string().into()))?;
    Ok(())
}

fn receive(socket: &mut WebSocket<TcpStream>) -> Result<Value> {
    loop {
        match socket.read()? {
            Message::Text(text) => return Ok(serde_json::from_str(text.as_str())?),
            Message::Ping(_) => socket.flush()?,
            Message::Pong(_) => {},
            message => bail!("expected a JSON bridge response, received {message:?}"),
        }
    }
}

fn assert_rejected(socket: &mut WebSocket<TcpStream>, request: Value) -> Result<()> {
    send(socket, request)?;
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                let response: Value = serde_json::from_str(text.as_str())?;
                ensure!(response["type"] == "error", "unauthenticated request was not rejected: {response}");
                ensure!(response["message"].as_str().is_some_and(|s| !s.is_empty()), "rejection has no explanation: {response}");
                return Ok(());
            }
            Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed)
                | Err(tungstenite::Error::AlreadyClosed) => return Ok(()),
            Ok(Message::Ping(_)) => socket.flush()?,
            Ok(Message::Pong(_)) => {},
            other => bail!("expected explicit authentication rejection, received {other:?}"),
        }
    }
}

fn edit(id: &str, source: &Path, depth: u32, return_mode: &str, project: Option<&Path>) -> Value {
    json!({
        "type": "edit", "id": id, "source": source, "projectPath": project,
        "document": {"name": "Bridge protocol acceptance", "id": 1,
            "width": 32, "height": 24, "depth": depth},
        "returnMode": return_mode,
    })
}

fn assert_tiff(path: &Path, expected: &Path, depth: SampleDepth) -> Result<Picture> {
    ensure!(path.is_absolute() && path == expected, "wrong TIFF path: {} (expected {})", path.display(), expected.display());
    let bytes = fs::read(path)?;
    ensure!(bytes.starts_with(b"II\x2a\x00") || bytes.starts_with(b"MM\x00\x2a"), "output is not a TIFF: {}", path.display());
    let picture = moya_io::load(path).with_context(|| format!("loading {}", path.display()))?;
    ensure!((picture.width, picture.height, picture.depth) == (32, 24, depth), "wrong TIFF dimensions/depth: {}", path.display());
    ensure!(picture.rgba.len() == 32 * 24 * 4, "incomplete TIFF pixels");
    ensure!(picture.rgba.iter().all(|value| value.is_finite()), "TIFF contains nonfinite samples");
    ensure!(picture.rgba.chunks_exact(4).all(|pixel| (0.0..=1.0).contains(&pixel[3])), "TIFF contains invalid alpha");
    Ok(picture)
}

fn assert_error(socket: &mut WebSocket<TcpStream>, request: Value, previous: Option<&str>) -> Result<()> {
    let id = request["id"].clone();
    send(socket, request)?;
    if let Some(previous) = previous {
        let cancelled = receive(socket)?;
        ensure!(cancelled["type"] == "cancelled" && cancelled["id"] == previous, "expected replacement cancellation: {cancelled}");
    }
    let response = receive(socket)?;
    ensure!(response["type"] == "error" && response["id"] == id, "expected request error: {response}");
    ensure!(response["message"].as_str().is_some_and(|message| !message.is_empty()), "error has no explanation: {response}");
    Ok(())
}

/// Exercises the real GUI application's Open -> HQ render -> Send path.
/// Requires an interactive macOS desktop and no existing Moya bridge server.
#[test]
#[ignore = "launches the macOS GUI and uses the fixed Moya bridge port"]
fn bridge_protocol_depth_roundtrips() -> Result<()> {
    let reservation = TcpListener::bind(ADDRESS).with_context(|| {
        format!("bridge acceptance test needs {ADDRESS} free; quit the existing Moya/server yourself (the test will not kill it)")
    })?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = std::env::temp_dir().join(format!("moya-bridge-{}-{nonce}", std::process::id()));
    fs::create_dir(&temp)?;
    let mut app = TestApp {
        child: None,
        discovery: platform_paths::bridge_discovery_path()?,
        temp,
    };
    let depths = [(8, SampleDepth::U8), (16, SampleDepth::U16), (32, SampleDepth::F32)];
    for (bits, depth) in depths {
        let mut rgba = Vec::with_capacity(32 * 24 * 4);
        for y in 0..24 {
            for x in 0..32 {
                rgba.extend_from_slice(&[x as f32 / 31.0, y as f32 / 23.0, 0.75, 1.0]);
            }
        }
        moya_io::save(&app.temp.join(format!("source-{bits}.tiff")), &Picture {
            width: 32, height: 24, depth, rgba, icc_profile: None,
        }, depth)?;
    }
    drop(reservation);
    app.child = Some(Command::new(env!("CARGO_BIN_EXE_moya"))
        .arg("--bridge-test-auto-send").spawn().context("launching Moya")?);
    let pid = app.child.as_ref().unwrap().id();
    let deadline = Instant::now() + WAIT;
    let mut last_discovery = String::from("discovery file not yet readable");
    let discovery = loop {
        if let Some(status) = app.child.as_mut().unwrap().try_wait()? {
            bail!("Moya exited before publishing bridge discovery: {status}; {last_discovery}");
        }
        if let Ok(bytes) = fs::read(&app.discovery) {
            last_discovery = String::from_utf8_lossy(&bytes).into_owned();
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                if value["pid"].as_u64() == Some(u64::from(pid)) {
                    ensure!(value["version"] == 2, "unsupported discovery version: {value}");
                    ensure!(value["port"] == 47811, "unexpected discovery port: {value}");
                    let token = value["token"].as_str().context("discovery token is missing")?;
                    ensure!(token.len() == 32 && token.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)), "invalid discovery token: {value}");
                    break value;
                }
            }
        }
        ensure!(Instant::now() < deadline, "timed out waiting for bridge discovery for child pid {pid}: {last_discovery}");
        thread::sleep(Duration::from_millis(100));
    };
    let hello = json!({"type": "hello", "version": 2, "token": discovery["token"], "client": "moya-bridge", "nonce": "0".repeat(64)});
    let mut bad_hello = hello.clone();
    let mut bad_token = discovery["token"].as_str().unwrap().to_owned();
    let replacement = if bad_token.starts_with('0') { "1" } else { "0" };
    bad_token.replace_range(..1, replacement);
    bad_hello["token"] = json!(bad_token);
    let mut unauthenticated = connect()?;
    assert_rejected(&mut unauthenticated, bad_hello).context("bad-token hello")?;
    drop(unauthenticated);
    let mut pre_hello = connect()?;
    assert_rejected(&mut pre_hello, edit("00000000-0000-4000-8000-000000000000", &app.temp.join("source-8.tiff"), 8, "layers", None))
        .context("edit before hello")?;
    drop(pre_hello);

    let mut socket = connect()?;
    send(&mut socket, hello)?;
    let response = receive(&mut socket)?;
    ensure!(response["type"] == "hello" && response["version"] == 2 && response["app"] == "moya", "hello failed: {response}");
    ensure!(response["appVersion"].as_str().is_some_and(|s| !s.is_empty()), "hello lacks app version: {response}");
    let source = app.temp.join("source-8.tiff");
    let error_id = "00000000-0000-4000-8000-000000000001";
    assert_error(&mut socket, edit(error_id, &source, 8, "layers", Some(Path::new("relative.moya"))), None)?;
    assert_error(&mut socket, edit(error_id, &source, 12, "layers", None), None)?;
    assert_error(&mut socket, edit(error_id, &app.temp.join("missing.tiff"), 8, "layers", None), None)?;
    assert_error(&mut socket, edit(error_id, &source, 8, "layers", Some(&app.temp.join("missing.moya"))), Some(error_id))?;

    // Distinct bottom-to-top metadata catches reordered, flattened or omitted hidden layers.
    let custom_path = app.temp.join("custom.moya");
    let custom = moya_project::Project {
        version: moya_project::VERSION,
        source: moya_project::SourceRef::Path(source.clone()),
        layers: [LayerBlend::Screen, LayerBlend::LinearDodge, LayerBlend::Lighten, LayerBlend::Normal]
            .into_iter().enumerate().map(|(index, blend)| {
                let mut settings = LayerSettings {
                    id: index as u64 + 1, name: format!("Saved layer {index}"),
                    blend, opacity: (index + 1) as f32 * 0.25, visible: index != 1,
                    ..LayerSettings::default()
                };
                settings.params.threshold = 0.0;
                moya_project::ProjectLayer { settings, mask: None }
            }).collect(),
        selected: Some(3),
    };
    moya_project::save(&custom_path, &custom)?;
    let mut cases = Vec::new();
    for (bits, depth) in depths {
        for mode in ["layers", "composite"] {
            cases.push((bits, depth, mode, None));
        }
    }
    cases.push((8, SampleDepth::U8, "layers", Some(custom_path.clone())));
    cases.push((8, SampleDepth::U8, "composite", Some(custom_path)));
    let mut previous_id = Some(error_id.to_owned());
    for (index, (bits, depth, return_mode, project)) in cases.into_iter().enumerate() {
        let id = format!("{:08x}-0000-4000-8000-{:012x}", pid, ((nonce + index as u128) & 0xffffffffffff) as u64);
        let source = app.temp.join(format!("source-{bits}.tiff"));
        send(&mut socket, edit(&id, &source, bits, return_mode, project.as_deref()))?;
        if let Some(previous) = &previous_id {
            let cancelled = receive(&mut socket).context("waiting for previous session replacement")?;
            ensure!(cancelled["type"] == "cancelled" && cancelled["id"] == *previous,
                "expected replacement cancellation for {previous}: {cancelled}");
        }
        let opened = receive(&mut socket).with_context(|| format!("waiting for {bits}-bit opened"))?;
        ensure!(opened["type"] == "opened" && opened["id"] == id, "expected opened for {id}: {opened}");
        let result = receive(&mut socket).with_context(|| format!("waiting for {bits}-bit HQ Send result"))?;
        ensure!(result["type"] == "result" && result["id"] == id, "expected result for {id}: {result}");
        ensure!(result["depth"] == bits && result["returnMode"] == return_mode, "wrong result depth/mode: {result}");
        ensure!(result["groupName"] == "Moya", "wrong result group: {result}");
        for legacy in ["params", "path", "blend", "layerName"] {
            ensure!(result.get(legacy).is_none(), "legacy {legacy} field in v2 result: {result}");
        }
        let project_path = PathBuf::from(result["projectPath"].as_str().context("missing projectPath")?);
        ensure!(project_path.is_absolute() && project_path == source.with_extension("moya"), "wrong project path: {result}");
        let bytes = fs::read(&project_path)?;
        ensure!(bytes.starts_with(b"PK\x03\x04"), "project must be a ZIP archive");
        let saved = moya_project::load(&project_path).context("reading exported project")?;
        ensure!(saved.source == moya_project::SourceRef::Path(source.clone()), "project must reference the source, not embed it");
        if project.is_some() {
            ensure!(saved.layers == custom.layers && saved.selected == custom.selected, "re-edit did not preserve saved layer settings/order");
        } else {
            ensure!(saved.layers.len() == 1, "new session must contain exactly one default layer");
            let settings = &saved.layers[0].settings;
            ensure!(settings.name == "Layer 1" && settings.visible && settings.opacity == 1.0 && settings.blend == LayerBlend::Auto, "wrong default layer");
        }
        let layers = result["layers"].as_array().context("missing layers array")?;
        let stem = source.file_stem().unwrap().to_string_lossy();
        if return_mode == "layers" {
            ensure!(result.get("composite") == Some(&Value::Null), "layers result must have null composite");
            ensure!(layers.len() == saved.layers.len(), "not all project layers were exported");
            for (index, (layer, saved_layer)) in layers.iter().zip(&saved.layers).enumerate() {
                let settings = &saved_layer.settings;
                let blend = match settings.blend {
                    LayerBlend::Auto if bits == 32 => "linearDodge",
                    LayerBlend::Auto | LayerBlend::Screen => "screen",
                    LayerBlend::LinearDodge => "linearDodge",
                    LayerBlend::Lighten => "lighten",
                    LayerBlend::Normal => "normal",
                };
                ensure!(layer["name"] == settings.name && layer["blend"] == blend && layer["visible"] == settings.visible, "wrong layer metadata/order: {layer}");
                let opacity = layer["opacity"].as_f64().context("missing numeric opacity")?;
                ensure!((0.0..=100.0).contains(&opacity) && (opacity - f64::from(settings.opacity) * 100.0).abs() < 0.0001, "wrong layer opacity: {layer}");
                let path = PathBuf::from(layer["path"].as_str().context("missing layer TIFF path")?);
                let expected = source.with_file_name(format!("{stem}.moya-layer-{}.tif", index + 1));
                let picture = assert_tiff(&path, &expected, depth)?;
                if project.is_some() {
                    ensure!(picture.rgba.chunks_exact(4).any(|pixel| pixel[..3].iter().any(|value| *value > 0.0)), "saved layer TIFF contains no effect pixels");
                }
            }
        } else {
            ensure!(layers.is_empty(), "composite result must not contain layers");
            let path = PathBuf::from(result["composite"].as_str().context("missing composite TIFF path")?);
            let picture = assert_tiff(&path, &source.with_file_name(format!("{stem}.moya.tif")), depth)?;
            ensure!(picture.rgba.chunks_exact(4).all(|pixel| pixel[3] == 1.0), "opaque source must produce an opaque composite");
            ensure!(picture.rgba.chunks_exact(4).any(|pixel| pixel[..3].iter().any(|value| *value > 0.0)), "composite TIFF is empty");
        }
        send(&mut socket, json!({"type": "placed", "id": id}))?;
        previous_id = Some(id);
    }
    let _ = socket.close(None);
    Ok(())
}
