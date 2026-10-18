//! Authenticated localhost transport for the Photoshop bridge.
use anyhow::{Context, Result, bail};
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const PORT: u16 = 47811;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    pub id: String,
    pub source: PathBuf,
    pub project_path: Option<PathBuf>,
    pub document: Document,
    pub return_mode: ReturnMode,
}

#[derive(Clone, Deserialize)]
pub struct Document {
    pub name: String,
    pub id: i64,
    pub depth: u8,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ReturnMode {
    Layers,
    Composite,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum LayerBlend {
    Screen,
    LinearDodge,
    Lighten,
    Normal,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResultLayer {
    pub path: PathBuf,
    pub name: String,
    pub blend: LayerBlend,
    pub opacity: f32,
    pub visible: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultMessage {
    pub id: String,
    pub depth: u8,
    pub return_mode: ReturnMode,
    pub group_name: String,
    pub layers: Vec<ResultLayer>,
    pub composite: Option<PathBuf>,
    pub project_path: PathBuf,
}

impl ResultMessage {
    pub fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) { bail!("result id must be a UUID"); }
        if !matches!(self.depth, 8 | 16 | 32) { bail!("result depth must be 8, 16, or 32"); }
        if self.group_name != "Moya" { bail!("result groupName must be Moya"); }
        if !self.project_path.is_absolute() { bail!("projectPath must be an absolute path"); }
        match self.return_mode {
            ReturnMode::Layers if self.composite.is_some() => bail!("layers result cannot have a composite"),
            ReturnMode::Composite if self.composite.as_ref().is_none_or(|path| !path.is_absolute())
                || !self.layers.is_empty() => bail!("composite result needs an absolute composite path and no layers"),
            _ => {}
        }
        for layer in &self.layers {
            if !layer.path.is_absolute() { bail!("layer path must be absolute"); }
            if !layer.opacity.is_finite() || !(0.0..=100.0).contains(&layer.opacity) {
                bail!("layer opacity must be between 0 and 100");
            }
        }
        Ok(())
    }
}

pub enum Event {
    Edit(Edit),
    Placed(String),
    Error { id: String, message: String },
    Disconnected,
}

pub struct Bridge {
    pub events: mpsc::Receiver<Event>,
    outgoing: mpsc::Sender<Value>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    discovery: PathBuf,
    token: String,
}

impl Bridge {
    pub fn start(ctx: egui::Context) -> Result<Self> {
        // Bind before touching discovery: a second app must leave the owner alone.
        let listeners = bind_loopback(PORT)?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|error| anyhow::anyhow!("Generating bridge token: {error}"))?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let discovery = crate::platform_paths::bridge_discovery_path()?;
        let directory = discovery.parent().context("Discovery path has no directory")?;
        fs::create_dir_all(directory)?;
        let temporary = directory.join(format!(".bridge-{token}.json"));
        let publish = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            // Windows inherits the current user's private APPDATA directory ACL.
            let mut file = options.open(&temporary)?;
            serde_json::to_writer(&mut file, &json!({
                "version": 2, "port": PORT, "token": token, "pid": std::process::id(),
            }))?;
            file.flush()?;
            fs::rename(&temporary, &discovery)?;
            Ok(())
        })();
        if let Err(error) = publish {
            let _ = fs::remove_file(&temporary);
            return Err(error.context("publishing Photoshop bridge discovery"));
        }
        let (event_tx, events) = mpsc::channel();
        let (outgoing, outgoing_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_token = token.clone();
        let worker = thread::Builder::new().name("photoshop-bridge".into()).spawn(move || {
            serve(listeners, &worker_token, worker_stop, event_tx, outgoing_rx, ctx);
        });
        match worker {
            Ok(worker) => Ok(Self {
                events, outgoing, stop, thread: Some(worker), discovery, token,
            }),
            Err(error) => {
                remove_owned_discovery(&discovery, &token);
                Err(error.into())
            }
        }
    }

    pub fn send(&self, value: Value) {
        let _ = self.outgoing.send(value);
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
        remove_owned_discovery(&self.discovery, &self.token);
    }
}

fn remove_owned_discovery(path: &Path, token: &str) {
    if fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|value| value["token"].as_str() == Some(token))
    {
        let _ = fs::remove_file(path);
    }
}

