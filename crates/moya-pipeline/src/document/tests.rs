use super::*;

fn request() -> RenderRequest {
    RenderRequest {
        mode: RenderMode::Draft,
        view: ViewFlags { xray: false, solo: false, host_is_float: false },
        selected: None,
        solo_layer: None,
        want_composite: true,
        want_glow_composite: false,
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

    assert_eq!((output.composite.as_ref().unwrap().width(), output.composite.as_ref().unwrap().height()), (4001, 1));
    let mut first = LayerStages::shared(gpu.clone(), document.source.clone(), Depth::Eight);
    first.prepare(&settings[0].params, RenderMode::Hq);
    let original = first.working.as_ref().unwrap().source.clone();
    let view = req.view;
    let first_output = first.render_selected(&settings[0].params, RenderMode::Hq, &view, false, false, |_, _, _| {});
    let raw_glow = &first_output.working_glow;
    assert_eq!((original.width(), original.height()), (4000, 1));
    assert_eq!((raw_glow.width(), raw_glow.height()), (4000, 1));
    let expected = gpu.blend(&original, raw_glow, B::Screen, settings[0].opacity, 1., Depth::Eight);
    let full_below = gpu.blend(&document.source, &first_output.glow, B::Screen, settings[0].opacity, 1., Depth::Eight);
    let mut second = LayerStages::shared(gpu.clone(), Arc::new(full_below.clone()), Depth::Eight);
    let mut preparer = LayerStages::shared(gpu.clone(), Arc::new(expected), Depth::Eight);
    preparer.geometry = document.geometry;
    preparer.source_is_working = true;
    preparer.prepare(&settings[1].params, RenderMode::Hq);
    second.working = preparer.working.take();
    let second_output = second.render_selected(&settings[1].params, RenderMode::Hq, &view, false, false, |_, _, _| {});
    let expected = gpu.blend(&full_below, &second_output.glow, B::Screen, 1., 1., Depth::Eight);
    assert_eq!(gpu.download_rgba32f(output.composite.as_ref().unwrap()), gpu.download_rgba32f(&expected),
        "Below must use raw working glow, not downsample a final-size composite");
    assert!(document.layers.is_empty());
    assert!(document.grids.is_empty());
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
fn output_selection_and_hq_release_preserve_owned_results() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut document = Document::new(gpu.clone(), gpu.fill(3, 3, [0.25, 0.5, 0.8, 1.]), Depth::Eight);
    let mut settings = [layer(1), layer(2)];
    settings[1].input = LayerInput::Below;
    let mut req = request();
    req.want_glow_composite = true;
    let full = document.render(&inputs(&settings), &req);
    req.want_composite = false;
    req.want_layer_glows = true;
    let glow_only = document.render(&inputs(&settings), &req);
    assert!(glow_only.composite.is_none());
    assert_eq!(glow_only.layer_glows.len(), 2);
    assert_eq!(gpu.download_rgba32f(glow_only.glow_composite.as_ref().unwrap()),
        gpu.download_rgba32f(full.glow_composite.as_ref().unwrap()));
    req.mode = RenderMode::Hq;
    req.want_glow_composite = false;
    let hq = document.render(&inputs(&settings), &req);
    assert!(hq.composite.is_none());
    assert!(hq.glow_composite.is_none());
    assert!(document.layers.is_empty());
    assert!(document.grids.is_empty());
    assert_eq!(document.retained_bytes(), 0);
    assert_eq!(gpu.download_rgba32f(&hq.layer_glows[1].1).len(), 36);
    req.want_layer_glows = false;
    req.want_xray = true;
    req.view.xray = true;
    req.selected = Some(2);
    let xray = document.render(&inputs(&settings), &req);
    assert!(xray.composite.is_none());
    assert!(xray.glow_composite.is_none());
    assert!(document.layers.is_empty());
    assert!(document.grids.is_empty());
    assert_eq!(gpu.download_rgba32f(xray.xray.as_ref().unwrap()).len(), 36);
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
        if mode == RenderMode::Draft {
            let entry = &document.layers[&1];
            assert_eq!(entry.input_mode, Some(mode));
            assert!(entry.stages.source_is_working);
            assert_eq!((entry.stages.source.width(), entry.stages.source.height()), document.geometry.working_size(mode));
            if let Some(previous) = previous {
                assert_ne!(entry.input_generation, previous, "mode switch must rebuild Below source");
            }
            previous = Some(entry.input_generation);
            assert!(document.grids.iter().all(|grid| grid.input == 0 || grid.input == entry.input_generation));
        } else {
            assert!(document.layers.is_empty());
            assert!(document.grids.is_empty());
        }

        let mut reference = Document::new(gpu.clone(), document.source.as_ref().clone(), Depth::Eight);
        let cold = reference.render(&inputs(&settings), &req);
        assert_eq!(gpu.download_rgba32f(output.composite.as_ref().unwrap()), gpu.download_rgba32f(cold.composite.as_ref().unwrap()));
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
        "shared working images and aliased stage images count once; mutable mask snapshots count separately");

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
    let pixels = gpu.download_rgba32f(output.composite.as_ref().unwrap());
    let cold = cold.elapsed();
    assert_eq!(pixels.len(), 512 * 512 * 4);
    assert!(pixels.iter().all(|value| value.is_finite()));

    settings[2].params.threshold += 0.1;
    let edit = Instant::now();
    let output = document.render(&inputs(&settings), &req);
    let pixels = gpu.download_rgba32f(output.composite.as_ref().unwrap());
    let edit = edit.elapsed();
    assert_eq!(pixels.len(), 512 * 512 * 4);
    assert!(pixels.iter().all(|value| value.is_finite()));
    eprintln!("3-layer fx Draft generated 512x512, composite readback included: cold={cold:?}, top-layer threshold edit={edit:?}");
}

