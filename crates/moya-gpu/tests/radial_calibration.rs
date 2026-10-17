//! Regression for the PS 27.11 normalized-center and symmetric-support calibration.
//! This checks our deterministic quadrature, not Photoshop's unmatched random sampler.
use moya_gpu::{Gpu, RadialQuality};

#[test]
fn normalized_center_clamps_and_zoom_samples_both_radial_directions() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    let (width, height) = (65u32, 49u32);
    let impulse = [40.0f32, 20.0];
    let mut pixels = vec![0.0; (width * height * 4) as usize];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 1.0;
    }
    let index = ((20 * width + 40) * 4) as usize;
    pixels[index..index + 3].fill(1.0);
    let image = gpu.upload_rgba32f(width, height, &pixels);
    for (quality, taps) in [
        (RadialQuality::Draft, 16),
        (RadialQuality::Good, 64),
        (RadialQuality::Best, 256),
    ] {
        for center in [[0.25f32, 0.75], [0.503, 0.497], [0.0, 0.0], [1.0, 1.0], [128.0, 128.0]] {
            for amount in [0.0f32, 10.0, 50.0, 100.0] {
                let actual = gpu.download_rgba32f(
                    &gpu.radial_zoom_blur(&image, center, amount, quality),
                );
                let c = [
                    (center[0].clamp(0.0, 1.0) * 128.0).floor() * (width as f32 / 128.0) - 1.0,
                    (center[1].clamp(0.0, 1.0) * 128.0).floor() * (height as f32 / 128.0) - 1.0,
                ];
                let mut inward = 0.0;
                let mut outward = 0.0;
                for y in 0..height {
                    for x in 0..width {
                        let mut expected = 0.0f32;
                        for k in 0..taps {
                            let scale = 1.0 + amount / 200.0
                                * ((k as f32 + 0.5) / taps as f32 - 0.5);
                            let q = [
                                c[0] + (x as f32 - c[0]) * scale,
                                c[1] + (y as f32 - c[1]) * scale,
                            ];
                            expected += (1.0 - (q[0] - impulse[0]).abs()).max(0.0)
                                * (1.0 - (q[1] - impulse[1]).abs()).max(0.0)
                                / taps as f32;
                        }
                        let offset = ((y * width + x) * 4) as usize;
                        for channel in 0..3 {
                            assert!((actual[offset + channel] - expected).abs() < 3e-5,
                                "center={center:?}, amount={amount}, taps={taps}, pixel=({x},{y})");
                        }
                        assert!((actual[offset + 3] - 1.0).abs() < 3e-5);
                        if x < 40 { inward += actual[offset]; }
                        if x > 40 { outward += actual[offset]; }
                    }
                }
                if amount > 0.0 && center[0] == 0.25 {
                    assert!(inward > 0.0 && outward > 0.0, "zoom must spread both ways");
                }
            }
        }
    }
}

#[test]
fn measured_amount_ten_matches_center_response_at_every_depth() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    let mut pixels = vec![0.0; 256 * 256 * 4];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 1.0;
    }
    pixels[(128 * 256 + 128) * 4..(128 * 256 + 128) * 4 + 3].fill(1.0);
    let image = gpu.upload_rgba32f(256, 256, &pixels);
    // PS 27.11 radialBlur-focus-{depth}-{quality}-center, normalized TIFF codes.
    // The tolerance covers observed stochastic phase, not the old half-pixel center.
    for (quality, measured) in [
        (RadialQuality::Draft, [249.0 / 255.0, 63911.0 / 65535.0, 0.975237]),
        (RadialQuality::Good, [249.0 / 255.0, 63909.0 / 65535.0, 0.975185]),
        (RadialQuality::Best, [248.0 / 255.0, 63909.0 / 65535.0, 0.9751892]),
    ] {
        let result = gpu.download_rgba32f(&gpu.radial_zoom_blur(
            &image, [0.5, 0.5], 10.0, quality,
        ));
        let peak = result[(128 * 256 + 128) * 4];
        for (depth, expected) in measured.into_iter().enumerate() {
            let tolerance = if depth == 0 { 1.0 / 255.0 } else { 0.0003 };
            assert!((peak - expected).abs() < tolerance, "{quality:?}, depth index {depth}: {peak} vs {expected}");
        }
        assert_eq!(result[(127 * 256 + 128) * 4], 0.0);
        assert_eq!(result[(128 * 256 + 127) * 4], 0.0);
    }
    let mut odd = vec![0.0; 65 * 49 * 4];
    for pixel in odd.chunks_exact_mut(4) {
        pixel[3] = 1.0;
    }
    odd[(23 * 65 + 31) * 4..(23 * 65 + 31) * 4 + 3].fill(1.0);
    let image = gpu.upload_rgba32f(65, 49, &odd);
    let result = gpu.download_rgba32f(&gpu.radial_zoom_blur(
        &image, [0.5, 0.5], 100.0, RadialQuality::Best,
    ));
    // radialBlur-station-odd: distinguish normalized bins from a two-pixel canvas grid.
    assert!((result[(23 * 65 + 31) * 4] - 0.88013136).abs() < 0.0003);
}
