#![cfg(target_os = "macos")]

use anyhow::{Context, Result, bail, ensure};
use moya_core::Params;
use moya_io::{Picture, SampleDepth};
use serde_json::{Value, json};
use std::{
    fs,
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tungstenite::{Message, WebSocket};

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

fn edit(id: &str, source: &PathBuf, depth: u32, return_mode: &str) -> Value {
    json!({
        "type": "edit", "id": id, "source": source, "params": null,
        "document": {"name": "Bridge protocol acceptance", "id": 1,
            "width": 32, "height": 24, "depth": depth},
        "returnMode": return_mode,
    })
}

/// Exercises the real GUI application's Open -> HQ render -> Send path.
/// Requires an interactive macOS desktop and no existing Moya bridge server.
#[test]
#[ignore = "launches the macOS GUI and uses the fixed Moya bridge port"]
fn bridge_protocol_depth_roundtrips() -> Result<()> {
    let reservation = TcpListener::bind(ADDRESS).with_context(|| {
        format!("bridge acceptance test needs {ADDRESS} free; quit the existing Moya/server yourself (the test will not kill it)")
    })?;
    let home = std::env::var_os("HOME").context("HOME is required to find Moya bridge discovery")?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = std::env::temp_dir().join(format!("moya-bridge-{}-{nonce}", std::process::id()));
    fs::create_dir(&temp)?;
    let mut app = TestApp {
        child: None,
        discovery: PathBuf::from(home).join("Library/Application Support/Moya/bridge.json"),
        temp,
    };
    let cases = [(8, SampleDepth::U8, "glow"), (16, SampleDepth::U16, "composite"), (32, SampleDepth::F32, "glow")];
    for (bits, depth, _) in cases {
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
                    ensure!(value["version"] == 1, "unsupported discovery version: {value}");
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
    let hello = json!({"type": "hello", "version": 1, "token": discovery["token"], "client": "moya-bridge"});
    let mut bad_hello = hello.clone();
    let mut bad_token = discovery["token"].as_str().unwrap().to_owned();
    let replacement = if bad_token.starts_with('0') { "1" } else { "0" };
    bad_token.replace_range(..1, replacement);
    bad_hello["token"] = json!(bad_token);
    let mut unauthenticated = connect()?;
    assert_rejected(&mut unauthenticated, bad_hello).context("bad-token hello")?;
    drop(unauthenticated);
    let mut pre_hello = connect()?;
    assert_rejected(&mut pre_hello, edit("00000000-0000-4000-8000-000000000000", &app.temp.join("source-8.tiff"), 8, "glow"))
        .context("edit before hello")?;
    drop(pre_hello);

    let mut socket = connect()?;
    send(&mut socket, hello)?;
    let response = receive(&mut socket)?;
    ensure!(response["type"] == "hello" && response["version"] == 1 && response["app"] == "moya", "hello failed: {response}");
    ensure!(response["appVersion"].as_str().is_some_and(|s| !s.is_empty()), "hello lacks app version: {response}");
    let mut previous_id = None::<String>;
    for (index, (bits, depth, return_mode)) in cases.into_iter().enumerate() {
        let id = format!("{:08x}-0000-4000-8000-{:012x}", pid, ((nonce + index as u128) & 0xffffffffffff) as u64);
        send(&mut socket, edit(&id, &app.temp.join(format!("source-{bits}.tiff")), bits, return_mode))?;
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
        let blend = match (return_mode, bits) {
            ("composite", _) => "normal",
            ("glow", 32) => "linearDodge",
            _ => "screen",
        };
        ensure!(result["blend"] == blend && result["layerName"] == "Moya Glow", "wrong placement metadata: {result}");
        let params: Params = serde_json::from_value(result["params"].clone()).context("result must contain full Params")?;
        let serialized_params = serde_json::to_value(&params)?;
        ensure!(serialized_params.as_object().unwrap().keys().all(|key| result["params"].get(key).is_some()), "result params must contain the complete parameter model");
        ensure!(params.hdr32 == (bits == 32), "result params do not match {bits}-bit document");
        if bits == 32 { ensure!(params.gamma == 1.0, "32-bit document must use linear gamma"); }
        if return_mode == "glow" { ensure!(!params.unmult, "glow metadata must preserve the re-edit params, not export-only Unmult"); }
        let path = PathBuf::from(result["path"].as_str().context("result TIFF path is missing")?);
        ensure!(path.is_absolute(), "result path must be absolute: {}", path.display());
        ensure!(path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("tif") || ext.eq_ignore_ascii_case("tiff")), "result is not a TIFF: {}", path.display());
        ensure!(path.file_name().is_some_and(|name| name.to_string_lossy().ends_with(".moya.tif")), "wrong Moya result filename: {}", path.display());
        let picture = moya_io::load(&path).with_context(|| format!("loading {bits}-bit result {}", path.display()))?;
        assert_eq!((picture.width, picture.height, picture.depth), (32, 24, depth), "result TIFF dimensions/depth");
        send(&mut socket, json!({"type": "placed", "id": id}))?;
        previous_id = Some(id);
    }
    let _ = socket.close(None);
    Ok(())
}
