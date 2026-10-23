use std::{io::Write, path::{Path, PathBuf}};
use anyhow::{Context, Result, ensure};
use eframe::egui;
use moya_core::LayerSettings;
use moya_io::{Picture, SampleDepth};
use moya_project::{Project, SourceRef};
use crate::{App, layers};

#[derive(Default)]
pub struct State {
    path: Option<PathBuf>,
    source: Option<SourceRef>,
    embed_source: bool,
    saved_embed: bool,
    saved: Vec<(LayerSettings, (u32, u32, bool, u64))>,
    closing: bool,
    /// Action waiting on the in-app "Unsaved Moya project" dialog.
    pending: Option<Pending>,
}

/// Actions that replace the current document and therefore need the unsaved-changes check.
pub(crate) enum Pending { Open(PathBuf), Edit(crate::bridge::Edit), Close }

pub fn is_project(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("moya"))
}

/// Hide exactly one project extension, but retain source-image extensions and dirty state.
fn title_for_path(path: Option<&Path>, dirty: bool) -> String {
    let name = path.and_then(|path| if is_project(path) { path.file_stem() } else { path.file_name() });
    match name {
        Some(name) => format!("{}{} — Moya", name.to_string_lossy(), if dirty { " *" } else { "" }),
        None => "Moya".into(),
    }
}

/// Decode embedded originals through the same bounded, depth/ICC-preserving loader as images.
fn load_source(source: &SourceRef) -> Result<(PathBuf, Picture)> {
    match source {
        SourceRef::Path(path) => Ok((path.clone(), moya_io::load(path)?)),
        SourceRef::Embedded { file_name, bytes } => {
            let extension = Path::new(file_name).extension().and_then(|s| s.to_str()).unwrap_or("img");
            let mut temporary = tempfile::Builder::new().suffix(&format!(".{extension}")).tempfile()?;
            temporary.write_all(bytes)?;
            temporary.flush()?;
            Ok((PathBuf::from(file_name), moya_io::load(temporary.path())?))
        }
    }
}

impl App {
    fn mark_project_saved(&mut self) {
        self.project.saved = self.layers.iter()
            .map(|layer| (layer.settings.clone(), layer.mask.fingerprint())).collect();
        self.project.saved_embed = self.project.embed_source;
    }

    /// Called only after a new image has been installed, not on ordinary edits.
    pub(crate) fn reset_project(&mut self) {
        self.project = State::default();
        self.mark_project_saved();
    }

    pub(crate) fn project_is_dirty(&self) -> bool {
        self.picture.is_some() && (self.project.embed_source != self.project.saved_embed
            || self.project.saved.len() != self.layers.len()
            || self.layers.iter().zip(&self.project.saved).any(|(layer, (settings, mask))|
                &layer.settings != settings || layer.mask.fingerprint() != *mask))
    }

    pub(crate) fn project_title(&self) -> String {
        title_for_path(self.project.path.as_deref().or(self.path.as_deref()), self.project_is_dirty())
    }

