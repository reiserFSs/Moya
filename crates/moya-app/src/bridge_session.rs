//! App-side Photoshop sessions and owned multi-file results.
use crate::{App, bridge, layers, workers};
use eframe::egui;
use moya_core::{LayerBlend, RenderMode};
use moya_gpu::{Depth, Gpu, Image};
use moya_io::SampleDepth;
use moya_pipeline::{LayerInputs, RenderRequest, ViewFlags};
use std::path::PathBuf;

impl App {
    pub(super) fn bridge_reply(&self, value: serde_json::Value) {
        if let Some(bridge) = &self.bridge { bridge.send(value); }
    }

    pub(super) fn cancel_session(&mut self) {
        if let Some(session) = self.session.take() {
            self.bridge_reply(serde_json::json!({"type":"cancelled","id":session.id}));
        }
        self.session_ready = false;
        self.send_pending = false;
        self.sending = None;
        self.trim_idle();
        self.loader.invalidate();
        self.loading = None;
    }

    pub(super) fn queue_send(&mut self) {
        if self.session_ready && self.sending.is_none() && !self.send_pending {
            self.send_pending = true;
            self.status = "Rendering HQ for Photoshop…".into();
            self.egui_ctx.request_repaint();
        }
    }

    pub(super) fn send_to_photoshop(&mut self) {
        self.send_pending = false;
        let Some(session) = self.session.clone() else { return; };
        let ctx = self.egui_ctx.clone();
        self.sync_mask(&ctx);
        let result = self.prepare_bridge_send(&session);
        self.dirty = true;
        self.force = true;
        match result {
            Ok((batch, message)) => {
                self.sending = Some((batch, Some(message)));
                self.status = "Sending to Photoshop…".into();
            }
            Err(error) => {
                self.trim_idle();
                self.status = crate::failure("Send", &error);
                self.bridge_reply(serde_json::json!({"type":"error","id":session.id,"message":self.status}));
            }
        }
    }

    fn prepare_bridge_send(&mut self, session: &bridge::Edit) -> anyhow::Result<(workers::SendBatch, serde_json::Value)> {
        // Render and convert under GPU error capture: running out of video memory must fail
        // this send (reported to Photoshop, retryable), not close Moya. Writers stream during the
        // render; any failure drops the batch, which cancels and removes what was written.
        let gpu = self.gpu.clone();
        let rendered = gpu.capture_errors(|| self.render_bridge_send(session));
        let (mut batch, message, project_path, project) = rendered.inspect_err(|_| self.drop_render_caches())??;
        batch.finish(project_path, project);
        Ok((batch, message))
    }

