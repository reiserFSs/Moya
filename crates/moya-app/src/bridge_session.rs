//! App-side Photoshop sessions and owned multi-file results.
use crate::{App, bridge, layers, workers};
use eframe::egui;
use moya_core::{LayerBlend, RenderMode};
use moya_gpu::{Depth, Gpu, Image};
use moya_io::SampleDepth;
use moya_pipeline::{LayerInputs, RenderRequest, ViewFlags};

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
                self.status = format!("Send failed: {error:#}");
                self.bridge_reply(serde_json::json!({"type":"error","id":session.id,"message":self.status}));
            }
        }
    }

    fn prepare_bridge_send(&mut self, session: &bridge::Edit) -> anyhow::Result<(workers::SendBatch, serde_json::Value)> {
        let depth = match session.document.depth {
            8 => SampleDepth::U8, 16 => SampleDepth::U16, _ => SampleDepth::F32,
        };
        let target = match depth { SampleDepth::U8 => Depth::Eight, SampleDepth::U16 => Depth::Sixteen, SampleDepth::F32 => Depth::Float };
        let project_path = session.source.with_extension("moya");
        let project = layers::to_project(&self.layers, self.selected, moya_project::SourceRef::Path(session.source.clone()));
        let icc = self.picture.as_ref().and_then(|picture| picture.icc_profile.clone());
        let renderer = self.renderer.as_mut().ok_or_else(|| anyhow::anyhow!("Open an image first"))?;
        // Photoshop blend layers need premultiplied light over opaque black.
        let mut settings: Vec<_> = self.layers.iter().map(|layer| {
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
            want_layer_glows: session.return_mode == bridge::ReturnMode::Layers,
            want_xray: false,
        };
        let output = renderer.render(&bridge_inputs(&settings, &self.layers), &request);
        let stem = session.source.file_stem().unwrap_or_default().to_string_lossy();
        let mut images = Vec::with_capacity(self.layers.len().max(1));
        let mut returned = Vec::with_capacity(self.layers.len());
        let composite = if session.return_mode == bridge::ReturnMode::Composite {
            let path = session.source.with_file_name(format!("{stem}.moya.tif"));
            let from = if !settings.iter().any(|layer| layer.visible) { target }
                else if session.document.depth == 32 { Depth::Float } else { Depth::Eight };
            let image = output.composite.as_ref().ok_or_else(|| anyhow::anyhow!("Missing rendered composite"))?;
            images.push((path.clone(), bridge_image(&self.gpu, image, from, target, depth, false, &icc)));
            Some(path)
        } else {
            for (index, layer) in self.layers.iter().enumerate() {
                // Preserve hidden layers without altering the visible composite or Below inputs.
                let hidden;
                let glow = if layer.settings.visible {
                    output.layer_glows.iter().find(|(id, _)| *id == layer.settings.id).map(|(_, image)| image)
                } else {
                    settings[index].visible = true;
                    hidden = renderer.render(&bridge_inputs(&settings, &self.layers), &request);
                    settings[index].visible = false;
                    hidden.layer_glows.iter().find(|(id, _)| *id == layer.settings.id).map(|(_, image)| image)
                }.ok_or_else(|| anyhow::anyhow!("Missing rendered glow for layer {}", layer.settings.id))?;
                let path = session.source.with_file_name(format!("{stem}.moya-layer-{}.tif", index + 1));
                images.push((path.clone(), bridge_image(&self.gpu, glow, Depth::Eight, target, depth, true, &icc)));
                returned.push(bridge::ResultLayer {
                    path, name: layer.settings.name.clone(),
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
        Ok((workers::SendBatch::new(self.egui_ctx.clone(), self.gpu.clone(), images, project_path, project), message))
    }

    pub(super) fn poll_bridge(&mut self) {
        // Paint the queued busy state before the synchronous GPU render.
        if self.send_pending { self.send_to_photoshop(); }
        loop {
            let event = self.bridge.as_ref().and_then(|bridge| bridge.events.try_recv().ok());
            let Some(event) = event else { break; };
            match event {
                bridge::Event::Edit(edit) => {
                    if !self.confirm_discard() {
                        self.bridge_reply(serde_json::json!({"type":"cancelled","id":edit.id}));
                        continue;
                    }
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
                bridge::Event::Placed(id) => {
                    if self.session.as_ref().is_some_and(|session| session.id == id) {
                        if let Some((batch, _)) = self.sending.take() { batch.keep_output(); }
                        self.status = "Placed in Photoshop".into();
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
                    self.status = format!("Send failed: {error:#}");
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
