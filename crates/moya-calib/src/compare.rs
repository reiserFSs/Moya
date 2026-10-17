use anyhow::{bail, ensure, Context, Result};
use moya_gpu::{BlendMode, Depth, Gpu, GradientStop, Image, RadialQuality};
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::{Component, Path, PathBuf}};

#[derive(Default)]
pub(crate) struct Metrics {
    pub(crate) max: f64, squares: f64, samples: usize,
    float_max_ratio: f64, float_max_ulps: f64, float_failed_samples: usize,
}
impl Metrics {
    pub(crate) fn measure(actual: &[[f32; 4]], expected: &[[f32; 4]], peak: f64, float_limits: (f64, f64)) -> Result<Self> {
        ensure!(actual.len() == expected.len() && !actual.is_empty(), "pixel counts differ or are empty");
        let mut m = Self::default();
        // TIFF corpus inputs/outputs are RGB; alpha is not an oracle measurement.
        for (a, b) in actual.iter().zip(expected) {
            for c in 0..3 {
                ensure!(a[c].is_finite() && b[c].is_finite(), "non-finite RGB sample");
                let error = (depth_units(a[c], peak) - depth_units(b[c], peak)).abs();
                m.max = m.max.max(error);
                m.squares += error * error;
                m.samples += 1;
                if peak == 1. {
                    let ulp = float_ulp(b[c]);
                    let ratio = error / float_limits.0.max(float_limits.1 * ulp);
                    m.float_max_ratio = m.float_max_ratio.max(ratio);
                    m.float_max_ulps = m.float_max_ulps.max(error / ulp);
                    m.float_failed_samples += usize::from(ratio > 1.);
                }
            }
        }
        Ok(m)
    }
    pub(crate) fn rmse(&self) -> f64 { (self.squares / self.samples as f64).sqrt() }
    pub(crate) fn psnr(&self, peak: f64) -> f64 { 20. * (peak / self.rmse()).log10() }
    fn merge(&mut self, other: &Self) {
        self.max = self.max.max(other.max);
        self.squares += other.squares;
        self.samples += other.samples;
        self.float_max_ratio = self.float_max_ratio.max(other.float_max_ratio);
        self.float_max_ulps = self.float_max_ulps.max(other.float_max_ulps);
        self.float_failed_samples += other.float_failed_samples;
    }
}

fn depth_units(sample: f32, scale: f64) -> f64 {
    let value = sample as f64 * scale;
    // Integer TIFF samples represent discrete Photoshop codes, not f32 decoding noise.
    if scale > 1. { value.round() } else { value }
}

fn float_ulp(expected: f32) -> f64 {
    let exponent = (expected.abs().to_bits() >> 23) & 0xff;
    // IEEE binary32 spacing at |PS|, including subnormals and the largest
    // finite f32 (whose next representable value would be infinity).
    2_f64.powi(if exponent == 0 { -149 } else { exponent as i32 - 150 })
}

