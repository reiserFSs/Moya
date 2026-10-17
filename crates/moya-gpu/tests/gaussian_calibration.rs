use moya_gpu::{BlurSource, Depth, Gpu};

#[test]
fn photoshop_float_impulse_coefficients() {
    let gpu = Gpu::new_blocking().expect("GPU required");
    let mut pixels = vec![0.; 65 * 65 * 4];
    for channel in 0..4 { pixels[(32 * 65 + 32) * 4 + channel] = 1.; }
    let image = gpu.upload_rgba32f(65, 65, &pixels);
    // Center and adjacent horizontal values from captured Photoshop 27.11
    // Float impulse TIFFs; these distinguish the PS kernel from sigma=radius.
    for (radius, center, neighbor) in [
        (0.25, 0.64752197265625, 0.078582763671875),
        (0.5, 0.31640625, 0.120849609375),
        (1., 0.140625, 0.087890625),
        (2., 0.03515625, 0.0322265625),
        (2.5, 0.02209443598985672, 0.020699771121144295),
        (5., 0.006074604112654924, 0.005966399796307087),
    ] {
        let result = gpu.download_rgba32f(&gpu.gaussian_blur(&image, radius));
        assert!((result[(32 * 65 + 32) * 4] - center).abs() < 2e-7, "radius {radius} center");
        assert!((result[(32 * 65 + 33) * 4] - neighbor).abs() < 2e-7, "radius {radius} neighbor");
        let mass: f32 = result.chunks_exact(4).map(|p| p[0]).sum();
        assert!((mass - 1.).abs() < 2e-5, "radius {radius} mass");
    }
}

#[test]
fn shared_glow_reductions_preserve_every_pixel_and_chunk() {
    let gpu = Gpu::new_blocking().expect("GPU required");
    for (w, h) in [(257, 193), (1, 73), (97, 1)] {
        let pixels: Vec<f32> = (0..w * h * 4)
            .map(|i| ((i * 73 + i / 7) % 1021) as f32 / 997.)
            .collect();
        let image = gpu.upload_rgba32f(w, h, &pixels);
        for depth in [Depth::Float, Depth::Sixteen] {
            let mut source = BlurSource::new(&gpu, &image);
            // Draft, native-size portrait HQ, and the 4000px HQ ceiling.
            for scale in [1., 1448. / 1300., 4000. / 1300.] {
                for i in 0..12 {
                    let radius = 1000. * scale * 0.5_f32.powi(i);
                    let first = radius.min(1000.).max(0.1);
                    let mut expected = gpu.quantize_owned(gpu.gaussian_blur(&image, first), depth);
                    let mut actual = gpu.quantize_owned(source.blur(first), depth);
                    let mut remaining = radius - 1000.;
                    while remaining > 0. {
                        let chunk = remaining.min(1000.).max(0.1);
                        expected = gpu.quantize_owned(gpu.gaussian_blur(&expected, chunk), depth);
                        actual = gpu.quantize_owned(gpu.gaussian_blur(&actual, chunk), depth);
                        remaining -= 1000.;
                    }
                    let bits = |img: &moya_gpu::Image| {
                        gpu.download_rgba32f(img).into_iter().map(f32::to_bits).collect::<Vec<_>>()
                    };
                    assert_eq!(bits(&actual), bits(&expected),
                        "{w}x{h}, {depth:?}, scale={scale}, layer={i}");
                }
            }
        }
    }
}
