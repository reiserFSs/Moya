use std::{
    collections::VecDeque,
    ffi::OsString,
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, bail, ensure};
use moya_core::{Params, RenderMode, presets::parse_presets};
use moya_gpu::{Depth, Gpu};
use moya_io::{Picture, SampleDepth};
use moya_pipeline::{Renderer, ViewFlags};

const HELP: &str = "\
Usage:
  moya-cli render <in> <out> [--preset file.json --index N | --params params.json]
                    [--hq] [--glow-only] [--depth 8|16|32]
  moya-cli bench <in> [--preset file.json --index N | --params params.json]
                   [--hq] [--iters N] [--interactive]

Without --preset or --params, uses the default parameters.
Preset indices are zero-based; --preset and --index must be supplied together.
--depth defaults to the input depth; the output format must support that depth.
--hq uses HQ rendering; otherwise uses Draft.
bench defaults to 9 iterations, after one untimed warmup. Each cold timing includes
rendering and synchronized GPU readback, but excludes file I/O and GPU setup.
--interactive also times single-field edits, priming the base outside each timing.
Use -- before positional paths that start with '-'.";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Render,
    Bench,
}

#[derive(Debug)]
struct Options {
    command: Command,
    input: PathBuf,
    output: Option<PathBuf>,
    preset: Option<PathBuf>,
    index: Option<usize>,
    params: Option<PathBuf>,
    hq: bool,
    glow_only: bool,
    depth: Option<SampleDepth>,
    iters: usize,
    interactive: bool,
}

fn value(args: &mut VecDeque<OsString>, flag: &str) -> Result<OsString> {
    args.pop_front().with_context(|| format!("{flag} requires a value"))
}

