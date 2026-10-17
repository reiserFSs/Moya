//! Mandatory-device numerical checks against the documented implementable
//! baselines. No adapter means a test failure.
use moya_gpu::{BlendMode, Depth, Gpu, reference};
use std::time::Instant;

fn close(label: &str, actual: &[f32], expected: &[f32], tolerance: f32) {
    assert_eq!(actual.len(), expected.len(), "{label}: length");
    let mut maximum = 0.0f32;
    let mut worst = 0;
    for (index, (&a, &b)) in actual.iter().zip(expected).enumerate() {
        assert!(
            a.is_finite() && b.is_finite(),
            "{label}: nonfinite channel {index}: {a} versus {b}"
        );
        let error = (a - b).abs();
        if error > maximum {
            maximum = error;
            worst = index;
        }
    }
    assert!(
        maximum <= tolerance,
        "{label}: max error {maximum} > {tolerance} at channel {worst}: {} versus {}",
        actual[worst],
        expected[worst]
    );
}

fn decode(x: f32) -> f32 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(x: f32) -> f32 {
    if x <= 0.0031308 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

#[test]
fn levels_exposure_and_storage() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    let pixels = [
        -0.1, 0.0, 0.04045, 0.23, 0.18, 0.4, 0.8, 0.67, 1.0, 1.4, 3.0, 1.0,
    ];
    let image = gpu.upload_rgba32f(3, 1, &pixels);
    for &(ib, iw, gamma, ob, ow) in &[
        (0.0, 255.0, 1.0, 0.0, 255.0),
        (23.0, 217.0, 1.7, 15.0, 239.0),
        (0.0, 201.0, 0.37, 255.0, 0.0),
    ] {
        for depth in [Depth::Eight, Depth::Sixteen, Depth::Float] {
            close(
                "levels",
                &gpu.download_rgba32f(&gpu.levels(&image, ib, iw, gamma, ob, ow, depth)),
                &reference::levels(&pixels, ib, iw, gamma, ob, ow, depth),
                2e-5,
            );
        }
    }
    // Photoshop 27.11 levels-8-ramp256-00106: the shadow toe is not plain pow.
    let toe = gpu.upload_rgba32f(2, 1, &[1./255., 2./255., 3./255., 1., 4./255., 5./255., 6./255., 1.]);
    close(
        "Photoshop Levels shadow toe",
        &gpu.download_rgba32f(&gpu.quantize(&gpu.levels(&toe, 0., 235., 2.2, 0., 255., Depth::Eight), Depth::Eight)),
        &[5./255., 9./255., 13./255., 1., 18./255., 22./255., 30./255., 1.],
        1.0001 / 255.,
    );
    // levels-focus-4 distinguishes the measured 4096 junction grid from 2048.
    let junction = gpu.fill(1, 1, [1145./32768., 1145./32768., 1145./32768., 1.]);
    close(
        "Photoshop Levels 4096-grid junction",
        &gpu.download_rgba32f(&gpu.levels(&junction, 0., 255., 4., 0., 255., Depth::Sixteen)),
        &[13247./32768., 13247./32768., 13247./32768., 1.],
        5. / 32768.,
    );
    for depth in [Depth::Eight, Depth::Sixteen, Depth::Float] {
        for (stops, offset, gamma) in [(1.3f32, 0.04f32, 1.4f32), (-1.0, -0.2, 1.0), (0.0, -0.1, 2.2), (0.0, 0.0, 1.0)] {
            let expected = reference::exposure(&pixels, stops, offset, gamma, depth);
            close(
                "exposure",
                &gpu.download_rgba32f(&gpu.exposure(&image, stops, offset, gamma, depth)),
                &expected,
                3e-5,
            );
        }
        let scale = if depth == Depth::Eight {
            255.0
        } else {
            32768.0
        };
        let expected: Vec<f32> = pixels
            .iter()
            .map(|&v| {
                if depth == Depth::Float {
                    v
                } else {
                    (v.clamp(0.0, 1.0) * scale + 0.5).floor() / scale
                }
            })
            .collect();
        close(
            "quantize",
            &gpu.download_rgba32f(&gpu.quantize(&image, depth)),
            &expected,
            1e-7,
        );
    }
}

