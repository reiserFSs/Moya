use std::{
    collections::VecDeque,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, bail, ensure};
use moya_core::{LayerSettings, Params, RenderMode, presets::parse_presets};
use moya_gpu::{Depth, Gpu};
use moya_io::{Picture, SampleDepth};
use moya_pipeline::{Document, LayerInputs, RenderRequest, ViewFlags};
use moya_project::{MaskData, ProjectLayer, SourceRef};

const HELP: &str = "\
Usage:
  moya-cli render <in> <out> [--preset file.json --index N | --params params.json]
                    [--hq] [--glow-only] [--depth 8|16|32]
  moya-cli bench <in> [--preset file.json --index N | --params params.json]
                   [--hq] [--iters N] [--interactive]
  moya-cli render <project.moya> <out> [--hq] [--glow-only] [--depth 8|16|32]
  moya-cli bench <project.moya> [--hq] [--iters N] [--interactive]

Without --preset or --params, image inputs use the default parameters.
Project inputs use all saved layers and masks; parameter overrides are not accepted.
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
    if paths[0].extension().is_some_and(|ext| ext.eq_ignore_ascii_case("moya")) {
        ensure!(params.is_none() && preset.is_none(), "project inputs use saved layer parameters; --params and --preset are not supported");
    }
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

fn load_source(source: &SourceRef) -> Result<Picture> {
    match source {
        SourceRef::Path(path) => moya_io::load(path)
            .with_context(|| format!("loading project source {}", path.display())),
        SourceRef::Embedded { file_name, bytes } => {
            use std::io::Write;
            let extension = Path::new(file_name).extension().and_then(|s| s.to_str())
                .context("embedded source has no file extension")?;
            let mut file = tempfile::Builder::new().suffix(&format!(".{extension}")).tempfile()?;
            file.write_all(bytes).context("writing embedded source for decoding")?;
            moya_io::load(file.path())
                .with_context(|| format!("decoding embedded source {file_name}"))
        }
    }
}

fn load_input(options: &Options) -> Result<(Picture, Vec<ProjectLayer>, Option<u64>, bool)> {
    if options.input.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("moya")) {
        let project = moya_project::load(&options.input)
            .with_context(|| format!("loading project {}", options.input.display()))?;
        let source = load_source(&project.source)?;
        Ok((source, project.layers, project.selected, true))
    } else {
        let settings = LayerSettings { params: load_params(options)?, ..LayerSettings::default() };
        let selected = Some(settings.id);
        let source = moya_io::load(&options.input)
            .with_context(|| format!("loading {}", options.input.display()))?;
        Ok((source, vec![ProjectLayer { settings, mask: None }], selected, false))
    }
}