fn text<'a>(v: &'a Value, field: &str) -> Result<&'a str> {
    v[field].as_str().with_context(|| format!("missing string {field}"))
}
fn number(v: &Value) -> Result<f32> {
    let n = v.as_f64().or_else(|| v["_value"].as_f64()).context("expected numeric argument or unit value")?;
    ensure!(n.is_finite() && (n as f32).is_finite(), "non-finite argument");
    Ok(n as f32)
}
fn num(v: &Value, field: &str, default: f32) -> Result<f32> {
    if v[field].is_null() { Ok(default) } else { number(&v[field]).with_context(|| field.to_owned()) }
}
fn enumeration(v: &Value) -> Result<&str> {
    v.as_str().or_else(|| v["_value"].as_str()).context("expected enum argument")
}
fn depth(bits: u8) -> Result<Depth> {
    match bits { 8 => Ok(Depth::Eight), 16 => Ok(Depth::Sixteen), 32 => Ok(Depth::Float), _ => bail!("unsupported depth {bits}") }
}
fn peak(bits: u8) -> f64 { match bits { 8 => 255., 16 => 32768., _ => 1. } }
fn bits(v: &Value) -> Result<u8> {
    let n = number(v)?;
    ensure!(n == 8. || n == 16. || n == 32., "unsupported depth {n}");
    Ok(n as u8)
}
fn relative(root: &Path, value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    ensure!(!path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)), "path must be relative to corpus root: {value}");
    Ok(root.join(path))
}
fn blend(v: &Value) -> Result<BlendMode> {
    Ok(match enumeration(v)? {
        "normal" => BlendMode::Normal,
        "screen" => BlendMode::Screen,
        "linearDodge" | "linearDodgeAdd" => BlendMode::LinearDodge,
        "colorDodge" => BlendMode::ColorDodge,
        "multiply" => BlendMode::Multiply,
        "lighten" => BlendMode::Lighten,
        "overlay" => BlendMode::Overlay,
        "softLight" => BlendMode::SoftLight,
        "divide" | "blendDivide" => BlendMode::Divide,
        "color" => BlendMode::Color,
        other => bail!("unsupported blend mode {other}"),
    })
}
fn calculation(v: &Value) -> Result<BlendMode> {
    // PS 27.11 calculationType silently treats "divide" as Normal; the real enum is blendDivide.
    if enumeration(v)? == "divide" { Ok(BlendMode::Normal) } else { blend(v) }
}
fn upload(gpu: &Gpu, root: &Path, file: &str) -> Result<Image> {
    let (w, h, p) = crate::io::load(&relative(root, file)?)?;
    ensure!(w > 0 && h > 0, "empty input image");
    Ok(gpu.upload_rgba32f(w, h, p.as_flattened()))
}

