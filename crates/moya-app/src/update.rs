use eframe::egui;
use std::{sync::mpsc, time::{Duration, SystemTime, UNIX_EPOCH}};

const DAY: u64 = 24 * 60 * 60;
const SETTINGS: &str = "moya.check_updates";
const LAST_CHECK: &str = "moya.last_update_check";

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

/// A newer release, with its notes for the changelog dialog.
#[derive(Debug, PartialEq)]
struct Available {
    version: String,
    title: String,
    notes: Vec<Line>,
}

/// Release notes reduced to what the dialog draws (GitHub markdown is not rendered fully).
#[derive(Debug, PartialEq)]
enum Line { Heading(String), Bullet(String), Text(String), Gap }

pub struct Updates {
    pub enabled: bool,
    last_check: Option<u64>,
    receiver: Option<mpsc::Receiver<Option<Available>>>,
    available: Option<Available>,
    dialog_open: bool,
}

fn version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let mut number = || {
        let part = parts.next()?;
        if part.is_empty() || (part.len() > 1 && part.starts_with('0')) || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let result = (number()?, number()?, number()?);
    (parts.next().is_none()).then_some(result)
}

fn newer_tag(tag: &str, current: &str) -> Option<String> {
    let tag = tag.strip_prefix('v')?;
    (version(tag)? > version(current)?).then(|| tag.to_owned())
}

fn due(enabled: bool, last: Option<u64>, now: u64) -> bool {
    enabled && last.is_none_or(|last| now.saturating_sub(last) >= DAY)
}

fn parse_release(json: &str, current: &str) -> Option<Available> {
    let release: Release = serde_json::from_str(json).ok()?;
    let version = newer_tag(&release.tag_name, current)?;
    let title = release.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("Moya {version}"));
    Some(Available { version, title, notes: notes(release.body.as_deref().unwrap_or("")) })
}

/// Markdown-lite: headings, bullets and paragraphs; inline `**`/`` ` `` markers dropped; links shown
/// as their text. Stops at the checksum section, which isn't useful in the app.
fn notes(body: &str) -> Vec<Line> {
    fn inline(text: &str) -> String {
        let mut out = text.replace("**", "").replace('`', "");
        // [text](url) -> text
        while let (Some(open), Some(mid)) = (out.find('['), out.find("](")) {
            let Some(close) = out[mid..].find(')').map(|i| mid + i) else { break };
            if open > mid { break; }
            out = format!("{}{}{}", &out[..open], &out[open + 1..mid], &out[close + 1..]);
        }
        out.trim().to_owned()
    }
    let mut lines = Vec::new();
    for raw in body.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') && trimmed.to_ascii_lowercase().contains("sha-256") { break; }
        if trimmed.starts_with("```") { continue; }
        let next = if trimmed.is_empty() { Line::Gap }
            else if let Some(h) = trimmed.strip_prefix('#') { Line::Heading(inline(h.trim_start_matches('#'))) }
            else if let Some(b) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) { Line::Bullet(inline(b)) }
            else { Line::Text(inline(trimmed)) };
        if next == Line::Gap && matches!(lines.last(), None | Some(Line::Gap)) { continue; }
        lines.push(next);
    }
    while lines.last() == Some(&Line::Gap) { lines.pop(); }
    lines
}

