//! Authenticated localhost transport for the Photoshop bridge.
use anyhow::{Context, Result, bail};
use eframe::egui;
use moya_core::Params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket};

const PORT: u16 = 47811;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    pub id: String,
    pub source: PathBuf,
    pub params: Option<Params>,
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
    Glow,
    Composite,
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
        let listener = TcpListener::bind(("127.0.0.1", PORT))
            .context("starting Photoshop bridge on port 47811")?;
        listener.set_nonblocking(true)?;
        let mut random = [0u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let home = std::env::var_os("HOME").context("HOME is unavailable")?;
        let directory = PathBuf::from(home).join("Library/Application Support/Moya");
        fs::create_dir_all(&directory)?;
        let discovery = directory.join("bridge.json");
        let temporary = directory.join(format!(".bridge-{token}.json"));
        let publish = (|| -> Result<()> {
            let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600)
                .open(&temporary)?;
            serde_json::to_writer(&mut file, &json!({
                "version": 1, "port": PORT, "token": token, "pid": std::process::id(),
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
            serve(listener, &worker_token, worker_stop, event_tx, outgoing_rx, ctx);
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

fn authenticated(value: &Value, token: &str) -> bool {
    value["type"] == "hello" && value["version"].as_u64() == Some(1)
        && value["token"].as_str() == Some(token)
        && value["client"] == "moya-bridge"
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
    listener: TcpListener,
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
        for _ in 0..4 {
            let Ok((stream, _)) = listener.accept() else { break };
            if candidates.len() >= 8 { continue; }
            let timeout = Some(Duration::from_millis(100));
            if stream.set_read_timeout(timeout).is_err() || stream.set_write_timeout(timeout).is_err() {
                continue;
            }
            if let Ok(socket) = tungstenite::accept(stream) {
                if socket.get_ref().set_nonblocking(true).is_ok() {
                    candidates.push((socket, Instant::now()));
                }
            }
        }
        let mut index = 0;
        while index < candidates.len() {
            let (socket, started) = &mut candidates[index];
            let mut remove = started.elapsed() > Duration::from_secs(5);
            let mut accept = false;
            if !remove {
                match socket.read() {
                    Ok(Message::Text(text)) => {
                        accept = serde_json::from_str::<Value>(&text).ok()
                            .is_some_and(|value| authenticated(&value, token));
                        remove = true;
                    }
                    Ok(Message::Ping(_) | Message::Pong(_)) => { let _ = socket.flush(); }
                    Ok(_) => remove = true,
                    Err(error) => remove = !pending(&error),
                }
            }
            if remove {
                let (mut socket, _) = candidates.remove(index);
                if accept && send(&mut socket, json!({
                    "type": "hello", "app": "moya", "version": 1, "appVersion": env!("CARGO_PKG_VERSION"),
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

    #[test]
    fn authentication_and_edit_validation() {
        let hello = json!({"type":"hello", "version":1, "token":"secret", "client":"moya-bridge"});
        assert!(authenticated(&hello, "secret"));
        assert!(!authenticated(&hello, "wrong"));
        for (field, value) in [("type", json!("edit")), ("version", json!(2)), ("client", json!("other"))] {
            let mut invalid = hello.clone();
            invalid[field] = value;
            assert!(!authenticated(&invalid, "secret"));
        }
        let edit = json!({
            "type":"edit", "id":"00112233-4455-6677-8899-aabbccddeeff",
            "source":"/tmp/source.tif", "returnMode":"glow",
            "document":{"name":"Photo", "id":1, "depth":16, "width":20, "height":10},
        });
        assert!(matches!(parse_event(edit.clone()), Ok(Event::Edit(_))));
        for (field, value) in [("id", json!("invalid")), ("source", json!("relative.tif")), ("returnMode", json!("unknown"))] {
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
    }
}
