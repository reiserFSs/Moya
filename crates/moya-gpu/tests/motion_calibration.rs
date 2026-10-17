//! Photoshop 27.11 motion impulse calibration measurements.
//! Long-distance quadrature remains approximate.
use moya_gpu::{Depth, Gpu};

#[test]
fn photoshop_float_motion_impulse() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for motion calibration");
    let mut pixels = vec![0.; 256 * 256 * 4];
    for p in pixels.chunks_exact_mut(4) {
        p[3] = 1.;
    }
    pixels[(128 * 256 + 128) * 4] = 1.;
    let image = gpu.upload_rgba32f(256, 256, &pixels);
    // Axis-aligned PS kernels include both endpoints, including distance one.
    for distance in [1, 5, 20, 100] {
        for angle in [0., 90.] {
            let actual = gpu.download_rgba32f(&gpu.motion_blur(&image, angle, distance as f32, Depth::Float));
            for y in 0..256 {
                for x in 0..256 {
                    let (major, minor) = if angle == 0. { (x, y) } else { (y, x) };
                    let offset = major as i32 - 128;
                    let expected = if minor == 128
                        && offset >= -((distance + 1) / 2)
                        && offset <= distance / 2
                    {
                        1. / (distance + 1) as f32
                    } else {
                        0.
                    };
                    assert!((actual[(y * 256 + x) * 4] - expected).abs() < 2e-6,
                        "axis angle={angle} distance={distance} at ({x},{y})");
                }
            }
        }
    }
    // The measured 45-degree line has a 15-column box and a [1,2,1]/4
    // transverse footprint, rather than a bilinearly sampled ideal line.
    let actual = gpu.download_rgba32f(&gpu.motion_blur(&image, 45., 20., Depth::Float));
    for y in 0..256 {
        for x in 0..256 {
            let dx = x as i32 - 128;
            let transverse = y as i32 - 128 + dx;
            let expected = if (-7..=7).contains(&dx) {
                match transverse { -1 | 1 => 1. / 60., 0 => 1. / 30., _ => 0. }
            } else { 0. };
            assert!((actual[(y * 256 + x) * 4] - expected).abs() < 2e-6,
                "45-degree footprint at ({x},{y})");
        }
    }
    let actual = gpu.download_rgba32f(&gpu.motion_blur(&image, 30., 20., Depth::Float));
    // PS's non-integral shear phases retain a measured residual below .0003.
    for (dy, dx, expected) in [
        (-1, 0, 0.013224283), (0, 0, 0.028672962), (1, 0, 0.013658312),
        (-6, 8, 0.006523980), (5, -9, 0.025580512), (6, -9, 0.023451064),
        (4, -8, 0.019924587),
    ] {
        let index = (((128 + dy) * 256 + 128 + dx) * 4) as usize;
        assert!((actual[index] - expected).abs() < 0.0003,
            "30-degree PS sample ({dx},{dy}): {} versus {expected}", actual[index]);
    }
}

#[test]
fn photoshop_integer_motion_cubic_shears() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for motion calibration");
    let mut pixels = vec![0.; 256 * 256 * 4];
    for p in pixels.chunks_exact_mut(4) { p[3] = 1.; }
    pixels[(128 * 256 + 128) * 4] = 1.;
    let image = gpu.upload_rgba32f(256, 256, &pixels);
    // Actual 16-bit TIFF words at 45 degrees, distance one. Linear shears
    // give .125/.25/.125 instead; rounding cannot explain this difference.
    for depth in [Depth::Eight, Depth::Sixteen] {
        let actual = gpu.download_rgba32f(&gpu.motion_blur(&image, 45., 1., depth));
        // PS motion-zero probes: zero is the same minimum kernel, not identity.
        let zero = gpu.download_rgba32f(&gpu.motion_blur(&image, 45., 0., depth));
        assert_eq!(zero, actual);
        assert_ne!(zero, pixels);
        for (dy, dx, word) in [(-1,0,9728.),(0,0,23104.),(1,0,9728.),
                              (0,-1,9728.),(1,-1,23104.),(2,-1,9728.)] {
            let index = (((128 + dy) * 256 + 128 + dx) * 4) as usize;
            let tolerance = if depth == Depth::Eight { 1. / 255. } else { 2e-6 };
            assert!((actual[index] - word / 65536.).abs() < tolerance,
                "integer cubic shear depth={depth:?} at ({dx},{dy})");
        }
    }
}

#[test]
fn photoshop_long_motion_impulse() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for motion calibration");
    let mut pixels = vec![0.; 256 * 256 * 4];
    for p in pixels.chunks_exact_mut(4) { p[3] = 1.; }
    pixels[(128 * 256 + 128) * 4] = 1.;
    let image = gpu.upload_rgba32f(256, 256, &pixels);
    let axial = gpu.download_rgba32f(&gpu.motion_blur(&image, 0., 500., Depth::Float));
    for x in 0..256 {
        assert!((axial[(128 * 256 + x) * 4] - 1. / 501.).abs() < 2e-6);
    }
    // PS 27.11 distance500/45-degree impulse: Float .00070423/.00140845,
    // native16 TIFF words56/130/56, and byte zero after its box rounding.
    for (depth, expected, tolerance) in [
        (Depth::Float, [1. / 1420., 1. / 710., 1. / 1420.], 2e-6),
        (Depth::Sixteen, [56. / 65535., 130. / 65535., 56. / 65535.], 2. / 32768.),
        (Depth::Eight, [0.; 3], 1. / 255.),
    ] {
        let actual = gpu.download_rgba32f(&gpu.motion_blur(&image, 45., 500., depth));
        for (y, expected) in (127..130).zip(expected) {
            assert!((actual[(y * 256 + 128) * 4] - expected).abs() < tolerance,
                "long measured impulse depth={depth:?} y={y}");
        }
    }
}