fn number(value: OsString, flag: &str) -> Result<usize> {
    value.to_str().and_then(|s| s.parse().ok())
        .with_context(|| format!("{flag} requires a nonnegative integer"))
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Option<Options>> {
    let mut args: VecDeque<_> = args.into_iter().collect();
    let Some(command) = args.pop_front() else {
        bail!("{HELP}");
    };
    let command = match command.to_str() {
        Some("-h" | "--help") => return Ok(None),
        Some("render") => Command::Render,
        Some("bench") => Command::Bench,
        _ => bail!("expected 'render' or 'bench'; use --help for usage"),
    };
    let mut paths = Vec::new();
    let mut preset = None;
    let mut index = None;
    let mut params = None;
    let mut hq = false;
    let mut glow_only = false;
    let mut depth = None;
    let mut iters = None;
    let mut interactive = false;
    while let Some(arg) = args.pop_front() {
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(None),
            Some("--") => {
                paths.extend(args.into_iter().map(PathBuf::from));
                break;
            }
            Some("--hq") => hq = true,
            Some("--glow-only") => glow_only = true,
            Some("--interactive") => interactive = true,
            Some("--preset") => {
                ensure!(preset.is_none(), "--preset may only be supplied once");
                preset = Some(PathBuf::from(value(&mut args, "--preset")?));
            }
            Some("--params") => {
                ensure!(params.is_none(), "--params may only be supplied once");
                params = Some(PathBuf::from(value(&mut args, "--params")?));
            }
            Some("--index") => {
                ensure!(index.is_none(), "--index may only be supplied once");
                index = Some(number(value(&mut args, "--index")?, "--index")?);
            }
            Some("--depth") => {
                ensure!(depth.is_none(), "--depth may only be supplied once");
                depth = Some(match value(&mut args, "--depth")?.to_str() {
                    Some("8") => SampleDepth::U8,
                    Some("16") => SampleDepth::U16,
                    Some("32") => SampleDepth::F32,
                    _ => bail!("--depth must be 8, 16, or 32"),
                });
            }
            Some("--iters") => {
                ensure!(iters.is_none(), "--iters may only be supplied once");
                let n = number(value(&mut args, "--iters")?, "--iters")?;
                ensure!(n > 0, "--iters must be greater than zero");
                iters = Some(n);
            }
            Some(s) if s.starts_with('-') => bail!("unknown option {s}; use --help for usage"),
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    match command {
        Command::Render => {
            ensure!(paths.len() == 2, "render requires exactly <in> and <out>");
            ensure!(iters.is_none(), "--iters is only valid with bench");
            ensure!(!interactive, "--interactive is only valid with bench");
        }
        Command::Bench => {
            ensure!(paths.len() == 1, "bench requires exactly <in>");
            ensure!(
                depth.is_none() && !glow_only,
                "--depth and --glow-only are only valid with render"
            );
        }
    }
    ensure!(preset.is_some() == index.is_some(), "--preset and --index must be supplied together");
    ensure!(params.is_none() || preset.is_none(), "--params and --preset are mutually exclusive");
    let mut paths = paths.into_iter();
    Ok(Some(Options {
        command,
        input: paths.next().expect("validated input path"),
        output: paths.next(),
        preset,
        index,
        params,
        hq,
        glow_only,
        depth,
        iters: iters.unwrap_or(9),
        interactive,
    }))
}

fn load_params(options: &Options) -> Result<Params> {
    let mut params = if let Some(path) = &options.params {
        let file = std::fs::File::open(path)
            .with_context(|| format!("opening parameters {}", path.display()))?;
        serde_json::from_reader(file)
            .with_context(|| format!("parsing parameters {}", path.display()))?
    } else if let Some(path) = &options.preset {
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("reading presets {}", path.display()))?;
        let presets = parse_presets(&json)
            .with_context(|| format!("parsing presets {}", path.display()))?;
        let index = options.index.expect("validated preset index");
        let count = presets.len();
        presets.into_iter().nth(index)
            .with_context(|| format!("preset index {index} out of range ({count} presets)"))?
            .params
    } else {
        Params::default()
    };
    params.sanitize();
    Ok(params)
}

fn median(times: &mut [f64]) -> f64 {
    times.sort_unstable_by(f64::total_cmp);
    let middle = times.len() / 2;
    if times.len() % 2 == 0 {
        (times[middle - 1] + times[middle]) / 2.0
    } else {
        times[middle]
    }
}

fn interactive_variants(base: &Params) -> [(&'static str, Params); 5] {
    ["threshold", "hue", "gamma", "CA distance", "glare length"].map(|name| {
        let mut changed = base.clone();
        let (field, a, b) = match name {
            "gamma" => (&mut changed.gamma, 1.3, 1.4),
            "hue" => (&mut changed.hue, 0., 10.),
            "CA distance" => (&mut changed.ca_distance, 2., 3.),
            "glare length" => (&mut changed.glare_length, 1.5, 1.6),
            "threshold" => (&mut changed.threshold, 0.15, 0.16),
            _ => unreachable!(),
        };
        *field = if *field == a { b } else { a };
        (name, changed)
    })
}

fn run(options: Options) -> Result<()> {
    let params = load_params(&options)?;
    let source = moya_io::load(&options.input)
        .with_context(|| format!("loading {}", options.input.display()))?;
    let host_depth = match source.depth {
        SampleDepth::U8 => Depth::Eight,
        SampleDepth::U16 => Depth::Sixteen,
        SampleDepth::F32 => Depth::Float,
    };
    let mode = if options.hq { RenderMode::Hq } else { RenderMode::Draft };
    let view = ViewFlags {
        xray: false,
        solo: false,
        host_is_float: source.depth == SampleDepth::F32,
    };
    let gpu = Arc::new(Gpu::new_blocking().context("initializing GPU")?);
    let limit = gpu.max_image_dimension();
    if source.width > limit || source.height > limit {
        bail!("{}×{} exceeds this GPU's {limit} px texture limit", source.width, source.height);
    }
    let image = gpu.upload_rgba32f(source.width, source.height, &source.rgba);
    drop(source.rgba);
    let mut renderer = Renderer::new(Arc::clone(&gpu), image, host_depth);
    match options.command {
        Command::Render => {
            let rendered = renderer.render(&params, mode, &view);
            let image = if options.glow_only { rendered.glow } else { rendered.composite };
            let output_depth = options.depth.unwrap_or(source.depth);
            let from = if !options.glow_only && view.host_is_float && !view.solo {
                Depth::Float
            } else {
                Depth::Eight
            };
            let to = match output_depth {
                SampleDepth::U8 => Depth::Eight,
                SampleDepth::U16 => Depth::Sixteen,
                SampleDepth::F32 => Depth::Float,
            };
            let image = if from == to { image } else { gpu.convert_depth(&image, from, to) };
            let picture = Picture {
                width: image.width(),
                height: image.height(),
                depth: output_depth,
                rgba: gpu.download_rgba32f(&image),
                icc_profile: source.icc_profile,
            };
            let path = options.output.as_ref().expect("validated output path");
            moya_io::save(path, &picture, output_depth)
                .with_context(|| format!("saving {}", path.display()))?;
        }
        Command::Bench => {
            let mut times = Vec::new();
            times.try_reserve_exact(options.iters).context("allocating benchmark timings")?;
            // Readback submits pending commands and waits for completion, including setup.
            let warmup = renderer.render(&params, mode, &view);
            drop(gpu.download_rgba32f(&warmup.composite));
            drop(warmup);
            for i in 0..options.iters {
                let start = Instant::now();
                renderer.clear_stage_cache();
                let output = renderer.render(&params, mode, &view);
                let pixels = gpu.download_rgba32f(&output.composite);
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                times.push(ms);
                println!("render {}: {:.3} ms", i + 1, ms);
                drop(pixels);
            }
            let ms = median(&mut times);
            println!("median: {ms:.3} ms ({} cold renders)", times.len());
            if options.interactive {
                println!("interactive single-field edits ({mode:?}; supplied parameters are the base):");
                println!("CA/glare edits retain the base enable flags; enable those effects in --params.");
                let mut cold_controls = Vec::new();
                cold_controls.try_reserve_exact(options.iters)
                    .context("allocating cold control timings")?;
                for (name, changed) in interactive_variants(&params) {
                    times.clear();
                    cold_controls.clear();
                    for _ in 0..options.iters {
                        // Restore the base every time: repeating the changed value
                        // would benchmark a cache hit rather than invalidation.
                        let prime = renderer.render(&params, mode, &view);
                        drop(gpu.download_rgba32f(&prime.composite));
                        drop(prime);
                        let start = Instant::now();
                        let output = renderer.render(&changed, mode, &view);
                        let pixels = gpu.download_rgba32f(&output.composite);
                        times.push(start.elapsed().as_secs_f64() * 1000.0);
                        drop(pixels);
                        drop(output);
                        if matches!(name, "threshold" | "hue") {
                            // Compare identical parameters close in time rather
                            // than against a potentially cooler earlier baseline.
                            let start = Instant::now();
                            renderer.clear_stage_cache();
                            let output = renderer.render(&changed, mode, &view);
                            let pixels = gpu.download_rgba32f(&output.composite);
                            cold_controls.push(start.elapsed().as_secs_f64() * 1000.0);
                            drop(pixels);
                            drop(output);
                        }
                    }
                    let ms = median(&mut times);
                    println!("interactive {name} median: {ms:.3} ms ({} edits)", times.len());
                    if !cold_controls.is_empty() {
                        let ms = median(&mut cold_controls);
                        println!("interactive {name} cold control median: {ms:.3} ms ({} renders)",
                            cold_controls.len());
                    }
                }
            }
        }
    }
    Ok(())
}

fn main() {
    let result = parse_args(std::env::args_os().skip(1)).and_then(|options| {
        match options {
            Some(options) => run(options),
            None => {
                println!("{HELP}");
                Ok(())
            }
        }
    });
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_and_rejects_invalid_combinations() {
        let parse = |args: &[&str]| parse_args(args.iter().map(OsString::from));
        let render = parse(&[
            "render", "in.png", "out.tif", "--preset", "looks.json", "--index", "2",
            "--hq", "--glow-only", "--depth", "16",
        ]).unwrap().unwrap();
        assert_eq!(render.command, Command::Render);
        assert_eq!(render.index, Some(2));
        assert_eq!(render.depth, Some(SampleDepth::U16));
        assert!(render.hq && render.glow_only);
        let bench = parse(&["bench", "in.exr", "--hq", "--iters", "4"]).unwrap().unwrap();
        assert_eq!(bench.command, Command::Bench);
        assert_eq!(bench.iters, 4);
        let bench = parse(&[
            "bench", "in.exr", "--preset", "looks.json", "--index", "2",
        ]).unwrap().unwrap();
        assert_eq!(bench.preset, Some(PathBuf::from("looks.json")));
        assert_eq!(bench.index, Some(2));
        let bench = parse(&["bench", "in.exr", "--params", "params.json"]).unwrap().unwrap();
        assert_eq!(bench.params, Some(PathBuf::from("params.json")));
        assert_eq!(bench.iters, 9);
        assert!(!bench.interactive);
        assert!(parse(&["bench", "in.png", "--interactive"]).unwrap().unwrap().interactive);
        assert!(parse(&["render", "in.png", "out.png", "--params", "params.json"]).is_ok());
        assert!(parse(&["render", "--", "-in.png", "-out.png"]).is_ok());
        assert!(parse(&["--help"]).unwrap().is_none());
        for args in [
            vec!["render", "in.png"],
            vec!["render", "in.png", "out.png", "--depth", "12"],
            vec!["render", "in.png", "out.png", "--preset", "looks.json"],
            vec!["render", "in.png", "out.png", "--index", "0"],
            vec!["render", "in.png", "out.png", "--preset", "looks.json", "--index", "0", "--params", "p.json"],
            vec!["render", "in.png", "out.png", "--iters", "2"],
            vec!["render", "in.png", "out.png", "--interactive"],
            vec!["bench", "in.png", "--iters", "0"],
            vec!["bench", "in.png", "--iters", "-1"],
            vec!["bench", "in.png", "--iters"],
            vec!["bench", "in.png", "--depth", "8"],
            vec!["bench", "in.png", "--glow-only"],
            vec!["bench", "in.png", "--preset", "looks.json"],
            vec!["bench", "in.png", "--index", "0"],
            vec!["bench", "in.png", "--preset", "looks.json", "--index", "0", "--params", "p.json"],
            vec!["bench", "in.png", "--wat"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn interactive_edits_change_exactly_one_valid_field_and_medians_are_sorted() {
        for mut base in [Params::default(), Params { colorize: true, ..Params::default() }] {
            for pass in 0..2 {
                for (_, changed) in interactive_variants(&base) {
                    let mut sanitized = changed.clone();
                    sanitized.sanitize();
                    assert_eq!(sanitized, changed);
                    assert_eq!(base.to_slots().iter().zip(changed.to_slots())
                        .filter(|(a, b)| **a != *b).count(), 1);
                }
                if pass == 0 {
                    base.gamma = 1.4;
                    base.hue = 10.;
                    base.ca_distance = 3.;
                    base.glare_length = 1.6;
                    base.threshold = 0.16;
                }
            }
        }
        assert_eq!(median(&mut [3., 1., 2.]), 2.);
        assert_eq!(median(&mut [4., 1., 3., 2.]), 2.5);
    }
}
