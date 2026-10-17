use super::*;

fn request() -> RenderRequest {
    RenderRequest {
        mode: RenderMode::Draft,
        view: ViewFlags { xray: false, solo: false, host_is_float: false },
        selected: None,
        solo_layer: None,
        want_layer_glows: false,
        want_xray: false,
    }
}

fn layer(id: u64) -> LayerSettings {
    LayerSettings {
        id,
        params: Params { radius: 2., glow_iterations: 1., ..Params::default() },
        ..LayerSettings::default()
    }
}

fn inputs(settings: &[LayerSettings]) -> Vec<LayerInputs<'_>> {
    settings.iter().map(|settings| LayerInputs { settings, mask: None, mask_generation: 0 }).collect()
}

fn generations(stages: &LayerStages) -> (u64, u64, u64) {
    (
        stages.extraction_cache.as_ref().unwrap().generation,
        stages.glow_cache.as_ref().unwrap().generation,
        stages.output_cache.as_ref().unwrap().generation,
    )
}

#[test]
fn hq_below_uses_capped_working_glow_without_output_resize_roundtrip() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let pixels: Vec<f32> = (0..4001).flat_map(|x| {
        let light = if x % 11 < 3 { 1. } else { 0.03 };
        [light, light * 0.7, light * 0.4, 1.]
    }).collect();
    let mut document = Document::new(gpu.clone(), gpu.upload_rgba32f(4001, 1, &pixels), Depth::Eight);
    let mut settings = [layer(1), layer(2)];
    settings[0].blend = LayerBlend::Screen;
    settings[0].opacity = 0.65;
    settings[1].input = LayerInput::Below;
    let mut req = request();
    req.mode = RenderMode::Hq;
    let output = document.render(&inputs(&settings), &req);

    assert_eq!((output.composite.width(), output.composite.height()), (4001, 1));
    let original = document.layers[&1].stages.working.as_ref().unwrap();
    let first_output = document.layers[&1].stages.output_cache.as_ref().unwrap();
    let raw_glow = &first_output.images[3];
    assert_eq!((original.source.width(), original.source.height()), (4000, 1));
    assert_eq!((raw_glow.width(), raw_glow.height()), (4000, 1));
    let expected = gpu.blend(&original.source, raw_glow, B::Screen, settings[0].opacity, 1., Depth::Eight);
    let below = document.layers[&2].stages.working.as_ref().unwrap();
    assert_eq!((below.source.width(), below.source.height()), (4000, 1));
    assert_eq!(gpu.download_rgba32f(&below.source), gpu.download_rgba32f(&expected),
        "Below must blend original-grid source with raw working glow, not downsample a final-size composite");
}

#[test]
fn selected_original_xray_only_builds_extraction_and_heatmap() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut document = Document::new(gpu.clone(), gpu.fill(3, 3, [0.25, 0.5, 0.8, 1.]), Depth::Eight);
    let mut settings = [layer(1)];
    let mut req = request();
    req.view.xray = true;
    req.want_xray = true;
    req.selected = Some(1);
    let mut previous = None;
    for _ in 0..2 {
        let output = document.render(&inputs(&settings), &req);
        assert!(output.xray.is_some());
        assert!(output.layer_glows.is_empty());
        let stages = &document.layers[&1].stages;
        let generation = stages.extraction_cache.as_ref().unwrap().generation;
        if let Some(previous) = previous {
            assert_ne!(generation, previous, "threshold edit must refresh extraction");
        }
        assert!(stages.heatmap_cache.is_some());
        assert!(stages.exposure_cache.is_none());
        assert!(stages.glow_cache.is_none());
        assert!(stages.glare_cache.is_none());
        assert!(stages.tone_cache.is_none());
        assert!(stages.ca_cache.is_none());
        assert!(stages.output_cache.is_none());
        previous = Some(generation);
        settings[0].params.threshold += 0.2;
    }
}