    pub(crate) fn document_title(&self) -> String {
        let title = self.project.path.as_deref().or(self.path.as_deref())
            .and_then(|path| path.file_name()).map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into());
        if self.project_is_dirty() { format!("{title} · edited") } else { title }
    }

    pub(crate) fn save_project(&mut self, save_as: bool) -> bool {
        if self.picture.is_none() { return false; }
        for layer in &mut self.layers { layer.mask.end_stroke(); }
        self.history.observe(&mut self.layers, self.selected, false);
        let path = if !save_as && self.project.path.is_some() {
            self.project.path.clone().unwrap()
        } else {
            let mut dialog = rfd::FileDialog::new().add_filter("Moya project", &["moya"]);
            if let Some(path) = self.project.path.as_ref().or(self.path.as_ref()) {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) { dialog = dialog.set_directory(parent); }
                // The native dialog supplies the filtered extension; suggest only the basename.
                if let Some(name) = path.file_stem() { dialog = dialog.set_file_name(name.to_string_lossy()); }
            }
            let Some(mut path) = dialog.save_file() else { return false; };
            if !is_project(&path) { path.set_extension("moya"); }
            path
        };
        let result = (|| -> Result<()> {
            let source = self.project_source()?;
            let project = layers::to_project(&self.layers, self.selected, source);
            moya_project::save(&path, &project)
        })();
        match result {
            Ok(()) => {
                self.project.path = Some(path.clone());
                self.last_folder = path.parent().map(Path::to_owned);
                self.mark_project_saved();
                self.status = format!("Saved project {}", path.display());
                true
            }
            Err(error) => { self.status = format!("Project save failed: {error:#}"); false }
        }
    }

    fn project_source(&self) -> Result<SourceRef> {
        if let Some(SourceRef::Embedded { file_name, bytes }) = &self.project.source {
            ensure!(self.project.embed_source, "This source exists only inside the project; keep Embed source enabled");
            return Ok(SourceRef::Embedded { file_name: file_name.clone(), bytes: bytes.clone() });
        }
        let path = match &self.project.source {
            Some(SourceRef::Path(path)) => path,
            _ => self.path.as_ref().context("No source image is open")?,
        };
        let path = std::path::absolute(path)?;
        if self.project.embed_source {
            Ok(SourceRef::Embedded {
                file_name: path.file_name().context("Source has no filename")?.to_string_lossy().into_owned(),
                bytes: std::fs::read(&path).with_context(|| format!("reading source {}", path.display()))?,
            })
        } else { Ok(SourceRef::Path(path)) }
    }

    pub(crate) fn open_project(&mut self, path: &Path) -> bool {
        self.install_project(path, None)
    }

    pub(crate) fn open_project_with_source(&mut self, path: &Path, source: &Path) -> bool {
        self.install_project(path, Some(source))
    }

    fn install_project(&mut self, path: &Path, replacement: Option<&Path>) -> bool {
        let result = (|| -> Result<(Project, PathBuf, Picture, Vec<layers::Layer>, u64)> {
            // A Photoshop replacement intentionally does not require the old external source.
            let mut project = if replacement.is_some() {
                moya_project::from_bytes(&std::fs::read(path)?)?
            } else { moya_project::load(path)? };
            if let Some(source) = replacement { project.source = SourceRef::Path(std::path::absolute(source)?); }
            let (source_path, picture) = load_source(&project.source)?;
            if let Some(session) = &self.session {
                let depth = match picture.depth { SampleDepth::U8 => 8, SampleDepth::U16 => 16, SampleDepth::F32 => 32 };
                ensure!(picture.width == session.document.width && picture.height == session.document.height
                    && depth == session.document.depth, "Source TIFF dimensions/depth do not match the Photoshop document");
            }
            let (layers, selected) = layers::from_project(&project, picture.width, picture.height)?;
            Ok((project, source_path, picture, layers, selected))
        })();
        let (project, source_path, picture, layers, selected) = match result {
            Ok(loaded) => loaded,
            Err(error) => { self.status = format!("Project open failed: {error:#}"); return false; }
        };
        if !self.finish_open(&source_path, picture) { return false; }
        self.loading = None;
        self.layers = layers;
        self.selected = selected;
        self.solo_layer = None;
        self.history.clear(&self.layers, self.selected);
        self.project.path = Some(path.to_owned());
        self.project.embed_source = matches!(&project.source, SourceRef::Embedded { .. });
        self.project.source = Some(project.source);
        self.mark_project_saved();
        self.last_folder = path.parent().map(Path::to_owned);
        self.dirty = true;
        self.force = true;
        self.status = format!("Opened project {}", path.display());
        true
    }

    /// Runs `action` now when nothing is unsaved; otherwise shows the in-app dialog and runs it on Save/Discard.
    pub(crate) fn guard_discard(&mut self, action: Pending) {
        for layer in &mut self.layers { layer.mask.end_stroke(); }
        self.history.observe(&mut self.layers, self.selected, false);
        if !self.project_is_dirty() { self.run_pending(action); return; }
        if let Some(Pending::Edit(old)) = self.project.pending.replace(action) {
            self.bridge_reply(serde_json::json!({"type":"cancelled","id":old.id}));
        }
    }

    fn run_pending(&mut self, action: Pending) {
        match action {
            Pending::Open(path) => self.open_now(&path),
            Pending::Edit(edit) => self.start_edit(edit),
            Pending::Close => {
                self.project.closing = true;
                self.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Save / Discard / Cancel, drawn by egui so the event loop keeps running. Enter saves, Esc cancels.
    pub(crate) fn unsaved_dialog(&mut self, ctx: &egui::Context) {
        if self.project.pending.is_none() { return; }
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::unique("moya.unsaved")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("Unsaved Moya project");
            ui.add_space(4.0);
            ui.label("Save your project before continuing? Unsaved changes will be lost.");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) { decision = Some(Some(true)); }
                if ui.button("Discard").clicked() { decision = Some(Some(false)); }
                if ui.button("Cancel").clicked() { decision = Some(None); }
            });
        });
        if modal.should_close() && decision.is_none() { decision = Some(None); }
        let Some(choice) = decision else { return; };
        // A failed or cancelled save keeps the dialog (and the action) pending.
        if choice == Some(true) && !self.save_project(false) { return; }
        let Some(action) = self.project.pending.take() else { return; };
        match choice {
            Some(_) => self.run_pending(action),
            None => if let Pending::Edit(edit) = action {
                self.bridge_reply(serde_json::json!({"type":"cancelled","id":edit.id}));
            },
        }
    }

    pub(crate) fn handle_project_close(&mut self, ctx: &egui::Context) {
        if self.project.closing || !ctx.input(|input| input.viewport().close_requested()) { return; }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.guard_discard(Pending::Close);
    }

    pub(crate) fn project_controls(&mut self, ui: &mut egui::Ui) {
        let enabled = self.picture.is_some() && self.loading.is_none();
        if ui.add_enabled(enabled, egui::Button::new("Save")).on_hover_text("Save project (Cmd+S); Save As (Cmd+Shift+S)").clicked() {
            self.save_project(false);
        }
        ui.menu_button("▾", |ui| {
            if ui.add_enabled(enabled, egui::Button::new("Save as…")).clicked() {
                self.save_project(true);
                ui.close();
            }
            let embedded_only = matches!(&self.project.source, Some(SourceRef::Embedded { .. }));
            ui.add_enabled_ui(enabled && !embedded_only, |ui| {
                ui.checkbox(&mut self.project.embed_source, "Embed source")
                    .on_hover_text("Store the original image inside the .moya file so it can move with the project");
            }).response.on_disabled_hover_text("This project's source is stored only inside the project");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_titles_strip_only_one_project_extension() {
        assert_eq!(title_for_path(Some(Path::new("/projects/image.moya")), false), "image — Moya");
        assert_eq!(title_for_path(Some(Path::new("/projects/image.moya.moya")), false), "image.moya — Moya");
        assert_eq!(title_for_path(Some(Path::new("image.MOYA")), true), "image * — Moya");
        assert_eq!(title_for_path(Some(Path::new("image.tiff")), false), "image.tiff — Moya");
        assert_eq!(title_for_path(None, false), "Moya");
    }

    #[test]
    fn embedded_source_preserves_depth_and_pixels() -> Result<()> {
        // A plain path in a temp dir: Windows refuses to rename over a still-open NamedTempFile.
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("source.tiff");
        let picture = Picture { width: 1, height: 1, depth: SampleDepth::F32,
            rgba: vec![2.0, 0.25, -0.5, 0.75], icc_profile: Some(b"opaque ICC".to_vec()) };
        moya_io::save(&source, &picture, picture.depth)?;
        let (_, loaded) = load_source(&SourceRef::Embedded {
            file_name: "source.tiff".into(), bytes: std::fs::read(&source)?,
        })?;
        assert_eq!(loaded.depth, picture.depth);
        assert_eq!(loaded.rgba, picture.rgba);
        assert_eq!(loaded.icc_profile, picture.icc_profile);
        assert!(is_project(Path::new("image.MOYA")));
        Ok(())
    }
}
