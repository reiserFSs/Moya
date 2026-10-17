use std::{collections::{HashMap, VecDeque}, path::Path, time::{Duration, Instant}};
use eframe::egui::{self, Color32, TextureHandle};
use moya_gpu::{Gpu, Image};
use moya_io::Picture;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool { None, Brush, Eraser }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine { Replace, Add, Subtract, Intersect }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskView { Overlay, MaskOnly, Off }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskKind { Luminosity, Transparency, Outlines }
pub struct BrushSettings { pub size: f32, pub hardness: f32, pub opacity: f32 }
pub enum MaskSync { Unchanged, Painting, Committed }
#[derive(Clone, Copy)]
struct Change { index: u32, before: u16, after: u16 }
enum HistoryEntry { Diff(Vec<Change>), Snapshot(Vec<u16>) }
impl HistoryEntry {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + match self {
            Self::Diff(changes) => changes.len() * std::mem::size_of::<Change>(),
            Self::Snapshot(values) => values.len() * std::mem::size_of::<u16>(),
        }
    }
}
struct Stroke { erase: bool, last: Option<[f32; 2]>, pixels: HashMap<u32, (f32, f32)> }

pub struct Mask {
    pub enabled: bool, pub tool: Tool, pub brush: BrushSettings, pub view: MaskView,
    pub overlay_opacity: f32, pub combine: Combine, pub lum_range: [f32; 2],
    pub feather: f32, pub grow: f32,
    w: u32, h: u32, data: Vec<f32>, sum: f64, dirty: Option<[u32; 4]>,
    image: Option<Image>, overlay: Option<TextureHandle>, thumb: Option<TextureHandle>,
    stroke: Option<Stroke>, past: VecDeque<HistoryEntry>, future: Vec<HistoryEntry>,
    history_bytes: usize, history_limit: usize, steps: usize, evicted: usize,
    committed: bool, preview_pending: bool, last_preview: Instant,
    appearance: (MaskView, f32), was_enabled: bool,
}
impl Default for Mask {
    fn default() -> Self {
        Self { enabled: false, tool: Tool::None, brush: BrushSettings { size: 30.0, hardness: 0.75, opacity: 1.0 }, view: MaskView::Overlay, overlay_opacity: 0.4, combine: Combine::Replace, lum_range: [0.0, 1.0], feather: 5.0, grow: 5.0,
            w: 0, h: 0, data: Vec::new(), sum: 0.0, dirty: None, image: None, overlay: None, thumb: None,
            stroke: None, past: VecDeque::new(), future: Vec::new(), history_bytes: 0, history_limit: 256 * 1024 * 1024, steps: 0, evicted: 0,
            committed: false, preview_pending: false, last_preview: Instant::now() - Duration::from_secs(1), appearance: (MaskView::Overlay, 0.4), was_enabled: false }
    }
}
impl Mask {
    pub fn reset(&mut self, w: u32, h: u32) {
        *self = Self::default(); self.w = w; self.h = h;
        self.data = vec![0.0; w as usize * h as usize]; self.mark_all(); self.committed = true;
    }
    fn mark_all(&mut self) { if self.w > 0 && self.h > 0 { self.dirty = Some([0, 0, self.w, self.h]); } }
    fn mark(&mut self, x: u32, y: u32) {
        self.dirty = Some(match self.dirty { Some([a,b,c,d]) => [a.min(x), b.min(y), c.max(x+1), d.max(y+1)], None => [x,y,x+1,y+1] });
    }
    fn put(&mut self, index: usize, value: f32) {
        // Store the same 16-bit precision as history, so undo restores exact values, including zero.
        let value = quantize(value) as f32 / 65535.0;
        if self.data[index] != value { self.sum += value as f64 - self.data[index] as f64; self.data[index] = value; self.mark(index as u32 % self.w, index as u32 / self.w); }
    }
    pub fn begin_stroke(&mut self, erase: bool) {
        if let Some(stroke) = &mut self.stroke { stroke.erase = erase; }
        else { self.stroke = Some(Stroke { erase, last: None, pixels: HashMap::new() }); }
    }
    pub fn stroke_to(&mut self, xy: [f32; 2]) {
        if self.stroke.is_none() || xy.iter().any(|v| !v.is_finite()) || !self.brush.size.is_finite() || self.brush.size <= 0.0 { return; }
        let mut stroke = self.stroke.take().unwrap();
        let from = stroke.last.unwrap_or(xy);
        let distance = (xy[0]-from[0]).hypot(xy[1]-from[1]);
        let count = (distance / (self.brush.size * 0.25).max(0.5)).ceil().max(1.0) as usize;
        for n in 1..=count {
            let t = n as f32 / count as f32;
            let center = [from[0] + (xy[0]-from[0])*t, from[1] + (xy[1]-from[1])*t];
            self.dab(&mut stroke, center);
        }
        stroke.last = Some(xy); self.stroke = Some(stroke);
    }
    fn dab(&mut self, stroke: &mut Stroke, center: [f32; 2]) {
        let radius = self.brush.size;
        let x0 = (center[0]-radius).floor().clamp(0.0, self.w as f32) as u32;
        let y0 = (center[1]-radius).floor().clamp(0.0, self.h as f32) as u32;
        let x1 = (center[0]+radius).ceil().clamp(0.0, self.w as f32) as u32;
        let y1 = (center[1]+radius).ceil().clamp(0.0, self.h as f32) as u32;
        for y in y0..y1 { for x in x0..x1 {
            let d = (x as f32+0.5-center[0]).hypot(y as f32+0.5-center[1]) / radius;
            let coverage = falloff(d, self.brush.hardness) * self.brush.opacity.clamp(0.0, 1.0);
            if coverage <= 0.0 { continue; }
            let index = y*self.w+x;
            let entry = stroke.pixels.entry(index).or_insert((self.data[index as usize], 0.0));
            entry.1 = entry.1.max(coverage);
            let value = if stroke.erase { entry.0 * (1.0-entry.1) } else { entry.0 + (1.0-entry.0)*entry.1 };
            self.put(index as usize, value);
        }}
        self.preview_pending |= self.dirty.is_some();
    }
    pub fn end_stroke(&mut self) {
        let Some(stroke) = self.stroke.take() else { return; };
        let changes = stroke.pixels.into_iter().filter_map(|(index,(before,_))| {
            let (before, after) = (quantize(before), quantize(self.data[index as usize]));
            (before != after).then_some(Change { index, before, after })
        }).collect::<Vec<_>>();
        if !changes.is_empty() { self.commit(HistoryEntry::Diff(changes)); }
    }
    fn commit(&mut self, entry: HistoryEntry) {
        self.clear_redo(); self.history_bytes += entry.bytes();
        self.past.push_back(entry); self.steps += 1;
        // Keep the newest edit undoable even when it alone exceeds the budget.
        while self.past.len() > 1 && (self.history_bytes > self.history_limit || self.past.len() > 200) {
            let old = self.past.pop_front().unwrap();
            self.history_bytes -= old.bytes(); self.evicted += 1;
        }
        self.committed = true;
    }
    fn replace(&mut self, values: Vec<f32>) {
        self.end_stroke();
        assert_eq!(values.len(), self.data.len());
        let count = values.iter().zip(&self.data).filter(|(after, before)| quantize(**after) != quantize(**before)).count();
        if count == 0 { return; }
        let entry = if self.data.len() * std::mem::size_of::<u16>() < count * std::mem::size_of::<Change>() {
            HistoryEntry::Snapshot(self.data.iter().map(|&v| quantize(v)).collect())
        } else {
            HistoryEntry::Diff(values.iter().zip(&self.data).enumerate().filter_map(|(index, (&after, &before))| {
                let (before, after) = (quantize(before), quantize(after));
                (before != after).then_some(Change { index: index as u32, before, after })
            }).collect())
        };
        for (index, value) in values.into_iter().enumerate() { self.put(index, value); }
        self.commit(entry);
    }
    fn combine_values(&mut self, values: Vec<f32>) {
        let values = values.into_iter().zip(&self.data).map(|(v,&old)| combine(old,v,self.combine)).collect(); self.replace(values); self.enabled = true;
    }
    pub fn build(&mut self, pic: &Picture, kind: MaskKind) {
        let mut values = vec![0.0; pic.width as usize * pic.height as usize];
        for y in 0..pic.height { for x in 0..pic.width {
            let i = (y*pic.width+x) as usize; let p = &pic.rgba[i*4..i*4+4];
            values[i] = match kind {
                MaskKind::Luminosity => luminosity(p[0].max(p[1]).max(p[2]), self.lum_range) * p[3],
                MaskKind::Transparency => p[3],
                MaskKind::Outlines => {
                    let mut lo = p[3]; let mut hi = p[3];
                    for (dx,dy) in [(0,-1),(-1,0),(1,0),(0,1)] {
                        let (nx,ny) = (x as i64+dx,y as i64+dy);
                        let a = if nx>=0 && ny>=0 && nx<pic.width as i64 && ny<pic.height as i64 { pic.rgba[((ny as u32*pic.width+nx as u32)*4+3) as usize] } else { 0.0 };
                        lo=lo.min(a); hi=hi.max(a);
                    } hi-lo
                }
            }.clamp(0.0,1.0);
        }}
        self.combine_values(resample(&values, pic.width, pic.height, self.w, self.h));
    }
    pub fn load_image(&mut self, pic: &Picture) {
        let gray: Vec<f32> = pic.rgba.chunks_exact(4).map(|p| (0.2126*p[0]+0.7152*p[1]+0.0722*p[2])*p[3]).collect();
        self.combine_values(resample(&gray,pic.width,pic.height,self.w,self.h));
    }
    pub fn invert(&mut self) { self.replace(self.data.iter().map(|v| 1.0-v).collect()); }
    pub fn clear(&mut self) { self.replace(vec![0.0; self.data.len()]); }
    pub fn apply_feather(&mut self, gpu: &Gpu) {
        if self.feather <= 0.0 || self.data.is_empty() { return; }
        let image = gpu.upload_gray32f(self.w,self.h,&self.data);
        let blurred = gpu.gaussian_blur(&image,self.feather);
        self.replace(gpu.download_rgba32f(&blurred).chunks_exact(4).map(|p| p[0]).collect());
    }
    pub fn apply_grow(&mut self, _gpu: &Gpu) {
        // Square grayscale morphology, separable exact max/min; fractional radii blend adjacent radii.
        let radius = self.grow.abs(); if radius == 0.0 || self.data.is_empty() { return; }
        let lo = radius.floor() as usize;
        let a = morphology(&self.data,self.w as usize,self.h as usize,lo,self.grow>0.0);
        let result = if radius.fract() > 0.0 {
            let b = morphology(&self.data,self.w as usize,self.h as usize,lo+1,self.grow>0.0);
            a.into_iter().zip(b).map(|(a,b)| a+(b-a)*radius.fract()).collect()
        } else { a };
        self.replace(result);
    }
    fn restore(&mut self, entry: &mut HistoryEntry, redo: bool) {
        match entry {
            HistoryEntry::Diff(changes) => for c in changes {
                self.put(c.index as usize, if redo { c.after } else { c.before } as f32 / 65535.0);
            },
            // A snapshot holds the opposite state; swapping supports both undo and redo.
            HistoryEntry::Snapshot(values) => for (index, value) in values.iter_mut().enumerate() {
                let current = quantize(self.data[index]);
                self.put(index, *value as f32 / 65535.0);
                *value = current;
            },
        }
    }
    pub fn undo(&mut self) -> bool {
        self.end_stroke(); let Some(mut entry) = self.past.pop_back() else { return false; };
        self.restore(&mut entry, false);
        self.future.push(entry); self.committed=true; true
    }
    pub fn redo(&mut self) -> bool {
        let Some(mut entry) = self.future.pop() else { return false; };
        self.restore(&mut entry, true);
        self.past.push_back(entry); self.committed=true; true
    }
    pub fn clear_redo(&mut self) { for entry in self.future.drain(..) { self.history_bytes -= entry.bytes(); } }
    pub fn take_history_steps(&mut self) -> (usize,usize) { (std::mem::take(&mut self.steps),std::mem::take(&mut self.evicted)) }
    pub fn coverage(&self) -> f32 { if self.data.is_empty() { 0.0 } else { (self.sum/self.data.len() as f64).clamp(0.0,1.0) as f32 } }
    pub fn is_empty(&self) -> bool { self.sum <= 0.0 }
    pub fn save_png(&self, path: &Path) -> anyhow::Result<()> {
        let gray: Vec<u8> = self.data.iter().map(|v| (v.clamp(0.0,1.0)*255.0).round() as u8).collect();
        moya_io::save_gray_png(path,self.w,self.h,&gray)
    }
    pub fn gpu_image(&self) -> Option<&Image> { if self.enabled && !self.is_empty() { self.image.as_ref() } else { None } }
    pub fn overlay_texture(&self) -> Option<&TextureHandle> { self.overlay.as_ref() }
    pub fn thumbnail(&self) -> Option<&TextureHandle> { self.thumb.as_ref() }
    pub fn sync(&mut self, ctx: &egui::Context, gpu: &Gpu) -> MaskSync {
        let appearance_changed = self.appearance != (self.view,self.overlay_opacity);
        let enabled_changed = self.was_enabled != self.enabled;
        self.was_enabled = self.enabled;
        let dirty = self.dirty.take();
        if appearance_changed { self.appearance = (self.view,self.overlay_opacity); }
        if let Some(rect) = if appearance_changed { Some([0,0,self.w,self.h]) } else { dirty } {
            self.upload(ctx,gpu,rect,dirty.is_some());
            self.preview_pending |= dirty.is_some();
        }
        if self.committed || enabled_changed { self.committed=false; self.preview_pending=false; self.last_preview=Instant::now(); return MaskSync::Committed; }
        if self.preview_pending {
            let delay = Duration::from_millis(63);
            if self.last_preview.elapsed() >= delay { self.preview_pending=false; self.last_preview=Instant::now(); return MaskSync::Painting; }
            ctx.request_repaint_after(delay.saturating_sub(self.last_preview.elapsed()));
        }
        MaskSync::Unchanged
    }
    fn upload(&mut self, ctx: &egui::Context, gpu: &Gpu, mut rect: [u32;4], pixels_changed: bool) {
        if self.w==0 || self.h==0 { return; }
        if self.image.is_none() { rect=[0,0,self.w,self.h]; }
        let [x0,y0,x1,y1]=rect;
        if pixels_changed || self.image.is_none() {
            if self.image.is_none() || rect == [0,0,self.w,self.h] {
                self.image=Some(gpu.upload_gray32f(self.w,self.h,&self.data));
            } else if let Some(image)=&self.image {
                let mut rgba=Vec::with_capacity(((x1-x0)*(y1-y0)*4) as usize);
                for y in y0..y1 { for x in x0..x1 { let v=self.data[(y*self.w+x) as usize]; rgba.extend_from_slice(&[v,v,v,1.0]); }}
                gpu.write_region(image,x0,y0,x1-x0,y1-y0,&rgba);
            }
        }
        // Cap egui overlay to 2048px long side; GPU mask and editing retain full resolution.
        let scale=(2048.0/self.w.max(self.h) as f32).min(1.0);
        let (ow,oh)=(((self.w as f32*scale).round() as u32).max(1),((self.h as f32*scale).round() as u32).max(1));
        let full=self.overlay.is_none();
        let (a,b,c,d)=if full { (0,0,ow,oh) } else {
            (((x0 as f32*ow as f32/self.w as f32).floor() as u32).saturating_sub(1),((y0 as f32*oh as f32/self.h as f32).floor() as u32).saturating_sub(1),
             ((x1 as f32*ow as f32/self.w as f32).ceil() as u32+1).min(ow),((y1 as f32*oh as f32/self.h as f32).ceil() as u32+1).min(oh)) };
        let mut pixels=Vec::with_capacity(((c-a)*(d-b)) as usize);
        for y in b..d { for x in a..c {
            let v=sample(&self.data,self.w,self.h,(x as f32+0.5)*self.w as f32/ow as f32-0.5,(y as f32+0.5)*self.h as f32/oh as f32-0.5);
            pixels.push(if self.view==MaskView::MaskOnly { Color32::from_gray((v*255.0).round() as u8) } else { Color32::from_rgba_unmultiplied(255,0,190,(v*self.overlay_opacity.clamp(0.0,1.0)*255.0).round() as u8) });
        }}
        let image=egui::ColorImage::new([(c-a) as usize,(d-b) as usize],pixels);
        if let Some(texture)=&mut self.overlay { texture.set_partial([a as usize,b as usize],image,egui::TextureOptions::LINEAR); }
        else { self.overlay=Some(ctx.load_texture("Mask overlay",image,egui::TextureOptions::LINEAR)); }
        let (tw,th)=((self.w as f32*96.0/self.w.max(self.h) as f32).round().max(1.0) as u32,(self.h as f32*96.0/self.w.max(self.h) as f32).round().max(1.0) as u32);
        let pixels=resample(&self.data,self.w,self.h,tw,th).into_iter().map(|v| Color32::from_gray((v*255.0).round() as u8)).collect();
        let image=egui::ColorImage::new([tw as usize,th as usize],pixels);
        if let Some(t)=&mut self.thumb { t.set(image,egui::TextureOptions::LINEAR); } else { self.thumb=Some(ctx.load_texture("Mask thumbnail",image,egui::TextureOptions::LINEAR)); }
    }
}
fn quantize(v:f32)->u16 { (v.clamp(0.0,1.0)*65535.0).round() as u16 }
fn falloff(d:f32,hardness:f32)->f32 { let h=hardness.clamp(0.0,1.0); if d>=1.0 { 0.0 } else if d<=h { 1.0 } else { let t=(d-h)/(1.0-h); 1.0-t*t*(3.0-2.0*t) } }
// Linear ramp: the default [0, 1] range leaves max(R,G,B) unchanged.
fn luminosity(v:f32,range:[f32;2])->f32 { let lo=range[0].min(range[1]); let hi=range[0].max(range[1]); if hi<=lo { return if v>=hi {1.0} else {0.0}; } ((v-lo)/(hi-lo)).clamp(0.0,1.0) }
fn combine(old:f32,new:f32,mode:Combine)->f32 { match mode { Combine::Replace=>new,Combine::Add=>old+new,Combine::Subtract=>old-new,Combine::Intersect=>old.min(new) }.clamp(0.0,1.0) }
fn sample(data:&[f32],w:u32,h:u32,x:f32,y:f32)->f32 {
    let x=x.clamp(0.0,(w-1) as f32); let y=y.clamp(0.0,(h-1) as f32);
    let (a,b)=(x.floor() as u32,y.floor() as u32); let (c,d)=((a+1).min(w-1),(b+1).min(h-1));
    let (tx,ty)=(x-a as f32,y-b as f32); let p=|x,y| data[(y*w+x) as usize];
    (p(a,b)*(1.0-tx)+p(c,b)*tx)*(1.0-ty)+(p(a,d)*(1.0-tx)+p(c,d)*tx)*ty
}
fn resample(data:&[f32],w:u32,h:u32,nw:u32,nh:u32)->Vec<f32> {
    if w==nw && h==nh { return data.to_vec(); }
    if w==0 || h==0 { return vec![0.0;nw as usize*nh as usize]; }
    (0..nh).flat_map(|y| (0..nw).map(move |x| sample(data,w,h,(x as f32+0.5)*w as f32/nw as f32-0.5,(y as f32+0.5)*h as f32/nh as f32-0.5))).collect()
}
fn morphology(data:&[f32],w:usize,h:usize,radius:usize,grow:bool)->Vec<f32> {
    fn pass(src:&[f32],dst:&mut[f32],length:usize,lines:usize,stride:usize,line_stride:usize,r:usize,grow:bool) {
        for line in 0..lines { let mut deque:VecDeque<usize>=VecDeque::new(); let mut next=0;
            for i in 0..length { let end=i.saturating_add(r).min(length-1);
                while next<=end { let value=src[line*line_stride+next*stride]; while deque.back().is_some_and(|&j| if grow {src[line*line_stride+j*stride]<=value} else {src[line*line_stride+j*stride]>=value}) {deque.pop_back();} deque.push_back(next); next+=1; }
                while deque.front().is_some_and(|&j| j<i.saturating_sub(r)) {deque.pop_front();}
                dst[line*line_stride+i*stride]=src[line*line_stride+deque[0]*stride];
            }
        }
    }
    let mut tmp=vec![0.0;data.len()]; let mut out=vec![0.0;data.len()];
    pass(data,&mut tmp,w,h,1,w,radius,grow); pass(&tmp,&mut out,h,w,w,1,radius,grow); out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn brush_falloff_and_opacity_cap() {
        assert_eq!(falloff(0.2,0.5),1.0); assert_eq!(falloff(1.0,0.5),0.0); assert!((falloff(0.75,0.5)-0.5).abs()<1e-6);
        let mut m=Mask::default(); m.reset(16,16); m.brush.size=5.0; m.brush.hardness=1.0; m.brush.opacity=0.4;
        m.begin_stroke(false); for _ in 0..10 {m.stroke_to([8.5,8.5]);} m.end_stroke();
        assert!((m.data[8*16+8]-0.4).abs()<1e-6); assert!(m.undo()); assert!(m.is_empty()); assert!(m.redo()); assert!((m.data[8*16+8]-0.4).abs()<2e-5);
    }
    #[test] fn combination_and_luminosity_range() {
        assert_eq!(combine(0.6,0.5,Combine::Replace),0.5); assert_eq!(combine(0.6,0.5,Combine::Add),1.0);
        assert!((combine(0.6,0.5,Combine::Subtract)-0.1).abs()<1e-6); assert_eq!(combine(0.6,0.5,Combine::Intersect),0.5);
        assert_eq!(luminosity(0.1,[0.2,0.8]),0.0); assert!((luminosity(0.35,[0.2,0.8])-0.25).abs()<1e-6); assert_eq!(luminosity(0.9,[0.2,0.8]),1.0);
    }
    #[test] fn memory_cap_evicts_oldest_and_keeps_redo_bounded() {
        let mut m=Mask::default();m.reset(2,1);m.history_limit=std::mem::size_of::<HistoryEntry>() + 2*std::mem::size_of::<u16>();
        m.replace(vec![1.0,0.0]);m.replace(vec![1.0,1.0]); assert_eq!(m.past.len(),1);assert_eq!(m.evicted,1);
        assert!(m.undo());assert_eq!(m.data,vec![1.0,0.0]);assert!(!m.undo());assert!(m.redo());assert!(m.history_bytes<=m.history_limit);
    }
    #[test] fn history_bytes_ignore_spare_capacity_and_clear_redo_exactly() {
        let mut m=Mask::default();m.reset(8,1);
        let mut changes=Vec::with_capacity(128);
        changes.push(Change { index:0,before:0,after:65535 });
        m.put(0,1.0);m.commit(HistoryEntry::Diff(changes));
        let bytes=std::mem::size_of::<HistoryEntry>()+std::mem::size_of::<Change>();
        assert_eq!(m.history_bytes,bytes);
        assert!(m.undo());assert_eq!(m.history_bytes,bytes);
        m.clear_redo();assert_eq!(m.history_bytes,0);assert!(!m.redo());
    }
    #[test] fn newest_oversized_snapshot_is_retained_and_swaps_exactly() {
        let mut m=Mask::default();m.reset(8,1);m.history_limit=1;
        m.replace(vec![0.25;8]);
        let before=m.data.clone();m.invert();let after=m.data.clone();
        assert_eq!(m.past.len(),1);assert_eq!(m.take_history_steps(),(2,1));
        assert!(matches!(m.past.back(),Some(HistoryEntry::Snapshot(values)) if values.len()==8));
        assert_eq!(m.history_bytes,std::mem::size_of::<HistoryEntry>()+16);
        assert!(m.history_bytes>m.history_limit);
        assert!(m.undo());assert_eq!(m.data,before);assert!(!m.undo());
        assert!(m.redo());assert_eq!(m.data,after);
    }
    #[test] fn sparse_replacement_chooses_diff_and_new_edit_discards_snapshot_redo() {
        let mut m=Mask::default();m.reset(8,1);
        let mut values=vec![0.0;8];values[3]=0.5;m.replace(values);
        assert!(matches!(m.past.back(),Some(HistoryEntry::Diff(changes)) if changes.len()==1));
        let before=m.data.clone();m.invert();assert!(m.undo());assert_eq!(m.data,before);
        assert!(matches!(m.future.last(),Some(HistoryEntry::Snapshot(_))));
        m.clear();
        assert!(m.future.is_empty());assert!(!m.redo());
        assert_eq!(m.history_bytes,m.past.iter().map(HistoryEntry::bytes).sum::<usize>());
        assert!(m.undo());assert_eq!(m.data,before);
    }
    #[test] fn built_masks_and_clear_preserve_zero() {
        let pic=Picture { width:2,height:1,depth:moya_io::SampleDepth::U8,rgba:vec![0.2,0.8,0.4,0.5,1.0,0.0,0.0,0.0],icc_profile:None };
        let mut m=Mask::default();m.reset(2,1);
        m.build(&pic,MaskKind::Transparency);assert!((m.data[0]-0.5).abs()<2e-5);assert_eq!(m.data[1],0.0);
        // Default range: max(R,G,B) * alpha.
        m.build(&pic,MaskKind::Luminosity);assert!((m.data[0]-0.8*0.5).abs()<2e-5);
        m.replace(vec![1e-7,0.5]);m.clear();assert!(m.is_empty());assert_eq!(m.data,vec![0.0;2]);
        m.build(&pic,MaskKind::Outlines);assert!((m.data[0]-0.5).abs()<2e-5);assert!((m.data[1]-0.5).abs()<2e-5);
    }
    #[test] fn multi_dab_stroke_over_built_mask_undoes_once() {
        let mut m=Mask::default();m.reset(64,64);m.brush.size=3.0;
        m.replace((0..4096).map(|i| if i>3000 {0.5} else {0.0}).collect());
        let before=m.data.clone();m.take_history_steps();
        for x in 10..40 { m.begin_stroke(false);m.stroke_to([x as f32,20.0]); }
        m.end_stroke();assert_eq!(m.take_history_steps().0,1);
        assert!(m.undo());assert_eq!(m.data,before);assert!(m.redo());assert_ne!(m.data,before);
    }
    #[test] fn morphology_grows_and_contracts() { assert_eq!(morphology(&[0.0,1.0,0.0],3,1,1,true),vec![1.0;3]);assert_eq!(morphology(&[1.0,0.0,1.0],3,1,1,false),vec![0.0;3]); }
}
