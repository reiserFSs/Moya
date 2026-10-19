//! Chunked lane vs plain Gpu: tiling must be bit-identical, ordered, cancellable and pool-safe.
use super::*;
use std::sync::atomic::AtomicBool;

const D3: [Depth; 3] = [Depth::Eight, Depth::Sixteen, Depth::Float];

fn gpu() -> Gpu {
    Gpu::new_blocking().expect("gpu")
}

fn noise(w: u32, h: u32, seed: u32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(2654435761).max(1);
    (0..w * h * 4)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let v = (s >> 8) as f32 / (1u32 << 24) as f32; // [0,1)
            if i % 4 == 3 { v * 1.2 } else { v * 2.5 - 0.5 } // values <0, >1
        })
        .collect()
}

fn chunking(max_pixels: u64) -> (Chunking, Arc<AtomicBool>) {
    let cancel = Arc::new(AtomicBool::new(false));
    (Chunking { max_pixels, target: Duration::from_secs(10), duty: 1.0, cancel: cancel.clone() }, cancel)
}

fn lane(g: &Gpu, max_pixels: u64) -> Gpu {
    let l = g.lane();
    l.set_chunking(Some(chunking(max_pixels).0));
    l
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|f| f.to_bits()).collect()
}

/// Run `f` on inputs a, b (w x h) plainly and on a tiny-tile lane; assert bit equality.
fn same_on(name: &str, w: u32, h: u32, f: impl Fn(&Gpu, &Image, &Image) -> Image) {
    let g = gpu();
    let a = g.upload_rgba32f(w, h, &noise(w, h, 1));
    let b = g.upload_rgba32f(w, h, &noise(w, h, 2));
    g.flush();
    let want = f(&g, &a, &b);
    let want_px = g.download_rgba32f(&want);
    for max_pixels in [600, 2000] {
        let l = lane(&g, max_pixels);
        let got = f(&l, &a, &b);
        assert_eq!((got.width(), got.height()), (want.width(), want.height()), "{name} size");
        l.flush();
        let got_px = g.download_rgba32f(&got);
        assert!(bits(&got_px) == bits(&want_px), "{name}: tiled (max_pixels={max_pixels}) differs from untiled");
    }
}

fn same(name: &str, f: impl Fn(&Gpu, &Image, &Image) -> Image) {
    same_on(name, 67, 53, &f);
    same_on(name, 131, 97, &f);
}

#[test]
fn tiled_basic_ops() {
    same("copy", |g, a, _| g.copy(a));
    same("fill", |g, a, _| g.fill(a.width(), a.height(), [0.25, 1.5, -0.5, 1.]));
    same("resize_bilinear", |g, a, _| g.resize_bilinear(a, 41, 29));
    same("resize_bilinear_up", |g, a, _| g.resize_bilinear(a, 150, 111));
    same("bicubic_reduce", |g, a, _| g.resize_bicubic(a, a.width() / 4 + 1, a.height() / 5 + 1));
    same("bicubic_enlarge", |g, a, _| g.resize_bicubic(a, 150, 111));
    same("scale_about", |g, a, _| g.scale_about(a, [20.5, 11.25], 1.7));
    same("offset_repeat", |g, a, _| g.offset_repeat(a, 7, -19));
    same("ripple", |g, a, _| g.ripple(a, 40., true, 7));
}

#[test]
fn tiled_blurs() {
    for r in [0.8, 3.5, 8.0, 9.0, 20., 64.] {
        same(&format!("gaussian {r}"), move |g, a, _| g.gaussian_blur(a, r));
    }
    for d in D3 {
        // span <= 256 direct; slope != 0 uses the two-pass shear for integer depths
        same(&format!("motion shear {d:?}"), move |g, a, _| g.motion_blur(a, 30., 40., d));
        same(&format!("motion flat {d:?}"), move |g, a, _| g.motion_blur(a, 0., 25., d));
        same(&format!("motion vertical {d:?}"), move |g, a, _| g.motion_blur(a, 100., 31., d));
    }
    // span > 256 on a > 256 wide image: strip / prefix-scan path
    for d in D3 {
        same_on(&format!("motion long {d:?}"), 301, 37, move |g, a, _| g.motion_blur(a, 10., 280., d));
        same_on(&format!("motion long vertical {d:?}"), 37, 301, move |g, a, _| g.motion_blur(a, 80., 280., d));
    }
    for q in [RadialQuality::Draft, RadialQuality::Good, RadialQuality::Best] {
        same(&format!("radial {q:?}"), move |g, a, _| g.radial_zoom_blur(a, [0.3, 0.6], 40., q));
    }
    for d in D3 {
        same(&format!("smart_sharpen {d:?}"), move |g, a, _| g.smart_sharpen(a, 150., 2.5, d));
        same(&format!("highlights_flatten {d:?}"), move |g, a, _| g.highlights_flatten(a, d));
    }
}

