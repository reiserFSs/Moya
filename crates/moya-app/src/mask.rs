use std::{collections::VecDeque, path::Path, time::{Duration, Instant}};
use eframe::egui::{self, Color32, TextureHandle};
use moya_gpu::{Gpu, Image, MaskSourceKind};
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
/// Stroke-local undo record in 64×64 tiles: each touched pixel's value before the stroke and
/// its maximum coverage so far (0 = untouched). Indexed directly, no per-pixel hashing.
const TILE: u32 = 64;
struct Tile { before: [u16; (TILE * TILE) as usize], cover: [f32; (TILE * TILE) as usize] }
/// `mode_changed`: brush/eraser was toggled mid-stroke (Alt), so revisited pixels must be
/// recomputed even when their coverage doesn't grow.
struct Stroke { erase: bool, mode_changed: bool, last: Option<[f32; 2]>, tiles_x: u32, tiles: Vec<Option<Box<Tile>>> }

/// Editable pixels and undo share exact normalized u16 values; float conversion is transient.
pub struct Mask {
    pub enabled: bool, pub tool: Tool, pub brush: BrushSettings, pub view: MaskView,
    pub overlay_opacity: f32, pub combine: Combine, pub lum_range: [f32; 2],
    pub feather: f32, pub grow: f32,
    w: u32, h: u32, data: Vec<u16>, sum: u64, fingerprint: u64, dirty: Option<[u32; 4]>,
    image: Option<Image>, overlay: Option<TextureHandle>, thumb: Option<TextureHandle>, gpu_generation: u64,
    stroke: Option<Stroke>, past: VecDeque<HistoryEntry>, future: Vec<HistoryEntry>,
    history_bytes: usize, history_limit: usize, steps: usize, evicted: usize,
    committed: bool, preview_pending: bool, last_preview: Instant, last_dab: Instant,
    appearance: (MaskView, f32), was_enabled: bool,
}
impl Default for Mask {
    fn default() -> Self {
        Self { enabled: false, tool: Tool::None, brush: BrushSettings { size: 30.0, hardness: 0.75, opacity: 1.0 }, view: MaskView::Overlay, overlay_opacity: 0.4, combine: Combine::Replace, lum_range: [0.0, 1.0], feather: 5.0, grow: 5.0,
            w: 0, h: 0, data: Vec::new(), sum: 0, fingerprint: 0, dirty: None, image: None, overlay: None, thumb: None, gpu_generation: 0,
            stroke: None, past: VecDeque::new(), future: Vec::new(), history_bytes: 0, history_limit: 256 * 1024 * 1024, steps: 0, evicted: 0,
            committed: false, preview_pending: false, last_preview: Instant::now() - Duration::from_secs(1), last_dab: Instant::now(), appearance: (MaskView::Overlay, 0.4), was_enabled: false }
    }
}
impl Mask {
    pub fn reset(&mut self, w: u32, h: u32) {
        let generation = self.gpu_generation;
        *self = Self::default(); self.w = w; self.h = h; self.gpu_generation = generation;
        self.data = vec![0; w as usize * h as usize]; self.mark_all(); self.committed = true;
    }
    fn mark_all(&mut self) { if self.w > 0 && self.h > 0 { self.dirty = Some([0, 0, self.w, self.h]); } }
    fn mark(&mut self, x: u32, y: u32) {
        self.dirty = Some(match self.dirty { Some([a,b,c,d]) => [a.min(x), b.min(y), c.max(x+1), d.max(y+1)], None => [x,y,x+1,y+1] });
    }
    fn put(&mut self, index: usize, value: u16) {
        if self.data[index] != value {
            self.fingerprint ^= pixel_hash(index, self.data[index]) ^ pixel_hash(index, value);
            self.sum = self.sum - u64::from(self.data[index]) + u64::from(value);
            self.data[index] = value; self.mark(index as u32 % self.w, index as u32 / self.w);
        }
    }
    pub fn begin_stroke(&mut self, erase: bool) {
        if let Some(stroke) = &mut self.stroke {
            stroke.mode_changed |= stroke.erase != erase;
            stroke.erase = erase;
        } else {
            let tiles_x = self.w.div_ceil(TILE);
            let tiles = (tiles_x * self.h.div_ceil(TILE)) as usize;
            self.stroke = Some(Stroke { erase, mode_changed: false, last: None, tiles_x, tiles: (0..tiles).map(|_| None).collect() });
        }
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
        let opacity = self.brush.opacity.clamp(0.0, 1.0);
        for y in y0..y1 {
            let dy = y as f32+0.5-center[1];
            let tile_row = (y / TILE) * stroke.tiles_x;
            let slot_row = (y % TILE) * TILE;
            for x in x0..x1 {
                let d = (x as f32+0.5-center[0]).hypot(dy) / radius;
                let coverage = falloff(d, self.brush.hardness) * opacity;
                if coverage <= 0.0 { continue; }
                let tile = stroke.tiles[(tile_row + x / TILE) as usize]
                    .get_or_insert_with(|| Box::new(Tile { before: [0; (TILE * TILE) as usize], cover: [0.0; (TILE * TILE) as usize] }));
                let slot = (slot_row + x % TILE) as usize;
                let index = (y*self.w+x) as usize;
                if tile.cover[slot] == 0.0 { tile.before[slot] = self.data[index]; }
                // Max-blend: lower coverage on an already painted pixel can't change it.
                else if coverage <= tile.cover[slot] && !stroke.mode_changed { continue; }
                tile.cover[slot] = tile.cover[slot].max(coverage);
                let before = tile.before[slot] as f32 / 65535.0;
                let value = if stroke.erase { before * (1.0-tile.cover[slot]) } else { before + (1.0-before)*tile.cover[slot] };
                self.put(index, quantize(value));
            }
        }
        self.preview_pending |= self.dirty.is_some();
        self.last_dab = Instant::now();
    }
    pub fn end_stroke(&mut self) {
        let Some(stroke) = self.stroke.take() else { return; };
        let mut changes = Vec::new();
        for (t, tile) in stroke.tiles.into_iter().enumerate() {
            let Some(tile) = tile else { continue; };
            let (tx, ty) = (t as u32 % stroke.tiles_x * TILE, t as u32 / stroke.tiles_x * TILE);
            for (slot, &cover) in tile.cover.iter().enumerate() {
                if cover == 0.0 { continue; }
                let index = (ty + slot as u32 / TILE) * self.w + tx + slot as u32 % TILE;
                let (before, after) = (tile.before[slot], self.data[index as usize]);
                if before != after { changes.push(Change { index, before, after }); }
            }
        }
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
    fn replace(&mut self, values: Vec<u16>) {
        self.end_stroke();
        assert_eq!(values.len(), self.data.len());
        let count = values.iter().zip(&self.data).filter(|(after, before)| after != before).count();
        if count == 0 { return; }
        let entry = if self.data.len() * std::mem::size_of::<u16>() < count * std::mem::size_of::<Change>() {
            HistoryEntry::Snapshot(self.data.clone())
        } else {
            HistoryEntry::Diff(values.iter().zip(&self.data).enumerate().filter_map(|(index, (&after, &before))| {
                (before != after).then_some(Change { index: index as u32, before, after })
            }).collect())
        };
        for (index, value) in values.into_iter().enumerate() { self.put(index, value); }
        self.commit(entry);
    }
    fn combine_values(&mut self, mut values: Vec<u16>) {
        for (value, &old) in values.iter_mut().zip(&self.data) {
            *value = match self.combine {
                Combine::Replace => *value, Combine::Add => old.saturating_add(*value),
                Combine::Subtract => old.saturating_sub(*value), Combine::Intersect => old.min(*value),
            };
        }
        self.replace(values); self.enabled = true;
    }
    fn combine_float_values(&mut self, values: Vec<f32>) {
        let values=values.into_iter().zip(&self.data).map(|(value,&old)| {
            quantize(combine(old as f32/65535.0,value,self.combine))
        }).collect();
        self.replace(values);self.enabled=true;
    }
    pub fn build(&mut self, gpu: &Gpu, source: &Image, kind: MaskKind) {
        let kind = match kind {
            MaskKind::Luminosity => MaskSourceKind::Luminosity { low: self.lum_range[0], high: self.lum_range[1] },
            MaskKind::Transparency => MaskSourceKind::Transparency,
            MaskKind::Outlines => MaskSourceKind::Outlines,
        };
        let image = gpu.mask_from_source(source, kind);
        let values = gpu.download_gray_u16(&image);
        if (source.width(), source.height()) == (self.w, self.h) { self.combine_values(values); }
        else { self.combine_float_values(resample(&values, source.width(), source.height(), self.w, self.h,65535.0)); }
    }
    pub fn load_image(&mut self, pic: &Picture) {
        let gray: Vec<f32> = pic.rgba.chunks_exact(4).map(|p| (0.2126*p[0]+0.7152*p[1]+0.0722*p[2])*p[3]).collect();
        self.combine_float_values(resample(&gray,pic.width,pic.height,self.w,self.h,1.0));
    }
    pub fn invert(&mut self) { self.replace(self.data.iter().map(|v| u16::MAX-v).collect()); }
    pub fn clear(&mut self) { self.replace(vec![0; self.data.len()]); }
    pub fn apply_feather(&mut self, gpu: &Gpu) {
        if self.feather <= 0.0 || self.is_empty() { return; }
        let image = gpu.upload_gray_u16(self.w,self.h,&self.data);
        let blurred = gpu.gaussian_blur(&image,self.feather);
        self.replace(gpu.download_gray_u16(&blurred));
    }
    pub fn apply_grow(&mut self, _gpu: &Gpu) {
        // Square grayscale morphology, separable exact max/min; fractional radii blend adjacent radii.
        let radius = self.grow.abs(); if radius == 0.0 || self.data.is_empty() { return; }
        let lo = radius.floor() as usize;
        let a = morphology(&self.data,self.w as usize,self.h as usize,lo,self.grow>0.0);
        let result = if radius.fract() > 0.0 {
            let b = morphology(&self.data,self.w as usize,self.h as usize,lo+1,self.grow>0.0);
            a.into_iter().zip(b).map(|(a,b)| {
                let (a,b)=(a as f32/65535.0,b as f32/65535.0);
                quantize(a+(b-a)*radius.fract())
            }).collect()
        } else { a };
        self.replace(result);
    }
    fn restore(&mut self, entry: &mut HistoryEntry, redo: bool) {
        match entry {
            HistoryEntry::Diff(changes) => for c in changes {
                self.put(c.index as usize, if redo { c.after } else { c.before });
            },
            // A snapshot holds the opposite state; swapping supports both undo and redo.
            HistoryEntry::Snapshot(values) => for (index, value) in values.iter_mut().enumerate() {
                let current = self.data[index];
                self.put(index, *value);
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
    pub fn fingerprint(&self) -> (u32, u32, bool, u64) { (self.w, self.h, self.enabled, self.fingerprint) }
    pub fn generation(&self) -> u64 {
        self.gpu_generation ^ if self.enabled { u64::MAX } else { 0 }
    }
    pub fn drop_farthest_redo(&mut self) {
        if !self.future.is_empty() { self.history_bytes -= self.future.remove(0).bytes(); }
    }
    pub fn history_bytes(&self) -> usize { self.history_bytes }
    pub fn retained_bytes(&self) -> usize { self.data.capacity() * std::mem::size_of::<u16>() + self.history_bytes }
    pub fn release_gpu(&mut self) { self.image = None; self.overlay = None; self.thumb = None; self.mark_all(); }
    pub fn drop_oldest_history(&mut self) -> bool {
        let Some(entry) = self.past.pop_front() else { return false; };
        self.history_bytes -= entry.bytes(); true
    }
    pub fn export(&self) -> Option<moya_project::MaskData> {
        (self.w > 0 && self.h > 0 && (self.enabled || !self.is_empty())).then(|| moya_project::MaskData {
            width: self.w, height: self.h, enabled: self.enabled,
            data: self.data.clone(),
        })
    }
    pub fn import(mask: &moya_project::MaskData, width: u32, height: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(mask.width > 0 && mask.height > 0 && mask.width <= 16384 && mask.height <= 16384, "invalid mask dimensions");
        anyhow::ensure!(mask.data.len() == mask.width as usize * mask.height as usize, "invalid mask pixel count");
        anyhow::ensure!(width > 0 && height > 0 && width <= 16384 && height <= 16384, "invalid source dimensions");
        let values = if (mask.width, mask.height) == (width, height) { mask.data.clone() }
            else { resample(&mask.data, mask.width, mask.height, width, height,65535.0).into_iter().map(quantize).collect() };
        let mut result = Self::default(); result.reset(width, height); result.enabled = mask.enabled;
        for (index, value) in values.into_iter().enumerate() { result.put(index, value); }
        Ok(result)
    }
    pub fn duplicate(&self) -> Self {
        let mut result = Self::default();
        result.w = self.w; result.h = self.h; result.data = self.data.clone();
        result.sum = self.sum; result.fingerprint = self.fingerprint; result.enabled = self.enabled;
        result.tool = self.tool; result.view = self.view; result.overlay_opacity = self.overlay_opacity;
        result.brush = BrushSettings { size: self.brush.size, hardness: self.brush.hardness, opacity: self.brush.opacity };
        result.combine = self.combine; result.lum_range = self.lum_range;
        result.feather = self.feather; result.grow = self.grow;
        result.mark_all(); result.committed = true; result
    }
    pub fn coverage(&self) -> f32 { if self.data.is_empty() { 0.0 } else { (self.sum as f64/(self.data.len() as f64*65535.0)).clamp(0.0,1.0) as f32 } }
    pub fn is_empty(&self) -> bool { self.sum == 0 }
    pub fn save_png(&self, path: &Path) -> anyhow::Result<()> {
        let gray: Vec<u8> = self.data.iter().map(|&v| (v as f32/65535.0*255.0).round() as u8).collect();
        moya_io::save_gray_png(path,self.w,self.h,&gray)
    }
    pub fn gpu_image(&self) -> Option<&Image> { if self.enabled && !self.is_empty() { self.image.as_ref() } else { None } }
    fn needs_gpu_upload(&self, pixels_changed: bool) -> bool {
        self.enabled && !self.is_empty() && (pixels_changed || self.image.is_none())
    }
    pub fn overlay_texture(&self) -> Option<&TextureHandle> { self.overlay.as_ref() }
    pub fn thumbnail(&self) -> Option<&TextureHandle> { self.thumb.as_ref() }
    /// `live`: re-render every 63 ms while painting. Otherwise (previews too slow to keep the
    /// brush smooth) re-render only when the brush rests for 120 ms; stroke end always renders.
    pub fn sync(&mut self, ctx: &egui::Context, gpu: &Gpu, live: bool) -> MaskSync {
        let appearance_changed = self.appearance != (self.view,self.overlay_opacity);
        let enabled_changed = self.was_enabled != self.enabled;
        self.was_enabled = self.enabled;
        let dirty = self.dirty.take();
        if (!self.enabled || self.is_empty()) && self.image.take().is_some() {
            self.gpu_generation = self.gpu_generation.wrapping_add(1);
        }
        if appearance_changed { self.appearance = (self.view,self.overlay_opacity); }
        // A dirty rectangle wins even before the first GPU upload: upload() widens it to the
        // full image unless everything outside it is still zero.
        let rect = if appearance_changed { Some([0,0,self.w,self.h]) } else if dirty.is_some() { dirty }
            else if self.needs_gpu_upload(false) { Some([0,0,self.w,self.h]) } else { None };
        if let Some(rect) = rect {
            self.upload(ctx,gpu,rect,dirty.is_some());
            self.preview_pending |= dirty.is_some();
        }
        if self.committed || enabled_changed { self.committed=false; self.preview_pending=false; self.last_preview=Instant::now(); return MaskSync::Committed; }
        if self.preview_pending {
            let (since, delay) = if live { (self.last_preview, Duration::from_millis(63)) } else { (self.last_dab, Duration::from_millis(120)) };
            if since.elapsed() >= delay { self.preview_pending=false; self.last_preview=Instant::now(); return MaskSync::Painting; }
            ctx.request_repaint_after(delay.saturating_sub(since.elapsed()));
        }
        MaskSync::Unchanged
    }
    fn upload(&mut self, ctx: &egui::Context, gpu: &Gpu, mut rect: [u32;4], pixels_changed: bool) {
        if self.w==0 || self.h==0 { return; }
        let full = [0, 0, self.w, self.h];
        if self.image.is_none() {
            // First paint on an empty mask: everything outside the brushed rectangle is still zero,
            // so create the texture as zeros on the GPU (same (v,v,v,1) texels an upload of zeros
            // gives) and upload only the rectangle, instead of the whole full-resolution mask.
            let [x0, y0, x1, y1] = rect;
            let zero_outside = rect != full && self.needs_gpu_upload(pixels_changed) && self.sum == (y0..y1)
                .map(|y| self.data[(y*self.w+x0) as usize..(y*self.w+x1) as usize].iter().map(|&v| u64::from(v)).sum::<u64>()).sum::<u64>();
            if zero_outside {
                self.image = Some(gpu.fill(self.w, self.h, [0.0, 0.0, 0.0, 1.0]));
            } else { rect = full; }
        }
        let [x0,y0,x1,y1]=rect;
        if self.needs_gpu_upload(pixels_changed) {
            if self.image.is_none() || rect == full {
                self.image=Some(gpu.upload_gray_u16(self.w,self.h,&self.data));
            } else if let Some(image)=&self.image {
                let mut rgba=Vec::with_capacity(((x1-x0)*(y1-y0)*4) as usize);
                for y in y0..y1 { for x in x0..x1 { let v=self.data[(y*self.w+x) as usize] as f32/65535.0; rgba.extend_from_slice(&[v,v,v,1.0]); }}
                gpu.write_region(image,x0,y0,x1-x0,y1-y0,&rgba);
            }
            self.gpu_generation = self.gpu_generation.wrapping_add(1);
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
            let v=sample(&self.data,self.w,self.h,(x as f32+0.5)*self.w as f32/ow as f32-0.5,(y as f32+0.5)*self.h as f32/oh as f32-0.5,65535.0);
            pixels.push(if self.view==MaskView::MaskOnly { Color32::from_gray((v*255.0).round() as u8) } else { Color32::from_rgba_unmultiplied(255,0,190,(v*self.overlay_opacity.clamp(0.0,1.0)*255.0).round() as u8) });
        }}
        let image=egui::ColorImage::new([(c-a) as usize,(d-b) as usize],pixels);
        if let Some(texture)=&mut self.overlay { texture.set_partial([a as usize,b as usize],image,egui::TextureOptions::LINEAR); }
        else { self.overlay=Some(ctx.load_texture("Mask overlay",image,egui::TextureOptions::LINEAR)); }
        let (tw,th)=((self.w as f32*96.0/self.w.max(self.h) as f32).round().max(1.0) as u32,(self.h as f32*96.0/self.w.max(self.h) as f32).round().max(1.0) as u32);
        let pixels=resample(&self.data,self.w,self.h,tw,th,65535.0).into_iter().map(|v| Color32::from_gray((v*255.0).round() as u8)).collect();
        let image=egui::ColorImage::new([tw as usize,th as usize],pixels);
        if let Some(t)=&mut self.thumb { t.set(image,egui::TextureOptions::LINEAR); } else { self.thumb=Some(ctx.load_texture("Mask thumbnail",image,egui::TextureOptions::LINEAR)); }
    }
}
fn pixel_hash(index: usize, value: u16) -> u64 {
    if value == 0 { return 0; }
    let mut x = (index as u64).wrapping_mul(0x9e3779b97f4a7c15) ^ value as u64;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}
fn quantize(v:f32)->u16 { (v.clamp(0.0,1.0)*65535.0).round() as u16 }
fn combine(old:f32,new:f32,mode:Combine)->f32 { match mode { Combine::Replace=>new,Combine::Add=>old+new,Combine::Subtract=>old-new,Combine::Intersect=>old.min(new) }.clamp(0.0,1.0) }
fn falloff(d:f32,hardness:f32)->f32 { let h=hardness.clamp(0.0,1.0); if d>=1.0 { 0.0 } else if d<=h { 1.0 } else { let t=(d-h)/(1.0-h); 1.0-t*t*(3.0-2.0*t) } }
fn sample<T: Copy + Into<f32>>(data:&[T],w:u32,h:u32,x:f32,y:f32,scale:f32)->f32 {
    let x=x.clamp(0.0,(w-1) as f32); let y=y.clamp(0.0,(h-1) as f32);
    let (a,b)=(x.floor() as u32,y.floor() as u32); let (c,d)=((a+1).min(w-1),(b+1).min(h-1));
    let (tx,ty)=(x-a as f32,y-b as f32); let p=|x,y| -> f32 { data[(y*w+x) as usize].into()/scale };
    (p(a,b)*(1.0-tx)+p(c,b)*tx)*(1.0-ty)+(p(a,d)*(1.0-tx)+p(c,d)*tx)*ty
}
fn resample<T: Copy + Into<f32>>(data:&[T],w:u32,h:u32,nw:u32,nh:u32,scale:f32)->Vec<f32> {
    if w==nw && h==nh { return data.iter().map(|&v| v.into()/scale).collect(); }
    if w==0 || h==0 { return vec![0.0;nw as usize*nh as usize]; }
    (0..nh).flat_map(|y| (0..nw).map(move |x| sample(data,w,h,(x as f32+0.5)*w as f32/nw as f32-0.5,(y as f32+0.5)*h as f32/nh as f32-0.5,scale))).collect()
}
fn morphology(data:&[u16],w:usize,h:usize,radius:usize,grow:bool)->Vec<u16> {
    fn pass(src:&[u16],dst:&mut[u16],length:usize,lines:usize,stride:usize,line_stride:usize,r:usize,grow:bool) {
        for line in 0..lines { let mut deque:VecDeque<usize>=VecDeque::new(); let mut next=0;
            for i in 0..length { let end=i.saturating_add(r).min(length-1);
                while next<=end { let value=src[line*line_stride+next*stride]; while deque.back().is_some_and(|&j| if grow {src[line*line_stride+j*stride]<=value} else {src[line*line_stride+j*stride]>=value}) {deque.pop_back();} deque.push_back(next); next+=1; }
                while deque.front().is_some_and(|&j| j<i.saturating_sub(r)) {deque.pop_front();}
                dst[line*line_stride+i*stride]=src[line*line_stride+deque[0]*stride];
            }
        }
    }
    let mut tmp=vec![0;data.len()]; let mut out=vec![0;data.len()];
    pass(data,&mut tmp,w,h,1,w,radius,grow); pass(&tmp,&mut out,h,w,w,1,radius,grow); out
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Reference: the original per-pixel model (pixel -> value before stroke, max coverage).
    fn reference_stroke(data: &mut [u16], w: u32, h: u32, brush: (f32, f32, f32), points: &[([f32; 2], bool)]) -> Vec<(u32, u16, u16)> {
        let (size, hardness, opacity) = brush;
        let mut pixels = std::collections::HashMap::<u32, (u16, f32)>::new();
        let mut last: Option<[f32; 2]> = None;
        for &(xy, erase) in points {
            let from = last.unwrap_or(xy);
            let count = ((xy[0]-from[0]).hypot(xy[1]-from[1]) / (size * 0.25).max(0.5)).ceil().max(1.0) as usize;
            for n in 1..=count {
                let t = n as f32 / count as f32;
                let c = [from[0] + (xy[0]-from[0])*t, from[1] + (xy[1]-from[1])*t];
                let (x0, y0) = ((c[0]-size).floor().clamp(0.0, w as f32) as u32, (c[1]-size).floor().clamp(0.0, h as f32) as u32);
                let (x1, y1) = ((c[0]+size).ceil().clamp(0.0, w as f32) as u32, (c[1]+size).ceil().clamp(0.0, h as f32) as u32);
                for y in y0..y1 { for x in x0..x1 {
                    let coverage = falloff((x as f32+0.5-c[0]).hypot(y as f32+0.5-c[1]) / size, hardness) * opacity.clamp(0.0, 1.0);
                    if coverage <= 0.0 { continue; }
                    let index = y*w+x;
                    let entry = pixels.entry(index).or_insert((data[index as usize], 0.0));
                    entry.1 = entry.1.max(coverage);
                    let before = entry.0 as f32 / 65535.0;
                    data[index as usize] = quantize(if erase { before * (1.0-entry.1) } else { before + (1.0-before)*entry.1 });
                }}
            }
            last = Some(xy);
        }
        let mut changes: Vec<_> = pixels.into_iter().filter(|(i, (b, _))| data[*i as usize] != *b).map(|(i, (b, _))| (i, b, data[i as usize])).collect();
        changes.sort_unstable();
        changes
    }
    #[test] fn tiled_strokes_match_per_pixel_reference_including_undo() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; (seed >> 11) as f32 / (1u64 << 53) as f32 };
        for case in 0..24 {
            // Odd sizes so edge tiles are partial; brushes up to beyond the image.
            let (w, h) = (37 + (next() * 200.0) as u32, 29 + (next() * 150.0) as u32);
            let brush = (0.5 + next() * 70.0, next(), 0.05 + next());
            let mut m = Mask::default(); m.reset(w, h);
            let initial: Vec<u16> = (0..w * h).map(|_| (next() * 65535.0) as u16).collect();
            m.replace(initial.clone()); m.take_history_steps();
            m.brush = BrushSettings { size: brush.0, hardness: brush.1, opacity: brush.2 };
            let points: Vec<([f32; 2], bool)> = (0..12).map(|i| (
                [next() * (w as f32 + 40.0) - 20.0, next() * (h as f32 + 40.0) - 20.0],
                case % 3 == 1 && i >= 6 || case % 3 == 2 && i % 4 == 3,
            )).collect();
            let mut expected = initial.clone();
            let expected_changes = reference_stroke(&mut expected, w, h, brush, &points);
            for &(xy, erase) in &points { m.begin_stroke(erase); m.stroke_to(xy); }
            m.end_stroke();
            assert_eq!(m.data, expected, "case {case}: mask values");
            let actual_changes = match m.past.back() {
                Some(HistoryEntry::Diff(c)) => { let mut v: Vec<_> = c.iter().map(|c| (c.index, c.before, c.after)).collect(); v.sort_unstable(); v }
                None => Vec::new(),
                Some(HistoryEntry::Snapshot(_)) => panic!("case {case}: strokes record diffs"),
            };
            assert_eq!(actual_changes, expected_changes, "case {case}: undo record");
            if !expected_changes.is_empty() { assert!(m.undo()); assert_eq!(m.data, initial, "case {case}: undo restores"); }
        }
    }
    #[test] fn fractional_paths_preserve_normalized_float_ordering() {
        let values=vec![0u16,1,1234,32767,32768,65534,65535];
        let normalized:Vec<f32>=values.iter().map(|&v| v as f32/65535.0).collect();
        for width in [3,11,29] {
            assert_eq!(resample(&values,7,1,width,1,65535.0),resample(&normalized,7,1,width,1,1.0));
        }
        let mut m=Mask::default();m.reset(7,1);
        for mode in [Combine::Replace,Combine::Add,Combine::Subtract,Combine::Intersect] {
            for offset in [-0.00000001,0.0,0.00000001] {
                let incoming:Vec<f32>=values.iter().map(|&v| (v as f32+0.5)/65535.0+offset).collect();
                let expected:Vec<u16>=normalized.iter().zip(&incoming).map(|(&old,&new)| quantize(combine(old,new,mode))).collect();
                m.replace(values.clone());m.combine=mode;m.combine_float_values(incoming);
                assert_eq!(m.data,expected);
            }
        }
        for &old in &values { for &new in &values {
            assert_eq!(old.saturating_add(new),quantize(combine(old as f32/65535.0,new as f32/65535.0,Combine::Add)));
            assert_eq!(old.saturating_sub(new),quantize(combine(old as f32/65535.0,new as f32/65535.0,Combine::Subtract)));
            assert_eq!(65535-old,quantize(1.0-old as f32/65535.0));
        }}
    }
    #[test] fn brush_falloff_and_opacity_cap() {
        assert_eq!(falloff(0.2,0.5),1.0); assert_eq!(falloff(1.0,0.5),0.0); assert!((falloff(0.75,0.5)-0.5).abs()<1e-6);
        let mut m=Mask::default(); m.reset(16,16); m.brush.size=5.0; m.brush.hardness=1.0; m.brush.opacity=0.4;
        m.begin_stroke(false); for _ in 0..10 {m.stroke_to([8.5,8.5]);} m.end_stroke();
        assert_eq!(m.data[8*16+8],quantize(0.4)); assert!(m.undo()); assert!(m.is_empty()); assert!(m.redo()); assert_eq!(m.data[8*16+8],quantize(0.4));
    }
    #[test] fn combination_uses_saturating_u16_arithmetic() {
        let mut m=Mask::default();m.reset(1,1);
        for (mode,expected) in [(Combine::Replace,30000),(Combine::Add,65535),(Combine::Subtract,10000),(Combine::Intersect,30000)] {
            m.replace(vec![40000]);m.combine=mode;m.combine_values(vec![30000]);assert_eq!(m.data,vec![expected]);
        }
    }
    #[test] fn memory_cap_evicts_oldest_and_keeps_redo_bounded() {
        let mut m=Mask::default();m.reset(2,1);m.history_limit=std::mem::size_of::<HistoryEntry>() + 2*std::mem::size_of::<u16>();
        m.replace(vec![65535,0]);m.replace(vec![65535,65535]); assert_eq!(m.past.len(),1);assert_eq!(m.evicted,1);
        assert!(m.undo());assert_eq!(m.data,vec![65535,0]);assert!(!m.undo());assert!(m.redo());assert!(m.history_bytes<=m.history_limit);
    }
    #[test] fn history_bytes_ignore_spare_capacity_and_clear_redo_exactly() {
        let mut m=Mask::default();m.reset(8,1);
        let mut changes=Vec::with_capacity(128);
        changes.push(Change { index:0,before:0,after:65535 });
        m.put(0,65535);m.commit(HistoryEntry::Diff(changes));
        let bytes=std::mem::size_of::<HistoryEntry>()+std::mem::size_of::<Change>();
        assert_eq!(m.history_bytes,bytes);
        assert!(m.undo());assert_eq!(m.history_bytes,bytes);
        m.clear_redo();assert_eq!(m.history_bytes,0);assert!(!m.redo());
    }
    #[test] fn newest_oversized_snapshot_is_retained_and_swaps_exactly() {
        let mut m=Mask::default();m.reset(8,1);m.history_limit=1;
        m.replace(vec![quantize(0.25);8]);
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
        let mut values=vec![0;8];values[3]=quantize(0.5);m.replace(values);
        assert!(matches!(m.past.back(),Some(HistoryEntry::Diff(changes)) if changes.len()==1));
        let before=m.data.clone();m.invert();assert!(m.undo());assert_eq!(m.data,before);
        assert!(matches!(m.future.last(),Some(HistoryEntry::Snapshot(_))));
        m.clear();
        assert!(m.future.is_empty());assert!(!m.redo());
        assert_eq!(m.history_bytes,m.past.iter().map(HistoryEntry::bytes).sum::<usize>());
        assert!(m.undo());assert_eq!(m.data,before);
    }
    #[test] fn built_masks_and_clear_preserve_zero() {
        let gpu=Gpu::new_blocking().expect("GPU is mandatory for mask build tests");
        let source=gpu.upload_rgba32f(2,1,&[0.2,0.8,0.4,0.5,1.0,0.0,0.0,0.0]);
        let mut m=Mask::default();m.reset(2,1);
        m.build(&gpu,&source,MaskKind::Transparency);assert_eq!(m.data,vec![quantize(0.5),0]);
        m.build(&gpu,&source,MaskKind::Luminosity);assert_eq!(m.data,vec![quantize(0.8*0.5),0]);
        m.lum_range=[0.2,0.8];m.build(&gpu,&source,MaskKind::Luminosity);assert_eq!(m.data,vec![quantize(0.5),0]);
        m.replace(vec![quantize(1e-7),quantize(0.5)]);m.clear();assert!(m.is_empty());assert_eq!(m.data,vec![0;2]);
        m.build(&gpu,&source,MaskKind::Outlines);assert_eq!(m.data,vec![quantize(0.5);2]);
    }
    #[test] fn u16_storage_import_and_history_are_exact() {
        let values=vec![0,1,32767,32768,65534,65535];
        let saved=moya_project::MaskData { width:6,height:1,enabled:false,data:values.clone() };
        let mut m=Mask::import(&saved,6,1).unwrap();
        assert_eq!(std::mem::size_of_val(m.data.as_slice()),12);
        assert_eq!(m.retained_bytes(),m.data.capacity()*2);
        assert_eq!(m.export().unwrap(),saved);
        let original=m.fingerprint();m.invert();
        assert_eq!(m.data,values.iter().map(|v| 65535-v).collect::<Vec<_>>());
        assert!(m.undo());assert_eq!(m.data,values);assert_eq!(m.fingerprint(),original);
        assert!(m.redo());assert!(m.undo());
        let mut duplicate=m.duplicate();duplicate.clear();assert_eq!(m.data,values);
    }
    #[test] fn first_stroke_on_empty_mask_uploads_only_its_rect_with_exact_gpu_contents() {
        let gpu=Gpu::new_blocking().expect("GPU is mandatory for mask upload tests");
        let ctx=egui::Context::default();
        let mut m=Mask::default();m.reset(301,203);m.enabled=true;m.brush.size=12.0;
        let gpu_matches=|m:&Mask| gpu.download_gray_u16(m.gpu_image().unwrap())==m.data;
        m.begin_stroke(false);m.stroke_to([40.5,60.0]);m.stroke_to([90.0,70.0]);
        m.sync(&ctx,&gpu,true);assert!(gpu_matches(&m),"zero-filled texture plus brushed rect");
        m.stroke_to([280.0,190.0]);m.end_stroke();m.sync(&ctx,&gpu,true);assert!(gpu_matches(&m));
        // A disabled non-empty mask drops its texture; enabling must upload everything again.
        m.enabled=false;m.sync(&ctx,&gpu,true);assert!(m.gpu_image().is_none());
        m.enabled=true;m.begin_stroke(false);m.stroke_to([150.0,20.0]);m.end_stroke();
        m.sync(&ctx,&gpu,true);assert!(gpu_matches(&m),"non-empty mask is fully uploaded, not zero-filled");
    }
    #[test] fn disabled_and_empty_masks_never_upload_and_enabling_uploads_once() {
        let gpu=Gpu::new_blocking().expect("GPU is mandatory for mask upload tests");
        let ctx=egui::Context::default();
        let mut m=Mask::default();m.reset(2,1);
        let generation=m.gpu_generation;
        m.sync(&ctx, &gpu, true);assert_eq!(m.gpu_generation,generation);assert!(m.image.is_none());assert!(m.gpu_image().is_none());
        m.enabled=true;m.sync(&ctx, &gpu, true);assert_eq!(m.gpu_generation,generation);assert!(m.image.is_none());
        m.enabled=false;m.replace(vec![1,65535]);m.sync(&ctx, &gpu, true);
        assert_eq!(m.gpu_generation,generation);assert!(m.image.is_none());assert!(m.gpu_image().is_none());
        m.enabled=true;m.sync(&ctx, &gpu, true);assert_eq!(m.gpu_generation,generation+1);assert!(m.gpu_image().is_some());
        assert_eq!(gpu.download_gray_u16(m.gpu_image().unwrap()),m.data);
        m.sync(&ctx, &gpu, true);m.overlay_opacity=0.7;m.sync(&ctx, &gpu, true);assert_eq!(m.gpu_generation,generation+1);
        m.enabled=false;m.sync(&ctx, &gpu, true);assert_eq!(m.gpu_generation,generation+2);assert!(m.image.is_none());assert!(m.gpu_image().is_none());
        m.enabled=true;m.sync(&ctx, &gpu, true);assert_eq!(m.gpu_generation,generation+3);
        m.clear();m.sync(&ctx, &gpu, true);assert_eq!(m.gpu_generation,generation+4);assert!(m.image.is_none());assert!(m.gpu_image().is_none());
        assert!(m.undo());m.sync(&ctx, &gpu, true);assert_eq!(m.gpu_generation,generation+5);assert!(m.gpu_image().is_some());
    }
    #[test] fn multi_dab_stroke_over_built_mask_undoes_once() {
        let mut m=Mask::default();m.reset(64,64);m.brush.size=3.0;
        m.replace((0..4096).map(|i| if i>3000 {quantize(0.5)} else {0}).collect());
        let before=m.data.clone();m.take_history_steps();
        for x in 10..40 { m.begin_stroke(false);m.stroke_to([x as f32,20.0]); }
        m.end_stroke();assert_eq!(m.take_history_steps().0,1);
        assert!(m.undo());assert_eq!(m.data,before);assert!(m.redo());assert_ne!(m.data,before);
    }
    #[test] fn morphology_grows_and_contracts() { assert_eq!(morphology(&[0,65535,0],3,1,1,true),vec![65535;3]);assert_eq!(morphology(&[65535,0,65535],3,1,1,false),vec![0;3]); }
}