#[test]
fn every_blend_mode_opacity_fill_and_alpha() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    let base = [
        0.0, 0.2, 0.8, 1.0, 0.1, 0.5, 0.9, 0.4, 0.85, 0.3, 0.15, 0.0, 0.2, 0.6, 0.4, 0.7,
    ];
    let top = [
        1.0, 0.0, 0.7, 1.0, 0.8, 0.2, 0.6, 0.65, 0.1, 0.9, 0.2, 0.8, 0.9, 0.2, 0.8, 0.0,
    ];
    let b = gpu.upload_rgba32f(4, 1, &base);
    let s = gpu.upload_rgba32f(4, 1, &top);
    for mode in [
        BlendMode::Normal,
        BlendMode::Screen,
        BlendMode::LinearDodge,
        BlendMode::ColorDodge,
        BlendMode::Multiply,
        BlendMode::Lighten,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::Divide,
        BlendMode::Color,
    ] {
        for depth in [Depth::Eight, Depth::Sixteen, Depth::Float] {
            for (opacity, fill) in [(1.0, 1.0), (0.37, 0.23), (0.0, 1.0), (1.0, 0.0)] {
                let expected = reference::blend(&base, &top, mode, opacity, fill, depth);
                close(
                    &format!("{mode:?}/{depth:?}/opacity={opacity}/fill={fill}"),
                    &gpu.download_rgba32f(&gpu.blend(&b, &s, mode, opacity, fill, depth)),
                    &expected,
                    3e-5,
                );
            }
        }
    }
    // Distinguish special Fill from ordinary opacity using independent scalar answers.
    let b = gpu.fill(1, 1, [0.4, 0.4, 0.4, 1.0]);
    let s = gpu.fill(1, 1, [0.8, 0.8, 0.8, 1.0]);
    // Corpus multiply-32: 20% opacity × 60% fill rounds to 31/255 coverage.
    for depth in [Depth::Eight, Depth::Sixteen, Depth::Float] {
        let blended = if depth == Depth::Eight { 82.0 / 255.0 } else { 0.4 * 0.8 };
        let value = 0.4 + (blended - 0.4) * (31.0 / 255.0);
        close(
            "ordinary fill uses byte coverage",
            &gpu.download_rgba32f(&gpu.blend(&b, &s, BlendMode::Multiply, 0.2, 0.6, depth)),
            &[value, value, value, 1.0],
            2e-6,
        );
    }
    close(
        "dodge Fill scales source",
        &gpu.download_rgba32f(&gpu.blend(&b, &s, BlendMode::ColorDodge, 1.0, 0.5, Depth::Sixteen)),
        &[2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 1.0],
        2e-6,
    );
    close(
        "dodge opacity interpolates clipped result",
        &gpu.download_rgba32f(&gpu.blend(&b, &s, BlendMode::ColorDodge, 0.5, 1.0, Depth::Sixteen)),
        &[0.7, 0.7, 0.7, 1.0],
        2e-6,
    );
    let hdr = gpu.fill(1, 1, [2.0, 3.0, 4.0, 1.0]);
    close(
        "HDR additive Fill",
        &gpu.download_rgba32f(&gpu.blend(&hdr, &s, BlendMode::LinearDodge, 1.0, 0.5, Depth::Float)),
        &[2.4, 3.4, 4.4, 1.0],
        2e-6,
    );
}