#[test]
fn tiled_color_ops() {
    let stops = [
        GradientStop { pos: 0., rgb: [0., 0.1, 0.4] },
        GradientStop { pos: 0.5, rgb: [1., 0.5, 0.] },
        GradientStop { pos: 1., rgb: [1., 1., 0.9] },
    ];
    for d in D3 {
        same(&format!("add_noise {d:?}"), move |g, a, _| g.add_noise(a, 30., false, 99, d));
        same(&format!("add_noise mono {d:?}"), move |g, a, _| g.add_noise(a, 30., true, 5, d));
        same(&format!("vibrance {d:?}"), move |g, a, _| g.vibrance(a, 40., -20., d));
        same(&format!("hue_sat {d:?}"), move |g, a, _| g.hue_saturation(a, 30., 20., -10., false, d));
        same(&format!("colorize {d:?}"), move |g, a, _| g.hue_saturation(a, 200., 50., 5., true, d));
        same(&format!("levels {d:?}"), move |g, a, _| g.levels(a, 0.1, 0.9, 1.3, 0.05, 0.95, d));
        same(&format!("exposure {d:?}"), move |g, a, _| g.exposure(a, 0.7, 0.02, 1.2, d));
        same(&format!("desaturate {d:?}"), move |g, a, _| g.desaturate(a, d));
        same(&format!("luminance {d:?}"), move |g, a, _| g.luminance(a, d));
        same(&format!("quantize {d:?}"), move |g, a, _| g.quantize(a, d));
        same(&format!("quantize_owned {d:?}"), move |g, a, _| {
            let c = g.copy(a);
            g.quantize_owned(g.hue_saturation(&c, 10., 10., 0., false, Depth::Float), d)
        });
    }
    for d in [Depth::Eight, Depth::Sixteen] {
        same(&format!("gradient_map {d:?}"), move |g, a, _| g.gradient_map(a, &stops, d));
    }
    same("black_and_white", |g, a, _| g.black_and_white_default(a));
    same("tonemap_exp", |g, a, _| g.tonemap_exp(a, 1.5, 1.2));
    same("unmult", |g, a, _| g.unmult(a));
    same("convert_depth", |g, a, _| {
        let mut out = g.copy(a);
        for f in D3 {
            for t in D3 {
                out = g.convert_depth(&out, f, t);
            }
        }
        out
    });
    for f in D3 {
        for t in D3 {
            same(&format!("convert {f:?}->{t:?}"), move |g, a, _| g.convert_depth(a, f, t));
        }
    }
}

#[test]
fn tiled_binary_ops() {
    for mode in [BlendMode::Normal, BlendMode::Screen, BlendMode::LinearDodge] {
        for d in D3 {
            same(&format!("blend {mode:?} {d:?}"), move |g, a, b| g.blend(a, b, mode, 0.7, 0.9, d));
            same(&format!("apply_image {mode:?} {d:?}"), move |g, a, b| g.apply_image(a, b, mode, 0.6, true, d));
        }
        same(&format!("apply_to_channel {mode:?}"), move |g, a, b| g.apply_to_channel(a, b, 1, mode, 0.8));
    }
    same("extract_channel", |g, a, _| g.extract_channel(a, 2));
    same("mask_alpha", |g, a, b| g.mask_alpha(a, b));
    same("mix_by_mask", |g, a, b| {
        let m = g.extract_channel(b, 0);
        g.mix_by_mask(a, b, &m)
    });
}