fn render(gpu: &Gpu, root: &Path, c: &Value) -> Result<(Image, u8)> {
    let op = text(c, "op")?;
    let a = &c["args"];
    let input_bits = if op == "convertMode" {
        let input = relative(root, text(c, "input")?)?;
        let mut decoder = tiff::decoder::Decoder::new(std::io::BufReader::new(fs::File::open(input)?))?;
        let source_bits = match decoder.colortype()? {
            tiff::ColorType::RGB(n) | tiff::ColorType::RGBA(n) | tiff::ColorType::Gray(n) => n,
            other => bail!("unsupported source TIFF color type {other:?}"),
        };
        depth(source_bits)?;
        ensure!(bits(&c["depth"])? == bits(&a["depth"])?, "conversion case depth must equal target depth");
        source_bits
    } else {
        bits(&c["depth"])?
    };
    let d = depth(input_bits)?;
    let img = upload(gpu, root, text(c, "input")?)?;
    let mut output_bits = input_bits;
    let result = match op {
        "gaussianBlur" => gpu.gaussian_blur(&img, num(a, "radius", 0.)?),
        "motionBlur" => gpu.motion_blur(&img, num(a, "angle", 0.)?, num(a, "distance", 1.)?, d),
        "radialBlur" => {
            ensure!(a["blurMethod"].is_null() || enumeration(&a["blurMethod"])? == "zoom", "GPU supports zoom radial blur only");
            let quality = if a["blurQuality"].is_null() { "good" } else { enumeration(&a["blurQuality"])? };
            let q = match quality { "draft" => RadialQuality::Draft, "good" => RadialQuality::Good, "best" => RadialQuality::Best, _ => bail!("unknown radial quality {quality}") };
            // centerSpace labels the probe; Photoshop receives these descriptor values unchanged.
            let center = [num(&a["center"], "horizontal", 0.5)?, num(&a["center"], "vertical", 0.5)?];
            gpu.radial_zoom_blur(&img, center, num(a, "amount", 0.)?, q)
        },
        "levels" => {
            let adjustment = a["adjustment"].as_array().context("missing levels adjustment")?;
            ensure!(adjustment.len() == 1, "GPU levels supports one master adjustment");
            let l = &adjustment[0];
            if !l["channel"].is_null() { ensure!(enumeration(&l["channel"])? == "composite", "GPU levels supports composite channel only"); }
            let b = number(&l["input"][0])?;
            let w = number(&l["input"][1])?;
            let g = num(l, "gamma", 1.)?;
            ensure!(w > b && g > 0., "invalid levels input/gamma");
            gpu.levels(&img, b, w, g, number(&l["output"][0])?, number(&l["output"][1])?, d)
        },
        "exposure" => {
            let g = num(a, "gammaCorrection", 1.)?;
            ensure!(g > 0., "exposure gamma must be positive");
            gpu.exposure(&img, num(a, "exposure", 0.)?, num(a, "offset", 0.)?, g, d)
        },
        "hueSaturation" => {
            let adjustment = a["adjustment"].as_array().context("missing hue/saturation adjustment")?;
            ensure!(adjustment.len() == 1, "GPU hue/saturation supports one master adjustment");
            let h = &adjustment[0];
            gpu.hue_saturation(&img, num(h, "hue", 0.)?, num(h, "saturation", 0.)?, num(h, "lightness", 0.)?, a["colorize"].as_bool().unwrap_or(false), d)
        },
        "vibrance" => gpu.vibrance(&img, num(a, "vibrance", 0.)?, num(a, "saturation", 0.)?, d),
        "desaturate" => gpu.desaturate(&img, d),
        "blackAndWhite" => gpu.black_and_white_default(&img),
        "gradientMapEvent" => {
            let colors = a["gradient"]["colors"].as_array().context("missing gradient colors")?;
            ensure!(!colors.is_empty(), "empty gradient");
            let stops = colors.iter().map(|s| Ok(GradientStop {
                pos: number(&s["location"])? / 4096.,
                rgb: [number(&s["color"]["red"])? / 255., number(&s["color"]["grain"])? / 255., number(&s["color"]["blue"])? / 255.],
            })).collect::<Result<Vec<_>>>()?;
            gpu.gradient_map(&img, &stops, d)
        },
        "addNoise" => gpu.add_noise(&img, num(a, "noise", 0.)?, a["monochromatic"].as_bool().unwrap_or(false), seed(a)?, d),
        "ripple" => {
            let large = if a["rippleSize"].is_null() { true } else {
                match enumeration(&a["rippleSize"])? { "large" => true, "small" => false, other => bail!("unsupported ripple size {other}") }
            };
            gpu.ripple(&img, num(a, "amount", 0.)?, large, seed(a)?)
        },
        "smartSharpen" => gpu.smart_sharpen(&img, num(a, "amount", 0.)?, num(a, "radius", 0.)?, d),
        "offset" => {
            if !a["fill"].is_null() { ensure!(enumeration(&a["fill"])? == "repeat", "GPU offset supports repeat edge fill only"); }
            gpu.offset_repeat(&img, integer(a, "horizontal", 0)?, integer(a, "vertical", 0)?)
        },
        "copyToLayer" if c["top"].is_null() => gpu.copy(&img),
        "layerComposite" | "copyToLayer" | "applyImageEvent" => {
            let top = &c["top"];
            let src = upload(gpu, root, text(top, "input")?)?;
            ensure!((img.width(), img.height()) == (src.width(), src.height()), "top dimensions differ");
            if op != "applyImageEvent" {
                let mode = if top["blend"].is_null() { &a["mode"] } else { &top["blend"] };
                let opacity = num(top, "opacity", num(a, "opacity", 100.)?)? / 100.;
                let fill = num(top, "fill", num(a, "fill", 100.)?)? / 100.;
                ensure!((0. ..=1.).contains(&opacity) && (0. ..=1.).contains(&fill), "invalid opacity/fill");
                let mode = blend(mode)?;
                let composited = gpu.blend(&img, &src, mode, opacity, fill, d);
                if op == "copyToLayer" {
                    // Copying the selected top layer inherits its blending properties.
                    let composited = gpu.quantize(&composited, d);
                    gpu.blend(&composited, &src, mode, opacity, fill, d)
                } else {
                    composited
                }
            } else {
                let opacity = num(a, "opacity", 100.)? / 100.;
                ensure!((0. ..=1.).contains(&opacity), "invalid opacity");
                gpu.apply_image(&img, &src, calculation(&a["calculation"])?, opacity, a["invert"].as_bool().unwrap_or(false), d)
            }
        },
        "convertMode" => {
            output_bits = bits(&a["depth"])?;
            gpu.convert_depth(&img, d, depth(output_bits)?)
        },
        "copy" => gpu.copy(&img),
        "luminance" => gpu.luminance(&img, d),
        "unmult" => gpu.unmult(&img),
        "highlightsFlatten" => gpu.highlights_flatten(&img, d),
        "Adobe Camera Raw Filter" => {
            ensure!(num(a, "$Hi12", 0.)? == -100., "GPU ACR candidate supports Highlights -100 only");
            ensure!(a.as_object().context("ACR args must be an object")?.keys().all(|k| matches!(k.as_str(), "$PrVN" | "$PrVe" | "$Hi12")), "unsupported ACR controls");
            eprintln!("NOTE: ACR Highlights -100 uses the approximate GPU highlights_flatten candidate");
            gpu.highlights_flatten(&img, d)
        },
        "quantize" => gpu.quantize(&img, d),
        "tonemapExp" => {
            let g = num(a, "gamma", 1.)?;
            ensure!(g > 0., "tonemap gamma must be positive");
            gpu.tonemap_exp(&img, num(a, "exposure", 0.)?, g)
        },
        other => bail!("no GPU equivalent for operation {other}"),
    };
    // Match the destructive Photoshop operation boundary, not intermediate GPU passes.
    Ok((gpu.quantize(&result, depth(output_bits)?), output_bits))
}
fn seed(a: &Value) -> Result<u32> {
    if a["seed"].is_null() { return Ok(0); }
    let n = a["seed"].as_u64().context("seed must be an unsigned integer")?;
    u32::try_from(n).context("seed exceeds u32")
}
fn integer(a: &Value, field: &str, default: i32) -> Result<i32> {
    let n = num(a, field, default as f32)?;
    ensure!(n.fract() == 0. && (n as f64) >= i32::MIN as f64 && (n as f64) <= i32::MAX as f64, "{field} must be an i32");
    Ok(n as i32)
}

