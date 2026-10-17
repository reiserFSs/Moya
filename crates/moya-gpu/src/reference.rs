//! Independent CPU references for the concrete baselines in `docs/photoshop-op-math.md`.
//! These are not claims of measured Photoshop parity. Pixels are straight RGBA.

use crate::{BlendMode, Depth};

fn icc_srgb_point(i: usize) -> f64 {
    let x = i as f64 / 1023.;
    let y = if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
    (y * 65535. + 0.5).floor() / 65535.
}

fn icc_srgb_decode(x: f32) -> f32 {
    let x = f64::from(x.clamp(0., 1.)) * 1023.;
    let i = (x.floor() as usize).min(1022);
    (icc_srgb_point(i) + (icc_srgb_point(i + 1) - icc_srgb_point(i)) * (x - i as f64)) as f32
}

fn icc_srgb_encode(x: f32) -> f32 {
    let x = f64::from(x.clamp(0., 1.));
    let mut lo = 0;
    let mut hi = 1023;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if icc_srgb_point(mid) <= x { lo = mid; } else { hi = mid; }
    }
    ((lo as f64 + (x - icc_srgb_point(lo)) / (icc_srgb_point(hi) - icc_srgb_point(lo))) / 1023.) as f32
}

fn vibrance_input(x: f32, depth: Depth) -> f32 {
    if depth == Depth::Sixteen { (((x * 65535. + 0.5).floor() + 1.) * 0.5).floor() / 32768. } else { x }
}

/// Measured Vibrance component, before the separate saturation slider.
pub fn vibrance_component(rgba: &[f32], vibrance: f32, depth: Depth) -> Vec<f32> {
    assert!(vibrance >= -100.0 && vibrance <= 100.0);
    assert_eq!(rgba.len() % 4, 0);
    let rounded = (vibrance + 0.5).floor();
    let q = rounded.abs() / 100.0;
    let mut output = rgba.to_vec();
    if q == 0.0 { return output; }
    let decode = |v| icc_srgb_decode(vibrance_input(v, depth));
    let encode = icc_srgb_encode;
    for pixel in output.chunks_exact_mut(4) {
        let linear = [pixel[0], pixel[1], pixel[2]].map(decode);
        let hi = linear.into_iter().fold(f32::NEG_INFINITY, f32::max);
        let lo = linear.into_iter().fold(f32::INFINITY, f32::min);
        if hi == lo { continue; }
        let t = lo / hi;
        let s = 1.0 - t;
        let shadow = 1.0 - (1.0 - 16.0 * hi).max(0.0).powi(2);
        let loss = lo * s * (2.0 - t + t * t) * (1.0 - hi) * shadow;
        let mut top = hi - q * loss;
        let mut saturation = s * (1.0 - q * 0.25) * (1.0 - q + q * 0.5 * shadow * s * (1.0 + s));
        if rounded > 0.0 {
            let mut skin = 1.0;
            if linear[0] == hi {
                skin = (linear[1] - linear[2]).abs() / (hi - lo);
                if linear[1] >= linear[2] { skin = (4.0 * skin - 2.0).clamp(0.0, 1.0); }
            }
            let protect = s * s + (1.0 - s * s) * skin;
            let amount = q + (1.0 - q) * protect;
            top = hi + loss * q * amount * 0.25;
            let floor_protect = protect + 0.0285 * (1.0 - protect);
            let attenuation = (5.0 / 6.0) * (1.0 - s) * (1.0 - hi * (1.0 - s)) * shadow * 0.5 * (1.0 + floor_protect) * amount;
            saturation = s / (1.0 - q * attenuation);
        }
        let bottom = top * (1.0 - saturation);
        for c in 0..3 {
            pixel[c] = encode(bottom + (linear[c] - lo) * (top - bottom) / (hi - lo));
        }
    }
    output
}