#[test]
fn first_below_layer_switches_draft_hq_source_and_matches_cold_reference() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    // Crossing the Draft width cap makes a stale Draft source observable in HQ.
    let pixels: Vec<f32> = (0..1501).flat_map(|x| {
        let light = if x % 7 == 0 { 1. } else { 0.05 };
        [light, light * 0.7, light * 0.4, 1.]
    }).collect();
    let mut document = Document::new(gpu.clone(), gpu.upload_rgba32f(1501, 1, &pixels), Depth::Eight);
    let mut settings = [layer(1)];
    settings[0].input = LayerInput::Below;
    let mut previous = None;
    for mode in [RenderMode::Draft, RenderMode::Hq, RenderMode::Draft] {
        let mut req = request();
        req.mode = mode;
        let output = document.render(&inputs(&settings), &req);
        let entry = &document.layers[&1];
        assert_eq!(entry.input_mode, Some(mode));
        assert_eq!(entry.stages.source_is_working, mode == RenderMode::Draft);
        let expected_size = if mode == RenderMode::Draft {
            document.geometry.working_size(mode)
        } else { (1501, 1) };
        assert_eq!((entry.stages.source.width(), entry.stages.source.height()), expected_size);
        if let Some(previous) = previous {
            assert_ne!(entry.input_generation, previous, "mode switch must rebuild Below source");
        }
        previous = Some(entry.input_generation);
        assert!(document.grids.iter().all(|grid| grid.input == 0 || grid.input == entry.input_generation),
            "mode switch must remove the stale Below grid");

        let mut reference = Document::new(gpu.clone(), gpu.copy(&document.source), Depth::Eight);
        let cold = reference.render(&inputs(&settings), &req);
        assert_eq!(gpu.download_rgba32f(&output.composite), gpu.download_rgba32f(&cold.composite));
    }
}

#[test]
fn editing_layer_b_preserves_layer_a_stage_generations() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut document = Document::new(gpu.clone(), gpu.fill(3, 3, [0.25, 0.5, 0.8, 1.]), Depth::Eight);
    let mut settings = [layer(1), layer(2)];
    document.render(&inputs(&settings), &request());
    let a = generations(&document.layers[&1].stages);
    let b = generations(&document.layers[&2].stages);

    settings[1].params.threshold += 0.2;
    document.render(&inputs(&settings), &request());

    assert_eq!(generations(&document.layers[&1].stages), a);
    let edited = generations(&document.layers[&2].stages);
    assert_ne!(edited.0, b.0, "B extraction must be invalidated");
    assert_ne!(edited.1, b.1, "B glow must be invalidated");
    assert_ne!(edited.2, b.2, "B output must be invalidated");
}

#[test]
fn original_layers_reuse_one_working_grid_arc() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut document = Document::new(gpu.clone(), gpu.fill(3, 3, [0.25, 0.5, 0.8, 1.]), Depth::Eight);
    let mut settings = [layer(1), layer(2), layer(3)];
    settings[1].params.threshold += 0.2;
    settings[2].params.radius = 3.;
    document.render(&inputs(&settings), &request());

    assert_eq!(document.grids.len(), 1);
    assert_eq!(document.grids[0].input, 0);
    let working = document.layers[&1].stages.working.as_ref().unwrap().clone();
    for entry in document.layers.values() {
        assert!(Arc::ptr_eq(&working, entry.stages.working.as_ref().unwrap()));
    }
    assert!(Arc::ptr_eq(&working, &document.grids[0].working));

    settings[2].params.threshold += 0.1;
    document.render(&inputs(&settings), &request());
    assert_eq!(document.grids.len(), 1);
    assert!(Arc::ptr_eq(&working, &document.grids[0].working));
}

#[test]
fn removing_layer_drops_its_stages_and_stale_below_grid() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut document = Document::new(gpu.clone(), gpu.fill(3, 3, [0.25, 0.5, 0.8, 1.]), Depth::Eight);
    let mut settings = [layer(1), layer(2)];
    settings[1].input = LayerInput::Below;
    document.render(&inputs(&settings), &request());
    let below_generation = document.layers[&2].input_generation;
    assert_ne!(below_generation, 0);
    assert!(document.grids.iter().any(|grid| grid.input == below_generation));
    let below_working = Arc::downgrade(document.layers[&2].stages.working.as_ref().unwrap());
    let original = document.layers[&1].stages.working.as_ref().unwrap().clone();
    let a = generations(&document.layers[&1].stages);

    document.render(&inputs(&settings[..1]), &request());

    assert_eq!(document.layers.len(), 1);
    assert!(!document.layers.contains_key(&2));
    assert_eq!(document.grids.len(), 1);
    assert!(document.grids.iter().all(|grid| grid.input == 0));
    assert!(below_working.upgrade().is_none(), "stale Below grid must not remain retained");
    assert!(Arc::ptr_eq(&original, &document.grids[0].working));
    assert_eq!(generations(&document.layers[&1].stages), a);
}