fn residual_stats(output: &[[f32; 4]], input: &[[f32; 4]], scale: f64) -> Result<([f64; 3], [f64; 3])> {
    ensure!(output.len() == input.len() && !input.is_empty(), "noise input/output counts differ or are empty");
    let mut mean = [0.; 3];
    let mut m2 = [0.; 3];
    for (i, (out, source)) in output.iter().zip(input).enumerate() {
        for channel in 0..3 {
            ensure!(out[channel].is_finite() && source[channel].is_finite(), "non-finite noise sample");
            let residual = depth_units(out[channel], scale) - depth_units(source[channel], scale);
            let delta = residual - mean[channel];
            mean[channel] += delta / (i + 1) as f64;
            m2[channel] += delta * (residual - mean[channel]);
        }
    }
    Ok((mean, m2.map(|v| (v.max(0.) / input.len() as f64).sqrt())))
}

fn json_number(value: f64) -> Value {
    if value == f64::INFINITY { Value::String("Infinity".into()) }
    else { serde_json::json!(value) }
}

fn metrics_json(m: &Metrics, scale: f64) -> Value {
    serde_json::json!({"maxAbs": m.max, "rmse": m.rmse(), "psnrDb": json_number(m.psnr(scale))})
}

fn noise_errors(ps: ([f64; 3], [f64; 3]), gpu: ([f64; 3], [f64; 3]), scale: f64) -> Metrics {
    let mut m = Metrics::default();
    // Compare six distribution parameters, not unrelated random pixel samples.
    for (a, b) in ps.0.iter().chain(&ps.1).zip(gpu.0.iter().chain(&gpu.1)) {
        let error = (a - b).abs() / scale;
        m.max = m.max.max(error);
        m.squares += error * error;
        m.samples += 1;
    }
    m
}