    fn render_bridge_send(&mut self, session: &bridge::Edit)
        -> anyhow::Result<(workers::SendBatch, serde_json::Value, PathBuf, moya_project::Project)> {
        let depth = match session.document.depth {
            8 => SampleDepth::U8, 16 => SampleDepth::U16, _ => SampleDepth::F32,
        };
        let target = match depth { SampleDepth::U8 => Depth::Eight, SampleDepth::U16 => Depth::Sixteen, SampleDepth::F32 => Depth::Float };
        let project_path = session.source.with_extension("moya");
        let project = layers::to_project(&self.layers, self.selected, moya_project::SourceRef::Path(session.source.clone()));
        let icc = self.picture.as_ref().and_then(|picture| picture.icc_profile.clone());
        let renderer = self.renderer.as_mut().ok_or_else(|| anyhow::anyhow!("Open an image first"))?;
        // Photoshop blend layers need premultiplied light over opaque black.
        let settings: Vec<_> = self.layers.iter().map(|layer| {
            let mut settings = layer.settings.clone();
            settings.params.unmult = false;
            settings
        }).collect();
        let request = RenderRequest {
            mode: RenderMode::Hq,
            view: ViewFlags { xray: false, solo: false, host_is_float: session.document.depth == 32 },
            selected: Some(self.selected),
            solo_layer: None,
            want_composite: session.return_mode == bridge::ReturnMode::Composite,
            want_glow_composite: false,
            want_layer_glows: false,
            want_xray: false,
        };
        let mut batch = workers::SendBatch::new(self.egui_ctx.clone(), self.gpu.clone());
        let inputs = bridge_inputs(&settings, &self.layers);
        let stem = session.source.file_stem().unwrap_or_default().to_string_lossy();
        let layer_path = |index: usize| session.source.with_file_name(format!("{stem}.moya-layer-{}.tif", index + 1));
        let mut returned = Vec::with_capacity(self.layers.len());
        let composite = if session.return_mode == bridge::ReturnMode::Composite {
            let output = renderer.render(&inputs, &request);
            let path = session.source.with_file_name(format!("{stem}.moya.tif"));
            let from = if !settings.iter().any(|layer| layer.visible) { target }
                else if session.document.depth == 32 { Depth::Float } else { Depth::Eight };
            let image = output.composite.as_ref().ok_or_else(|| anyhow::anyhow!("Missing rendered composite"))?;
            batch.push(path.clone(), bridge_image(&self.gpu, image, from, target, depth, false, &icc))?;
            Some(path)
        } else {
            // Hidden layers are preserved too, without altering the composite or Below inputs.
            stream_layer_glows(&self.gpu, renderer, &inputs, &request, target, depth, &icc,
                |index, image| batch.push(layer_path(index), image))?;
            for (index, layer) in self.layers.iter().enumerate() {
                returned.push(bridge::ResultLayer {
                    path: layer_path(index), name: layer.settings.name.clone(),
                    blend: match layer.settings.blend {
                        LayerBlend::Auto if session.document.depth == 32 => bridge::LayerBlend::LinearDodge,
                        LayerBlend::Auto | LayerBlend::Screen => bridge::LayerBlend::Screen,
                        LayerBlend::LinearDodge => bridge::LayerBlend::LinearDodge,
                        LayerBlend::Lighten => bridge::LayerBlend::Lighten,
                        LayerBlend::Normal => bridge::LayerBlend::Normal,
                    },
                    opacity: layer.settings.opacity * 100.0, visible: layer.settings.visible,
                });
            }
            None
        };
        let result = bridge::ResultMessage {
            id: session.id.clone(), depth: session.document.depth, return_mode: session.return_mode,
            group_name: "Moya".into(), layers: returned, composite, project_path: project_path.clone(),
        };
        result.validate()?;
        let mut message = serde_json::to_value(result)?;
        message["type"] = serde_json::json!("result");
        Ok((batch, message, project_path, project))
    }