/// Perceptual, midpoint-50, smoothness-4096 gradient; native PS32 is unsupported.
/// Photoshop's remaining integer LUT rounding is tracked in inventory B10.
pub fn gradient_map(rgba: &[f32], stops: &[crate::GradientStop], depth: Depth) -> Vec<f32> {
    assert!(!stops.is_empty());
    let mul = |m: [[f64; 3]; 3], x: [f64; 3]| m.map(|r| r[0] * x[0] + r[1] * x[1] + r[2] * x[2]);
    let mut stops: Vec<_> = stops.iter().map(|s| {
        let rgb = s.rgb.map(|v| {
            let v = f64::from(v);
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        });
        let lms = mul([
            [0.4122214708, 0.5363325363, 0.0514459929],
            [0.2119034982, 0.6806995451, 0.1073969566],
            [0.0883024619, 0.2817188376, 0.6299787005],
        ], rgb).map(f64::cbrt);
        let lab = mul([
            [0.2104542553, 0.7936177850, -0.0040720468],
            [1.9779984951, -2.4285922050, 0.4505937099],
            [0.0259040371, 0.7827717662, -0.8086757660],
        ], lms);
        ((f64::from(s.pos) * 4096.).round() / 4096., lab)
    }).collect();
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = rgba.to_vec();
    for pixel in out.chunks_exact_mut(4) {
        let input = [pixel[0], pixel[1], pixel[2]].map(|v| {
            let v = f64::from(v);
            if depth == Depth::Sixteen { (((v * 65535. + 0.5).floor() + 1.) * 0.5).floor() / 32768. } else { v }
        });
        let value = ((0.30 * input[0] + 0.59 * input[1] + 0.11 * input[2]) * 4096. + 0.5).floor() / 4096.;
        let mut lab = stops[0].1;
        for i in 1..stops.len() {
            if value < stops[i - 1].0 { continue; }
            let width = ((stops[i].0 - stops[i - 1].0) * 4096.).max(1.);
            let x = ((value - stops[i - 1].0) * 4096.).clamp(0., width);
            let midpoint = (width / 2.).ceil();
            let t = if x >= width { 1. } else if x < midpoint { 0.5 * x / midpoint } else { 0.5 + 0.5 * (x - midpoint) / (width - midpoint).max(1.) };
            let p0 = stops[i.saturating_sub(2)].1;
            let p1 = stops[i - 1].1;
            let p2 = stops[i].1;
            let p3 = stops[(i + 1).min(stops.len() - 1)].1;
            for k in 0..3 {
                lab[k] = (0.5 * (2. * p1[k] + (-p0[k] + p2[k]) * t
                    + (2. * p0[k] - 5. * p1[k] + 4. * p2[k] - p3[k]) * t * t
                    + (-p0[k] + 3. * p1[k] - 3. * p2[k] + p3[k]) * t * t * t))
                    .clamp(p1[k].min(p2[k]), p1[k].max(p2[k]));
            }
        }
        let lms = mul([
            [1., 0.3963377774, 0.2158037573],
            [1., -0.1055613458, -0.0638541728],
            [1., -0.0894841775, -1.2914855480],
        ], lab).map(|v| v * v * v);
        let rgb = mul([
            [4.0767416621, -3.3077115913, 0.2309699292],
            [-1.2684380046, 2.6097574011, -0.3413193965],
            [-0.0041960863, -0.7034186147, 1.7076147010],
        ], lms);
        for k in 0..3 {
            let v = rgb[k];
            pixel[k] = (if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1. / 2.4) - 0.055 }).clamp(0., 1.) as f32;
        }
    }
    out
}

