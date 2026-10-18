use super::*;

#[test]
fn strips_match_full_shear_bitwise() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    let (w, h) = (2500, 1800);
    let pixels: Vec<f32> = (0..w * h * 4).map(|i| ((i * 17 % 251) as f32 - 125.) / 83.).collect();
    let image = gpu.upload_rgba32f(w, h, &pixels);
    for d in [Depth::Float, Depth::Eight, Depth::Sixteen] {
        let extent = h + w - 1 + 4;
        let mut prefix = gpu.run(32, &[&image], w, extent, &[1., 0., -2., depth(d)], &[]);
        let mut offset = 1;
        while offset < w {
            prefix = gpu.run(33, &[&prefix], w, extent, &[
                0., offset as f32, if 2 * offset < w { 1. } else { 0. },
                if 4 * offset < w { 1. } else { 0. },
                if 8 * offset < w { 1. } else { 0. },
            ], &[]);
            offset *= 16;
        }
        let old = gpu.run(34, &[&prefix], w, h, &[1., 0., -2., depth(d), 283.], &[]);
        let actual = gpu.motion_blur(&image, 45., 400., d);
        assert_eq!(
            gpu.download_rgba32f(&actual).iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            gpu.download_rgba32f(&old).iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn large_motion_and_integer_guards_fit_device() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    if gpu.device.limits().max_texture_dimension_2d < 16384 { return; }
    for (w, h, angle, distance, d) in [
        (16000, 400, 45., 400., Depth::Float),
        (16384, 8, 89., 100., Depth::Eight),
        (8, 16384, 1., 100., Depth::Sixteen),
    ] {
        let image = gpu.fill(w, h, [0.25, 0.5, 0.75, 1.]);
        let actual = gpu.download_rgba32f(&gpu.motion_blur(&image, angle, distance, d));
        let bad = actual.chunks_exact(4).enumerate().find(|(_, pixel)| {
            pixel.iter().zip([0.25, 0.5, 0.75, 1.]).any(|(v, e)| !(v.is_finite() && (v - e).abs() < 0.01))
        });
        if let Some((index, pixel)) = bad {
            let limits = gpu.device.limits();
            panic!("{w}x{h} angle {angle} distance {distance} {d:?}: pixel ({}, {}) = {pixel:?} (max texture {}, max buffer {})",
                index as u32 % w, index as u32 / w, limits.max_texture_dimension_2d, limits.max_buffer_size);
        }
    }
}

#[test]
fn prefix_sanitizes_isolated_nonfinite_channels() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
    let mut pixels = vec![0.5; 1024 * 4];
    pixels[100 * 4] = f32::INFINITY;
    pixels[200 * 4 + 1] = f32::NAN;
    pixels[300 * 4 + 2] = f32::NEG_INFINITY;
    let image = gpu.upload_rgba32f(1024, 1, &pixels);
    let actual = gpu.download_rgba32f(&gpu.motion_blur(&image, 0., 400., Depth::Float));
    for value in &mut pixels {
        if !value.is_finite() { *value = 0.; }
    }
    let sanitized = gpu.upload_rgba32f(1024, 1, &pixels);
    let expected = gpu.download_rgba32f(&gpu.motion_blur(&sanitized, 0., 400., Depth::Float));
    assert!(actual.iter().all(|value| value.is_finite()));
    assert_eq!(actual, expected);
    assert_eq!(&actual[800 * 4..], vec![0.5; 224 * 4]);
}