fn region_random(seed: &mut u64) -> u32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed >> 32) as u32
}

fn finish_refine(document: &mut Document, layers: &[LayerInputs<'_>], view: &ViewFlags) {
    let total = layers.iter().filter(|input| input.settings.visible).count();
    let mut done = match document.refine_state(layers, view) {
        RefineState::Ready => return,
        RefineState::Idle => 0,
        RefineState::Pending { done, total: actual } => {
            assert_eq!(actual, total);
            done
        }
    };
    while done < total {
        done += 1;
        let expected = if done == total { RefineState::Ready }
            else { RefineState::Pending { done, total } };
        assert_eq!(document.refine_step(layers, view), expected,
            "one call must refine exactly one visible layer");
        assert_eq!(document.refine_state(layers, view), expected);
    }
    assert_eq!(document.refine_step(layers, view), RefineState::Ready);
}

#[test]
fn refined_regions_match_hq_export_bits_randomized_stacks_and_capped_edges() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut seed = 0xa153_0fed_93b7_4621;
    for host_depth in [Depth::Eight, Depth::Sixteen, Depth::Float] {
        // The thin image crosses the HQ 4000px bicubic cap without a huge fixture.
        for (width, height) in [(19, 13), (4003, 3)] {
            let scale = match host_depth {
                Depth::Eight => 255.,
                Depth::Sixteen => 32768.,
                Depth::Float => 0.,
            };
            let pixels: Vec<f32> = (0..width * height).flat_map(|_| {
                let mut pixel = [0.; 4];
                for value in &mut pixel {
                    *value = (region_random(&mut seed) % 10001) as f32 / 10000.;
                    if scale != 0. { *value = (*value * scale).round() / scale; }
                }
                if host_depth == Depth::Float { pixel[0] *= 1.5; }
                pixel
            }).collect();
            let source = gpu.upload_rgba32f(width, height, &pixels);
            let mask_pixels: Vec<f32> = (0..width * height).flat_map(|index| {
                let value = if index % 7 == 0 { 0. } else {
                    (region_random(&mut seed) % 256) as f32 / 255.
                };
                [value, value, value, 1.]
            }).collect();
            let mask = gpu.upload_rgba32f(width, height, &mask_pixels);
            let mut settings: Vec<_> = (1..=6).map(layer).collect();
            for (index, blend) in [LayerBlend::Auto, LayerBlend::Screen,
                LayerBlend::LinearDodge, LayerBlend::Lighten, LayerBlend::Normal,
                LayerBlend::Screen].into_iter().enumerate() {
                let settings = &mut settings[index];
                settings.blend = blend;
                settings.opacity = 0.2 + (region_random(&mut seed) % 801) as f32 / 1000.;
                settings.params.threshold = 0.15 + (region_random(&mut seed) % 400) as f32 / 1000.;
                settings.params.radius = 1. + (region_random(&mut seed) % 3) as f32;
                settings.params.exposure = (region_random(&mut seed) % 100) as f32 / 100.;
                settings.params.hdr32 = host_depth == Depth::Float;
                settings.params.unmult = index % 2 == 0;
                settings.input = if index == 2 || index == 4 { LayerInput::Below }
                    else { LayerInput::Original };
            }
            settings[5].visible = false;
            let layers: Vec<_> = settings.iter().enumerate().map(|(index, settings)| LayerInputs {
                settings, mask: (index % 2 == 0).then_some(&mask), mask_generation: 1,
            }).collect();
            let mut document = Document::new(gpu.clone(), source.clone(), host_depth);
            let mut oracle = Document::new(gpu.clone(), source, host_depth);
            let mut regions = vec![
                ViewRegion { x: 0, y: 0, w: width, h: height },
                ViewRegion { x: 0, y: 0, w: 1, h: 1 },
                ViewRegion { x: width - 1, y: 0, w: 1, h: 1 },
                ViewRegion { x: 0, y: height - 1, w: 1, h: 1 },
                ViewRegion { x: width - 1, y: height - 1, w: 1, h: 1 },
                ViewRegion { x: width - 7, y: 0, w: 7, h: height },
            ];
            for _ in 0..3 {
                let x = region_random(&mut seed) % width;
                let y = region_random(&mut seed) % height;
                regions.push(ViewRegion { x, y,
                    w: 1 + region_random(&mut seed) % (width - x),
                    h: 1 + region_random(&mut seed) % (height - y) });
            }
            for solo in [false, true] {
                for solo_layer in [None, Some(3), Some(6)] {
                    let mut req = request();
                    req.mode = RenderMode::Hq;
                    req.view.host_is_float = host_depth == Depth::Float;
                    req.view.solo = solo;
                    req.solo_layer = solo_layer;
                    req.want_glow_composite = true;
                    finish_refine(&mut document, &layers, &req.view);
                    // HQ intentionally releases caches, so use a separate legacy oracle.
                    let legacy = oracle.render(&layers, &req);
                    for glow_only in [false, true] {
                        req.want_composite = !glow_only;
                        let legacy_image = if glow_only { legacy.glow_composite.as_ref().unwrap() }
                            else { legacy.composite.as_ref().unwrap() };
                        // Same conversion as app export; source-free solo composites are Eight.
                        let from = if !glow_only && req.view.host_is_float && !solo {
                            Depth::Float
                        } else { Depth::Eight };
                        for export_depth in [Depth::Eight, Depth::Sixteen, Depth::Float] {
                            let expected = if from != export_depth {
                                gpu.convert_depth(legacy_image, from, export_depth)
                            } else { legacy_image.clone() };
                            let expected_bits: Vec<_> = gpu.download_rgba32f(&expected)
                                .into_iter().map(f32::to_bits).collect();
                            for region in &regions {
                                let (actual, quality) = document.render_region(&layers, &req,
                                    *region, region.w, region.h, export_depth);
                                assert_eq!(quality, RegionQuality::Exact);
                                assert_eq!((actual.width(), actual.height()), (region.w, region.h));
                                let actual_bits: Vec<_> = gpu.download_rgba32f(&actual)
                                    .into_iter().map(f32::to_bits).collect();
                                let crop_bits: Vec<_> = (region.y..region.y + region.h)
                                    .flat_map(|y| {
                                        let start = ((y * width + region.x) * 4) as usize;
                                        expected_bits[start..start + (region.w * 4) as usize].iter().copied()
                                    }).collect();
                                assert_eq!(actual_bits, crop_bits,
                                    "HQ export bit mismatch: host={host_depth:?}, export={export_depth:?}, size={width}x{height}, solo={solo}, solo_layer={solo_layer:?}, glow={glow_only}, region={region:?}");
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn refinement_is_incremental_invalidates_dependents_and_survives_draft() {
    let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut document = Document::new(gpu.clone(), gpu.fill(7, 5, [0.8, 0.4, 0.2, 0.7]), Depth::Eight);
    let mask = gpu.fill(7, 5, [0.5, 0.5, 0.5, 1.]);
    let mut settings = [layer(1), layer(2), layer(3), layer(4), layer(5)];
    settings[3].input = LayerInput::Below;
    settings[4].visible = false;
    let mut req = request();
    req.solo_layer = Some(1);
    assert_eq!(document.refine_state(&inputs(&settings), &req.view), RefineState::Idle);
    finish_refine(&mut document, &inputs(&settings), &req.view);
    let bytes = document.refine_bytes();
    assert!(bytes > 0);
    document.render(&inputs(&settings), &req);
    document.clear_stage_caches();
    assert_eq!(document.refine_state(&inputs(&settings), &req.view), RefineState::Ready);
    assert_eq!(document.refine_bytes(), bytes, "Draft must not evict independently retained HQ glows");

    // Editing one Original invalidates it and the later Below, not independent Originals.
    settings[1].params.threshold += 0.2;
    assert_eq!(document.refine_state(&inputs(&settings), &req.view),
        RefineState::Pending { done: 2, total: 4 });
    finish_refine(&mut document, &inputs(&settings), &req.view);
    settings[1].opacity = 0.3;
    settings[1].blend = LayerBlend::Normal;
    assert_eq!(document.refine_state(&inputs(&settings), &req.view),
        RefineState::Pending { done: 3, total: 4 });
    finish_refine(&mut document, &inputs(&settings), &req.view);

    for generation in [1, 2] {
        let masked: Vec<_> = settings.iter().enumerate().map(|(index, settings)| LayerInputs {
            settings, mask: (index == 1).then_some(&mask),
            mask_generation: if index == 1 { generation } else { 0 },
        }).collect();
        assert_eq!(document.refine_state(&masked, &req.view),
            RefineState::Pending { done: 2, total: 4 });
        finish_refine(&mut document, &masked, &req.view);
    }
    finish_refine(&mut document, &inputs(&settings), &req.view);
    settings[4].visible = true;
    assert_eq!(document.refine_state(&inputs(&settings), &req.view),
        RefineState::Pending { done: 4, total: 5 });
    finish_refine(&mut document, &inputs(&settings), &req.view);
    let region = ViewRegion { x: 1, y: 1, w: 3, h: 2 };
    let (_, quality) = document.render_region(&inputs(&settings), &req, region, 3, 2, Depth::Eight);
    assert_eq!(quality, RegionQuality::Exact);
    let (_, quality) = document.render_region(&inputs(&settings), &req, region, 6, 4, Depth::Eight);
    assert_eq!(quality, RegionQuality::Preview);
    req.view.xray = true;
    req.selected = Some(1);
    let (_, quality) = document.render_region(&inputs(&settings), &req, region, 3, 2, Depth::Eight);
    assert_eq!(quality, RegionQuality::Preview);
    document.drop_refine();
    assert_eq!(document.refine_bytes(), 0);
    assert_eq!(document.refine_state(&inputs(&settings), &req.view), RefineState::Idle);
    settings.iter_mut().for_each(|settings| settings.visible = false);
    assert_eq!(document.refine_state(&inputs(&settings), &req.view), RefineState::Ready);
}
