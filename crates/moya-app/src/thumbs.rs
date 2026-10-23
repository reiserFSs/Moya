//! Layer-panel thumbnails: each layer's retained Draft glow, reduced on the GPU (antialiased
//! bicubic pyramid) into a small texture registered with egui. No CPU readback. A thumbnail
//! is rebuilt only when its layer's glow changes (generation or storage differs), so idle
//! frames cost a hash lookup per layer.

use eframe::{egui, egui_wgpu, wgpu};
use moya_gpu::Gpu;
use moya_pipeline::Document;
use std::collections::HashMap;

/// Long side in physical pixels: a 32-point tile on a 2x display.
const SIZE: u32 = 64;

struct Thumb {
    /// (glow generation, glow storage id) the texture was built from.
    source: (u64, usize),
    texture: wgpu::Texture,
    id: egui::TextureId,
    size: [u32; 2],
}

#[derive(Default)]
pub struct Thumbs {
    document: u64,
    entries: HashMap<u64, Thumb>,
}

impl Thumbs {
    /// Refreshes thumbnails for `layers` from the document's cached glows and frees the rest.
    /// Layers without a cached glow keep their previous thumbnail until one is available.
    pub fn sync(&mut self, gpu: &Gpu, state: &egui_wgpu::RenderState, document: Option<&Document>, layers: &[u64]) {
        let current = document.map_or(0, Document::id);
        if current != self.document {
            self.document = current;
            self.entries.retain(|_, thumb| { state.renderer.write().free_texture(&thumb.id); false });
        }
        self.entries.retain(|id, thumb| {
            let keep = layers.contains(id);
            if !keep { state.renderer.write().free_texture(&thumb.id); }
            keep
        });
        let Some(document) = document else { return };
        let mut wrote = false;
        for &layer in layers {
            let Some((glow, generation)) = document.layer_glow(layer) else { continue };
            let source = (generation, glow.storage_id());
            if self.entries.get(&layer).is_some_and(|thumb| thumb.source == source) { continue; }
            let scale = SIZE as f32 / glow.width().max(glow.height()) as f32;
            let size = [
                ((glow.width() as f32 * scale).round() as u32).clamp(1, SIZE),
                ((glow.height() as f32 * scale).round() as u32).clamp(1, SIZE),
            ];
            let small = gpu.resize_bicubic(glow, size[0], size[1]);
            if self.entries.get(&layer).is_none_or(|thumb| thumb.size != size) {
                if let Some(old) = self.entries.remove(&layer) { state.renderer.write().free_texture(&old.id); }
                let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
                    label: Some("Moya layer thumbnail"),
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                    view_formats: &[],
                });
                let view = texture.create_view(&Default::default());
                let id = state.renderer.write().register_native_texture(gpu.device(), &view, wgpu::FilterMode::Linear);
                self.entries.insert(layer, Thumb { source, texture, id, size });
            }
            let thumb = self.entries.get_mut(&layer).expect("thumbnail just ensured");
            gpu.write_display_rgba8(&small, &thumb.texture);
            thumb.source = source;
            wrote = true;
        }
        if wrote { gpu.flush(); }
    }

    /// Texture and pixel size for a layer's thumbnail, if one has been built.
    pub fn get(&self, layer: u64) -> Option<(egui::TextureId, [u32; 2])> {
        self.entries.get(&layer).map(|thumb| (thumb.id, thumb.size))
    }
}