#[test]
fn hue_saturation_tonemap_unmult_and_offset() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    let rgb = gpu.upload_rgba32f(2, 1, &[0.8, 0.2, 0.2, 0.3, 0.4, 0.4, 0.4, 0.8]);
    for (hue, sat, light, colorize, expected) in [
        (
            120.0,
            0.0,
            0.0,
            false,
            [0.2, 0.8, 0.2, 0.3, 0.4, 0.4, 0.4, 0.8],
        ),
        (
            -120.0,
            0.0,
            0.0,
            false,
            [0.2, 0.2, 0.8, 0.3, 0.4, 0.4, 0.4, 0.8],
        ),
        (
            0.0,
            -100.0,
            0.0,
            false,
            [0.5, 0.5, 0.5, 0.3, 0.4, 0.4, 0.4, 0.8],
        ),
        (
            0.0,
            25.0,
            0.0,
            false,
            [0.9, 0.1, 0.1, 0.3, 0.4, 0.4, 0.4, 0.8],
        ),
        (
            0.0,
            0.0,
            50.0,
            false,
            [0.9, 0.6, 0.6, 0.3, 0.7, 0.7, 0.7, 0.8],
        ),
        (
            0.0,
            0.0,
            -50.0,
            false,
            [0.4, 0.1, 0.1, 0.3, 0.2, 0.2, 0.2, 0.8],
        ),
        (
            240.0,
            100.0,
            0.0,
            true,
            [0.0, 0.0, 1.0, 0.3, 0.0, 0.0, 0.8, 0.8],
        ),
    ] {
        close(
            "hue/saturation",
            &gpu.download_rgba32f(&gpu.hue_saturation(&rgb, hue, sat, light, colorize, Depth::Sixteen)),
            &expected,
            3e-5,
        );
    }
    let primary = gpu.upload_rgba32f(1, 1, &[1., 0., 0., 0.3]);
    close(
        "Float hue/saturation reciprocal cap",
        &gpu.download_rgba32f(&gpu.hue_saturation(&primary, 0., 100., 0., false, Depth::Float)),
        &[64.498046875, -63.498046875, -63.498046875, 0.3],
        1e-4,
    );
    let dark_red = gpu.upload_rgba32f(1, 1, &[0.4, 0.2, 0.2, 0.7]);
    close(
        "Integer lightness precedes saturation",
        &gpu.download_rgba32f(&gpu.hue_saturation(&dark_red, 0., 100., 50., false, Depth::Sixteen)),
        &[1., 0.3, 0.3, 0.7],
        5e-5,
    );
    for (input, hue, expected) in [
        ([191., 176., 52.], 0., [243., 215., 0.]),
        ([191., 124., 58.], -90., [249., 0., 248.]),
    ] {
        let byte = gpu.upload_rgba32f(1, 1, &[input[0] / 255., input[1] / 255., input[2] / 255., 0.7]);
        close(
            "Photoshop byte hue sextant rounding",
            &gpu.download_rgba32f(&gpu.quantize(&gpu.hue_saturation(&byte, hue, 100., 0., false, Depth::Eight), Depth::Eight)),
            &[expected[0] / 255., expected[1] / 255., expected[2] / 255., 0.7],
            1.01 / 255.,
        );
    }
    let pixels = [
        0.0f32, 0.18, 2.0, 0.25, 0.4, 0.8, 0.2, 0.7, 0.0, 0.0, 0.0, 1.0,
    ];
    let input = gpu.upload_rgba32f(3, 1, &pixels);
    for exposure in [0.0f32, 0.7, 3.0] {
        let mut expected = pixels.to_vec();
        for p in expected.chunks_exact_mut(4) {
            for v in &mut p[..3] {
                *v = ((1.0 - (-3.0 * exposure * *v).exp()).powf(1.0 / 1.3) * 10000.0).round()
                    / 10000.0;
            }
            p[3] = 1.0;
        }
        close(
            "tonemap",
            &gpu.download_rgba32f(&gpu.tonemap_exp(&input, exposure, 1.3)),
            &expected,
            1.1e-4,
        );
    }
    close(
        "unmult including black",
        &gpu.download_rgba32f(&gpu.unmult(&input)),
        &[0.0, 0.09, 1.0, 2.0, 0.5, 1.0, 0.25, 0.8, 0.0, 0.0, 0.0, 0.0],
        2e-6,
    );
    let unique: Vec<f32> = (0..3 * 2 * 4).map(|v| v as f32 * 0.13 - 0.4).collect();
    let input = gpu.upload_rgba32f(3, 2, &unique);
    for (dx, dy) in [(1i32, 1i32), (-7, 5), (9, -4), (0, 0)] {
        let mut expected = vec![0.0; unique.len()];
        for y in 0..2i32 {
            for x in 0..3i32 {
                let source = (((y - dy).clamp(0, 1) * 3 + (x - dx).clamp(0, 2)) * 4) as usize;
                let dest = ((y * 3 + x) * 4) as usize;
                expected[dest..dest + 4].copy_from_slice(&unique[source..source + 4]);
            }
        }
        close(
            "offset repeated edges",
            &gpu.download_rgba32f(&gpu.offset_repeat(&input, dx, dy)),
            &expected,
            0.0,
        );
    }
}

fn smooth_image(width: u32, height: u32) -> Vec<f32> {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            data.extend_from_slice(&[
                0.4 + 0.15 * (u * 4.0).sin() * (v * 3.0).cos(),
                0.2 + 0.3 * u * v,
                0.6 + 0.2 * (u * 2.0 + v).cos(),
                0.7 + 0.1 * (u + v).sin(),
            ]);
        }
    }
    data
}

#[test]
fn gaussian_matches_measured_clamped_kernel_reference() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    // Modest dimensions keep the exact CPU oracle cheap even for radius=3000.
    for (w, h) in [(97, 65), (1, 17), (19, 1)] {
        let pixels = smooth_image(w, h);
        let image = gpu.upload_rgba32f(w, h, &pixels);
        for radius in [0.0, 0.5, 2.0, 8.0, 50.0, 500.0, 3000.0] {
            let expected = reference::gaussian_blur(&pixels, w, h, radius);
            close(
                &format!("Gaussian {w}x{h} radius {radius}"),
                &gpu.download_rgba32f(&gpu.gaussian_blur(&image, radius)),
                &expected,
                1.0 / 255.0,
            );
        }
    }
}