fn bind_loopback(port: u16) -> Result<Vec<TcpListener>> {
    let ipv4 = TcpListener::bind(("127.0.0.1", port))
        .context("starting IPv4 Photoshop bridge")?;
    let port = ipv4.local_addr()?.port();
    let mut listeners = vec![ipv4];
    match TcpListener::bind(("::1", port)) {
        Ok(listener) => listeners.push(listener),
        Err(error) if matches!(error.kind(), io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported)
            || matches!(error.raw_os_error(), Some(47 | 97 | 10047)) => {
            eprintln!("IPv6 loopback unavailable; Photoshop bridge uses IPv4 only: {error}");
        }
        Err(error) => return Err(error).context("starting IPv6 Photoshop bridge"),
    }
    for listener in &listeners { listener.set_nonblocking(true)?; }
    Ok(listeners)
}

fn server_proof(token: &str, nonce: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(token.as_bytes());
    hash.update(nonce.as_bytes());
    format!("{:x}", hash.finalize())
}

fn accept_socket(stream: TcpStream) -> Result<WebSocket<TcpStream>> {
    // macOS accepted sockets inherit O_NONBLOCK from their listener.
    stream.set_nonblocking(false)?;
    let timeout = Some(Duration::from_millis(250));
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(4096))
        .max_frame_size(Some(4096));
    let socket = tungstenite::accept_with_config(stream, Some(config))?;
    socket.get_ref().set_nonblocking(true)?;
    Ok(socket)
}

fn authenticated(value: &Value, token: &str) -> bool {
    value["type"] == "hello" && value["version"].as_u64() == Some(2)
        && value["token"].as_str() == Some(token)
        && value["client"] == "moya-bridge"
        && value["nonce"].as_str().is_some_and(|nonce|
            nonce.len() == 64 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn valid_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) { byte == b'-' } else { byte.is_ascii_hexdigit() }
    })
}

fn parse_event(value: Value) -> Result<Event> {
    match value["type"].as_str() {
        Some("edit") => {
            let edit: Edit = serde_json::from_value(value)?;
            if !valid_id(&edit.id) { bail!("edit id must be a UUID"); }
            if !edit.source.is_absolute() { bail!("source must be an absolute path"); }
            if edit.project_path.as_ref().is_some_and(|path| !path.is_absolute()) {
                bail!("projectPath must be an absolute path");
            }
            if !matches!(edit.document.depth, 8 | 16 | 32) {
                bail!("document depth must be 8, 16, or 32");
            }
            if edit.document.width == 0 || edit.document.height == 0 {
                bail!("document dimensions must be nonzero");
            }
            Ok(Event::Edit(edit))
        }
        Some("placed") | Some("error") => {
            let id = value["id"].as_str().context("missing message id")?;
            if !valid_id(id) { bail!("message id must be a UUID"); }
            if value["type"] == "placed" {
                Ok(Event::Placed(id.into()))
            } else {
                Ok(Event::Error {
                    id: id.into(),
                    message: value["message"].as_str().context("missing error message")?.into(),
                })
            }
        }
        _ => bail!("unsupported bridge message"),
    }
}

fn pending(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(error)
        if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut))
}

fn send(socket: &mut WebSocket<TcpStream>, value: Value) -> bool {
    match socket.send(Message::Text(value.to_string().into())) {
        Ok(()) => true,
        Err(error) => pending(&error),
    }
}

fn notify(events: &mpsc::Sender<Event>, ctx: &egui::Context, event: Event) {
    let _ = events.send(event);
    ctx.request_repaint();
}

