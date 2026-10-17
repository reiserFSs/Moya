//! Streaming Moya effect graph for glow, glare, spectrum, and chromatic aberration.
//!
//! Draft caches a bilinear 1500-wide/1300-tall working copy; HQ caps the long side
//! at 4000 and returns original-size layers. Glow copies are blurred and merged
//! one at a time. Depth switches include sRGB transfer and Photoshop quantization.
//! Latest-stage caches are byte-bounded; checkpoint renders replay every stage.
//! Auto-update is a UI scheduling gate: callers decide whether to invoke `render`.
//! Stored glow is encoded 8-bit-equivalent, even with HDR rendering enabled.
use std::sync::Arc;
use moya_core::{CaChannels, CaType, DocGeometry, GlareBlend, GlareRays, Highlights, Params, RenderMode, SpectrumBlend, Transition};
use moya_gpu::{BlendMode as B, Depth, Gpu, GradientStop, Image, RadialQuality};

pub struct RenderOutput {
    pub glow: Image,
    /// Heat map at full opacity; the view applies its 80% layer opacity.
    pub xray: Image,
    pub composite: Image,
}
pub struct ViewFlags { pub xray: bool, pub solo: bool, pub host_is_float: bool }

#[derive(PartialEq)]
struct WorkingKey { mode:RenderMode, highlights:Highlights, host_depth:Depth }
struct Working { generation:u64, key:WorkingKey, source:Image, color:Image, luminance:Image, depth:Depth }
#[derive(PartialEq)]
struct HeatmapKey { upstream:u64, depth:Depth }
// All processing textures are RGBA32F, including integer-equivalent stages.
const CACHE_BYTES: u64 = 384 * 1024 * 1024;
fn image_bytes(image: &Image) -> u64 { image.width() as u64 * image.height() as u64 * 16 }
struct Cached<K> { key: K, generation: u64, images: Vec<Arc<Image>>, depths: Vec<Depth> }
macro_rules! stage_key {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Debug, PartialEq)]
        struct $name { upstream: u64, $($field: $ty),* }
        impl $name {
            fn new(upstream: u64, p: &Params) -> Self { Self { upstream, $($field: p.$field),* } }
        }
    };
}
stage_key!(ExtractionParams { threshold:f32, dr_compression:f32, transition:Transition, colorize:bool, hue:f32, saturation:f32 });
#[derive(Debug, PartialEq)]
struct ExtractionKey { params:ExtractionParams, mask_generation:u64 }
stage_key!(ExposureKey { exposure:f32, hdr32:bool, rays_on:bool, rays_center:[f32;2], rays_samples:f32, rays_spacing:f32, rays_falloff:f32, main_opacity:f32 });
stage_key!(GlowKey { radius:f32, aspect_ratio:f32, aspect_rotation:f32, hdr32:bool, glow_iterations:f32, colorize:bool, hue:f32, saturation:f32, rays_on:bool, main_opacity:f32, core_opacity:f32, glare_on:bool, glare_detail:f32, glare_sharpness:f32 });
stage_key!(GlareKey { glare_on:bool, glare_rays:GlareRays, glare_opacity:f32, main_opacity:f32, glare_detail:f32, glare_rotation:f32, glare_length:f32, glare_sharpness:f32, glare_gamma:f32, glare_blend:GlareBlend, colorize:bool, hdr32:bool, hue:f32, saturation:f32 });
stage_key!(ToneParams { tonemap:bool, exposure:f32, gamma:f32, colorize:bool, saturation:f32, protect_core:bool, spectrum_mix:f32, spectrum_frequency:f32, spectrum_saturation:f32, spectrum_blur:f32, spectrum_blend:SpectrumBlend, hdr32:bool, glare_blend:GlareBlend, main_opacity:f32 });
#[derive(Debug, PartialEq)]
struct ToneKey { params: ToneParams, glare: u64 }
stage_key!(CaKey { hdr32:bool, ca_on:bool, ca_distance:f32, rays_center:[f32;2], ca_channels:CaChannels, ca_direction:f32, ca_type:CaType, ca_blur:f32, ca_mix:bool, ca_intensity:f32 });
#[derive(PartialEq)]
struct OutputKey { ca:u64, heatmap:u64, mode:RenderMode, unmult:bool, hdr32:bool, xray:bool, solo:bool, host_is_float:bool, detail:bool }
/// Latest result per stage, with upstream generations rather than whole-Params keys.
pub struct Renderer {
    gpu: Arc<Gpu>, source: Image, host_depth: Depth, geometry: DocGeometry,
    mask: Option<Image>, mask_generation:u64, working: Option<Arc<Working>>, generation:u64,
    extraction_cache:Option<Cached<ExtractionKey>>, heatmap_cache:Option<Cached<HeatmapKey>>,
    exposure_cache:Option<Cached<ExposureKey>>, glow_cache:Option<Cached<GlowKey>>,
    glare_cache:Option<Cached<GlareKey>>, tone_cache:Option<Cached<ToneKey>>,
    ca_cache:Option<Cached<CaKey>>, output_cache:Option<Cached<OutputKey>>,
}
impl Renderer {
    pub fn new(gpu: Arc<Gpu>, source: Image, host_depth: Depth) -> Self {
        let geometry = DocGeometry { orig_w: source.width(), orig_h: source.height() };
        Self { gpu, source, host_depth, geometry, mask:None, mask_generation:0, working:None, generation:0,
            extraction_cache:None, heatmap_cache:None, exposure_cache:None, glow_cache:None,
            glare_cache:None, tone_cache:None, ca_cache:None, output_cache:None }
    }
    pub fn set_mask(&mut self, mask: Option<Image>) {
        self.mask=mask; self.mask_generation=self.next_generation(); self.extraction_cache=None;
    }
    fn next_generation(&mut self)->u64 { self.generation+=1; self.generation }
    fn cached<K>(&mut self,key:K,images:Vec<Arc<Image>>,depths:Vec<Depth>)->Cached<K> {
        Cached { key,generation:self.next_generation(),images,depths }
    }
    fn retained_bytes(&self)->u64 {
        // At most 18 image slots; avoid allocating a set on the render path.
        let mut seen=[std::ptr::null::<Image>();18]; let mut count=0; let mut bytes=0;
        let mut account=|image:&Image| {
            let identity=image as *const Image;
            if !seen[..count].contains(&identity) {
                seen[count]=identity; count+=1; bytes+=image_bytes(image);
            }
        };
        if let Some(w)=&self.working { account(&w.source); account(&w.color); account(&w.luminance); }
        macro_rules! account_stages { ($($field:ident),*) => { $(if let Some(c)=&self.$field { for image in &c.images { account(image); } })* }; }
        account_stages!(extraction_cache,heatmap_cache,exposure_cache,glow_cache,glare_cache,tone_cache,ca_cache,output_cache);
        bytes
    }
    fn trim_cache(&mut self) { self.trim_cache_to(CACHE_BYTES); }
    fn trim_cache_to(&mut self,budget:u64) {
        // Keep predecessors reachable: reconstructing an evicted upstream stage
        // changes its generation and invalidates every retained descendant.
        let mut bytes=self.retained_bytes();
        macro_rules! evict { ($($field:ident),*) => { $(if bytes>budget { self.$field=None; bytes=self.retained_bytes(); })* }; }
        evict!(output_cache,ca_cache,tone_cache,glare_cache,glow_cache,exposure_cache,heatmap_cache,extraction_cache,working);
        debug_assert!(bytes<=budget);
    }
    fn native(&self,image:Image,depth:Depth)->Image {
        self.gpu.quantize_owned(image,depth)
    }