fn mask_pixels(mask: &MaskData) -> Vec<f32> {
    mask.data.iter().flat_map(|&value| {
        let value = value as f32 / 65535.0;
        [value, value, value, 1.0]
    }).collect()
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
    let (source, layers, selected, is_project) = load_input(&options)?;
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
    let mut document = Document::new(Arc::clone(&gpu), image, host_depth);
    let masks = layers.iter().map(|layer| {
        layer.mask.as_ref().filter(|mask| mask.enabled).map(|mask| {
            ensure!(mask.width <= limit && mask.height <= limit,
                "mask {}×{} exceeds this GPU's {limit} px texture limit", mask.width, mask.height);
            Ok(gpu.upload_rgba32f(mask.width, mask.height, &mask_pixels(mask)))
        }).transpose()
    }).collect::<Result<Vec<_>>>()?;
    let inputs: Vec<_> = layers.iter().zip(&masks).map(|(layer, mask)| LayerInputs {
        settings: &layer.settings, mask: mask.as_ref(), mask_generation: 0,
    }).collect();
    let request = RenderRequest {
        mode,
        view,
        selected,
        solo_layer: None,
        want_layer_glows: options.command == Command::Render && !is_project,
        want_xray: false,
    };
    match options.command {
        Command::Render => {
            let rendered = document.render(&inputs, &request);
            let image = if options.glow_only {
                if is_project { rendered.glow_composite }
                else { rendered.layer_glows.into_iter().next().expect("requested layer glow").1 }
            } else {
                rendered.composite
            };
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
            let warmup = document.render(&inputs, &request);
            drop(gpu.download_rgba32f(&warmup.composite));
            drop(warmup);
            for i in 0..options.iters {
                let start = Instant::now();
                document.clear_stage_caches();
                let output = document.render(&inputs, &request);
                let pixels = gpu.download_rgba32f(&output.composite);
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                times.push(ms);
                println!("render {}: {:.3} ms", i + 1, ms);
                drop(pixels);
            }
            let ms = median(&mut times);
            println!("median: {ms:.3} ms ({} cold renders)", times.len());
            if options.interactive {
                let selected_index = layers.iter().position(|layer| Some(layer.settings.id) == selected)
                    .context("--interactive requires a selected project layer")?;
                let layer = &layers[selected_index].settings;
                println!("interactive single-field edits ({mode:?}; supplied parameters are the base):");
                println!("CA/glare edits retain the base enable flags; enable those effects in --params.");
                let mut cold_controls = Vec::new();
                cold_controls.try_reserve_exact(options.iters)
                    .context("allocating cold control timings")?;
                for (name, changed) in interactive_variants(&layer.params) {
                    let changed_layer = LayerSettings { params: changed, ..layer.clone() };
                    let changed_inputs: Vec<_> = inputs.iter().enumerate().map(|(index, input)| LayerInputs {
                        settings: if index == selected_index { &changed_layer } else { input.settings },
                        mask: input.mask, mask_generation: input.mask_generation,
                    }).collect();
                    times.clear();
                    cold_controls.clear();
                    for _ in 0..options.iters {
                        // Restore the base every time: repeating the changed value
                        // would benchmark a cache hit rather than invalidation.
                        let prime = document.render(&inputs, &request);
                        drop(gpu.download_rgba32f(&prime.composite));
                        drop(prime);
                        let start = Instant::now();
                        let output = document.render(&changed_inputs, &request);
                        let pixels = gpu.download_rgba32f(&output.composite);
                        times.push(start.elapsed().as_secs_f64() * 1000.0);
                        drop(pixels);
                        drop(output);
                        if matches!(name, "threshold" | "hue") {
                            // Compare identical parameters close in time rather
                            // than against a potentially cooler earlier baseline.
                            let start = Instant::now();
                            document.clear_stage_caches();
                            let output = document.render(&changed_inputs, &request);
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
    fn project_loading_preserves_layers_selection_and_decodes_embedded_source() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let image_path = dir.path().join("source.png");
        moya_io::save(&image_path, &Picture {
            width: 2, height: 1, depth: SampleDepth::U16,
            rgba: vec![0., 0.5, 1., 1., 1., 0., 0.25, 1.],
            icc_profile: None,
        }, SampleDepth::U16)?;
        let mask = MaskData { width: 2, height: 1, data: vec![0, 65535], enabled: false };
        let layers = vec![
            ProjectLayer { settings: LayerSettings { id: 7, ..Default::default() }, mask: Some(mask.clone()) },
            ProjectLayer { settings: LayerSettings { id: 9, ..Default::default() }, mask: None },
        ];
        let project_path = dir.path().join("layers.moya");
        let mut project = moya_project::Project {
            version: moya_project::VERSION,
            source: SourceRef::Embedded { file_name: "original.png".into(), bytes: std::fs::read(&image_path)? },
            layers: layers.clone(), selected: Some(9),
        };
        for embedded in [true, false] {
            if !embedded { project.source = SourceRef::Path(PathBuf::from("source.png")); }
            moya_project::save(&project_path, &project)?;
            let options = parse_args([
                OsString::from("render"), project_path.clone().into_os_string(), OsString::from("out.tif"),
            ])?.unwrap();
            let (picture, loaded_layers, selected, is_project) = load_input(&options)?;
            assert!(is_project);
            assert_eq!((picture.width, picture.height, picture.depth), (2, 1, SampleDepth::U16));
            assert_eq!(loaded_layers, layers);
            assert_eq!(selected, Some(9));
        }
        assert_eq!(mask_pixels(&mask), vec![0., 0., 0., 1., 1., 1., 1., 1.]);
        Ok(())
    }

    #[test]
    fn parses_project_commands_and_rejects_parameter_overrides() {
        let parse = |args: &[&str]| parse_args(args.iter().map(OsString::from));
        let render = parse(&["render", "layers.moya", "out.tif", "--hq", "--glow-only", "--depth", "16"])
            .unwrap().unwrap();
        assert!(render.hq && render.glow_only);
        assert_eq!(render.depth, Some(SampleDepth::U16));
        assert_eq!(parse(&["bench", "layers.MOYA"]).unwrap().unwrap().command, Command::Bench);
        assert!(parse(&["render", "layers.moya", "out.png", "--params", "p.json"]).is_err());
        assert!(parse(&["bench", "layers.moya", "--preset", "p.json", "--index", "0"]).is_err());
    }

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