/// Saturation component of Vibrance (vibrance = 0), measured on PS 27.11.
/// The luminance deliberately excludes blue; it is not HSL desaturation.
pub fn vibrance_saturation(rgba: &[f32], saturation: f32, depth: Depth) -> Vec<f32> {
    assert_eq!(rgba.len() % 4, 0);
    let saturation = (saturation + 0.5).floor();
    let mut output = rgba.to_vec();
    if saturation == 0.0 { return output; }
    let decode = |v| icc_srgb_decode(vibrance_input(v, depth));
    let encode = icc_srgb_encode;
    for pixel in output.chunks_exact_mut(4) {
        let linear = [pixel[0], pixel[1], pixel[2]].map(decode);
        let gray = 0.288 * linear[0] + 0.712 * linear[1];
        for c in 0..3 {
            pixel[c] = encode((gray + (linear[c] - gray) * (1.0 + saturation / 100.0)).clamp(0.0, 1.0));
        }
    }
    output
}

/// Measured Photoshop Exposure transfer; alpha is unchanged.
pub fn exposure(rgba: &[f32], stops: f32, offset: f32, gamma: f32, depth: Depth) -> Vec<f32> {
    assert!(gamma > 0.0);
    let mut result = rgba.to_vec();
    if stops == 0.0 && offset == 0.0 && gamma == 1.0 { return result; }
    for pixel in result.chunks_exact_mut(4) {
        for v in &mut pixel[..3] {
            let linear = if depth == Depth::Float { *v } else { v.max(0.0).powf(563.0 / 256.0) };
            let t = linear * stops.exp2() + offset;
            let transformed = if gamma == 1.0 { t } else { t.signum() * t.abs().powf(1.0 / gamma) };
            *v = if depth == Depth::Float {
                transformed
            } else {
                let t = transformed.clamp(0.0, 1.0) * 4096.0;
                let lo = t.floor();
                let a = (lo / 4096.0).powf(256.0 / 563.0);
                let b = ((lo + 1.0).min(4096.0) / 4096.0).powf(256.0 / 563.0);
                a + (b - a) * (t - lo)
            };
        }
    }
    result
}

/// Symmetric half-kernel measured from PS 27.11 float impulses.
/// Five odd boxes undershoot the requested variance; greedy residual boxes
/// reproduce measured radii 3..1000 to <3e-9 per coefficient.
pub(crate) fn gaussian_kernel(radius: f64) -> Vec<f64> {
    if radius <= 2.5 {
        // CALIBRATE: PS small kernels use a 1/256 grid. Between measured
        // radius anchors interpolation remains an approximation.
        const ANCHORS: &[(f64, &[f64])] = &[
            (0.0, &[256.]), (0.1, &[256.]), (0.25, &[206., 25.]),
            (0.5, &[144., 55., 1.]), (0.75, &[116., 61., 9.]),
            (1.0, &[96., 60., 18., 2.]), (1.5, &[66., 52., 29., 11., 3.]),
            (2.0, &[48., 44., 31., 17., 8., 3., 1.]),
        ];
        for pair in ANCHORS.windows(2) {
            let [(lo, a), (hi, b)] = pair else { unreachable!() };
            if radius <= *hi {
                let t = ((radius - lo) / (hi - lo)).clamp(0., 1.);
                return (0..b.len()).map(|i| {
                    (a.get(i).copied().unwrap_or(0.) * (1. - t) + b[i] * t) / 256.
                }).collect();
            }
        }
        let a = ANCHORS.last().unwrap().1;
        let b = [301., 282., 232., 166., 102., 52., 21., 6., 1.];
        let t = (radius - 2.) / 0.5;
        return b.iter().enumerate().map(|(i, v)| {
            a.get(i).copied().unwrap_or(0.) / 256. * (1. - t) + v / 2025. * t
        }).collect();
    }
    let mut n = (12. * radius * radius / 5. + 1.).sqrt().floor() as usize;
    if n % 2 == 0 { n -= 1; }
    let low = ((12. * radius * radius - 5. * (n * n) as f64 - 20. * n as f64 - 15.)
        / (-4. * n as f64 - 4.)).ceil().clamp(0., 5.) as usize;
    let mut boxes = vec![n; low];
    boxes.resize(5, n + 2);
    let mut residual = radius * radius
        - boxes.iter().map(|w| (w * w - 1) as f64 / 12.).sum::<f64>();
    while residual >= 2. / 3. - 1e-8 {
        let mut w = (12. * residual + 1.).sqrt().floor() as usize;
        if w % 2 == 0 { w -= 1; }
        if w < 3 { break; }
        boxes.push(w);
        residual -= (w * w - 1) as f64 / 12.;
    }
    let mut kernel = vec![1.];
    for w in boxes {
        if w == 1 { continue; }
        let mut next = vec![0.; kernel.len() + w - 1];
        let mut sum: f64 = 0.;
        for (i, value) in next.iter_mut().enumerate() {
            if i < kernel.len() { sum += kernel[i]; }
            if i >= w && i - w < kernel.len() { sum -= kernel[i - w]; }
            *value = sum.max(0.) / w as f64;
        }
        kernel = next;
    }
    kernel[kernel.len() / 2..].to_vec()
}