#[test]
fn gaussian_large_image_timings() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    // Normal (not ignored) test; no giant CPU convolution or readback allocation.
    // Queue completion is awaited so these are wall times, not encoder timings.
    for (width, height) in [(1500, 1000), (4000, 2667)] {
        let input = gpu.fill(width, height, [0.2, 0.4, 0.8, 1.0]);
        gpu.flush();
        gpu.device()
            .poll(moya_gpu::wgpu::PollType::wait_indefinitely())
            .expect("GPU warmup completion");
        for radius in [50.0, 500.0, 3000.0] {
            let start = Instant::now();
            let blurred = gpu.gaussian_blur(&input, radius);
            gpu.flush();
            gpu.device()
                .poll(moya_gpu::wgpu::PollType::wait_indefinitely())
                .expect("GPU timing completion");
            eprintln!(
                "Gaussian {width}x{height}, radius {radius}: {:.3} ms (encode + submit + GPU completion)",
                start.elapsed().as_secs_f64() * 1000.0
            );
            assert_eq!((blurred.width(), blurred.height()), (width, height));
        }
    }
}

#[test]
fn remaining_public_operations_have_numeric_checks() {
    use moya_gpu::{GradientStop, RadialQuality};
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    let pixels = [0.8, 0.2, 0.4, 0.3, 0.1, 0.6, 0.9, 0.7, 0.4, 0.4, 0.4, 1.0];
    let image = gpu.upload_rgba32f(3, 1, &pixels);
    let check = |label: &str, result: moya_gpu::Image, expected: &[f32]| {
        close(label, &gpu.download_rgba32f(&result), expected, 3e-5);
    };
    check("copy", gpu.copy(&image), &pixels);
    let color = [0.2, 0.4, 0.8, 0.6];
    let constant = gpu.fill(7, 5, color);
    check("fill", gpu.copy(&constant), &color.repeat(35));
    for (w, h) in [(19, 11), (2, 1)] {
        check(
            "bilinear constant",
            gpu.resize_bilinear(&constant, w, h),
            &color.repeat((w * h) as usize),
        );
        check(
            "bicubic constant",
            gpu.resize_bicubic(&constant, w, h),
            &color.repeat((w * h) as usize),
        );
    }
    check(
        "scale one",
        gpu.scale_about(&image, [1.5, 0.5], 1.0),
        &pixels,
    );
    check(
        "long motion constant",
        gpu.motion_blur(&constant, 37.0, 6000.0, Depth::Float),
        &color.repeat(35),
    );
    for quality in [
        RadialQuality::Draft,
        RadialQuality::Good,
        RadialQuality::Best,
    ] {
        check(
            "radial constant",
            gpu.radial_zoom_blur(&constant, [1.7, 3.2], 91.0, quality),
            &color.repeat(35),
        );
    }
    for large in [false, true] {
        check("ripple zero", gpu.ripple(&image, 0.0, large, 123), &pixels);
    }
    check("sharpen zero", gpu.smart_sharpen(&image, 0.0, 4.0, Depth::Float), &pixels);
    for depth in [Depth::Eight, Depth::Sixteen, Depth::Float] {
        check("sharpen subpixel bypass", gpu.smart_sharpen(&image, 50., 0.02, depth), &pixels);
        check("sharpen constant", gpu.smart_sharpen(&constant, 500., 1., depth), &color.repeat(35));
    }
    let mut impulse = [0.; 9 * 9 * 4];
    for pixel in impulse.chunks_exact_mut(4) { pixel[3] = 0.6; }
    impulse[(4 * 9 + 4) * 4] = 1.;
    let impulse_image = gpu.upload_rgba32f(9, 9, &impulse);
    let hdr = gpu.download_rgba32f(&gpu.smart_sharpen(&impulse_image, 500., 1., Depth::Float));
    assert!(hdr[(4 * 9 + 4) * 4] > 1.);
    assert!(hdr[(4 * 9 + 3) * 4] < 0.);
    for depth in [Depth::Eight, Depth::Sixteen] {
        check("sharpen saturated impulse", gpu.smart_sharpen(&impulse_image, 2500., 1., depth), &impulse);
    }
    assert!(hdr.chunks_exact(4).all(|pixel| pixel[3] == 0.6));
    check(
        "noise zero",
        gpu.add_noise(&image, 0.0, false, 123, Depth::Sixteen),
        &pixels,
    );
    for monochrome in [false, true] {
        let first = gpu.download_rgba32f(&gpu.add_noise(&image, 12.0, monochrome, 123, Depth::Sixteen));
        let repeated = gpu.download_rgba32f(&gpu.add_noise(&image, 12.0, monochrome, 123, Depth::Sixteen));
        close("noise seed reproducibility", &first, &repeated, 0.0);
        assert!(
            first
                .iter()
                .zip(pixels)
                .enumerate()
                .any(|(i, (a, b))| i % 4 != 3 && (a - b).abs() > 1e-5),
            "nonzero noise must alter RGB"
        );
        for i in [3, 7, 11] {
            assert_eq!(first[i], pixels[i], "noise preserves alpha");
        }
    }
    // PS 0.3% Gaussian: fit 8-bit pre-rounding sigma; .003 at16, .006 in Float.
    let noise_field = gpu.fill(256, 256, [0.5, 0.5, 0.5, 0.7]);
    for (d, sigma) in [(Depth::Eight, 0.00279435_f64), (Depth::Sixteen, 0.003), (Depth::Float, 0.006)] {
        let noisy = gpu.download_rgba32f(&gpu.add_noise(&noise_field, 0.3, false, 123, d));
        let residuals: Vec<f64> = noisy.chunks_exact(4).flat_map(|p| p[..3].iter().map(|v| *v as f64 - 0.5)).collect();
        let mean = residuals.iter().sum::<f64>() / residuals.len() as f64;
        let variance = residuals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / residuals.len() as f64;
        let fourth = residuals.iter().map(|v| (v - mean).powi(4)).sum::<f64>() / residuals.len() as f64;
        assert!(mean.abs() < sigma * 0.02, "Gaussian noise is centered");
        assert!((variance.sqrt() / sigma - 1.).abs() < 0.02, "PS noise standard deviation");
        assert!((fourth / variance.powi(2) - 3.).abs() < 0.15, "Gaussian, not uniform, tails");
    }
    check("vibrance zero", gpu.vibrance(&image, 0.0, 0.0, Depth::Eight), &pixels);
    for vibrance in [-100.0, -50.0, -12.5, 12.5, 50.0, 100.0] {
        check(
            "vibrance linear HSV",
            gpu.vibrance(&image, vibrance, 0.0, Depth::Eight),
            &reference::vibrance_component(&pixels, vibrance, Depth::Eight),
        );
    }
    for saturation in [-100.0, -1.0, -0.5, 0.5, 50.0, 100.0] {
        check(
            "vibrance linear saturation",
            gpu.vibrance(&image, 0.0, saturation, Depth::Eight),
            &reference::vibrance_saturation(&pixels, saturation, Depth::Eight),
        );
    }
    for (fractional, integer) in [(12.5, 13.0), (-12.5, -12.0)] {
        let expected = gpu.download_rgba32f(&gpu.vibrance(&image, integer, 0.0, Depth::Eight));
        check("vibrance integer parameter rounding", gpu.vibrance(&image, fractional, 0.0, Depth::Eight), &expected);
    }
    // PS 27.11 vibrance-16-hsv-00465: nearly gray orange amplifies TRC-table errors.
    let samples = [
        ([16384., 16003., 14842.], [17606., 15494., 5526.]),
        ([16384., 15567., 13814.], [18124., 14920., 3206.]),
        ([16384., 16320., 16127.], [16670., 16176., 14634.]),
    ];
    let source: Vec<f32> = samples.iter().flat_map(|(rgb, _)| [rgb[0] / 65535., rgb[1] / 65535., rgb[2] / 65535., 0.7]).collect();
    let probe = gpu.upload_rgba32f(3, 1, &source);
    let actual = gpu.download_rgba32f(&gpu.vibrance(&probe, 100., 100., Depth::Sixteen));
    for (pixel, (_, expected)) in actual.chunks_exact(4).zip(samples) {
        for channel in 0..3 {
            assert!((pixel[channel] - expected[channel] / 65535.).abs() * 32768. <= 16., "measured vibrance 16-bit TRC");
        }
        assert_eq!(pixel[3], 0.7);
    }
    check(
        "HSL desaturate",
        gpu.desaturate(&image, Depth::Float),
        &[0.5, 0.5, 0.5, 0.3, 0.5, 0.5, 0.5, 0.7, 0.4, 0.4, 0.4, 1.0],
    );
    let primaries = gpu.upload_rgba32f(
        3,
        1,
        &[1.0, 0.0, 0.0, 0.3, 0.0, 1.0, 0.0, 0.7, 0.0, 0.0, 1.0, 1.0],
    );
    check(
        "default black and white primaries",
        gpu.black_and_white_default(&primaries),
        &[0.4, 0.4, 0.4, 0.3, 0.4, 0.4, 0.4, 0.7, 0.2, 0.2, 0.2, 1.0],
    );
    let mut expected = pixels.to_vec();
    for p in expected.chunks_exact_mut(4) {
        let y = 0.30 * p[0] + 0.59 * p[1] + 0.11 * p[2];
        p[..3].fill(y);
    }
    check("luminance", gpu.luminance(&image, Depth::Float), &expected);
    for d in [Depth::Eight, Depth::Sixteen] {
        let mut expected = pixels.to_vec();
        for p in expected.chunks_exact_mut(4) {
            let y = reference::color_luminosity([p[0], p[1], p[2]], d);
            p[..3].fill(y);
        }
        check("integer Color luminance", gpu.luminance(&image, d), &expected);
    }
    let endpoints = gpu.upload_rgba32f(2, 1, &[0.0, 0.0, 0.0, 0.3, 1.0, 1.0, 1.0, 0.7]);
    let stops = [
        GradientStop {
            pos: 0.0,
            rgb: [0.1, 0.3, 0.5],
        },
        GradientStop {
            pos: 1.0,
            rgb: [0.9, 0.7, 0.2],
        },
    ];
    check(
        "gradient endpoints",
        gpu.gradient_map(&endpoints, &stops, Depth::Sixteen),
        &[0.1, 0.3, 0.5, 0.3, 0.9, 0.7, 0.2, 0.7],
    );
    let gradient_pixels: Vec<f32> = (0..=64).flat_map(|i| {
        let v = i as f32 / 64.;
        [v, v, v, 0.37]
    }).collect();
    let gradient_image = gpu.upload_rgba32f(65, 1, &gradient_pixels);
    let gradient_stops = [
        GradientStop { pos: 0., rgb: [35. / 255., 100. / 255., 180. / 255.] },
        GradientStop { pos: 1365. / 4096., rgb: [230. / 255., 75. / 255., 40. / 255.] },
        GradientStop { pos: 2731. / 4096., rgb: [40. / 255., 220. / 255., 100. / 255.] },
        GradientStop { pos: 1., rgb: [180. / 255., 40. / 255., 220. / 255.] },
    ];
    check(
        "perceptual gradient spline and component clamp",
        gpu.gradient_map(&gradient_image, &gradient_stops, Depth::Sixteen),
        &reference::gradient_map(&gradient_pixels, &gradient_stops, Depth::Sixteen),
    );
    let grayscale_stops = [
        GradientStop { pos: 0., rgb: [0.; 3] },
        GradientStop { pos: 1., rgb: [1.; 3] },
    ];
    let mapped = gpu.download_rgba32f(&gpu.gradient_map(&gradient_image, &grayscale_stops, Depth::Sixteen));
    // PS16 smoothness4096 isolated BW ramp: encoded-space midpoint is not 0.5.
    assert!((mapped[32 * 4] - 0.38858625).abs() < 0.0001);
    assert!(mapped.chunks_exact(4).all(|p| p[3] == 0.37));
    let highlights = gpu.fill(3, 2, [2.0, 1.5, 1.0, 0.4]);
    let flattened = gpu.download_rgba32f(&gpu.highlights_flatten(&highlights, Depth::Float));
    for p in flattened.chunks_exact(4) {
        assert!(p.iter().all(|x| x.is_finite()), "flatten must be finite");
        assert!(p[0] > 0.0 && p[0] < 2.0, "flatten must compress highlights");
        assert!((p[3] - 0.4).abs() < 1e-6, "flatten preserves alpha");
    }
    let source_pixels = [0.2, 0.9, 0.3, 0.0, 0.8, 0.2, 0.4, 0.5, 0.6, 0.1, 0.7, 1.0];
    let source = gpu.upload_rgba32f(3, 1, &source_pixels);
    let mut expected = pixels.to_vec();
    for (p, s) in expected
        .chunks_exact_mut(4)
        .zip(source_pixels.chunks_exact(4))
    {
        for c in 0..3 {
            p[c] = p[c] * 0.6 + (1.0 - s[c]) * 0.4;
        }
    }
    check(
        "apply image invert ignores source alpha",
        gpu.apply_image(&image, &source, BlendMode::Normal, 0.4, true, Depth::Float),
        &expected,
    );
    for channel in 0..3 {
        let mut extracted = Vec::new();
        let mut applied = pixels.to_vec();
        for (i, p) in pixels.chunks_exact(4).enumerate() {
            extracted.extend_from_slice(&[p[channel], p[channel], p[channel], 1.0]);
            applied[i * 4 + channel] = 0.75 * p[channel] + 0.25 * source_pixels[i * 4];
        }
        check(
            "extract channel",
            gpu.extract_channel(&image, channel as u32),
            &extracted,
        );
        check(
            "apply channel leaves siblings and alpha",
            gpu.apply_to_channel(&image, &source, channel as u32, BlendMode::Normal, 0.25),
            &applied,
        );
    }
    let mask_pixels = [0.0, 0.9, 0.8, 0.7, 0.25, 0.0, 0.0, 0.1, 1.0, 0.0, 0.0, 0.0];
    let mask = gpu.upload_rgba32f(3, 1, &mask_pixels);
    let mut masked = pixels.to_vec();
    let mut mixed = pixels.to_vec();
    for i in 0..3 {
        let weight = mask_pixels[i * 4];
        masked[i * 4 + 3] *= weight;
        for c in 0..4 {
            mixed[i * 4 + c] =
                pixels[i * 4 + c] * (1.0 - weight) + source_pixels[i * 4 + c] * weight;
        }
    }
    check("mask alpha", gpu.mask_alpha(&image, &mask), &masked);
    check(
        "mix by mask",
        gpu.mix_by_mask(&image, &source, &mask),
        &mixed,
    );
    for from in [Depth::Eight, Depth::Sixteen, Depth::Float] {
        for to in [Depth::Eight, Depth::Sixteen, Depth::Float] {
            let expected = reference::convert_depth(&pixels, from, to);
            check(
                "depth conversion sRGB and storage quantization",
                gpu.convert_depth(&image, from, to),
                &expected,
            );
        }
    }
}

