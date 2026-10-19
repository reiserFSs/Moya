use super::*;
use moya_gpu::{Filter, Region, ViewMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewRegion { pub x: u32, pub y: u32, pub w: u32, pub h: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionQuality { Preview, Exact }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefineState { Idle, Pending { done: usize, total: usize }, Ready }

#[derive(PartialEq)]
struct RefineKey {
    params: Params,
    input: LayerInput,
    mask_generation: u64,
    has_mask: bool,
    // Below extraction depends on the entire preceding blend chain, not the selected view layer.
    below: Vec<(LayerSettings, u64, bool)>,
    host_is_float: bool,
    solo: bool,
}
pub(super) struct RefinedLayer { key: Arc<RefineKey>, glow: Image }

/// Identifies the inputs a refinement was captured from; `Document::refine_current` tells
/// whether they still hold.
#[derive(Clone)]
pub struct RefineTicket { id: u64, key: Arc<RefineKey> }

/// Everything one layer's HQ refinement needs, captured on the owning thread.
/// `run` touches only its own lane, so it may execute on a worker thread.
pub struct RefineJob {
    ticket: RefineTicket,
    source: Arc<Image>,
    host_depth: Depth,
    geometry: DocGeometry,
    /// Preceding visible layers' refined glows, bottom to top (Below input only).
    priors: Vec<(LayerSettings, Image)>,
    mask: Option<Image>,
    mask_generation: u64,
    view: ViewFlags,
}

/// A finished HQ working-grid glow, valid only while its ticket is current.
pub struct RefinedGlow { ticket: RefineTicket, glow: Image }

impl RefineJob {
    pub fn ticket(&self) -> &RefineTicket { &self.ticket }

    /// Render the HQ glow on `lane`. Returns `None` if the lane's cancel flag fired (the lane
    /// stays usable). All of the lane's work is submitted before captured inputs are released.
    pub fn run(self, lane: &Arc<Gpu>) -> Option<RefinedGlow> {
        let glow = self.render(lane);
        if lane.cancelled() { lane.discard_pending(); } else { lane.flush(); }
        // HQ intermediates are transient: retain only the working-grid glow.
        lane.trim_idle(0);
        let glow = glow.filter(|_| !lane.cancelled())?;
        Some(RefinedGlow { ticket: self.ticket, glow })
    }

    fn render(&self, lane: &Arc<Gpu>) -> Option<Image> {
        let g = lane;
        let key = &*self.ticket.key;
        let mut stages = LayerStages::shared(g.clone(), self.source.clone(), self.host_depth);
        if key.input == LayerInput::Below {
            let mut preparer = LayerStages::shared(g.clone(), self.source.clone(), self.host_depth);
            preparer.prepare(&Params::default(), RenderMode::Hq);
            let mut below = preparer.working.take().unwrap().source.clone();
            let depth = blend_depth(&self.view);
            for (settings, glow) in &self.priors {
                let linear = (depth == Depth::Float).then(|| g.convert_depth(glow, Depth::Eight, Depth::Float));
                below = g.blend(&below, linear.as_ref().unwrap_or(glow), blend_mode(settings, &self.view), settings.opacity, 1., depth);
            }
            let mut preparer = LayerStages::shared(g.clone(), Arc::new(below), self.host_depth);
            preparer.geometry = self.geometry;
            preparer.source_is_working = true;
            preparer.prepare(&key.params, RenderMode::Hq);
            stages.working = preparer.working.take();
        }
        if g.cancelled() { return None; }
        // Aliased, not moved: the mask must outlive the lane's submission (see `run`).
        stages.mask = self.mask.clone();
        stages.mask_generation = self.mask_generation;
        let effect_view = ViewFlags { xray: false, ..self.view };
        let output = stages.render_selected_impl(&key.params, RenderMode::Hq, &effect_view, false, false, false, |_, _, _| {});
        Some(output.working_glow.clone())
    }
}

fn blend_mode(settings: &LayerSettings, view: &ViewFlags) -> B {
    match settings.blend {
        LayerBlend::Auto if view.host_is_float && !view.solo => B::LinearDodge,
        LayerBlend::Auto | LayerBlend::Screen => B::Screen,
        LayerBlend::LinearDodge => B::LinearDodge,
        LayerBlend::Lighten => B::Lighten,
        LayerBlend::Normal => B::Normal,
    }
}
fn blend_depth(view: &ViewFlags) -> Depth {
    if view.host_is_float && !view.solo { Depth::Float } else { Depth::Eight }
}
fn keys(layers: &[LayerInputs<'_>], view: &ViewFlags) -> Vec<(u64, RefineKey)> {
    let mut below = Vec::new();
    let mut result = Vec::new();
    for input in layers {
        let mut settings = input.settings.clone();
        settings.sanitize();
        if !settings.visible { continue; }
        result.push((settings.id, RefineKey {
            params: settings.params.clone(), input: settings.input,
            mask_generation: input.mask_generation, has_mask: input.mask.is_some(),
            below: if settings.input == LayerInput::Below { below.clone() } else { Vec::new() },
            host_is_float: settings.input == LayerInput::Below && view.host_is_float,
            solo: settings.input == LayerInput::Below && view.solo,
        }));
        below.push((settings, input.mask_generation, input.mask.is_some()));
    }
    result
}

impl Document {
    pub(super) fn invalidate_refine(&mut self, layers: &[LayerInputs<'_>], view: &ViewFlags) {
        let keys = keys(layers, view);
        let before = self.refined.len();
        self.refined.retain(|id, cached| keys.iter().any(|(key_id, key)| key_id == id && *key == *cached.key));
        if before != self.refined.len() { self.gpu.trim_idle(0); }
    }
    pub fn refine_state(&self, layers: &[LayerInputs<'_>], view: &ViewFlags) -> RefineState {
        let keys = keys(layers, view);
        let total = keys.len();
        let done = keys.iter().filter(|(id, key)| self.refined.get(id).is_some_and(|cached| *cached.key == *key)).count();
        if done == total { RefineState::Ready }
        else if done == 0 { RefineState::Idle }
        else { RefineState::Pending { done, total } }
    }
    /// Retained HQ glow payload only; excluded from the bounded Draft stage cache.
    pub fn refine_bytes(&self) -> u64 {
        self.refined.values().map(|cached| image_bytes(&cached.glow)).sum()
    }

    pub fn drop_refine(&mut self) {
        self.refined.clear();
        self.gpu.trim_idle(0);
    }

    #[cfg(test)]
    pub(super) fn refined_glow(&self, id: u64) -> Option<&Image> { self.refined.get(&id).map(|cached| &cached.glow) }

    /// Capture the next missing visible layer's HQ refinement, or `None` when all are current.
    /// The mask snapshot is taken (and the document's lane flushed) here, so the job can run
    /// on another thread and lane without ever touching the document.
    pub fn refine_job(&mut self, layers: &[LayerInputs<'_>], view: &ViewFlags) -> Option<RefineJob> {
        self.invalidate_refine(layers, view);
        let (id, key) = keys(layers, view).into_iter().find(|(id, _)| !self.refined.contains_key(id))?;
        let input = layers.iter().find(|input| input.settings.id == id).unwrap();
        let priors = if key.input == LayerInput::Below {
            layers.iter().take_while(|prior| prior.settings.id != id).filter_map(|prior| {
                let mut settings = prior.settings.clone(); settings.sanitize();
                let glow = settings.visible.then(|| self.refined[&settings.id].glow.clone())?;
                Some((settings, glow))
            }).collect()
        } else { Vec::new() };
        let snapshots = LayerStages::shared(self.gpu.clone(), self.source.clone(), self.host_depth);
        let mask = input.mask.map(|mask| snapshots.snapshot_mask(mask, RenderMode::Hq));
        self.gpu.flush();
        Some(RefineJob {
            ticket: RefineTicket { id, key: Arc::new(key) }, source: self.source.clone(), host_depth: self.host_depth,
            geometry: self.geometry, priors, mask, mask_generation: input.mask_generation, view: *view,
        })
    }

    /// Whether `ticket`'s captured inputs still match the current layers and view.
    pub fn refine_current(&self, ticket: &RefineTicket, layers: &[LayerInputs<'_>], view: &ViewFlags) -> bool {
        keys(layers, view).iter().any(|(id, key)| *id == ticket.id && *key == *ticket.key)
    }

    /// Retain a finished glow if its inputs are still current; otherwise drop it.
    pub fn accept_refine(&mut self, glow: RefinedGlow, layers: &[LayerInputs<'_>], view: &ViewFlags) -> bool {
        let current = self.refine_current(&glow.ticket, layers, view);
        if current { self.refined.insert(glow.ticket.id, RefinedLayer { key: glow.ticket.key, glow: glow.glow }); }
        // The mask snapshot returns to this lane's pool when the job ends; keep only the glow.
        self.gpu.trim_idle(0);
        current
    }

    /// Synchronous refinement of one layer on the document's own lane (tests, CLI).
    pub fn refine_step(&mut self, layers: &[LayerInputs<'_>], view: &ViewFlags) -> RefineState {
        if let Some(job) = self.refine_job(layers, view) {
            let gpu = self.gpu.clone();
            if let Some(glow) = job.run(&gpu) { self.accept_refine(glow, layers, view); }
        }
        self.refine_state(layers, view)
    }

    /// Exact native-grid export pixels, or a screen-resolution full-source preview.
    /// Preview is encoded Eight; Exact uses the requested export depth.
    pub fn render_region(&mut self, layers: &[LayerInputs<'_>], req: &RenderRequest,
        region: ViewRegion, out_w: u32, out_h: u32, export_depth: Depth) -> (Image, RegionQuality) {
        let (full_w, full_h) = (self.geometry.orig_w, self.geometry.orig_h);
        assert!(region.w > 0 && region.h > 0 && out_w > 0 && out_h > 0);
        assert!(region.x <= full_w && region.w <= full_w - region.x && region.y <= full_h && region.h <= full_h - region.y);
        self.invalidate_refine(layers, &req.view);
        let ready = self.refine_state(layers, &req.view) == RefineState::Ready;
        let exact = !req.view.xray && (out_w, out_h) == (region.w, region.h) && ready;
        let crop = Region { x: region.x, y: region.y, w: region.w, h: region.h };
        let glow_only = req.want_glow_composite && !req.want_composite;
        let g = self.gpu.clone();
        let source_map = ViewMap { origin_x: region.x as f64, origin_y: region.y as f64,
            src_per_dst: region.w as f64 / out_w as f64 };
        let mut composite = if glow_only { g.fill(out_w, out_h, [0.,0.,0.,1.]) }
            else if req.view.solo { g.fill(out_w, out_h, [0.;4]) }
            else if exact { g.copy_region(&self.source, full_w, full_h, crop) }
            else { g.sample_view(&self.source, source_map, out_w, out_h, Filter::Bilinear) };
        let depth = blend_depth(&req.view);
        if !exact && !req.view.solo && !glow_only {
            composite = g.convert_depth(&composite, self.host_depth, depth);
        }
        let draft = if ready && !req.view.xray { None } else {
            Some(self.render(layers, &RenderRequest { mode: RenderMode::Draft, view: req.view,
                selected: req.selected, solo_layer: req.solo_layer, want_composite: false,
                want_glow_composite: false, want_layer_glows: true, want_xray: req.view.xray }))
        };
        if req.view.xray {
            if let Some(heatmap) = draft.as_ref().and_then(|output| output.xray.as_ref()) {
                let map = ViewMap { origin_x: region.x as f64 * heatmap.width() as f64 / full_w as f64,
                    origin_y: region.y as f64 * heatmap.height() as f64 / full_h as f64,
                    src_per_dst: region.w as f64 / out_w as f64 * heatmap.width() as f64 / full_w as f64 };
                let heatmap = g.sample_view(heatmap, map, out_w, out_h, Filter::Bilinear);
                // X-Ray is always the Draft diagnostic, independent of export depth.
                if depth != Depth::Eight { composite = g.convert_depth(&composite, depth, Depth::Eight); }
                return (g.blend(&composite, &heatmap, B::Normal, 0.8, 1., Depth::Eight), RegionQuality::Preview);
            }
        }
        for input in layers {
            let mut settings = input.settings.clone(); settings.sanitize();
            if !settings.visible || req.solo_layer.is_some_and(|id| id != settings.id) { continue; }
            let glow = if exact {
                let glow = &self.refined[&settings.id].glow;
                if (glow.width(), glow.height()) == (full_w, full_h) { g.copy_region(glow, full_w, full_h, crop) }
                else { g.resize_bicubic_region(glow, full_w, full_h, crop) }
            } else {
                let glow = if ready && !req.view.xray { &self.refined[&settings.id].glow }
                    else { &draft.as_ref().unwrap().layer_glows.iter().find(|(id, _)| *id == settings.id).unwrap().1 };
                let map = ViewMap { origin_x: region.x as f64 * glow.width() as f64 / full_w as f64,
                    origin_y: region.y as f64 * glow.height() as f64 / full_h as f64,
                    src_per_dst: region.w as f64 / out_w as f64 * glow.width() as f64 / full_w as f64 };
                g.sample_view(glow, map, out_w, out_h, Filter::Bilinear)
            };
            let glow = if depth == Depth::Float { g.convert_depth(&glow, Depth::Eight, Depth::Float) } else { glow };
            composite = g.blend(&composite, &glow, blend_mode(&settings, &req.view), settings.opacity, 1., depth);
        }
        let from = if glow_only && depth == Depth::Float {
            composite = g.convert_depth(&composite, Depth::Float, Depth::Eight);
            Depth::Eight
        } else { depth };
        let to = if exact { export_depth } else { Depth::Eight };
        if from != to { composite = g.convert_depth(&composite, from, to); }
        (composite, if exact { RegionQuality::Exact } else { RegionQuality::Preview })
    }
}