    fn prepare(&mut self, p: &Params, mode: RenderMode) {
        let key=WorkingKey{mode,highlights:p.highlights,host_depth:self.host_depth};
        if self.working.as_ref().is_some_and(|w|w.key==key) { return; }
        let g = &self.gpu;
        let (w,h) = self.geometry.working_size(mode);
        let (cw,ch)=self.geometry.working_size(RenderMode::Hq);
        // Photoshop resampling writes the current document grid before the
        // Color-mode luminance merge; thresholding must not see fractional codes.
        let native=|image:Image| if self.host_depth==Depth::Float {image} else {g.convert_depth(&image,self.host_depth,self.host_depth)};
        let capped=if (cw,ch)!=(self.source.width(),self.source.height()) {Some(native(g.resize_bicubic(&self.source,cw,ch)))} else {None};
        let base=capped.as_ref().unwrap_or(&self.source);
        let source=if (w,h)==(base.width(),base.height()) {g.copy(base)} else {native(g.resize_bilinear(base,w,h))};
        let color = if p.highlights == Highlights::Flatten {
            let flattened = native(g.highlights_flatten(base,self.host_depth));
            if (w,h) == (base.width(),base.height()) { flattened }
            else { native(g.resize_bilinear(&flattened,w,h)) }
        } else { g.copy(&source) };
        let depth = if mode == RenderMode::Draft { Depth::Eight } else if self.host_depth == Depth::Eight { Depth::Sixteen } else { self.host_depth };
        let luminance = g.luminance(&color,self.host_depth);
        let color = g.convert_depth(&color,self.host_depth,depth);
        let luminance = g.convert_depth(&luminance,self.host_depth,depth);
        let generation=self.next_generation();
        self.working = Some(Arc::new(Working {generation,key,source,color,luminance,depth}));
        self.extraction_cache=None;
    }
    fn extraction(&self,p:&Params) -> Image {
        let w = self.working.as_ref().unwrap(); let g=&self.gpu;
        let color = self.native(g.levels(&w.color,0.,255.,p.threshold,0.,255.,w.depth),w.depth);
        let mut m = match p.transition {
            Transition::Soft => g.levels(&w.luminance,0.,255.-p.dr_compression,p.threshold,0.,255.,w.depth),
            Transition::Precise => {
                let b=1.+(p.threshold-0.01)*252./1.99;
                // AM Levels input lists coerce fractional white points to integers.
                g.levels(&w.luminance,0.,(255.-b).round(),0.01,0.,255.,w.depth)
            }
        };
        m=self.native(m,w.depth);
        if let Some(mask)=&self.mask {
            let mask=if (mask.width(),mask.height())==(m.width(),m.height()) {g.copy(mask)} else {g.resize_bilinear(mask,m.width(),m.height())};
            // CALIBRATE: ML indexing is ambiguous; multiply the thresholded luminance by mask.R.
            let gray=g.extract_channel(&mask,0);
            m=self.native(g.apply_image(&m,&gray,B::Multiply,1.,false,w.depth),w.depth);
        }
        let e=self.native(g.apply_image(&color,&m,B::Multiply,1.,false,w.depth),w.depth);
        let e=self.native(g.levels(&e,0.,255.-p.dr_compression,1.,0.,255.,w.depth),w.depth);
        if p.colorize {self.native(g.hue_saturation(&e,0.,-100.,0.,false,w.depth),w.depth)} else {
            let e=self.native(g.hue_saturation(&e,p.hue,0.,0.,false,w.depth),w.depth);
            // Photoshop rejects the direct extraction Vibrance descriptor in Float;
            // unlike gamma compensation, this source path does not convert depth.
            if w.depth==Depth::Float {e} else {self.native(g.vibrance(&e,0.,(p.saturation-1.).max(-100.),w.depth),w.depth)}
        }
    }
    fn heatmap(&self,e:&Image,depth:Depth)->Image {
        let g=&self.gpu;
        let e=if depth==Depth::Float {g.convert_depth(e,depth,Depth::Sixteen)} else {g.copy(e)};
        // CALIBRATE: GPU gradient interpolation approximates Photoshop's perceptual gradient.
        let stops=[(0.,[0.,0.,0.]),(0.01,[0.,0.,118.]),(0.02,[0.,30.,150.]),(0.03,[0.,255.,255.]),(0.05,[100.,255.,0.]),(0.08,[255.,255.,0.]),(0.10,[255.,0.,0.]),(1.,[255.,0.,0.])]
            .map(|(pos,rgb)|GradientStop{pos,rgb:rgb.map(|v|v/255.)});
        self.native(g.gradient_map(&e,&stops,if depth==Depth::Float {Depth::Sixteen} else {depth}),if depth==Depth::Float {Depth::Sixteen} else {depth})
    }
    fn ensure_extraction(&mut self,p:&Params)->u64 {
        let working=self.working.as_ref().unwrap();
        let depth=working.depth;
        let key=ExtractionKey{params:ExtractionParams::new(working.generation,p),mask_generation:self.mask_generation};
        if !self.extraction_cache.as_ref().is_some_and(|c|c.key==key) {
            // Release obsolete descendants before allocating their replacements.
            self.clear_stage_cache();
            let image=self.extraction(p);
            self.extraction_cache=Some(self.cached(key,vec![Arc::new(image)],vec![depth]));
        }
        let generation=self.extraction_cache.as_ref().unwrap().generation;
        let heatmap_key=HeatmapKey{upstream:generation,depth};
        if !self.heatmap_cache.as_ref().is_some_and(|c|c.key==heatmap_key) {
            self.heatmap_cache=None; self.output_cache=None;
            let image=self.heatmap(&self.extraction_cache.as_ref().unwrap().images[0],depth);
            self.heatmap_cache=Some(self.cached(heatmap_key,vec![Arc::new(image)],vec![depth]));
        }
        generation
    }
    /// Drop computed stages, retaining the prepared document grids.
    pub fn clear_stage_cache(&mut self) {
        self.extraction_cache=None; self.heatmap_cache=None; self.exposure_cache=None;
        self.glow_cache=None; self.glare_cache=None; self.tone_cache=None;
        self.ca_cache=None; self.output_cache=None;
    }
    /// Composite-only preview. X-Ray computes no invisible glow stages;
    /// `render` and checkpoint renders always return all three current layers.
    pub fn render_preview(&mut self,params:&Params,mode:RenderMode,view:&ViewFlags)->Image {
        if !view.xray { return self.render(params,mode,view).composite; }
        let mut p=params.clone(); p.sanitize(); self.prepare(&p,mode);
        self.ensure_extraction(&p);
        let working=self.working.as_ref().unwrap().clone();
        let xray=self.output_heatmap(&self.heatmap_cache.as_ref().unwrap().images[0],&p,mode);
        let g=&self.gpu;
        let transparent=view.solo.then(||g.fill(xray.width(),xray.height(),[0.;4]));
        let base=transparent.as_ref().unwrap_or(if mode==RenderMode::Hq {&self.source} else {&working.source});
        let base=g.convert_depth(base,self.host_depth,Depth::Eight);
        let result=g.blend(&base,&xray,B::Normal,0.8,1.,Depth::Eight);
        self.trim_cache();
        result
    }
    fn output_heatmap(&self,heatmap:&Arc<Image>,p:&Params,mode:RenderMode)->Arc<Image> {
        let g=&self.gpu;
        let mut image=heatmap.clone();
        if mode==RenderMode::Hq && p.glare_on && p.glare_detail>0. {
            let depth=if p.hdr32 {Depth::Float} else {Depth::Sixteen};
            let (dw,dh)=self.geometry.working_size(RenderMode::Draft);
            let small=self.native(g.resize_bilinear(&image,dw.min(image.width()),dh.min(image.height())),depth);
            image=Arc::new(self.native(g.resize_bicubic(&small,self.geometry.orig_w,self.geometry.orig_h),depth));
        }
        if mode==RenderMode::Hq && (image.width(),image.height())!=(self.geometry.orig_w,self.geometry.orig_h) {
            image=Arc::new(g.resize_bicubic(&image,self.geometry.orig_w,self.geometry.orig_h));
        }
        image
    }
    pub fn render(&mut self,params:&Params,mode:RenderMode,view:&ViewFlags)->RenderOutput {
        self.render_with_checkpoints(params,mode,view,|_,_,_| {})
    }
    /// Render with borrowed native-depth stages. The ordinary render uses an
    /// inlined no-op closure, so it performs no readbacks or checkpoint copies.
    pub fn render_with_checkpoints(&mut self,params:&Params,mode:RenderMode,view:&ViewFlags,mut checkpoint:impl FnMut(&str,&Image,Depth))->RenderOutput {
        let mut p=params.clone(); p.sanitize(); self.prepare(&p,mode);
        let g=self.gpu.clone(); let working=self.working.as_ref().unwrap().clone();
        let extraction_generation=self.ensure_extraction(&p);
        let extraction=self.extraction_cache.as_ref().unwrap().images[0].clone();
        let heatmap=self.heatmap_cache.as_ref().unwrap().images[0].clone();
        let heatmap_generation=self.heatmap_cache.as_ref().unwrap().generation;
        let mut depth=working.depth;
        checkpoint("extraction",&extraction,depth);
        let exposure_key=ExposureKey::new(extraction_generation,&p);
        if !self.exposure_cache.as_ref().is_some_and(|c|c.key==exposure_key) {
        self.exposure_cache=None; self.glow_cache=None; self.glare_cache=None;
        self.tone_cache=None; self.ca_cache=None; self.output_cache=None;
        let e=if depth==Depth::Float {
            Arc::new(g.convert_depth(&g.convert_depth(&extraction,depth,Depth::Sixteen),Depth::Sixteen,depth))
        } else { extraction.clone() };
        let mut core=self.native(if p.hdr32 {g.desaturate(&e,depth)} else {g.black_and_white_default(&e)},depth);
        let mut v=self.native(g.exposure(&e,p.exposure,0.,1.,depth),depth);
        let target=if p.hdr32 {Depth::Float} else {Depth::Sixteen};
        core=g.convert_depth(&core,depth,target); v=g.convert_depth(&v,depth,target); depth=target;
        let exposed=Arc::new(v);
        let v=if p.rays_on {Arc::new(self.rays(&exposed,&p,depth))} else {exposed.clone()};
        self.exposure_cache=Some(self.cached(exposure_key,vec![v,Arc::new(core),exposed],vec![depth]));
        }
        let exposure=self.exposure_cache.as_ref().unwrap();
        depth=exposure.depths[0];
        checkpoint("exposure_depth",&exposure.images[2],depth);
        let exposure_generation=exposure.generation;
        let exposure_v=exposure.images[0].clone(); let exposure_core=exposure.images[1].clone();
        let glare_key=GlareKey::new(exposure_generation,&p);
        if !self.glare_cache.as_ref().is_some_and(|c|c.key==glare_key) {
            self.glare_cache=None; self.tone_cache=None; self.ca_cache=None; self.output_cache=None;
            let images=if p.glare_on {vec![Arc::new(self.glare(&exposure_v,&p,mode,depth))]} else {vec![]};
            self.glare_cache=Some(self.cached(glare_key,images,vec![depth]));
        }
        let glare_generation=self.glare_cache.as_ref().unwrap().generation;
        let glare_image=self.glare_cache.as_ref().unwrap().images.first().cloned();
        let glow_key=GlowKey::new(exposure_generation,&p);
        if !self.glow_cache.as_ref().is_some_and(|c|c.key==glow_key) {
        self.glow_cache=None; self.tone_cache=None; self.ca_cache=None; self.output_cache=None;
        let mut v=exposure_v.clone(); let mut core=exposure_core.clone();
        // Glare is an independent branch of the unmodified rays result.
        if mode==RenderMode::Hq && p.glare_on && p.glare_detail>0. {
            // Glare Details resamples the entire document, including the
            // untouched glow copies and core, back to the document size.
            let (dw,dh)=self.geometry.working_size(RenderMode::Draft);
            let resample=|image:&Image| {
                let small=self.native(g.resize_bilinear(image,dw.min(image.width()),dh.min(image.height())),depth);
                self.native(g.resize_bicubic(&small,self.geometry.orig_w,self.geometry.orig_h),depth)
            };
            v=Arc::new(resample(&v)); core=Arc::new(resample(&core));
        }
        if p.glare_on && p.glare_sharpness>0. && depth==Depth::Float {
            // Smart Sharpen temporarily converts the whole document, not only glare.
            v=Arc::new(g.convert_depth(&g.convert_depth(&v,depth,Depth::Sixteen),Depth::Sixteen,depth));
            core=Arc::new(g.convert_depth(&g.convert_depth(&core,depth,Depth::Sixteen),Depth::Sixteen,depth));
        }
        // Stream each independently blurred copy directly into the merge accumulator.
        let mut glow=g.fill(v.width(),v.height(),[0.;4]);
        let blend=if p.hdr32 {B::LinearDodge} else {B::Screen};
        let n=p.glow_iterations as usize;
        let mut blur_source=moya_gpu::BlurSource::new(&g,&v);
        for i in 0..=n {
            let mut layer=self.blur_layer(&v,&p,mode,i,&mut blur_source);
            if p.colorize {
                let sat=if p.saturation-i as f32>20. {p.saturation-i as f32} else {p.saturation};
                layer=self.native(g.hue_saturation(&layer,p.hue,sat,0.,true,depth),depth);
            }
            // Rays' two merges bake the inherited Main fill; ordinary copies retain it.
            let mut fill=if p.rays_on {1.} else {p.main_opacity/100.};
            if i>=5 && p.core_opacity>0. && p.core_opacity<100. {fill=p.main_opacity/100.*p.core_opacity.trunc()/100.;}
            // Glare source setup resets the topmost glow copy's fill to 100%.
            if p.glare_on && i==n {fill=1.;}
            glow=g.blend(&glow,&layer,blend,1.,fill,depth);
        }
        glow=self.native(glow,depth);
        if p.hdr32 {glow=g.exposure(&glow,0.,0.,0.45,Depth::Float);}
        self.glow_cache=Some(self.cached(glow_key,vec![Arc::new(glow),core],vec![depth]));
        }
        let merged=self.glow_cache.as_ref().unwrap();
        depth=merged.depths[0];
        checkpoint("merged_glow",&merged.images[0],depth);
        let tone_key=ToneKey{params:ToneParams::new(merged.generation,&p),glare:glare_generation};
        let merged_glow=merged.images[0].clone(); let merged_core=merged.images[1].clone();
        if !self.tone_cache.as_ref().is_some_and(|c|c.key==tone_key) {
        self.tone_cache=None; self.ca_cache=None; self.output_cache=None;
        let mut glow=merged_glow; let mut core=merged_core;
        let mut glare=glare_image;
        if p.tonemap {
            glow=Arc::new(g.convert_depth(&glow,depth,Depth::Float));
            glow=Arc::new(g.tonemap_exp(&glow,p.exposure.max(0.),p.gamma));
            glow=Arc::new(g.convert_depth(&glow,Depth::Float,Depth::Sixteen));
            core=Arc::new(g.convert_depth(&core,depth,Depth::Sixteen));
            glare=glare.map(|x|Arc::new(g.convert_depth(&x,depth,Depth::Sixteen))); depth=Depth::Sixteen;
        } else {
            glow=Arc::new(self.native(g.exposure(&glow,p.exposure.max(0.),0.,p.gamma,depth),depth));
            if p.gamma>1. {
                let float=depth==Depth::Float;
                if float {
                    glow=Arc::new(g.convert_depth(&glow,depth,Depth::Sixteen));
                    core=Arc::new(g.convert_depth(&core,depth,Depth::Sixteen));
                    glare=glare.map(|x|Arc::new(g.convert_depth(&x,depth,Depth::Sixteen))); depth=Depth::Sixteen;
                }
                let x=if float {p.gamma*100./1.2} else {(p.gamma-1.)*100./1.2-if p.colorize {100.-p.saturation} else {0.}};
                glow=Arc::new(self.native(g.vibrance(&glow,0.5*x,0.,depth),depth));
            }
        }
        let tone=glow.clone(); let tone_depth=depth;
        if let Some(mut glare)=glare {
            if p.protect_core {
                let desaturated=self.native(g.desaturate(&glow,depth),depth);
                glare=Arc::new(self.native(g.apply_image(&glare,&desaturated,B::Multiply,1.,true,depth),depth));
            }
            if p.spectrum_mix>0. {
                glare=Arc::new(self.spectrum(&glare,&glow,&p,mode,depth));
                if depth==Depth::Float {
                    glow=Arc::new(g.convert_depth(&g.convert_depth(&glow,depth,Depth::Sixteen),Depth::Sixteen,depth));
                    core=Arc::new(g.convert_depth(&g.convert_depth(&core,depth,Depth::Sixteen),Depth::Sixteen,depth));
                }
                if p.hdr32 && depth!=Depth::Float {
                    glare=Arc::new(g.convert_depth(&glare,depth,Depth::Float));
                    glow=Arc::new(g.convert_depth(&glow,depth,Depth::Float));
                    core=Arc::new(g.convert_depth(&core,depth,Depth::Float));
                    depth=Depth::Float;
                }
            }
            if p.glare_blend==GlareBlend::ColorDodge {
                if p.hdr32 {
                    glow=Arc::new(g.convert_depth(&glow,depth,Depth::Sixteen));
                    glare=Arc::new(g.convert_depth(&glare,depth,Depth::Sixteen));
                    core=Arc::new(g.convert_depth(&core,depth,Depth::Sixteen));
                    glow=Arc::new(self.native(g.blend(&glow,&glare,B::ColorDodge,1.,1.,Depth::Sixteen),Depth::Sixteen));
                    glow=Arc::new(g.convert_depth(&glow,Depth::Sixteen,Depth::Float));
                    core=Arc::new(g.convert_depth(&core,Depth::Sixteen,Depth::Float));
                    depth=Depth::Float;
                } else {glow=Arc::new(self.native(g.blend(&glow,&glare,B::ColorDodge,1.,1.,depth),depth));}
            } else {glow=Arc::new(self.native(g.blend(&glow,&glare,B::LinearDodge,1.,1.,depth),depth));}
        }
        let glare_composite=glow.clone(); let glare_depth=depth;
        glow=Arc::new(self.native(g.blend(&glow,&core,if p.hdr32 {B::LinearDodge} else {B::ColorDodge},1.,p.main_opacity/100.,depth),depth));
        if p.hdr32 || mode==RenderMode::Hq {
            if depth==Depth::Float {glow=Arc::new(g.convert_depth(&glow,depth,Depth::Sixteen)); depth=Depth::Sixteen;}
            glow=Arc::new(self.native(g.add_noise(&glow,0.3,false,0x4f4e4952,depth),depth));
        }
        let bp=g.convert_depth(&glow,depth,Depth::Eight);
        self.tone_cache=Some(self.cached(tone_key,vec![Arc::new(bp),tone,glare_composite],vec![Depth::Eight,tone_depth,glare_depth]));
        }
        let tone=self.tone_cache.as_ref().unwrap();
        checkpoint("tone",&tone.images[1],tone.depths[1]);
        checkpoint("glare_composite",&tone.images[2],tone.depths[2]);
        checkpoint("final_glow",&tone.images[0],Depth::Eight);
        let ca_key=CaKey::new(tone.generation,&p); let bp=tone.images[0].clone();
        if !self.ca_cache.as_ref().is_some_and(|c|c.key==ca_key) {
            self.ca_cache=None; self.output_cache=None;
            let ca=self.ca(&bp,&p,mode);
            self.ca_cache=Some(self.cached(ca_key,vec![Arc::new(ca)],vec![if p.hdr32 {Depth::Float} else {Depth::Sixteen}]));
        }
        let ca=self.ca_cache.as_ref().unwrap();
        checkpoint("ca",&ca.images[0],ca.depths[0]);
        let output_key=OutputKey{ca:ca.generation,heatmap:heatmap_generation,mode,unmult:p.unmult,hdr32:p.hdr32,
            xray:view.xray,solo:view.solo,host_is_float:view.host_is_float,detail:p.glare_on&&p.glare_detail>0.};
        let ca_image=ca.images[0].clone();
        if !self.output_cache.as_ref().is_some_and(|c|c.key==output_key) {
            self.output_cache=None;
            let output=self.finish(&p,mode,view,&ca_image,&heatmap,&working.source);
            self.output_cache=Some(self.cached(output_key,output,vec![]));
        }
        let output=self.output_cache.as_ref().unwrap();
        checkpoint("output",&output.images[0],Depth::Eight);
        let output=RenderOutput{glow:g.copy(&output.images[0]),xray:g.copy(&output.images[1]),composite:g.copy(&output.images[2])};
        self.trim_cache();
        output
    }
    fn blur_layer(&self,v:&Image,p:&Params,mode:RenderMode,i:usize,blur_source:&mut moya_gpu::BlurSource)->Image {
        let g=&self.gpu; let r=p.radius*self.geometry.radius_scale(mode)*0.5_f32.powi(i as i32);
        if p.aspect_ratio==1. {
            // Photoshop's fitted kernels are not Gaussian: preserve the sequential calls.
            let mut remaining=r-1000.;
            let depth=if p.hdr32 {Depth::Float} else {Depth::Sixteen};
            let mut x=self.native(blur_source.blur(r.min(1000.).max(0.1)),depth);
            while remaining>0. {
                x=self.native(g.gaussian_blur(&x,remaining.min(1000.).max(0.1)),depth);
                remaining-=1000.;
            }
            if p.hdr32 {g.exposure(&x,0.,0.,2.2,Depth::Float)} else {x}
        } else {
            let mut x=g.copy(v);
            let angles=[-p.aspect_rotation,if p.aspect_rotation>0. {p.aspect_rotation-90.} else {p.aspect_rotation+90.}];
            for (mut d,angle) in [(r/2.,angles[0]),(r/2.*(2.-p.aspect_ratio),angles[1])] {
                let depth=if p.hdr32 {Depth::Float} else {Depth::Sixteen};
                while d>2000. {x=self.motion_pair(&x,angle,d,depth); d-=2000.;}
                x=self.motion_pair(&x,angle,d,depth);
            }
            x
        }
    }
    fn motion_pair(&self,x:&Image,angle:f32,d:f32,depth:Depth)->Image {
        // CALIBRATE: Photoshop rejects/clamps out-of-range motion descriptors; clamp to 1..2000.
        let x=self.native(self.gpu.motion_blur(x,angle,(2.*d).clamp(1.,2000.),depth),depth);
        self.native(self.gpu.motion_blur(&x,angle,(4.*d).clamp(1.,2000.),depth),depth)
    }
    fn rays(&self,v:&Image,p:&Params,depth:Depth)->Image {
        let g=&self.gpu; let mode=if p.hdr32 {B::LinearDodge} else {B::Screen};
        let center=[(p.rays_center[0]*v.width() as f32).trunc(),(p.rays_center[1]*v.height() as f32).trunc()];
        let fill=p.main_opacity/100.;
        let mut sample=g.copy(v); let mut opacity=1.;
        let mut sum=g.fill(v.width(),v.height(),[0.;4]); let n=p.rays_samples as usize;
        for i in 0..n {
            let k=(100.*i as f32/n as f32).round(); let weight=(-k*k/(2.*(100_f32/6.).powi(2))).exp();
            if weight<0.01 {break;}
            // The blur edits the previous layer before Layer via Copy creates its successor.
            if (1..=3).contains(&i) {sample=self.native(g.radial_zoom_blur(&sample,p.rays_center,10.,RadialQuality::Good),depth);}
            sum=g.blend(&sum,&sample,mode,opacity,fill,depth);
            sample=self.native(g.scale_about(&sample,center,1.+p.rays_spacing/100.),depth);
            opacity=weight;
        }
        sum=g.blend(&sum,&sample,mode,opacity,fill,depth);
        let sum=self.native(sum,depth);
        let sum=self.native(g.levels(&sum,0.,255.,p.rays_falloff,0.,255.,depth),depth);
        let base=g.blend(&g.fill(v.width(),v.height(),[0.;4]),v,mode,1.,fill,depth);
        self.native(g.blend(&base,&sum,mode,1.,1.,depth),depth)
    }
    fn glare(&self,v:&Image,p:&Params,mode:RenderMode,depth:Depth)->Image {
        let g=&self.gpu; let axes=match p.glare_rays {GlareRays::Two=>1,GlareRays::Four=>2,GlareRays::Six=>3};
        let alpha=p.glare_opacity*p.main_opacity/10000.;
        let mut source=g.copy(v);
        let (rw,rh)=if mode==RenderMode::Hq && p.glare_detail>0. {(self.geometry.orig_w,self.geometry.orig_h)} else {(v.width(),v.height())};
        let mut result=g.fill(rw,rh,[0.;4]);
        for j in 0..axes {
            if p.colorize {source=self.native(if p.hdr32 {g.desaturate(&source,depth)} else {g.black_and_white_default(&source)},depth);}
            if j==0 && p.glare_detail>0. {
                if mode==RenderMode::Hq {
                    let (dw,dh)=self.geometry.working_size(RenderMode::Draft);
                    let small=self.native(g.resize_bilinear(&source,dw.min(v.width()),dh.min(v.height())),depth);
                    let rippled=self.native(g.ripple(&small,(p.glare_detail*10.).min(999.),true,0),depth);
                    source=self.native(g.resize_bicubic(&rippled,rw,rh),depth);
                } else {source=self.native(g.ripple(&source,(p.glare_detail*10.).min(999.),true,0),depth);}
            }
            let angle=-(p.glare_rotation+j as f32*180./axes as f32);
            let d=2.*p.glare_length*100.*self.geometry.radius_scale(mode);
            let mut x=g.copy(&source);
            for iteration in 0..4 {
                x=self.native(g.motion_blur(&x,angle,d.clamp(1.,2000.),depth),depth);
                let copy=self.native(g.motion_blur(&x,angle,d.clamp(1.,2000.),depth),depth);
                x=self.native(g.blend(&x,&copy,if iteration==0 && !p.hdr32 {B::Screen} else {B::LinearDodge},1.,alpha,depth),depth);
            }
            x=self.native(g.motion_blur(&x,angle,d.clamp(1.,2000.),depth),depth);
            result=self.native(g.blend(&result,&x,B::LinearDodge,1.,1.,depth),depth);
        }
        if p.glare_sharpness>0. {
            let x=g.convert_depth(&result,depth,Depth::Sixteen);
            result=self.native(g.smart_sharpen(&x,500.*p.glare_sharpness,p.glare_sharpness*self.geometry.sharpen_scale(mode),Depth::Sixteen),Depth::Sixteen);
            result=g.convert_depth(&result,Depth::Sixteen,depth);
        }
        if p.glare_gamma!=1. && p.glare_blend==GlareBlend::ColorDodge {result=self.native(g.exposure(&result,0.,0.,p.glare_gamma,depth),depth);}
        if p.colorize && p.glare_blend==GlareBlend::LinearDodge {result=self.native(g.hue_saturation(&result,p.hue,p.saturation,0.,true,depth),depth);}
        result
    }
    fn spectrum(&self,glare:&Image,glow:&Image,p:&Params,mode:RenderMode,depth:Depth)->Image {
        let g=&self.gpu; let mut x=self.native(g.exposure(glow,1.,0.,2.,depth),depth);
        if depth==Depth::Float {x=g.convert_depth(&x,depth,Depth::Sixteen);}
        let period=11.-p.spectrum_frequency;
        let mut stops=Vec::new(); let mut t=0;
        while (t as f32)<100./period {
            let mut pos=period*(t+1) as f32; if pos>100. {pos-=100.;}
            let mut rgb=[0.;3]; rgb[t%3]=1.; stops.push(GradientStop{pos:pos/100.,rgb}); t+=1;
        }
        stops.sort_by(|a,b|a.pos.total_cmp(&b.pos));
        let spectrum_depth=if depth==Depth::Float {Depth::Sixteen} else {depth};
        x=self.native(g.gradient_map(&x,&stops,spectrum_depth),spectrum_depth);
        x=self.native(g.vibrance(&x,0.,p.spectrum_saturation-100.,spectrum_depth),spectrum_depth);
        x=self.native(g.gaussian_blur(&x,p.spectrum_blur*self.geometry.spectrum_scale(mode)),spectrum_depth);
        let blend=if depth==Depth::Float {B::Multiply} else {match p.spectrum_blend {SpectrumBlend::Overlay=>B::Overlay,SpectrumBlend::SoftLight=>B::SoftLight,SpectrumBlend::Multiply=>B::Multiply}};
        let target=g.convert_depth(glare,depth,if depth==Depth::Float {Depth::Sixteen} else {depth});
        let result=self.native(g.apply_image(&target,&x,blend,p.spectrum_mix/100.,false,spectrum_depth),spectrum_depth);
        g.convert_depth(&result,if depth==Depth::Float {Depth::Sixteen} else {depth},depth)
    }
    fn ca(&self,bp:&Image,p:&Params,mode:RenderMode)->Image {
        let g=&self.gpu; let depth=if p.hdr32 {Depth::Float} else {Depth::Sixteen};
        let mut image=g.convert_depth(bp,Depth::Eight,depth);
        if !p.ca_on || p.ca_distance<=0. {return image;}
        let center=[(p.rays_center[0]*image.width() as f32).trunc(),(p.rays_center[1]*image.height() as f32).trunc()];
        let signs=match p.ca_channels {CaChannels::Rgb=>[-1.,0.,1.],CaChannels::Bgr=>[1.,0.,-1.],CaChannels::MagentaGreen=>[-1.,1.,-1.],CaChannels::YellowBlue=>[-1.,-1.,1.],CaChannels::RedCyan=>[1.,-1.,-1.]};
        let scales=match p.ca_channels {CaChannels::Rgb=>[0.,0.5,1.],CaChannels::Bgr=>[1.,0.5,0.],CaChannels::MagentaGreen=>[0.,1.,0.],CaChannels::YellowBlue=>[0.,0.,1.],CaChannels::RedCyan=>[1.,0.,0.]};
        let angle=p.ca_direction.to_radians(); let d=(p.ca_distance*self.geometry.ca_scale(mode)).round();
        for channel in 0..3 {
            let gray=g.extract_channel(&image,channel);
            let mut shifted=match p.ca_type {
                // PS Offset repeat fill extends the edge pixels; it does not wrap.
                CaType::Offset=>g.offset_repeat(&gray,(signs[channel as usize]*d*angle.cos()).round() as i32,(signs[channel as usize]*d*angle.sin()).round() as i32),
                CaType::Center=>g.scale_about(&gray,center,1.+0.005*p.ca_distance*scales[channel as usize]),
            };
            shifted=self.native(shifted,depth);
            // Offset always issues both motion descriptors, even at Blur=0.
            // Measured PS distance 0 equals distance 1, not an identity.
            if p.ca_blur>0. || p.ca_type==CaType::Offset {
                shifted=match p.ca_type {
                    CaType::Offset=>self.motion_pair(&shifted,-p.ca_direction,self.geometry.ca_blur_base(mode,p.ca_blur),depth),
                    CaType::Center=>{
                        let q=if mode==RenderMode::Hq {RadialQuality::Best} else {RadialQuality::Draft};
                        let x=self.native(g.radial_zoom_blur(&shifted,p.rays_center,p.ca_blur,q),depth);
                        if p.ca_mix {self.native(g.radial_zoom_blur(&x,p.rays_center,p.ca_blur,q),depth)} else {x}
                    }
                };
            }
            let k=2.*p.ca_intensity;
            shifted=self.native(if k>0. {g.levels(&shifted,0.,255.-k,1.,0.,255.,depth)} else {g.levels(&shifted,0.,255.,1.,0.,255.-k.abs(),depth)},depth);
            image=self.native(g.apply_to_channel(&image,&shifted,channel,if p.ca_mix {B::Lighten} else {B::Normal},1.),depth);
            if p.ca_type==CaType::Offset {
                let gray=g.extract_channel(&image,channel);
                image=self.native(g.apply_to_channel(&image,&g.gaussian_blur(&gray,1.),channel,B::Normal,1.),depth);
            }
        }
        image
    }
    fn finish(&self,p:&Params,mode:RenderMode,view:&ViewFlags,ca:&Image,heatmap:&Arc<Image>,working_source:&Image)->Vec<Arc<Image>> {
        let g=&self.gpu;
        let depth=if p.hdr32 {Depth::Float} else {Depth::Sixteen};
        let mut glow=if p.unmult && (view.solo || mode==RenderMode::Hq) {
            let converted=g.convert_depth(ca,depth,Depth::Sixteen);
            let unmult=self.native(g.unmult(&converted),Depth::Sixteen);
            g.convert_depth(&unmult,Depth::Sixteen,Depth::Eight)
        } else {g.convert_depth(ca,depth,Depth::Eight)};
        let xray=self.output_heatmap(heatmap,p,mode);
        let source=if mode==RenderMode::Hq {&self.source} else {working_source};
        // HQ returns original document dimensions even when the 4000px working cap applies.
        if mode==RenderMode::Hq && (glow.width(),glow.height())!=(self.geometry.orig_w,self.geometry.orig_h) {
            glow=g.resize_bicubic(&glow,self.geometry.orig_w,self.geometry.orig_h);
        }
        // HQ hides the source inside the element, but the host composites that
        // element over the original document. Solo is the only source-free view.
        let hidden=view.solo;
        let transparent=hidden.then(||g.fill(glow.width(),glow.height(),[0.;4]));
        let base=transparent.as_ref().unwrap_or(source);
        let composite=if view.xray {
            let base=g.convert_depth(&base,self.host_depth,Depth::Eight);
            g.blend(&base,&xray,B::Normal,0.8,1.,Depth::Eight)
        } else if view.host_is_float && !hidden {
            let glow_linear=g.convert_depth(&glow,Depth::Eight,Depth::Float);
            g.blend(&base,&glow_linear,B::LinearDodge,1.,1.,Depth::Float)
        } else {g.blend(&base,&glow,B::Screen,1.,1.,Depth::Eight)};
        vec![Arc::new(glow),xray,Arc::new(composite)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage_keys_exclude_downstream_parameters() {
        let a=Params::default(); let mut b=a.clone(); b.ca_distance+=10.; b.ca_on=!b.ca_on; b.unmult=!b.unmult;
        assert_eq!(GlowKey::new(1,&a),GlowKey::new(1,&b));
        b.glare_length+=1.;
        assert_eq!(GlowKey::new(1,&a),GlowKey::new(1,&b));
        assert_ne!(GlareKey::new(1,&a),GlareKey::new(1,&b));
        assert_ne!(GlowKey::new(1,&a),GlowKey::new(2,&a));
    }
    #[test]
    fn cache_budget_deduplicates_images_and_preserves_predecessors() {
        let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
        let mut renderer=Renderer::new(gpu.clone(),gpu.fill(3,3,[0.25,0.5,0.8,1.]),Depth::Eight);
        let p=Params{radius:2.,glow_iterations:1.,..Params::default()};
        let view=ViewFlags{xray:false,solo:false,host_is_float:false};
        renderer.render(&p,RenderMode::Draft,&view);
        let extraction=renderer.extraction_cache.as_ref().unwrap().generation;
        let exposure=renderer.exposure_cache.as_ref().unwrap();
        assert!(Arc::ptr_eq(&exposure.images[0],&exposure.images[2]));
        let bytes=renderer.retained_bytes();
        let mut slots=3;
        macro_rules! count_slots { ($($field:ident),*) => { $(slots+=renderer.$field.as_ref().map_or(0,|c|c.images.len());)* }; }
        count_slots!(extraction_cache,heatmap_cache,exposure_cache,glow_cache,glare_cache,tone_cache,ca_cache,output_cache);
        assert!(bytes<slots as u64*3*3*16);
        renderer.trim_cache_to(bytes-1);
        assert!(renderer.output_cache.is_none());
        assert!(renderer.retained_bytes()<bytes);
        assert_eq!(renderer.extraction_cache.as_ref().unwrap().generation,extraction);
        renderer.render(&p,RenderMode::Draft,&view);
        assert_eq!(renderer.extraction_cache.as_ref().unwrap().generation,extraction);
        renderer.trim_cache_to(0);
        assert_eq!(renderer.retained_bytes(),0);
    }
    #[test]
    fn glare_edits_reuse_glow_and_xray_preview_skips_it() {
        let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
        let mut renderer=Renderer::new(gpu.clone(),gpu.fill(3,3,[0.25,0.5,0.8,1.]),Depth::Eight);
        let mut p=Params{radius:2.,glow_iterations:1.,glare_on:true,glare_length:0.01,..Params::default()};
        let mut view=ViewFlags{xray:false,solo:false,host_is_float:false};
        renderer.render(&p,RenderMode::Draft,&view);
        let glow=renderer.glow_cache.as_ref().unwrap().generation;
        let glare=renderer.glare_cache.as_ref().unwrap().generation;
        p.glare_length=0.02;
        renderer.render(&p,RenderMode::Draft,&view);
        assert_eq!(glow,renderer.glow_cache.as_ref().unwrap().generation);
        assert_ne!(glare,renderer.glare_cache.as_ref().unwrap().generation);
        p.threshold+=0.1; view.xray=true;
        let preview=renderer.render_preview(&p,RenderMode::Draft,&view);
        assert!(renderer.glow_cache.is_none(),"obsolete glow is released without recomputing it");
        let full=renderer.render(&p,RenderMode::Draft,&view);
        assert_eq!(gpu.download_rgba32f(&preview),gpu.download_rgba32f(&full.composite));
    }
    #[test]
    fn checkpoints_preserve_render_and_stage_order() {
        let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
        let mut renderer=Renderer::new(gpu.clone(),gpu.fill(3,3,[0.25,0.5,0.8,1.]),Depth::Eight);
        let p=Params{radius:2.,glow_iterations:1.,..Params::default()};
        let view=ViewFlags{xray:false,solo:false,host_is_float:false};
        let mut stages=Vec::new();
        let traced=renderer.render_with_checkpoints(&p,RenderMode::Draft,&view,|name,image,depth| {
            assert_eq!((image.width(),image.height()),(3,3));
            if name=="extraction" {
                for value in gpu.download_rgba32f(image) {
                    assert!((value*255.-(value*255.).round()).abs()<0.0001,"destructive byte boundary");
                }
            }
            stages.push((name.to_owned(),depth));
        });
        assert_eq!(stages.iter().map(|(name,_)|name.as_str()).collect::<Vec<_>>(),
            ["extraction","exposure_depth","merged_glow","tone","glare_composite","final_glow","ca","output"]);
        assert_eq!(stages[0].1,Depth::Eight);
        assert_eq!(stages[5].1,Depth::Eight);
        let plain=renderer.render(&p,RenderMode::Draft,&view);
        assert_eq!(gpu.download_rgba32f(&traced.glow),gpu.download_rgba32f(&plain.glow));
        let p=Params{hdr32:true,glare_on:true,glare_blend:GlareBlend::LinearDodge,
            glare_length:0.01,spectrum_mix:50.,..p};
        stages.clear();
        renderer.render_with_checkpoints(&p,RenderMode::Draft,&view,|name,_,depth|stages.push((name.to_owned(),depth)));
        assert_eq!(stages[3].1,Depth::Sixteen,"gamma compensation leaves integer depth");
        assert_eq!(stages[4].1,Depth::Float,"Spectrum restores HDR even from integer depth");
    }
    #[test]
    fn offset_ca_zero_blur_keeps_both_minimum_motion_passes() {
        let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
        let mut pixels=[0.;36];for pixel in pixels.chunks_exact_mut(4) {pixel[3]=1.;}pixels[16..20].fill(1.);
        let source=gpu.upload_rgba32f(3,3,&pixels);
        let renderer=Renderer::new(gpu.clone(),gpu.copy(&source),Depth::Eight);
        let p=Params{ca_on:true,ca_blur:0.,ca_distance:1.,ca_direction:45.,..Params::default()};
        let zero=renderer.ca(&source,&p,RenderMode::Draft);
        let minimum=renderer.ca(&source,&Params{ca_blur:0.001,..p},RenderMode::Draft);
        assert_eq!(gpu.download_rgba32f(&zero),gpu.download_rgba32f(&minimum));
        let p=Params{aspect_ratio:2.,aspect_rotation:30.,radius:2.,..Params::default()};
        let major=renderer.motion_pair(&source,-30.,1.,Depth::Sixteen);
        let expected=renderer.motion_pair(&major,-60.,0.,Depth::Sixteen);
        let mut blur_source=moya_gpu::BlurSource::new(&gpu,&source);
        let actual=renderer.blur_layer(&source,&p,RenderMode::Draft,0,&mut blur_source);
        assert_eq!(gpu.download_rgba32f(&actual),gpu.download_rgba32f(&expected),"aspect 2 retains zero-distance minor passes");
    }
    #[test]
    fn rays_copy_inherits_fill_and_merges_before_levels() {
        let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
        let source=gpu.quantize(&gpu.fill(3,3,[0.25,0.4,0.6,1.]),Depth::Sixteen);
        let renderer=Renderer::new(gpu.clone(),gpu.copy(&source),Depth::Sixteen);
        let p=Params{rays_samples:1.,rays_spacing:0.,rays_falloff:1.,main_opacity:50.,..Params::default()};
        for (hdr32,depth,mode) in [(false,Depth::Sixteen,B::Screen),(true,Depth::Float,B::LinearDodge)] {
            let p=Params{hdr32,..p.clone()};
            let empty=gpu.fill(3,3,[0.;4]);
            // Corpus confirms Layer via Copy retains mode, fill and opacity.
            let first=gpu.blend(&empty,&source,mode,1.,0.5,depth);
            let samples=renderer.native(gpu.blend(&first,&source,mode,1.,0.5,depth),depth);
            let samples=renderer.native(gpu.levels(&samples,0.,255.,1.,0.,255.,depth),depth);
            let expected=renderer.native(gpu.blend(&first,&samples,mode,1.,1.,depth),depth);
            let actual=renderer.rays(&source,&p,depth);
            assert_eq!(gpu.download_rgba32f(&actual),gpu.download_rgba32f(&expected));
        }
    }
    #[test]
    fn float_extraction_skips_unavailable_vibrance_without_conversion() {
        let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
        let mut renderer=Renderer::new(gpu.clone(),gpu.fill(3,3,[0.25,0.5,2.,1.]),Depth::Float);
        let p=Params{hdr32:true,colorize:false,saturation:1.,..Params::default()};
        renderer.prepare(&p,RenderMode::Hq);
        let expected=gpu.download_rgba32f(&renderer.extraction(&p));
        let p=Params{saturation:100.,..p};
        assert_eq!(expected,gpu.download_rgba32f(&renderer.extraction(&p)));
    }
    #[test]
    fn precise_threshold_rounds_descriptor_white_before_levels() {
        let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
        let mut renderer=Renderer::new(gpu.clone(),gpu.fill(1,1,[235./255.,235./255.,235./255.,1.]),Depth::Eight);
        let p=Params{transition:Transition::Precise,..Params::default()};
        renderer.prepare(&p,RenderMode::Draft);
        let result=gpu.download_rgba32f(&renderer.extraction(&p));
        let working=renderer.working.as_ref().unwrap();
        let color=gpu.quantize(&gpu.levels(&working.color,0.,255.,p.threshold,0.,255.,Depth::Eight),Depth::Eight);
        let mask=gpu.quantize(&gpu.levels(&working.luminance,0.,236.,0.01,0.,255.,Depth::Eight),Depth::Eight);
        let combined=gpu.quantize(&gpu.apply_image(&color,&mask,B::Multiply,1.,false,Depth::Eight),Depth::Eight);
        let expected=gpu.quantize(&gpu.levels(&combined,0.,225.,1.,0.,255.,Depth::Eight),Depth::Eight);
        let expected=gpu.quantize(&gpu.hue_saturation(&expected,0.,0.,0.,false,Depth::Eight),Depth::Eight);
        let expected=gpu.quantize(&gpu.vibrance(&expected,0.,-1.,Depth::Eight),Depth::Eight);
        assert_eq!(result,gpu.download_rgba32f(&expected));
    }
    #[test]
    fn glare_first_copy_keeps_screen_before_linear_dodge() {
        let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
        let source=gpu.fill(3,3,[0.01,0.02,0.03,1.]);
        let renderer=Renderer::new(gpu.clone(),gpu.copy(&source),Depth::Sixteen);
        let p=Params{hdr32:false,colorize:false,glare_rays:GlareRays::Two,
            glare_length:0.01,glare_rotation:0.,glare_opacity:100.,main_opacity:100.,
            glare_detail:0.,glare_sharpness:0.,glare_gamma:1.,..Params::default()};
        let d=(2.*p.glare_length*100.*renderer.geometry.radius_scale(RenderMode::Hq)).clamp(1.,2000.);
        let mut expected=gpu.copy(&source);
        for mode in [B::Screen,B::LinearDodge,B::LinearDodge,B::LinearDodge] {
            expected=gpu.quantize(&gpu.motion_blur(&expected,0.,d,Depth::Sixteen),Depth::Sixteen);
            let copy=gpu.quantize(&gpu.motion_blur(&expected,0.,d,Depth::Sixteen),Depth::Sixteen);
            expected=gpu.quantize(&gpu.blend(&expected,&copy,mode,1.,1.,Depth::Sixteen),Depth::Sixteen);
        }
        expected=gpu.quantize(&gpu.motion_blur(&expected,0.,d,Depth::Sixteen),Depth::Sixteen);
        let expected=gpu.quantize(&gpu.blend(&gpu.fill(3,3,[0.;4]),&expected,B::LinearDodge,1.,1.,Depth::Sixteen),Depth::Sixteen);
        let actual=renderer.glare(&source,&p,RenderMode::Hq,Depth::Sixteen);
        assert_eq!(gpu.download_rgba32f(&actual),gpu.download_rgba32f(&expected));
    }
}