#[test]
fn depth_conversion_storage_grid_and_hdr_clipping() {
    let pixels = [128.0 / 255.0, -0.25, 4.0, 0.37];
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    let image = gpu.upload_rgba32f(1, 1, &pixels);
    let expected = reference::convert_depth(&pixels, Depth::Eight, Depth::Float);
    let grid = (pixels[0] * 32768.0 + 0.5).floor() / 32768.0;
    assert_eq!(expected[0], decode(grid));
    assert_eq!(&expected[1..], &[0.0, 1.0, 0.37]);
    close("SDR conversion input grid", &gpu.download_rgba32f(&gpu.convert_depth(&image, Depth::Eight, Depth::Float)), &expected, 3e-6);
    for target in [Depth::Eight, Depth::Sixteen] {
        let expected = reference::convert_depth(&pixels, Depth::Float, target);
        assert_eq!(expected[1], 0.0);
        assert_eq!(expected[2], 1.0);
        close("HDR conversion clips without adaptive toning", &gpu.download_rgba32f(&gpu.convert_depth(&image, Depth::Float, target)), &expected, 3e-5);
        close("storage quantization", &gpu.download_rgba32f(&gpu.quantize(&image, target)), &reference::quantize(&pixels, target), 1e-7);
    }
}

#[test]
fn measured_color_mode_integer_luminance_and_unclipped_float() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    // Corpus layerComposite-8-rgbcube-00241: continuous luminance misses blue by six codes.
    let b = [1.0, 238.0 / 255.0, 178.0 / 255.0, 1.0];
    let s = [171.0 / 255.0, 191.0 / 255.0, 4.0 / 255.0, 1.0];
    let base = gpu.upload_rgba32f(1, 1, &b);
    let source = gpu.upload_rgba32f(1, 1, &s);
    let got = gpu.download_rgba32f(&gpu.quantize(&gpu.blend(&base, &source, BlendMode::Color, 1.0, 1.0, Depth::Eight), Depth::Eight));
    close("measured integer Color", &got, &[242.0 / 255.0, 1.0, 131.0 / 255.0, 1.0], 1e-6);
    for d in [Depth::Sixteen, Depth::Float] {
        let expected = reference::blend(&b, &s, BlendMode::Color, 1.0, 1.0, d);
        close("Color depth math", &gpu.download_rgba32f(&gpu.blend(&base, &source, BlendMode::Color, 1.0, 1.0, d)), &expected, 2e-6);
    }
    let hdr = gpu.fill(1, 1, [2.0, 2.0, 2.0, 1.0]);
    let red = gpu.fill(1, 1, [1.0, 0.0, 0.0, 1.0]);
    close("Color Float does not gamut clip", &gpu.download_rgba32f(&gpu.blend(&hdr, &red, BlendMode::Color, 1.0, 1.0, Depth::Float)), &[2.7, 1.7, 1.7, 1.0], 2e-6);
}

