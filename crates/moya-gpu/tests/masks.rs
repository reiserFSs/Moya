//! Raw masks must retain the CPU builder's f32 values, not merely look alike
//! after quantization. A GPU adapter is mandatory, as in the numeric suite.
use moya_gpu::{Gpu, MaskSourceKind};

fn cpu_mask(rgba: &[f32], width: u32, height: u32, kind: MaskSourceKind) -> Vec<f32> {
    let mut result = Vec::with_capacity(width as usize * height as usize);
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) as usize;
            let p = &rgba[index * 4..index * 4 + 4];
            let value = match kind {
                MaskSourceKind::Luminosity { low, high } => {
                    let lo = low.min(high);
                    let hi = low.max(high);
                    let v = p[0].max(p[1]).max(p[2]);
                    let ramp = if hi <= lo {
                        if v >= hi { 1.0 } else { 0.0 }
                    } else {
                        ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
                    };
                    ramp * p[3]
                }
                MaskSourceKind::Transparency => p[3],
                MaskSourceKind::Outlines => {
                    let mut lo = p[3];
                    let mut hi = p[3];
                    for (dx, dy) in [(0, -1), (-1, 0), (1, 0), (0, 1)] {
                        let nx = i64::from(x) + dx;
                        let ny = i64::from(y) + dy;
                        let a = if nx >= 0 && ny >= 0 && nx < i64::from(width) && ny < i64::from(height) {
                            rgba[((ny as u32 * width + nx as u32) * 4 + 3) as usize]
                        } else { 0.0 };
                        lo = lo.min(a);
                        hi = hi.max(a);
                    }
                    hi - lo
                }
            };
            result.push(value.clamp(0.0, 1.0));
        }
    }
    result
}

fn quantize(value: f32) -> u16 {
    (value.clamp(0.0, 1.0) * 65535.0).round() as u16
}

fn check(gpu: &Gpu, width: u32, height: u32, rgba: &[f32], kind: MaskSourceKind) {
    let source = gpu.upload_rgba32f(width, height, rgba);
    let mask = gpu.mask_from_source(&source, kind);
    assert_eq!((mask.width(), mask.height()), (width, height));
    let actual = gpu.download_rgba32f(&mask);
    let expected = cpu_mask(rgba, width, height, kind);
    for (index, (pixel, &value)) in actual.chunks_exact(4).zip(&expected).enumerate() {
        for channel in 0..3 {
            assert_eq!(pixel[channel].to_bits(), value.to_bits(),
                "{width}x{height} {kind:?}, pixel {index}, channel {channel}: {} versus {value}", pixel[channel]);
            assert_eq!(quantize(pixel[channel]), quantize(value),
                "{width}x{height} {kind:?}, pixel {index}: u16 conversion");
        }
        assert_eq!(pixel[3].to_bits(), 1.0f32.to_bits(), "mask alpha at {index}");
    }
    assert_eq!(actual.len(), expected.len() * 4);
}

fn random(state: &mut u32) -> f32 {
    *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
    f32::from_bits(0x3f800000 | (*state >> 9)) - 1.0
}

#[test]
fn masks_match_cpu_raw_bits_and_u16_on_random_and_edge_sizes() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for mask numerical tests");
    let kinds = [
        MaskSourceKind::Transparency,
        MaskSourceKind::Outlines,
        MaskSourceKind::Luminosity { low: 0.0, high: 1.0 },
        MaskSourceKind::Luminosity { low: 0.173, high: 0.819 },
        MaskSourceKind::Luminosity { low: 0.819, high: 0.173 },
        MaskSourceKind::Luminosity { low: -0.75, high: 1.375 },
        MaskSourceKind::Luminosity { low: 0.5, high: 0.5 },
        MaskSourceKind::Luminosity { low: -0.25, high: -0.25 },
        MaskSourceKind::Luminosity { low: 0.0, high: 0.0 },
    ];
    let mut state = 0xc0ffeeu32;
    for (width, height) in [(1, 1), (1, 19), (19, 1), (2, 2), (15, 17), (16, 16), (17, 15), (63, 35)] {
        let rgba: Vec<_> = (0..width * height * 4).map(|_| random(&mut state) * 2.0 - 0.25).collect();
        for kind in kinds { check(&gpu, width, height, &rgba, kind); }
    }

    // Exact threshold hits, clamping, hidden RGB, and u16 half-step boundaries.
    let mut rgba = Vec::new();
    for value in [0.0, 0.173, 0.5, 0.819, 1.0, -0.25, 1.75,
        0.5 / 65535.0, 1.5 / 65535.0, 32767.5 / 65535.0, 65534.5 / 65535.0] {
        for alpha in [0.0, 0.37, 1.0, -0.25, 1.25] {
            rgba.extend_from_slice(&[value, value, value, alpha]);
        }
    }
    for kind in kinds { check(&gpu, rgba.len() as u32 / 4, 1, &rgba, kind); }

    // The center only touches diagonal opaque pixels: outlines use four neighbors.
    let mut diagonal = vec![0.0; 3 * 3 * 4];
    for index in [0, 2, 6, 8] { diagonal[index * 4 + 3] = 1.0; }
    check(&gpu, 3, 3, &diagonal, MaskSourceKind::Outlines);
    check(&gpu, 3, 3, &[1.0; 36], MaskSourceKind::Outlines);
}

#[test]
fn luminosity_division_matches_cpu_across_normal_float_exponents() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory for mask numerical tests");
    let mut state = 0xdecafbad;
    for exponent in [-60, -17, 0, 19, 60] {
        let scale = 2.0f32.powi(exponent);
        let rgba: Vec<_> = (0..257).flat_map(|_| {
            [random(&mut state) * scale, random(&mut state) * scale,
                random(&mut state) * scale, random(&mut state)]
        }).collect();
        for (low, high) in [(0.0, 0.913 * scale), (0.137 * scale, 0.893 * scale)] {
            check(&gpu, 257, 1, &rgba, MaskSourceKind::Luminosity { low, high });
        }
    }
}