#[test]
fn tiled_region_ops() {
    for (w, h, r) in [
        (67, 53, Region { x: 5, y: 3, w: 40, h: 35 }),
        (131, 97, Region { x: 37, y: 11, w: 83, h: 70 }),
    ] {
        let name = |s: &str| format!("{s} {w}x{h}");
        same_on(&name("copy_region full"), w, h, move |g, a, _| g.copy_region(a, w, h, r));
        same_on(&name("copy_region cropped"), w, h, move |g, a, _| {
            let c = g.copy_region(a, w, h, r);
            g.copy_region(&c, w, h, r)
        });
        same_on(&name("resize_bilinear_region"), w, h, move |g, a, _| g.resize_bilinear_region(a, 2 * w + 1, 2 * h + 1, Region { x: r.x, y: r.y, w: r.w, h: r.h }));
        same_on(&name("bicubic_region enlarge"), w, h, move |g, a, _| g.resize_bicubic_region(a, w + 20, h + 9, r));
        same_on(&name("bicubic_region reduce"), w, h, move |g, a, _| {
            let (fw, fh) = (w / 3, h / 3);
            g.resize_bicubic_region(a, fw, fh, Region { x: 2, y: 1, w: fw - 2, h: fh - 1 })
        });
        same_on(&name("unmult_region"), w, h, move |g, a, _| g.unmult_region(a, w, h, r));
        same_on(&name("quantize_region"), w, h, move |g, a, _| g.quantize_region(a, Depth::Eight, w, h, r));
        same_on(&name("convert_region"), w, h, move |g, a, _| g.convert_depth_region(a, Depth::Float, Depth::Sixteen, w, h, r));
        same_on(&name("blend_region"), w, h, move |g, a, b| g.blend_region(a, b, BlendMode::Screen, 0.8, 1., Depth::Float, w, h, r));
        same_on(&name("blend_region cropped"), w, h, move |g, a, b| {
            let (ca, cb) = (g.copy_region(a, w, h, r), g.copy_region(b, w, h, r));
            g.blend_region(&ca, &cb, BlendMode::Normal, 1., 1., Depth::Float, w, h, r)
        });
        for filter in [Filter::Nearest, Filter::Bilinear] {
            same_on(&format!("sample_view {filter:?} {w}x{h}"), w, h, move |g, a, _| {
                let map = ViewMap { origin_x: -3.3, origin_y: 4.7, src_per_dst: 0.37 };
                g.sample_view(a, map, 61, 49, filter)
            });
            same_on(&format!("sample_view zoomout {filter:?} {w}x{h}"), w, h, move |g, a, _| {
                let map = ViewMap { origin_x: 2.0, origin_y: -5.0, src_per_dst: 1.9 };
                g.sample_view(a, map, 45, 37, filter)
            });
        }
    }
}

#[test]
fn lane_and_base_see_each_others_writes() {
    let g = gpu();
    let (w, h) = (67, 53);
    let l = lane(&g, 600);
    let img = g.upload_rgba32f(w, h, &noise(w, h, 3));
    g.flush();
    let patch = noise(10, 20, 4);
    g.write_region(&img, 4, 9, 10, 20, &patch);
    g.flush();
    // lane reads what the base wrote
    let via_lane = l.copy(&img);
    l.flush();
    let mut want = noise(w, h, 3);
    for y in 0..20usize {
        let dst = ((9 + y) * w as usize + 4) * 4;
        want[dst..dst + 40].copy_from_slice(&patch[y * 40..(y + 1) * 40]);
    }
    assert!(bits(&g.download_rgba32f(&via_lane)) == bits(&want));
    // base reads what the lane produced
    let other = g.upload_rgba32f(w, h, &noise(w, h, 5));
    g.flush();
    let lane_out = l.gaussian_blur(&via_lane, 3.);
    l.flush();
    let mixed = g.blend(&lane_out, &other, BlendMode::Normal, 0.5, 1., Depth::Float);
    let want_mixed = g.blend(&g.gaussian_blur(&via_lane, 3.), &other, BlendMode::Normal, 0.5, 1., Depth::Float);
    assert!(bits(&g.download_rgba32f(&mixed)) == bits(&g.download_rgba32f(&want_mixed)));
}