fn criterion(targets: &Value, operation: &str, bits: u8, m: &Metrics, noise: &Metrics) -> Result<(bool, Value)> {
    let contains = |field: &str| -> Result<bool> {
        Ok(targets[field].as_array().with_context(|| format!("targets {field} must be an array"))?
            .iter().any(|v| v.as_str() == Some(operation)))
    };
    if operation == "addNoise" || contains("stylizeOps")? {
        let min = targets["stylize"]["minPsnrDb"].as_f64().context("missing stylize minPsnrDb")?;
        ensure!(min.is_finite(), "invalid stylize minPsnrDb");
        let (metric, value) = if operation == "addNoise" {
            ("residualStatisticsPsnrDb", noise.psnr(1.))
        } else { ("psnrDb", m.psnr(peak(bits))) };
        return Ok((value >= min, serde_json::json!({"metric": metric, "value": json_number(value), "minPsnrDb": min})));
    }
    let group = if contains("blurOps")? { "blur" } else { "deterministic" };
    let limits = &targets[group][bits.to_string()];
    let max = limits["maxAbs"].as_f64().context("missing target maxAbs")?;
    ensure!(max.is_finite() && max >= 0., "invalid target maxAbs");
    if group == "deterministic" && bits == 32 {
        let ulps = limits["maxUlps"].as_f64().context("missing target maxUlps")?;
        return Ok((m.float_failed_samples == 0, serde_json::json!({
            "metric": "perSampleAbsOrUlps", "group": group,
            "formula": "abs(actual-expectedPS) <= max(maxAbs, maxUlps * ULP(abs(expectedPS)))",
            "maxAbs": max, "maxUlps": ulps, "ulpFormat": "IEEE-754 binary32",
            "maxErrorOverTolerance": m.float_max_ratio,
            "maxErrorUlps": m.float_max_ulps, "failedSamples": m.float_failed_samples,
            "samples": m.samples
        })));
    }
    let mut pass = m.max <= max;
    let mut result = serde_json::json!({"metric": "maxAbs", "group": group, "maxAbs": max});
    if let Some(rmse) = limits["rmse"].as_f64() {
        ensure!(rmse.is_finite() && rmse >= 0., "invalid target rmse");
        pass &= m.rmse() <= rmse;
        result["metric"] = Value::String("maxAbsAndRmse".into());
        result["rmse"] = serde_json::json!(rmse);
    }
    Ok((pass, result))
}