fn serve(
    listeners: Vec<TcpListener>,
    token: &str,
    stop: Arc<AtomicBool>,
    events: mpsc::Sender<Event>,
    outgoing: mpsc::Receiver<Value>,
    ctx: egui::Context,
) {
    let mut active: Option<WebSocket<TcpStream>> = None;
    let mut candidates: Vec<(WebSocket<TcpStream>, Instant)> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        // Bound unauthenticated connections and handshake time without disturbing the active client.
        for listener in &listeners {
            for _ in 0..4 {
                let Ok((stream, _)) = listener.accept() else { break };
                if candidates.len() >= 8 { candidates.remove(0); }
                if let Ok(socket) = accept_socket(stream) {
                    candidates.push((socket, Instant::now()));
                }
            }
        }
        let mut index = 0;
        while index < candidates.len() {
            let (socket, started) = &mut candidates[index];
            let mut remove = started.elapsed() > Duration::from_secs(1);
            let mut accept = false;
            let mut proof = String::new();
            if !remove {
                match socket.read() {
                    Ok(Message::Text(text)) => {
                        accept = text.len() <= 4096 && serde_json::from_str::<Value>(&text).ok()
                            .is_some_and(|value| {
                                if !authenticated(&value, token) { return false; }
                                proof = server_proof(token, value["nonce"].as_str().unwrap());
                                true
                            });
                        remove = true;
                    }
                    Ok(Message::Ping(_) | Message::Pong(_)) => { let _ = socket.flush(); }
                    Ok(_) => remove = true,
                    Err(error) => remove = !pending(&error),
                }
            }
            if remove {
                let (mut socket, _) = candidates.remove(index);
                socket.set_config(|config| {
                    config.max_message_size = Some(1024 * 1024);
                    config.max_frame_size = Some(1024 * 1024);
                });
                if accept && send(&mut socket, json!({
                    "type": "hello", "app": "moya", "version": 2, "appVersion": env!("CARGO_PKG_VERSION"),
                    "proof": proof,
                })) {
                    if let Some(mut previous) = active.take() {
                        let _ = previous.close(None);
                        notify(&events, &ctx, Event::Disconnected);
                    }
                    active = Some(socket);
                } else {
                    let _ = socket.close(None);
                }
            } else {
                index += 1;
            }
        }
        let mut disconnected = false;
        if let Some(socket) = &mut active {
            // Bound each turn so a busy client cannot delay shutdown or a replacement.
            for _ in 0..32 {
                match socket.read() {
                    Ok(Message::Text(text)) => {
                        let value = serde_json::from_str::<Value>(&text);
                        let id = value.as_ref().ok().and_then(|v| v["id"].as_str())
                            .unwrap_or("").to_owned();
                        match value.map_err(anyhow::Error::from).and_then(parse_event) {
                            Ok(event) => notify(&events, &ctx, event),
                            Err(error) => {
                                if !send(socket, json!({"type":"error", "id":id, "message":error.to_string()})) {
                                    disconnected = true;
                                    break;
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => { disconnected = true; break; }
                    Ok(Message::Ping(_) | Message::Pong(_)) => {}
                    Ok(_) => {
                        disconnected = true;
                        break;
                    }
                    Err(error) => {
                        disconnected = !pending(&error);
                        break;
                    }
                }
            }
            if !disconnected {
                for value in outgoing.try_iter().take(32) {
                    if !send(socket, value) { disconnected = true; break; }
                }
                if let Err(error) = socket.flush() {
                    disconnected |= !pending(&error);
                }
            }
        } else {
            // Responses belong to the connection that requested them, never its successor.
            for _ in outgoing.try_iter() {}
        }
        if disconnected {
            active = None;
            notify(&events, &ctx, Event::Disconnected);
        }
        thread::sleep(Duration::from_millis(10));
    }
    if let Some(mut socket) = active { let _ = socket.close(None); }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Server {
        port: u16,
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl Server {
        fn new() -> Self {
            let listeners = bind_loopback(0).unwrap();
            let port = listeners[0].local_addr().unwrap().port();
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = stop.clone();
            let (events, _) = mpsc::channel();
            let (_, outgoing) = mpsc::channel();
            let worker = thread::spawn(move || serve(listeners, "secret", worker_stop,
                events, outgoing, egui::Context::default()));
            Self { port, stop, worker: Some(worker) }
        }

        fn connect(&self, host: &str) -> WebSocket<TcpStream> {
            let stream = TcpStream::connect(format!("{host}:{}", self.port)).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            tungstenite::client(format!("ws://localhost:{}", self.port), stream).unwrap().0
        }

        fn authenticate(&self, socket: &mut WebSocket<TcpStream>) {
            let nonce = "a".repeat(64);
            socket.send(Message::Text(json!({"type":"hello", "version":2,
                "client":"moya-bridge", "token":"secret", "nonce":nonce}).to_string().into())).unwrap();
            let response: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(response["proof"], server_proof("secret", &nonce));
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    #[test]
    fn ipv6_loopback_authenticates() {
        let server = Server::new();
        server.authenticate(&mut server.connect("[::1]"));
    }

    #[test]
    fn ipv6_port_conflict_aborts_both_binds() {
        let reserved = TcpListener::bind("[::1]:0").unwrap();
        let port = reserved.local_addr().unwrap().port();
        assert!(bind_loopback(port).is_err());
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok());
    }

    #[test]
    fn oldest_idle_candidate_is_evicted_for_plugin() {
        let server = Server::new();
        let _idle: Vec<_> = (0..8).map(|_| server.connect("127.0.0.1")).collect();
        server.authenticate(&mut server.connect("127.0.0.1"));
    }

    #[test]
    fn blocking_upgrade_accepts_slow_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || loop {
            if let Ok((stream, _)) = listener.accept() {
                return accept_socket(stream).is_ok();
            }
            thread::sleep(Duration::from_millis(1));
        });
        let mut stream = TcpStream::connect(address).unwrap();
        thread::sleep(Duration::from_millis(50));
        stream.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n").unwrap();
        thread::sleep(Duration::from_millis(50));
        stream.write_all(b"Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").unwrap();
        assert!(worker.join().unwrap());
    }

    #[test]
    fn oversized_hello_header_rejected_without_payload() {
        let server = Server::new();
        let mut socket = server.connect("127.0.0.1");
        // Masked frame advertising 64 MiB; send only its header, not an allocation.
        socket.get_mut().write_all(&[0x81, 0xff, 0, 0, 0, 0, 4, 0, 0, 0, 1, 2, 3, 4]).unwrap();
        assert!(matches!(socket.read(), Ok(Message::Close(_)) | Err(_)));
    }

    #[test]
    fn authentication_and_edit_validation() {
        let temporary = std::env::temp_dir();
        let hello = json!({"type":"hello", "version":2, "token":"secret", "client":"moya-bridge", "nonce":"0".repeat(64)});
        assert!(authenticated(&hello, "secret"));
        assert!(!authenticated(&hello, "wrong"));
        for (field, value) in [("type", json!("edit")), ("version", json!(1)), ("client", json!("other"))] {
            let mut invalid = hello.clone();
            invalid[field] = value;
            assert!(!authenticated(&invalid, "secret"));
        }
        let edit = json!({
            "type":"edit", "id":"00112233-4455-6677-8899-aabbccddeeff",
            "source":temporary.join("source.tif"), "projectPath":null, "returnMode":"layers",
            "document":{"name":"Photo", "id":1, "depth":16, "width":20, "height":10},
        });
        assert!(matches!(parse_event(edit.clone()), Ok(Event::Edit(_))));
        for (field, value) in [("id", json!("invalid")), ("source", json!("relative.tif")), ("projectPath", json!("relative.moya")), ("returnMode", json!("glow"))] {
            let mut invalid = edit.clone();
            invalid[field] = value;
            assert!(parse_event(invalid).is_err());
        }
        for (field, value) in [("depth", json!(24)), ("width", json!(0)), ("height", json!(0))] {
            let mut invalid = edit.clone();
            invalid["document"][field] = value;
            assert!(parse_event(invalid).is_err());
        }
        for depth in [8, 16, 32] {
            let mut valid = edit.clone();
            valid["document"]["depth"] = json!(depth);
            valid["returnMode"] = json!("composite");
            assert!(parse_event(valid).is_ok());
        }
        let mut reedit = edit;
        reedit["projectPath"] = json!(temporary.join("session.moya"));
        assert!(matches!(parse_event(reedit), Ok(Event::Edit(Edit { project_path: Some(_), .. }))));
        for kind in ["placed", "error"] {
            assert!(parse_event(json!({"type":kind, "id":"bad", "message":"failed"})).is_err());
        }
    }

    #[test]
    fn result_layers_round_trip_and_validation() {
        let temporary = std::env::temp_dir();
        let mut result = ResultMessage {
            id: "00112233-4455-6677-8899-aabbccddeeff".into(),
            depth: 16,
            return_mode: ReturnMode::Layers,
            group_name: "Moya".into(),
            layers: vec![
                ResultLayer { path: temporary.join("first.tif"), name: "Bottom".into(),
                    blend: LayerBlend::Screen, opacity: 100.0, visible: true },
                ResultLayer { path: temporary.join("second.tif"), name: "Top".into(),
                    blend: LayerBlend::LinearDodge, opacity: 37.5, visible: false },
            ],
            composite: None,
            project_path: temporary.join("session.moya"),
        };
        result.validate().unwrap();
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["returnMode"], "layers");
        assert_eq!(value["groupName"], "Moya");
        assert_eq!(value["layers"][1]["blend"], "linearDodge");
        let decoded: ResultMessage = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.layers[0].name, "Bottom");
        assert!(!decoded.layers[1].visible);
        result.layers[0].opacity = f32::NAN;
        assert!(result.validate().is_err());
        result.layers[0].opacity = 101.0;
        assert!(result.validate().is_err());
        result.layers[0].opacity = 0.0;
        result.layers[0].path = "relative.tif".into();
        assert!(result.validate().is_err());
        result.layers.clear();
        result.return_mode = ReturnMode::Composite;
        assert!(result.validate().is_err());
        result.composite = Some(temporary.join("composite.tif"));
        result.validate().unwrap();
        result.project_path = "relative.moya".into();
        assert!(result.validate().is_err());
    }
}