#[test]
fn ripple_large_measured_coordinate_displacement() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    let pixels: Vec<f32> = (0..256).flat_map(|y| {
        (0..256).flat_map(move |x| [x as f32 / 255., y as f32 / 255., 0.5, 1.])
    }).collect();
    let image = gpu.upload_rgba32f(256, 256, &pixels);
    let actual = gpu.download_rgba32f(&gpu.ripple(&image, 100., true, 0));
    // Photoshop 27.11 coordinate-ramp samples; tolerance covers the fitted field residual.
    for (x, y, expected) in [
        (64, 64, [0.24644367, 0.25348714]),
        (128, 128, [0.5021915, 0.504283]),
        (200, 20, [0.7888812, 0.07880046]),
        (20, 200, [0.076386, 0.78279126]),
        (170, 100, [0.68292195, 0.38574395]),
    ] {
        let index = (y * 256 + x) * 4;
        close("large ripple PS coordinates", &actual[index..index + 2], &expected, 0.0001);
    }
    close("large ripple is seed-independent", &gpu.download_rgba32f(&gpu.ripple(&image, 100., true, 123)), &actual, 0.);
}

#[test]
fn highlights_flatten_measured_neutral_curve() {
    let gpu = Gpu::new_blocking().unwrap();
    let pixels: Vec<f32> = (0..256).flat_map(|i| {
        let value = i as f32 / 255.;
        [value, value, value, 0.6]
    }).collect();
    let image = gpu.upload_rgba32f(256, 1, &pixels);
    // Excerpts from PS27.11 ACR -100 ramp captures, not the fitted polynomial.
    for (depth, expected) in [
        (Depth::Eight, [0.10980392, 0.21960784, 0.43529412, 0.5372549, 0.92941177]),
        (Depth::Sixteen, [0.11142138, 0.21927215, 0.4343633, 0.5389334, 0.92852676]),
        (Depth::Float, [0.10071628, 0.19178459, 0.28731185, 0.3972418, 0.6456169]),
    ] {
        let result = gpu.download_rgba32f(&gpu.highlights_flatten(&image, depth));
        for (index, oracle) in [32, 64, 128, 192, 255].into_iter().zip(expected) {
            let pixel = &result[index * 4..index * 4 + 4];
            assert!((pixel[0] - oracle).abs() < 0.02, "ACR neutral tone-curve approximation");
            close("neutrality and alpha", pixel, &[pixel[0], pixel[0], pixel[0], 0.6], 1e-6);
        }
    }
}