#[test]
fn document_budget_counts_shared_grids_stages_and_masks_and_evicts_across_layers() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut document = Document::new(gpu.clone(), gpu.fill(3, 3, [0.25, 0.5, 0.8, 1.]), Depth::Eight);
    let settings = [layer(1), layer(2)];
    let mask = gpu.fill(3, 3, [1.; 4]);
    let masked: Vec<_> = settings.iter().map(|settings| LayerInputs {
        settings, mask: Some(&mask), mask_generation: 1,
    }).collect();
    document.render(&masked, &request());

    let image = image_bytes(&mask);
    let shared_grid = 3 * image;
    let masks = 2 * image;
    let per_layer: Vec<_> = document.layers.values().map(|layer| layer.stages.retained_bytes()).collect();
    let total = document.retained_bytes();
    assert_eq!(total, per_layer.iter().sum::<u64>() - shared_grid + masks,
        "shared working images and aliased stage images count once; copied layer masks count separately");

    // Both layers fit separately. Only a document-wide budget must evict here.
    document.budget = total - 1;
    assert!(per_layer.iter().all(|bytes| bytes + image < document.budget));
    let extraction: Vec<_> = settings.iter().map(|s| document.layers[&s.id].stages.extraction_cache.as_ref().unwrap().generation).collect();
    document.trim_cache();
    assert!(document.retained_bytes() <= document.budget);
    for (settings, generation) in settings.iter().zip(extraction) {
        let stages = &document.layers[&settings.id].stages;
        assert!(stages.output_cache.is_none(), "evict the output stage across all layers, not just one layer");
        assert_eq!(stages.extraction_cache.as_ref().unwrap().generation, generation);
    }

    document.budget = shared_grid + masks;
    document.trim_cache();
    assert_eq!(document.retained_bytes(), document.budget);
    assert_eq!(document.grids.len(), 1);
    for entry in document.layers.values() {
        let stages = &entry.stages;
        assert!(stages.working.is_some());
        assert!(stages.mask.is_some());
        assert!(stages.extraction_cache.is_none());
        assert!(stages.heatmap_cache.is_none());
        assert!(stages.exposure_cache.is_none());
        assert!(stages.glow_cache.is_none());
        assert!(stages.glare_cache.is_none());
        assert!(stages.tone_cache.is_none());
        assert!(stages.ca_cache.is_none());
        assert!(stages.output_cache.is_none());
    }

    document.budget = 0;
    document.trim_cache();
    assert_eq!(document.retained_bytes(), 0);
    assert!(document.grids.is_empty());
    for entry in document.layers.values() {
        assert!(entry.stages.working.is_none());
        assert!(entry.stages.mask.is_none());
    }
}

#[test]
#[ignore = "manual GPU timing: run with --ignored --nocapture"]
fn benchmark_three_layer_fx_draft_cold_and_top_layer_edit() {
    use std::time::Instant;

    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let pixels: Vec<f32> = (0..512 * 512).flat_map(|i| {
        let x = (i % 512) as f32;
        let y = (i / 512) as f32;
        let light = if (x - 180.).powi(2) + (y - 230.).powi(2) < 35_f32.powi(2)
            || (x - 360.).powi(2) + (y - 140.).powi(2) < 18_f32.powi(2) { 1. } else { 0.02 };
        [light, light * (0.4 + x / 1024.), light * (0.3 + y / 1024.), 1.]
    }).collect();
    let mut document = Document::new(gpu.clone(), gpu.upload_rgba32f(512, 512, &pixels), Depth::Eight);
    let mut settings = [layer(1), layer(2), layer(3)];
    for (index, layer) in settings.iter_mut().enumerate() {
        layer.params = Params {
            radius: 12. + index as f32 * 6., glow_iterations: 4.,
            glare_on: true, glare_length: 0.1, rays_on: true, rays_samples: 3.,
            ca_on: true, tonemap: true, spectrum_mix: 30., protect_core: true,
            ..Params::default()
        };
    }
    let req = request();
    let cold = Instant::now();
    let output = document.render(&inputs(&settings), &req);
    let pixels = gpu.download_rgba32f(&output.composite);
    let cold = cold.elapsed();
    assert_eq!(pixels.len(), 512 * 512 * 4);
    assert!(pixels.iter().all(|value| value.is_finite()));

    settings[2].params.threshold += 0.1;
    let edit = Instant::now();
    let output = document.render(&inputs(&settings), &req);
    let pixels = gpu.download_rgba32f(&output.composite);
    let edit = edit.elapsed();
    assert_eq!(pixels.len(), 512 * 512 * 4);
    assert!(pixels.iter().all(|value| value.is_finite()));
    eprintln!("3-layer fx Draft generated 512x512, composite readback included: cold={cold:?}, top-layer threshold edit={edit:?}");
}
