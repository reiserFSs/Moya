use super::*;

pub struct LayerInputs<'a> {
    pub settings: &'a LayerSettings,
    pub mask: Option<&'a Image>,
    pub mask_generation: u64,
}

pub struct RenderRequest {
    pub mode: RenderMode,
    pub view: ViewFlags,
    pub selected: Option<u64>,
    pub solo_layer: Option<u64>,
    pub want_layer_glows: bool,
    pub want_xray: bool,
}

pub struct DocumentOutput {
    pub composite: Image,
    pub glow_composite: Image,
    pub layer_glows: Vec<(u64, Image)>,
    pub xray: Option<Image>,
}

#[derive(Clone, PartialEq)]
struct Stamp { id: u64, generation: u64, opacity: u32, blend: LayerBlend }
struct LayerCache {
    stages: LayerStages,
    input: Option<Vec<Stamp>>,
    input_mode: Option<RenderMode>,
    input_generation: u64,
    has_mask: bool,
}
struct Grid { input: u64, working: Arc<Working> }

/// One source and one bounded stage-cache budget for the entire bottom-to-top stack.
pub struct Document {
    gpu: Arc<Gpu>,
    source: Arc<Image>,
    host_depth: Depth,
    geometry: DocGeometry,
    layers: HashMap<u64, LayerCache>,
    grids: Vec<Grid>,
    generation: u64,
    budget: u64,
}

impl Document {
    pub fn new(gpu: Arc<Gpu>, source: Image, host_depth: Depth) -> Self {
        let geometry = DocGeometry { orig_w: source.width(), orig_h: source.height() };
        Self { gpu, source: Arc::new(source), host_depth, geometry, layers: HashMap::new(), grids: Vec::new(), generation: 0, budget: CACHE_BYTES }
    }

    pub fn clear_caches(&mut self) {
        self.layers.clear();
        self.grids.clear();
    }

    /// Cold-stage benchmark/reset, retaining the shared prepared document grids.
    pub fn clear_stage_caches(&mut self) {
        for layer in self.layers.values_mut() { layer.stages.clear_stage_cache(); }
    }