/// Separable Photoshop box-spline kernel with replicated canvas edges.
/// All four straight channels are filtered equally.
/// Clamped tails are summed once, so even a radius much larger than the image
/// needs at most width + height samples per pixel, not eight times the radius.
pub fn gaussian_blur(rgba: &[f32], width: u32, height: u32, radius: f32) -> Vec<f32> {
    let (w, h) = (width as usize, height as usize);
    assert!(w > 0 && h > 0 && rgba.len() == w * h * 4);
    assert!(radius.is_finite() && radius >= 0.0);
    if radius == 0.0 {
        return rgba.to_vec();
    }
    let weights = gaussian_kernel(f64::from(radius));
    let support = weights.len() - 1;
    let mut tails = vec![0.0; support + 2];
    for k in (0..=support).rev() {
        tails[k] = weights[k] + tails[k + 1];
    }
    let axis = |length: usize| {
        let mut matrix = vec![0.0; length * length];
        for x in 0..length {
            for source in x.saturating_sub(support)..=(x + support).min(length - 1) {
                matrix[x * length + source] = weights[x.abs_diff(source)];
            }
            if x < support {
                matrix[x * length] += tails[x + 1];
            }
            let distance = length - 1 - x;
            if distance < support {
                matrix[x * length + length - 1] += tails[distance + 1];
            }
        }
        matrix
    };
    let horizontal = axis(w);
    let vertical = axis(h);
    let mut temporary = vec![0.0f64; rgba.len()];
    let mut output = vec![0.0; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            for source in x.saturating_sub(support)..=(x + support).min(w - 1) {
                let weight = horizontal[x * w + source];
                for c in 0..4 {
                    temporary[(y * w + x) * 4 + c] +=
                        weight * f64::from(rgba[(y * w + source) * 4 + c]);
                }
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0f64; 4];
            for source in y.saturating_sub(support)..=(y + support).min(h - 1) {
                let weight = vertical[y * h + source];
                for c in 0..4 {
                    sum[c] += weight * temporary[(source * w + x) * 4 + c];
                }
            }
            for c in 0..4 {
                output[(y * w + x) * 4 + c] = sum[c] as f32;
            }
        }
    }
    output
}

// PS integer Levels joins a cubic shadow toe to the power curve. Measured
// origin slope = 2^gamma; junction 2^(-g - 1/(g-1)) rounds on the 4096 grid.
pub(crate) fn levels_toe(gamma: f32) -> [f32; 4] {
    if gamma <= 1.0 { return [0.0; 4]; }
    let g = f64::from(gamma);
    let q = (2.0f64.powf(-g - 1.0 / (g - 1.0)) * 4096.0 + 0.5).floor() / 4096.0;
    if q == 0.0 { return [0.0; 4]; }
    let slope = 2.0f64.powf(g);
    let y = q.powf(1.0 / g);
    let derivative = y / (g * q);
    let a = (derivative + slope - 2.0 * y / q) / (q * q);
    let b = (3.0 * y / q - derivative - 2.0 * slope) / q;
    [q as f32, a as f32, b as f32, slope as f32]
}