fn long_sequence(g: &Gpu, a: &Image, b: &Image) -> Image {
    let mut x = g.copy(a);
    for i in 0..6 {
        x = g.gaussian_blur(&x, 12. + i as f32);
        x = g.blend(&x, b, BlendMode::Normal, 0.5, 1., Depth::Float);
        x = g.motion_blur(&x, 20., 30., Depth::Float);
        x = g.resize_bicubic(&x, a.width(), a.height());
    }
    x
}

#[test]
fn cancel_leaves_lane_and_base_usable() {
    let g = gpu();
    let (w, h) = (131, 97);
    let a = g.upload_rgba32f(w, h, &noise(w, h, 1));
    let b = g.upload_rgba32f(w, h, &noise(w, h, 2));
    g.flush();
    let want = bits(&g.download_rgba32f(&long_sequence(&g, &a, &b)));

    let l = g.lane();
    let (cfg, cancel) = chunking(600);
    l.set_chunking(Some(cfg.clone()));

    // cancelled before start
    cancel.store(true, Ordering::Relaxed);
    assert!(l.cancelled());
    let _ = long_sequence(&l, &a, &b);
    l.discard_pending();

    // cancelled from another thread mid-sequence (duty < 1 so the run takes a while)
    cancel.store(false, Ordering::Relaxed);
    l.set_chunking(Some(Chunking { duty: 0.2, ..cfg.clone() }));
    let flag = cancel.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        flag.store(true, Ordering::Relaxed);
    });
    let _ = long_sequence(&l, &a, &b);
    stopper.join().unwrap();
    assert!(l.cancelled());
    l.discard_pending();

    // same lane, flag cleared, still chunked: correct
    cancel.store(false, Ordering::Relaxed);
    l.set_chunking(Some(cfg));
    let out = long_sequence(&l, &a, &b);
    l.flush();
    assert!(bits(&g.download_rgba32f(&out)) == want, "lane after cancel");

    // chunking removed: lane and base still correct
    l.set_chunking(None);
    assert!(!l.cancelled());
    let out = long_sequence(&l, &a, &b);
    l.flush();
    assert!(bits(&g.download_rgba32f(&out)) == want, "unchunked lane after cancel");
    assert!(bits(&g.download_rgba32f(&long_sequence(&g, &a, &b))) == want, "base after cancel");
}

#[test]
fn image_from_lane_thread_dropped_elsewhere() {
    let g = gpu();
    let (w, h) = (67, 53);
    let l = lane(&g, 600);
    let src = g.upload_rgba32f(w, h, &noise(w, h, 1));
    g.flush();
    let made: Vec<Image> = std::thread::scope(|s| {
        s.spawn(|| {
            let v: Vec<Image> = (0..4).map(|_| l.gaussian_blur(&src, 5.)).chain([l.fill(w, h, [1.; 4])]).collect();
            l.flush();
            v
        })
        .join()
        .unwrap()
    });
    drop(made); // dropped on main, not the creating thread
    let want = g.download_rgba32f(&g.gaussian_blur(&src, 5.));
    let on_lane = l.gaussian_blur(&src, 5.);
    l.flush();
    assert!(bits(&g.download_rgba32f(&on_lane)) == bits(&want));
    let on_base = g.gaussian_blur(&src, 5.);
    assert!(bits(&g.download_rgba32f(&on_base)) == bits(&want));
}

#[test]
fn chunked_lane_splits_dispatches_into_16_row_tiles() {
    let g = gpu();
    assert_eq!(g.next_tile_rows(0, 1., 67, 53), Some(53), "plain gpu never tiles");
    let l = lane(&g, 600);
    // 600 px budget, 67 px rows: below the 16-row minimum, so exactly 16-row tiles.
    assert_eq!(l.next_tile_rows(0, 1., 67, 53), Some(16));
    assert_eq!(l.next_tile_rows(0, 1., 67, 10), Some(10), "the last tile is the remainder");
    let (cfg, cancel) = chunking(600);
    l.set_chunking(Some(cfg));
    cancel.store(true, Ordering::Relaxed);
    assert_eq!(l.next_tile_rows(0, 1., 67, 53), None, "cancelled lane yields no more tiles");
}