    /// Opens a Photoshop edit; called once the unsaved-changes check has passed.
    pub(crate) fn start_edit(&mut self, edit: bridge::Edit) {
        self.cancel_session();
        self.session = Some(edit.clone());
        if let Some(project) = &edit.project_path {
            if self.open_project_with_source(project, &edit.source) {
                self.session_ready = true;
                self.bridge_reply(serde_json::json!({"type":"opened","id":edit.id}));
                if self.bridge_test_auto_send { self.queue_send(); }
            } else {
                self.bridge_reply(serde_json::json!({"type":"error","id":edit.id,"message":self.status}));
                self.session = None;
            }
        } else {
            self.request_load(&edit.source, false);
        }
        self.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    pub(super) fn poll_bridge(&mut self) {
        // Paint the queued busy state before the synchronous GPU render.
        if self.send_pending { self.send_to_photoshop(); }
        loop {
            let event = self.bridge.as_ref().and_then(|bridge| bridge.events.try_recv().ok());
            let Some(event) = event else { break; };
            match event {
                bridge::Event::Edit(edit) => self.guard_discard(crate::projects::Pending::Edit(edit)),
                bridge::Event::Placed(id) => {
                    if self.session.as_ref().is_some_and(|session| session.id == id) {
                        if let Some((batch, _)) = self.sending.take() { batch.keep_output(); }
                        // The plugin closes its side after placing, so a second Send in this
                        // session would be ignored and would collide with this send's result files.
                        self.session = None;
                        self.session_ready = false;
                        self.send_pending = false;
                        self.status = "Placed in Photoshop. To change it, select the Moya group and choose Re-edit in Photoshop.".into();
                        self.trim_idle();
                    }
                }
                bridge::Event::Error { id, message } => {
                    if self.session.as_ref().is_some_and(|session| session.id == id) {
                        self.sending = None;
                        self.trim_idle();
                        self.status = format!("Photoshop: {message}");
                    }
                }
                bridge::Event::Disconnected => {
                    self.cancel_session();
                    self.status = "Photoshop bridge disconnected".into();
                }
            }
        }
        if let Some(result) = self.sending.as_ref().and_then(|(batch, _)| batch.poll()) {
            match result {
                Ok(()) => {
                    if let Some(message) = self.sending.as_mut().and_then(|(_, message)| message.take()) {
                        self.bridge_reply(message);
                    }
                    self.status = "Waiting for Photoshop…".into();
                }
                Err(error) => {
                    self.sending = None;
                    self.status = crate::failure("Send", &error);
                    if let Some(session) = &self.session {
                        self.bridge_reply(serde_json::json!({"type":"error","id":session.id,"message":self.status}));
                    }
                }
            }
            self.trim_idle();
        }
    }
}

fn bridge_image(gpu: &Gpu, image: &Image, from: Depth, to: Depth, depth: SampleDepth, opaque: bool, icc: &Option<Vec<u8>>) -> workers::ExportImage {
    let image = if from != to { gpu.convert_depth(image, from, to) } else { image.clone() };
    workers::ExportImage { image, depth, opaque, icc_profile: icc.clone() }
}

fn bridge_inputs<'a>(settings: &'a [moya_core::LayerSettings], layers: &'a [layers::Layer]) -> Vec<LayerInputs<'a>> {
    settings.iter().zip(layers).map(|(settings, layer)| LayerInputs {
        settings, mask: layer.mask.gpu_image(), mask_generation: layer.mask.generation(),
    }).collect()
}

/// Render every layer's HQ glow (hidden ones too) one at a time and hand each converted image to
/// `write(layer_index, image)`; the glow is released as soon as `write` returns, so at most one
/// full-size glow plus whatever `write` retains is alive. Stops at the first `write` error.
#[allow(clippy::too_many_arguments)]
fn stream_layer_glows(
    gpu: &Gpu, renderer: &mut moya_pipeline::Document, inputs: &[LayerInputs], request: &RenderRequest,
    target: Depth, depth: SampleDepth, icc: &Option<Vec<u8>>,
    mut write: impl FnMut(usize, workers::ExportImage) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut result = Ok(());
    renderer.render_layer_glows(inputs, request, |id, glow| {
        let index = inputs.iter().position(|input| input.settings.id == id).expect("rendered layer is an input");
        result = write(index, bridge_image(gpu, &glow, Depth::Eight, target, depth, true, icc));
        result.is_ok()
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use moya_core::{LayerInput, LayerSettings};
    use moya_pipeline::Document;
    use std::sync::Arc;

    const SIZE: u32 = 128;

    fn settings(count: usize) -> Vec<LayerSettings> {
        (0..count).map(|i| {
            let mut s = LayerSettings { id: i as u64 + 1, name: format!("L{i}"), ..Default::default() };
            s.params.threshold = 0.3 + 0.04 * i as f32;
            s.params.unmult = false;
            (s.visible, s.input) = match i % 4 {
                0 => (true, LayerInput::Original),
                1 => (false, LayerInput::Below),
                2 => (true, LayerInput::Below),
                _ => (true, LayerInput::Original),
            };
            s
        }).collect()
    }

    fn source(gpu: &Gpu) -> Image {
        let pixels: Vec<f32> = (0..SIZE * SIZE).flat_map(|p| {
            let (x, y) = (p % SIZE, p / SIZE);
            let bright = if (x / 9 + y / 7) % 5 == 0 { 1.0 } else { 0.1 + 0.4 * (x as f32 / SIZE as f32) };
            [bright, bright * 0.8, 0.2 + 0.5 * (y as f32 / SIZE as f32), 1.0]
        }).collect();
        gpu.upload_rgba32f(SIZE, SIZE, &pixels)
    }

    fn test_inputs<'a>(settings: &'a [LayerSettings], masks: &'a [Option<Image>]) -> Vec<LayerInputs<'a>> {
        settings.iter().zip(masks).map(|(settings, mask)| LayerInputs { settings, mask: mask.as_ref(), mask_generation: 1 }).collect()
    }

    /// Returns (peak live bytes above the baseline, file contents per layer).
    fn send(gpu: &Arc<Gpu>, count: usize, streaming: bool, depth: SampleDepth) -> (u64, Vec<Vec<u8>>) {
        let target = match depth { SampleDepth::U8 => Depth::Eight, SampleDepth::U16 => Depth::Sixteen, SampleDepth::F32 => Depth::Float };
        let gray: Vec<u16> = (0..SIZE * SIZE).map(|p| (p * 7 % 65535) as u16).collect();
        let masks: Vec<Option<Image>> = (0..count).map(|i| (i % 2 == 0).then(|| gpu.upload_gray_u16(SIZE, SIZE, &gray))).collect();
        let mut document = Document::new(gpu.clone(), source(gpu), if depth == SampleDepth::F32 { Depth::Float } else { Depth::Eight });
        let mut settings = settings(count);
        let float = depth == SampleDepth::F32;
        let request = RenderRequest {
            mode: RenderMode::Hq, view: ViewFlags { xray: false, solo: false, host_is_float: float },
            selected: Some(1), solo_layer: None, want_composite: false, want_glow_composite: false,
            want_layer_glows: !streaming, want_xray: false,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = |index: usize| dir.path().join(format!("layer-{index}.tif"));
        let icc = None;
        gpu.trim_idle(0);
        let baseline = gpu.memory_stats().live_bytes;
        gpu.reset_peak();
        if streaming {
            let layers = test_inputs(&settings, &masks);
            stream_layer_glows(gpu, &mut document, &layers, &request, target, depth, &icc,
                |index, image| image.save(gpu, &path(index))).unwrap();
        } else {
            // The previous implementation: collect visible glows, re-render the whole stack per hidden layer.
            let mut images = Vec::new();
            let output = document.render(&test_inputs(&settings, &masks), &request);
            for index in 0..count {
                let hidden;
                let glow = if settings[index].visible {
                    output.layer_glows.iter().find(|(id, _)| *id == settings[index].id).map(|(_, image)| image)
                } else {
                    settings[index].visible = true;
                    hidden = document.render(&test_inputs(&settings, &masks), &request);
                    settings[index].visible = false;
                    hidden.layer_glows.iter().find(|(id, _)| *id == settings[index].id).map(|(_, image)| image)
                }.unwrap();
                images.push((index, bridge_image(gpu, glow, Depth::Eight, target, depth, true, &icc)));
            }
            drop(output);
            for (index, image) in images { image.save(gpu, &path(index)).unwrap(); }
        }
        let peak = gpu.memory_stats().peak_live_bytes - baseline;
        (peak, (0..count).map(|i| std::fs::read(path(i)).unwrap()).collect())
    }

    #[test]
    fn layer_send_streams_with_constant_peak_and_identical_files() {
        let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
        for depth in [SampleDepth::U8, SampleDepth::F32] {
            let mut peaks = [0; 3];
            for (slot, count) in [(0, 2), (1, 8), (2, 16)] {
                let (peak, files) = send(&gpu, count, true, depth);
                if count <= 8 {
                    let (old_peak, old_files) = send(&gpu, count, false, depth);
                    assert!(old_files.iter().any(|f| f != &old_files[0]), "layers must differ");
                    assert_eq!(files, old_files, "{depth:?} {count} layers: files identical to the collect-all path");
                    eprintln!("{depth:?} {count} layers: streamed peak {peak} B, previous collect-all peak {old_peak} B");
                } else {
                    eprintln!("{depth:?} {count} layers: streamed peak {peak} B");
                }
                peaks[slot] = peak;
            }
            // Peak is a few full-size images regardless of layer count (one extra for a Below composite blend).
            let image = u64::from(SIZE * SIZE) * 16;
            assert!(peaks[1] <= peaks[0] + image + 4096 && peaks[2] <= peaks[1], "{depth:?}: peak grows with layers: {peaks:?}");
        }
    }
}