#[test]
fn highlights_flatten_bright_on_dark_extraction() {
    let gpu = Gpu::new_blocking().unwrap();
    // Exact synthetic replay fixture: PS ACR -100 followed by Flatten extraction
    // produces black. A landscape-only shoulder instead leaks bright cyan/red.
    let (width, height) = (256u32, 192u32);
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let r2 = ((x as f32 - 80.).powi(2) + (y as f32 - 70.).powi(2)) / 64.;
            let light = (-r2).exp();
            let bar = if (160..164).contains(&x) && (32..160).contains(&y) { 1. } else { 0. };
            for value in [0.002 + 0.998 * light, 0.003 + 0.797 * light + 0.7 * bar, 0.005 + 0.495 * light + 0.995 * bar] {
                pixels.push((value.clamp(0., 1.) * 255.).round() / 255.);
            }
            pixels.push(1.);
        }
    }
    let source = gpu.upload_rgba32f(width, height, &pixels);
    let flattened = gpu.highlights_flatten(&source, Depth::Eight);
    let color = gpu.quantize(&gpu.levels(&flattened, 0., 255., 0.25, 0., 255., Depth::Eight), Depth::Eight);
    let luminance = gpu.luminance(&flattened, Depth::Eight);
    let mask = gpu.quantize(&gpu.levels(&luminance, 0., 155., 0.25, 0., 255., Depth::Eight), Depth::Eight);
    let multiplied = gpu.quantize(&gpu.apply_image(&color, &mask, BlendMode::Multiply, 1., false, Depth::Eight), Depth::Eight);
    let extraction = gpu.quantize(&gpu.levels(&multiplied, 0., 155., 1., 0., 255., Depth::Eight), Depth::Eight);
    for pixel in gpu.download_rgba32f(&extraction).chunks_exact(4) {
        close("PS bright-on-dark extraction", pixel, &[0., 0., 0., 1.], 1e-6);
    }
}