pub fn compare(root: &Path, op: Option<&str>, case: Option<&str>, dump: bool, json_path: Option<&Path>) -> Result<()> {
    let manifest: Value = serde_json::from_slice(&fs::read(root.join("manifest.json")).context("reading corpus manifest.json")?)?;
    ensure!(manifest["schemaVersion"] == 1, "unsupported manifest schemaVersion");
    let cases = manifest["cases"].as_array().context("manifest cases must be an array")?;
    let selected = cases.iter().filter(|c| op.is_none_or(|o| c["op"].as_str() == Some(o)) && case.is_none_or(|id| c["id"].as_str() == Some(id))).collect::<Vec<_>>();
    ensure!(!selected.is_empty(), "no manifest cases match filters");
    let targets_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../calib/targets.json");
    let targets: Value = serde_json::from_slice(&fs::read(&targets_path).context("reading calibration targets.json")?)?;
    ensure!(targets["schemaVersion"] == 1, "unsupported targets schemaVersion");
    let float_target = &targets["deterministic"]["32"];
    let float_limits = (
        float_target["maxAbs"].as_f64().context("missing Float target maxAbs")?,
        float_target["maxUlps"].as_f64().context("missing Float target maxUlps")?,
    );
    ensure!(float_limits.0.is_finite() && float_limits.0 > 0.
        && float_limits.1.is_finite() && float_limits.1 > 0., "invalid Float target limits");
    println!("Photoshop: {}", manifest["photoshopVersion"]);
    println!("RGB metrics in depth units (8:255, 16:32768, 32:1); GPU kernels are calibration candidates, not established Photoshop parity.");
    println!("Noise acceptance uses normalized residual mean/std statistics PSNR; random pixel metrics are diagnostic only.");
    println!("Deterministic depth32: each RGB absdiff <= max({}, {} * ULP(abs(expectedPS))); ULP is IEEE binary32 spacing. Absolute max/RMSE/PSNR remain diagnostic.", float_limits.0, float_limits.1);
    let mut gpu = None;
    let mut summaries: BTreeMap<(String, u8), (usize, Metrics, Metrics)> = BTreeMap::new();
    let mut reports = Vec::new();
    let mut unavailable = Vec::new();
    let mut errors = Vec::new();
    let mut failures = 0;
    for c in selected {
        let id = c["id"].as_str().unwrap_or("<missing id>");
        if c["capture"]["status"].as_str() == Some("rejected") {
            let error = c["capture"]["error"].clone();
            let entry = serde_json::json!({"id": id, "op": c["op"], "depth": c["depth"], "status": "unavailable", "error": error});
            reports.push(entry.clone());
            unavailable.push(entry);
            continue;
        }
        let result = (|| -> Result<()> {
            let id = text(c, "id")?;
            ensure!(Path::new(id).components().count() == 1 && matches!(Path::new(id).components().next(), Some(Component::Normal(_))), "invalid case id");
            let operation = text(c, "op")?;
            let output = text(c, "output")?;
            ensure!(output == format!("ps/{id}.tif"), "capture output must be ps/{id}.tif");
            let oracle_path = relative(root, output)?;
            ensure!(oracle_path.is_file(), "absent Photoshop capture: {}", oracle_path.display());
            let (w, h, expected) = crate::io::load(&oracle_path).context("loading Photoshop capture")?;
            if gpu.is_none() { gpu = Some(Gpu::new_blocking().context("initializing GPU")?); }
            let gpu = gpu.as_ref().unwrap();
            let (rendered, output_bits) = render(gpu, root, c).context("rendering GPU equivalent")?;
            ensure!((w, h) == (rendered.width(), rendered.height()), "capture and GPU dimensions differ");
            let raw = gpu.download_rgba32f(&rendered);
            let actual: Vec<[f32; 4]> = raw.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
            let m = Metrics::measure(&actual, &expected, peak(output_bits), float_limits)?;
            let mut noise = Metrics::default();
            let mut report = serde_json::json!({"id": id, "op": operation, "depth": output_bits, "status": "compared", "metrics": metrics_json(&m, peak(output_bits))});
            if operation == "addNoise" {
                let (iw, ih, input) = crate::io::load(&relative(root, text(c, "input")?)?)?;
                ensure!((iw, ih) == (w, h), "noise input dimensions differ");
                let (ps_mean, ps_std) = residual_stats(&expected, &input, peak(output_bits))?;
                let (gpu_mean, gpu_std) = residual_stats(&actual, &input, peak(output_bits))?;
                noise = noise_errors((ps_mean, ps_std), (gpu_mean, gpu_std), peak(output_bits));
                report["residualStats"] = serde_json::json!({
                    "units": "depth", "photoshop": {"meanRgb": ps_mean, "stdRgb": ps_std},
                    "gpu": {"meanRgb": gpu_mean, "stdRgb": gpu_std},
                    "normalizedParameterErrors": metrics_json(&noise, 1.)
                });
                println!("{id} NOISE_STATISTICS_ONLY depth={output_bits} ps_residual_mean_rgb={ps_mean:?} ps_residual_std_rgb={ps_std:?} gpu_residual_mean_rgb={gpu_mean:?} gpu_residual_std_rgb={gpu_std:?}; pixel metrics below are diagnostic, not seed-wise parity");
            }
            let (pass, acceptance) = criterion(&targets, operation, output_bits, &m, &noise)?;
            report["pass"] = Value::Bool(pass);
            report["criterion"] = acceptance;
            if dump {
                let dir = Path::new("/tmp/moya-calib");
                crate::io::save(&dir.join(format!("{id}-output.tif")), w, h, &actual, output_bits)?;
                let diff: Vec<[f32; 4]> = actual.iter().zip(&expected).map(|(a, b)| [(a[0]-b[0]).abs(), (a[1]-b[1]).abs(), (a[2]-b[2]).abs(), 1.]).collect();
                // Float difference TIFF preserves sub-LSB errors and HDR deviations without clipping.
                crate::io::save(&dir.join(format!("{id}-diff.tif")), w, h, &diff, 32)?;
            }
            let kind = if operation == "addNoise" { "DIAGNOSTIC_ONLY" } else { "PIXEL_METRICS" };
            println!("{id} {kind} op={operation} depth={output_bits} max_abs={:.6} rmse={:.6} psnr={:.3}dB criterion={}", m.max, m.rmse(), m.psnr(peak(output_bits)), report["criterion"]);
            let entry = summaries.entry((operation.to_owned(), output_bits)).or_default();
            entry.0 += 1;
            entry.1.merge(&m);
            entry.2.merge(&noise);
            reports.push(report);
            Ok(())
        })();
        if let Err(error) = result {
            failures += 1;
            let error = format!("{error:#}");
            eprintln!("ERROR {id}: {error}");
            let entry = serde_json::json!({"id": id, "op": c["op"], "depth": c["depth"], "status": "error", "error": error});
            reports.push(entry.clone());
            errors.push(entry);
        }
    }
    println!("\n{:<25} {:>5} {:>6} {:>14} {:>14} {:>12} {:>6}", "Operation", "Depth", "Cases", "Max abs", "RMSE", "PSNR dB", "Result");
    println!("{}", "-".repeat(90));
    let mut summary_reports = Vec::new();
    for ((operation, bits), (count, m, noise)) in summaries {
        let (pass, acceptance) = criterion(&targets, &operation, bits, &m, &noise)?;
        if !pass { failures += 1; }
        println!("{operation:<25} {bits:>5} {count:>6} {:>14.6} {:>14.6} {:>12.3} {:>6} criterion={acceptance}",
            m.max, m.rmse(), m.psnr(peak(bits)), if pass { "PASS" } else { "FAIL" });
        summary_reports.push(serde_json::json!({"op": operation, "depth": bits, "cases": count,
            "metrics": metrics_json(&m, peak(bits)), "pass": pass, "criterion": acceptance}));
    }
    if !unavailable.is_empty() {
        println!("\nUnavailable Photoshop captures (not comparison failures):");
        for entry in &unavailable { println!("{}: {}", entry["id"], entry["error"]); }
    }
    if !errors.is_empty() {
        eprintln!("\nComparison errors:");
        for entry in &errors { eprintln!("{}: {}", entry["id"], entry["error"]); }
    }
    if let Some(path) = json_path {
        let report = serde_json::json!({"schemaVersion": 1, "photoshopVersion": manifest["photoshopVersion"],
            "colorSettings": manifest["colorSettings"], "targets": targets, "cases": reports,
            "summaries": summary_reports, "unavailable": unavailable, "errors": errors});
        fs::write(path, serde_json::to_vec_pretty(&report)?).with_context(|| format!("writing report {}", path.display()))?;
    }
    ensure!(failures == 0, "{failures} comparison error(s) or failed threshold group(s); see report above");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn apply_image_calculation_enum_is_not_layer_blend_enum() {
        for value in [serde_json::json!("divide"), serde_json::json!({"_enum":"calculationType","_value":"divide"})] {
            assert!(matches!(calculation(&value).unwrap(), BlendMode::Normal));
            assert!(matches!(blend(&value).unwrap(), BlendMode::Divide));
        }
        assert!(matches!(calculation(&serde_json::json!("blendDivide")).unwrap(), BlendMode::Divide));
        for (name, expected) in [("normal", BlendMode::Normal), ("lighten", BlendMode::Lighten),
            ("multiply", BlendMode::Multiply), ("overlay", BlendMode::Overlay), ("softLight", BlendMode::SoftLight)] {
            assert_eq!(calculation(&serde_json::json!(name)).unwrap() as u32, expected as u32);
        }
    }
    #[test]
    fn metrics_depth_units_and_exact_match() {
        let one_code = Metrics::measure(&[[129. / 255.; 4]], &[[128. / 255.; 4]], 255., (0.001, 4.)).unwrap();
        assert_eq!(one_code.max, 1.);
        assert_eq!(one_code.rmse(), 1.);
        assert_eq!(depth_units(1. / 32768., 32768.), 1.);
        assert_eq!(depth_units(0.001, 1.), 0.001_f32 as f64);
        let actual = [[1., 0., 0., 0.]];
        let expected = [[0., 0., 0., 1.]];
        for bits in [8, 16, 32] {
            let p = peak(bits);
            let m = Metrics::measure(&actual, &expected, p, (0.001, 4.)).unwrap();
            assert_eq!(m.max, p);
            assert!((m.rmse() - p / 3_f64.sqrt()).abs() < 1e-9);
            assert!((m.psnr(p) - 10. * 3_f64.log10()).abs() < 1e-9);
        }
        let m = Metrics::measure(&expected, &expected, 255., (0.001, 4.)).unwrap();
        assert_eq!(m.rmse(), 0.);
        assert_eq!(m.psnr(255.), f64::INFINITY);
        assert!(Metrics::measure(&[], &[], 1., (0.001, 4.)).is_err());
        assert!(Metrics::measure(&[[f32::NAN; 4]], &expected, 1., (0.001, 4.)).is_err());
        let source = [[0.; 4]; 2];
        let noisy = [[-1., 0., 1., 1.], [1., 0., 3., 1.]];
        let (mean, std) = residual_stats(&noisy, &source, 1.).unwrap();
        assert_eq!(mean, [0., 0., 2.]);
        assert_eq!(std, [1., 0., 1.]);
    }

    #[test]
    fn float_deterministic_accepts_hdr_ulps_and_near_zero_floor() {
        let targets: Value = serde_json::from_str(include_str!("../../../calib/targets.json")).unwrap();
        let expected = 65536_f32;
        let check = |actual: f32, ps: f32| {
            let m = Metrics::measure(&[[actual; 4]], &[[ps; 4]], 1., (0.001, 4.)).unwrap();
            criterion(&targets, "levels", 32, &m, &Metrics::default()).unwrap().0
        };
        assert!(float_ulp(expected) > 0.001);
        assert!(check(f32::from_bits(expected.to_bits() + 1), expected));
        assert!(check(f32::from_bits(expected.to_bits() + 4), expected));
        assert!(!check(f32::from_bits(expected.to_bits() + 5), expected));
        assert!(check(-0.000999, 0.));
        assert!(!check(0.001001, 0.));
    }

    #[test]
    fn thresholds_and_noise_statistics() {
        let targets = serde_json::json!({
            "deterministic": {"8": {"maxAbs": 1.}},
            "blur": {"8": {"maxAbs": 3., "rmse": 0.5}},
            "stylize": {"minPsnrDb": 40.},
            "blurOps": ["gaussianBlur"], "stylizeOps": ["addNoise", "ripple"]
        });
        let empty = Metrics::default();
        let boundary = Metrics { max: 1., squares: 1., samples: 1, ..Metrics::default() };
        assert!(criterion(&targets, "levels", 8, &boundary, &empty).unwrap().0);
        let over = Metrics { max: 1.000001, squares: 1., samples: 1, ..Metrics::default() };
        assert!(!criterion(&targets, "levels", 8, &over, &empty).unwrap().0);
        let blur = Metrics { max: 3., squares: 0.25, samples: 1, ..Metrics::default() };
        assert!(criterion(&targets, "gaussianBlur", 8, &blur, &empty).unwrap().0);
        let over = Metrics { max: 3., squares: 0.250001, samples: 1, ..Metrics::default() };
        assert!(!criterion(&targets, "gaussianBlur", 8, &over, &empty).unwrap().0);

        let source = [[0.; 4]; 2];
        let ps = [[-0.1, 0., 0.1, 1.], [0.1, 0., -0.1, 1.]];
        let gpu = [ps[1], ps[0]];
        for bits in [8, 16, 32] {
            let scale = peak(bits);
            let pixels = Metrics::measure(&ps, &gpu, scale, (0.001, 4.)).unwrap();
            let stats = noise_errors(residual_stats(&ps, &source, scale).unwrap(),
                residual_stats(&gpu, &source, scale).unwrap(), scale);
            assert!(pixels.psnr(scale) < 40.);
            assert_eq!(stats.rmse(), 0.);
            let (pass, acceptance) = criterion(&targets, "addNoise", bits, &pixels, &stats).unwrap();
            assert!(pass);
            assert_eq!(acceptance["metric"], "residualStatisticsPsnrDb");
            assert_eq!(acceptance["value"], "Infinity");
        }
        let noise = noise_errors(([0.; 3], [0.; 3]), ([0.01; 3], [0.01; 3]), 1.);
        let mut boundary_targets = targets.clone();
        boundary_targets["stylize"]["minPsnrDb"] = serde_json::json!(noise.psnr(1.));
        assert!(criterion(&boundary_targets, "addNoise", 32, &empty, &noise).unwrap().0);
        boundary_targets["stylize"]["minPsnrDb"] = serde_json::json!(noise.psnr(1.) + 0.000001);
        assert!(!criterion(&boundary_targets, "addNoise", 32, &empty, &noise).unwrap().0);
        assert_eq!(metrics_json(&Metrics { max: 0., squares: 0., samples: 1, ..Metrics::default() }, 1.)["psnrDb"], "Infinity");
    }
}