    pub fn render(&mut self, layers: &[LayerInputs<'_>], request: &RenderRequest) -> DocumentOutput {
        self.render_with_checkpoints(layers, request, |_, _, _| {})
    }

    /// Checkpoints retain their historical names on single-layer documents.
    pub fn render_with_checkpoints(&mut self, layers: &[LayerInputs<'_>], req: &RenderRequest,
        mut checkpoint: impl FnMut(&str, &Image, Depth)) -> DocumentOutput {
        self.layers.retain(|id, _| layers.iter().any(|l| l.settings.id == *id));
        let single=layers.iter().filter(|l|l.settings.visible).count()==1;
        let needs_below=layers.iter().any(|l|l.settings.visible && l.settings.input==LayerInput::Below);
        let (w,h) = if req.mode == RenderMode::Hq { (self.geometry.orig_w,self.geometry.orig_h) }
            else { self.geometry.working_size(req.mode) };
        let g = self.gpu.clone();
        let source = if (w,h)==(self.source.width(),self.source.height()) { g.copy(&self.source) }
            else if let Some(grid)=self.grids.iter().find(|grid|grid.input==0 && grid.working.key.mode==req.mode) {
                g.copy(&grid.working.source)
            } else {
                let mut preparer=LayerStages::shared(g.clone(),self.source.clone(),self.host_depth);
                preparer.prepare(&Params::default(),req.mode);
                self.generation+=1;
                Arc::get_mut(preparer.working.as_mut().unwrap()).unwrap().generation=self.generation;
                let working=preparer.working.take().unwrap();
                let source=g.copy(&working.source);
                self.grids.push(Grid{input:0,working});
                source
            };
        let mut below = source;
        let working_size=self.geometry.working_size(req.mode);
        let mut below_working=if needs_below && working_size!=(w,h) {
            let mut preparer=LayerStages::shared(g.clone(),self.source.clone(),self.host_depth);
            preparer.prepare(&Params::default(),req.mode);
            Some(g.copy(&preparer.working.as_ref().unwrap().source))
        } else {None};
        let mut composite = if req.view.solo { g.fill(w,h,[0.;4]) } else { g.copy(&below) };
        let xray_base=(req.want_xray && req.view.xray).then(||g.copy(&composite));
        let mut glow_composite = g.fill(w,h,[0.,0.,0.,1.]);
        let mut layer_glows = Vec::new();
        let mut xray = None;
        let mut stamps = Vec::new();
        for input in layers {
            let mut settings=input.settings.clone(); settings.sanitize();
            if !settings.visible { continue; }
            let id=settings.id;
            let entry=self.layers.entry(id).or_insert_with(|| LayerCache {
                stages:LayerStages::shared(g.clone(),self.source.clone(),self.host_depth),
                input:None,input_mode:None,input_generation:0,has_mask:false,
            });
            let signature=if settings.input==LayerInput::Below {Some(stamps.clone())} else {None};
            if entry.input!=signature || (signature.is_some() && entry.input_mode!=Some(req.mode)) {
                self.generation+=1;
                entry.input_generation=if signature.is_some() {self.generation} else {0};
                entry.input=signature;
                entry.input_mode=Some(req.mode);
                entry.stages.source_is_working=settings.input==LayerInput::Below && req.mode==RenderMode::Draft;
                entry.stages.source=if settings.input==LayerInput::Below {Arc::new(g.copy(&below))} else {self.source.clone()};
                entry.stages.working=None;
                entry.stages.clear_stage_cache();
            }
            let has_mask=input.mask.is_some();
            if entry.stages.mask_generation!=input.mask_generation || entry.has_mask!=has_mask {
                entry.stages.mask=input.mask.map(|m|g.copy(m));
                entry.stages.mask_generation=input.mask_generation;
                entry.has_mask=has_mask;
                entry.stages.clear_stage_cache();
            }
            let key=WorkingKey{mode:req.mode,highlights:settings.params.highlights,host_depth:self.host_depth};
            if let Some(grid)=self.grids.iter().find(|grid|grid.input==entry.input_generation && grid.working.key==key) {
                entry.stages.working=Some(grid.working.clone());
            } else {
                entry.stages.working=None;
                if settings.input==LayerInput::Below {
                    if let Some(below)=&below_working {
                        let mut preparer=LayerStages::shared(g.clone(),Arc::new(g.copy(below)),self.host_depth);
                        preparer.geometry=DocGeometry{orig_w:self.geometry.orig_w,orig_h:self.geometry.orig_h};
                        preparer.source_is_working=true;
                        preparer.prepare(&settings.params,req.mode);
                        entry.stages.working=preparer.working.take();
                    } else {entry.stages.prepare(&settings.params,req.mode);}
                } else {entry.stages.prepare(&settings.params,req.mode);}
                self.generation+=1;
                let working=Arc::get_mut(entry.stages.working.as_mut().unwrap()).expect("new working grid");
                working.generation=self.generation;
                self.grids.push(Grid{input:entry.input_generation,working:entry.stages.working.as_ref().unwrap().clone()});
            }
            if req.view.xray && !req.want_layer_glows && req.selected==Some(id) {
                entry.stages.ensure_extraction(&settings.params);
                let heatmap=entry.stages.output_heatmap(&entry.stages.heatmap_cache.as_ref().unwrap().images[0],&settings.params,req.mode);
                let base=g.convert_depth(xray_base.as_ref().unwrap_or(&composite),self.host_depth,Depth::Eight);
                let preview=g.blend(&base,&heatmap,B::Normal,0.8,1.,Depth::Eight);
                let xray=req.want_xray.then(||g.copy(&heatmap));
                self.trim_cache();
                return DocumentOutput{composite:preview,glow_composite,layer_glows,xray};
            }
            let view=ViewFlags{xray:false,solo:req.view.solo,host_is_float:req.view.host_is_float};
            let output=entry.stages.render_with_checkpoints(&settings.params,req.mode,&view,&mut checkpoint);
            let output_generation=entry.stages.output_cache.as_ref().unwrap().generation;
            let included=req.solo_layer.is_none_or(|solo|solo==id);
            let blend=match settings.blend {
                LayerBlend::Auto if req.view.host_is_float && !req.view.solo=>B::LinearDodge,
                LayerBlend::Auto|LayerBlend::Screen=>B::Screen,
                LayerBlend::LinearDodge=>B::LinearDodge,
                LayerBlend::Lighten=>B::Lighten,
                LayerBlend::Normal=>B::Normal,
            };
            let depth=if req.view.host_is_float && !req.view.solo {Depth::Float} else {Depth::Eight};
            let linear_glow=(depth==Depth::Float).then(||g.convert_depth(&output.glow,Depth::Eight,Depth::Float));
            let glow=linear_glow.as_ref().unwrap_or(&output.glow);
            if needs_below {below=g.blend(&below,glow,blend,settings.opacity,1.,depth);}
            if let Some(base)=below_working.as_mut() {
                let small=&entry.stages.output_cache.as_ref().unwrap().images[3];
                let linear=(depth==Depth::Float).then(||g.convert_depth(small,Depth::Eight,Depth::Float));
                *base=g.blend(base,linear.as_ref().unwrap_or(small),blend,settings.opacity,1.,depth);
            }
            if included {
                composite=if single && settings.input==LayerInput::Original && settings.blend==LayerBlend::Auto && settings.opacity==1. {
                    output.composite
                } else {g.blend(&composite,glow,blend,settings.opacity,1.,depth)};
                glow_composite=g.blend(&glow_composite,glow,blend,settings.opacity,1.,depth);
                if req.want_layer_glows {layer_glows.push((id,output.glow));}
            }
            if req.want_xray && req.selected==Some(id) {xray=Some(output.xray);}
            stamps.push(Stamp{id,generation:output_generation,opacity:settings.opacity.to_bits(),blend:settings.blend});
            // Render one layer at a time and reclaim cached descendants before the next HQ layer.
            self.trim_cache();
        }
        if req.view.xray {
            if let Some(heatmap)=&xray {
                let base=g.convert_depth(xray_base.as_ref().unwrap(),self.host_depth,Depth::Eight);
                composite=g.blend(&base,heatmap,B::Normal,0.8,1.,Depth::Eight);
            }
        }
        if req.view.host_is_float && !req.view.solo {
            glow_composite=g.convert_depth(&glow_composite,Depth::Float,Depth::Eight);
        }
        self.trim_cache();
        DocumentOutput{composite,glow_composite,layer_glows,xray}
    }

    fn retained_bytes(&self)->u64 {
        let mut images=std::collections::HashSet::new();
        let mut bytes=0;
        let mut account=|image:&Image| {if images.insert(image as *const Image) {bytes+=image_bytes(image);}};
        for grid in &self.grids {account(&grid.working.source);account(&grid.working.color);account(&grid.working.luminance);}
        for layer in self.layers.values() {
            let s=&layer.stages;
            if !Arc::ptr_eq(&s.source,&self.source) {account(&s.source);}
            if let Some(w)=&s.working {account(&w.source);account(&w.color);account(&w.luminance);}
            macro_rules! stages {($($field:ident),*)=>{$(if let Some(c)=&s.$field {for image in &c.images {account(image);}})*};}
            stages!(extraction_cache,heatmap_cache,exposure_cache,glow_cache,glare_cache,tone_cache,ca_cache,output_cache);
            if let Some(mask)=&s.mask {account(mask);}
        }
        bytes
    }

    fn trim_cache(&mut self) {
        // Drop unused Below grids as soon as their owning input changes or disappears.
        self.grids.retain(|grid|grid.input==0 || self.layers.values().any(|l|l.input_generation==grid.input));
        if self.retained_bytes()<=self.budget {return;}
        // Descendants first, across every layer: never strand a retained output behind evicted predecessors.
        macro_rules! evict {($($field:ident),*)=>{$(for layer in self.layers.values_mut() {layer.stages.$field=None;} if self.retained_bytes()<=self.budget {return;})*};}
        evict!(output_cache,ca_cache,tone_cache,glare_cache,glow_cache,exposure_cache,heatmap_cache,extraction_cache,working);
        self.grids.clear();
        if self.retained_bytes()>self.budget {
            for layer in self.layers.values_mut() {
                layer.stages.mask=None; layer.has_mask=false;
                layer.stages.source=self.source.clone(); layer.stages.source_is_working=false;
                layer.input=None; layer.input_mode=None; layer.input_generation=0;
            }
        }
    }
}

#[cfg(test)]
mod tests;