/// Levels endpoint units are 0..255. Float keeps signed values outside the
/// endpoints; integer documents clip. Alpha is unchanged.
pub fn levels(
    rgba: &[f32],
    in_black: f32,
    in_white: f32,
    gamma: f32,
    out_black: f32,
    out_white: f32,
    depth: crate::Depth,
) -> Vec<f32> {
    assert!(rgba.len() % 4 == 0 && in_white > in_black && gamma > 0.0);
    let gamma = gamma.clamp(0.01, 9.99);
    let mut output = rgba.to_vec();
    let [q, a, b, slope] = levels_toe(gamma);
    for pixel in output.chunks_exact_mut(4) {
        for value in &mut pixel[..3] {
            let input = if depth == crate::Depth::Sixteen { (*value * 32768.0 + 0.5).floor() / 32768.0 } else { *value };
            let mut t = (input * 255.0 - in_black) / (in_white - in_black);
            if depth != crate::Depth::Float { t = t.clamp(0.0, 1.0); }
            if depth == crate::Depth::Eight { t = (t * 255.0 + 0.5).floor() / 255.0; }
            let mapped = if depth != crate::Depth::Float && t < q {
                t * (slope + t * (b + t * a))
            } else { t.signum() * t.abs().powf(1.0 / gamma) };
            *value = (out_black + (out_white - out_black) * mapped) / 255.0;
        }
    }
    output
}