impl Updates {
    pub fn new(storage: Option<&dyn eframe::Storage>, ctx: egui::Context) -> Self {
        let enabled = storage.and_then(|s| s.get_string(SETTINGS)).is_none_or(|s| s != "false");
        let mut last_check = storage.and_then(|s| s.get_string(LAST_CHECK)).and_then(|s| s.parse().ok());
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let receiver = if due(enabled, last_check, now) {
            last_check = Some(now);
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let result = (|| {
                    let config = ureq::Agent::config_builder()
                        .timeout_global(Some(Duration::from_secs(5)))
                        .max_redirects(0)
                        .build();
                    let mut response = ureq::Agent::new_with_config(config)
                        .get("https://api.github.com/repos/reiserFSs/Moya/releases/latest")
                        .header("User-Agent", concat!("Moya/", env!("CARGO_PKG_VERSION")))
                        .call().ok()?;
                    let json = response.body_mut().with_config().limit(512 * 1024).read_to_string().ok()?;
                    parse_release(&json, env!("CARGO_PKG_VERSION"))
                })();
                let _ = tx.send(result);
                ctx.request_repaint();
            });
            Some(rx)
        } else { None };
        Self { enabled, last_check, receiver, available: None, dialog_open: false }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string(SETTINGS, self.enabled.to_string());
        if let Some(last) = self.last_check {
            storage.set_string(LAST_CHECK, last.to_string());
        }
    }

    pub fn note(&mut self, ui: &mut egui::Ui) {
        if let Some(receiver) = &self.receiver {
            match receiver.try_recv() {
                Ok(result) => { self.available = result; self.receiver = None; }
                Err(mpsc::TryRecvError::Disconnected) => self.receiver = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.enabled { return; }
        let Some(available) = &self.available else { return };
        ui.separator();
        if ui.small_button(format!("Moya {} available", available.version)).on_hover_text("Show what's new").clicked() {
            self.dialog_open = true;
        }
        if !self.dialog_open { return; }
        let url = format!("https://github.com/reiserFSs/Moya/releases/tag/v{}", available.version);
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("moya.update_notes")).show(ui.ctx(), |ui| {
            ui.set_width(520.0);
            ui.heading(&available.title);
            ui.label(egui::RichText::new(format!("You have {}.", env!("CARGO_PKG_VERSION"))).weak());
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                if available.notes.is_empty() { ui.label("No release notes."); }
                for line in &available.notes {
                    match line {
                        Line::Heading(text) => { ui.add_space(4.0); ui.label(egui::RichText::new(text).strong()); }
                        Line::Bullet(text) => { ui.horizontal_top(|ui| { ui.label("•"); ui.add(egui::Label::new(text).wrap()); }); }
                        Line::Text(text) => { ui.add(egui::Label::new(text).wrap()); }
                        Line::Gap => { ui.add_space(4.0); }
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Open release page").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
                    close = true;
                }
                if ui.button("Close").clicked() { close = true; }
            });
        });
        if close || modal.should_close() { self.dialog_open = false; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_versions_only_compare_semantically() {
        for tag in ["v1.2.4", "v1.3.0", "v2.0.0", "v1.10.0"] {
            assert!(newer_tag(tag, "1.2.3").is_some(), "{tag}");
        }
        for tag in ["v1.2.3", "v1.2.2", "v1.2.4-rc.1", "v2.0.0+build", "garbage", "1.2.4", "v01.2.4", "v1.2", "v1.2.4.0"] {
            assert!(newer_tag(tag, "1.2.3").is_none(), "{tag}");
        }
    }

    #[test]
    fn throttle_respects_setting_and_full_day() {
        assert!(due(true, None, 100));
        assert!(!due(false, None, 100));
        assert!(!due(true, Some(100), 100 + DAY - 1));
        assert!(due(true, Some(100), 100 + DAY));
        assert!(!due(true, Some(200), 100));
    }

    #[test]
    fn parses_github_response_without_network() {
        let response = r#"{"tag_name":"v1.2.4","html_url":"https://github.com/reiserFSs/Moya/releases/tag/v1.2.4","name":"Moya"}"#;
        assert_eq!(parse_release(response, "1.2.3").map(|a| a.version).as_deref(), Some("1.2.4"));
        assert_eq!(parse_release(response, "1.2.4"), None);
        assert_eq!(parse_release("{}", "1.2.3"), None);
        assert_eq!(parse_release("invalid", "1.2.3"), None);
    }

    #[test]
    fn release_notes_keep_structure_and_drop_checksums() {
        let body = "Intro with **bold** and `code`.\n\n### Fixes\n- **Brush** faster, see [docs](https://x.y/z).\n\n\n* Second\n## SHA-256\n```text\nabc  file.zip\n```\n";
        let available = parse_release(&serde_json::json!({"tag_name":"v9.0.0","name":"","body":body}).to_string(), "1.0.0").unwrap();
        assert_eq!(available.title, "Moya 9.0.0");
        assert_eq!(available.notes, vec![
            Line::Text("Intro with bold and code.".into()), Line::Gap,
            Line::Heading("Fixes".into()), Line::Bullet("Brush faster, see docs.".into()), Line::Gap,
            Line::Bullet("Second".into()),
        ]);
    }
}
