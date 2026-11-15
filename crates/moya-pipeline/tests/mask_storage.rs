//! Compact (single-channel) resident masks must render bit-identically to the RGBA32F masks
//! they replaced, in Draft and HQ, with and without a Below input and with a resized mask.
use std::sync::Arc;
use moya_core::{LayerBlend, LayerInput, LayerSettings, Params, RenderMode};
use moya_gpu::{Depth, Gpu, Image};
use moya_pipeline::{Document, LayerInputs, RenderRequest, ViewFlags};

fn render(gpu: &Arc<Gpu>, mode: RenderMode, masks: [&Image; 2]) -> (Vec<f32>, Vec<Vec<f32>>) {
    let (w, h) = (64u32, 48u32);
    let mut pixels = vec![0.; (w * h * 4) as usize];
    for y in 0..h { for x in 0..w {
        let i = ((y * w + x) * 4) as usize;
        let light = if (x as i32 - 28).pow(2) + (y as i32 - 20).pow(2) < 20 || x % 13 == 0 { 1. } else { 0.01 };
        pixels[i..i + 4].copy_from_slice(&[light, light * 0.8, light * 0.5, 1.]);
    }}
    let mut document = Document::new(gpu.clone(), gpu.upload_rgba32f(w, h, &pixels), Depth::Eight);
    let params = Params { radius: 6., glow_iterations: 2., glare_on: true, tonemap: true, glare_length: 0.1, ..Params::default() };
    let mut settings = [
        LayerSettings { id: 1, params: params.clone(), blend: LayerBlend::Screen, opacity: 0.8, ..LayerSettings::default() },
        LayerSettings { id: 2, params, input: LayerInput::Below, ..LayerSettings::default() },
    ];
    settings[1].params.threshold += 0.05;
    let inputs: Vec<_> = settings.iter().zip(masks).map(|(settings, mask)| LayerInputs { settings, mask: Some(mask), mask_generation: 1 }).collect();
    let view = ViewFlags { xray: false, solo: false, host_is_float: false };
    let out = document.render(&inputs, &RenderRequest {
        mode, view, selected: Some(1), solo_layer: None,
        want_composite: true, want_glow_composite: true, want_layer_glows: true, want_xray: false,
    });
    (gpu.download_rgba32f(out.composite.as_ref().unwrap()), out.layer_glows.iter().map(|(_, g)| gpu.download_rgba32f(g)).collect())
}

fn codes(w: u32, h: u32, seed: u32) -> Vec<u16> {
    let mut state = seed;
    (0..w * h).map(|_| { state = state.wrapping_mul(1664525).wrapping_add(1013904223); (state >> 16) as u16 }).collect()
}

#[test]
fn compact_masks_render_bit_identically_and_cost_a_fraction() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    // Full-size and resized (80x60 -> working size) masks.
    let sizes = [(64u32, 48u32), (80, 60)];
    let old: Vec<Image> = sizes.iter().enumerate().map(|(i, &(w, h))| gpu.upload_gray_u16(w, h, &codes(w, h, 7 + i as u32))).collect();
    let rgba_bytes = gpu.memory_stats().live_bytes;
    let new: Vec<Image> = sizes.iter().enumerate().map(|(i, &(w, h))| gpu.upload_mask_u16(w, h, &codes(w, h, 7 + i as u32))).collect();
    let compact_bytes = gpu.memory_stats().live_bytes - rgba_bytes;
    assert!(compact_bytes * 4 <= rgba_bytes, "compact {compact_bytes} vs rgba {rgba_bytes}");
    for mode in [RenderMode::Draft, RenderMode::Hq] {
        let expected = render(&gpu, mode, [&old[0], &old[1]]);
        let actual = render(&gpu, mode, [&new[0], &new[1]]);
        let bits = |v: &Vec<f32>| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&actual.0), bits(&expected.0), "{mode:?} composite");
        assert_eq!(actual.1.len(), expected.1.len());
        for (a, e) in actual.1.iter().zip(&expected.1) { assert_eq!(bits(a), bits(e), "{mode:?} layer glow"); }
        assert!(expected.0.iter().any(|v| *v > 0.2), "render must be non-trivial");
    }
}