fn luminosity(c: [f32; 3]) -> f32 {
    0.30 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

pub fn color_blend(base: [f32; 3], source: [f32; 3], depth: Depth) -> [f32; 3] {
    let l = color_luminosity(base, depth);
    let delta = l - color_luminosity(source, depth);
    let c = source.map(|v| v + delta);
    if depth == Depth::Float { return c; }
    let grid = if depth == Depth::Eight { 255 } else { 32768 };
    let level = (l * grid as f32).round() as i32;
    let mut codes = c.map(|v| (v * grid as f32).round() as i32);
    let low = *codes.iter().min().unwrap();
    let high = *codes.iter().max().unwrap();
    if low < 0 {
        let d = level - low;
        codes = codes.map(|v| level + ((v - level) * level + d / 2) / d);
    }
    if high > grid {
        let d = high - level;
        codes = codes.map(|v| level + ((v - level) * (grid - level) + d / 2) / d);
    }
    codes.map(|v| v as f32 / grid as f32)
}

/// Luminance of the Color-mode 50% gray merge, including integer intermediate rounding.
pub fn color_luminosity(c: [f32; 3], depth: Depth) -> f32 {
    let grid = match depth { Depth::Eight => 255.0, Depth::Sixteen => 32768.0, Depth::Float => return luminosity(c) };
    let v = c.map(|value| (value * grid + 0.5).floor() as i32);
    let l = (v[0] * 9830 + v[1] * 19334 + v[2] * 3604 + 16384) / 32768;
    l as f32 / grid
}

/// Source-over composition with W3C blend functions. Linear Dodge and Color
/// Dodge Fill scale source color, not coverage (the glow spec's explicit model).
/// Eight/Sixteen clamp the result; use quantization separately at storage writes.
pub fn blend(
    base: &[f32],
    top: &[f32],
    mode: BlendMode,
    opacity: f32,
    fill: f32,
    depth: Depth,
) -> Vec<f32> {
    assert!(base.len() == top.len() && base.len() % 4 == 0);
    let special = matches!(mode, BlendMode::LinearDodge | BlendMode::ColorDodge);
    let mut output = vec![0.0; base.len()];
    for ((b, s), out) in base
        .chunks_exact(4)
        .zip(top.chunks_exact(4))
        .zip(output.chunks_exact_mut(4))
    {
        let source = [s[0], s[1], s[2]].map(|v| {
            let v = if special { v * fill } else { v };
            if special && depth == Depth::Eight { (v * 255.0).round() / 255.0 } else { v }
        });
        let coverage = if special { opacity } else { (opacity * fill * 255.0).round() / 255.0 };
        let a = s[3] * coverage;
        let alpha = a + b[3] * (1.0 - a);
        let color = if mode == BlendMode::Color {
            color_blend([b[0], b[1], b[2]], source, depth)
        } else {
            std::array::from_fn(|c| {
                let (x, y) = (b[c], source[c]);
                match mode {
                    BlendMode::Normal => y,
                    BlendMode::Screen => 1.0 - (1.0 - x) * (1.0 - y),
                    BlendMode::LinearDodge => {
                        if depth == Depth::Float {
                            x + y
                        } else {
                            (x + y).min(1.0)
                        }
                    }
                    BlendMode::ColorDodge => {
                        if x == 0.0 {
                            0.0
                        } else if y >= 1.0 {
                            1.0
                        } else {
                            (x / (1.0 - y)).min(1.0)
                        }
                    }
                    BlendMode::Multiply => x * y,
                    BlendMode::Lighten => x.max(y),
                    BlendMode::Overlay => {
                        if x <= 0.5 {
                            2.0 * x * y
                        } else {
                            1.0 - 2.0 * (1.0 - x) * (1.0 - y)
                        }
                    }
                    BlendMode::SoftLight => {
                        if y <= 0.5 {
                            x - (1.0 - 2.0 * y) * x * (1.0 - x)
                        } else {
                            let d = if x <= 0.25 {
                                ((16.0 * x - 12.0) * x + 4.0) * x
                            } else {
                                x.sqrt()
                            };
                            x + (2.0 * y - 1.0) * (d - x)
                        }
                    }
                    BlendMode::Divide => {
                        if y == 0.0 {
                            1.0
                        } else if depth == Depth::Float {
                            x / y
                        } else {
                            (x / y).min(1.0)
                        }
                    }
                    BlendMode::Color => unreachable!(),
                }
            })
        };
        let color = if depth == Depth::Eight { color.map(|v| (v * 255.0).round() / 255.0) } else { color };
        for c in 0..3 {
            let premultiplied =
                a * (1.0 - b[3]) * source[c] + a * b[3] * color[c] + (1.0 - a) * b[3] * b[c];
            let value = if alpha > 0.0 {
                premultiplied / alpha
            } else {
                0.0
            };
            out[c] = if depth == Depth::Float {
                value
            } else {
                value.clamp(0.0, 1.0)
            };
        }
        out[3] = alpha;
    }
    output
}

/// Storage rounding, without Photoshop's spatial depth-conversion dither.
pub fn quantize(rgba: &[f32], depth: Depth) -> Vec<f32> {
    let scale = match depth {
        Depth::Eight => 255.0,
        Depth::Sixteen => 32768.0,
        Depth::Float => return rgba.to_vec(),
    };
    rgba.iter().map(|v| (v.clamp(0.0, 1.0) * scale + 0.5).floor() / scale).collect()
}

/// sRGB depth conversion with the measured SDR→Float 32768-step input grid.
/// HDR→SDR models exposure 0 / gamma 1 clipping, not local HDR toning.
/// Eight-bit output is the undithered model; PS differs by up to one output code.
pub fn convert_depth(rgba: &[f32], from: Depth, to: Depth) -> Vec<f32> {
    assert_eq!(rgba.len() % 4, 0);
    let mut output = rgba.to_vec();
    for p in output.chunks_exact_mut(4) {
        for c in &mut p[..3] {
            if from != Depth::Float && to == Depth::Float {
                let x = (c.clamp(0.0, 1.0) * 32768.0 + 0.5).floor() / 32768.0;
                *c = if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
            } else if from == Depth::Float && to != Depth::Float {
                *c = if *c <= 0.0031308 { *c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
            }
        }
    }
    if to != Depth::Float {
        let scale = if to == Depth::Eight { 255.0 } else { 32768.0 };
        for c in &mut output {
            *c = (c.clamp(0.0, 1.0) * scale + 0.5).floor() / scale;
        }
    }
    output
}
