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
pub(super) struct RefinedLayer { key: RefineKey, glow: Image }

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
        self.refined.retain(|id, cached| keys.iter().any(|(key_id, key)| key_id == id && key == &cached.key));
        if before != self.refined.len() { self.gpu.trim_idle(0); }
    }

    pub fn refine_state(&self, layers: &[LayerInputs<'_>], view: &ViewFlags) -> RefineState {
        let keys = keys(layers, view);
        let total = keys.len();
        let done = keys.iter().filter(|(id, key)| self.refined.get(id).is_some_and(|cached| &cached.key == key)).count();
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

    /// Produce at most one HQ working-grid glow. No original-size effect image is allocated.
    pub fn refine_step(&mut self, layers: &[LayerInputs<'_>], view: &ViewFlags) -> RefineState {
        self.invalidate_refine(layers, view);
        let Some((id, key)) = keys(layers, view).into_iter().find(|(id, _)| !self.refined.contains_key(id)) else {
            return RefineState::Ready;
        };
        let input = layers.iter().find(|input| input.settings.id == id).unwrap();
        let g = &self.gpu;
        let mut stages = LayerStages::shared(g.clone(), self.source.clone(), self.host_depth);
        if key.input == LayerInput::Below {
            let mut preparer = LayerStages::shared(g.clone(), self.source.clone(), self.host_depth);
            preparer.prepare(&Params::default(), RenderMode::Hq);
            let mut below = preparer.working.take().unwrap().source.clone();
            let depth = blend_depth(view);
            for prior in layers.iter().take_while(|prior| prior.settings.id != id) {
                let mut settings = prior.settings.clone(); settings.sanitize();
                if !settings.visible { continue; }
                let glow = &self.refined[&settings.id].glow;
                let linear = (depth == Depth::Float).then(|| g.convert_depth(glow, Depth::Eight, Depth::Float));
                below = g.blend(&below, linear.as_ref().unwrap_or(glow), blend_mode(&settings, view), settings.opacity, 1., depth);
            }
            let mut preparer = LayerStages::shared(g.clone(), Arc::new(below), self.host_depth);
            preparer.geometry = self.geometry;
            preparer.source_is_working = true;
            preparer.prepare(&key.params, RenderMode::Hq);
            stages.working = preparer.working.take();
        }
        stages.mask = input.mask.map(|mask| g.copy(mask));
        stages.mask_generation = input.mask_generation;
        let effect_view = ViewFlags { xray: false, ..*view };
        let output = stages.render_selected_impl(&key.params, RenderMode::Hq, &effect_view, false, false, false, |_, _, _| {});
        self.refined.insert(id, RefinedLayer { key, glow: output.working_glow.clone() });
        drop(output);
        drop(stages);
        // HQ intermediates are transient: retain only the working-grid glow,
        // not an idle-pool copy of the entire layer's peak allocation.
        self.gpu.trim_idle(0);
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
