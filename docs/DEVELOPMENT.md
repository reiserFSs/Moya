# Development

## Workspace

| Crate | Role |
|---|---|
| `moya-core` | Parameter model, presets, document-size math |
| `moya-gpu` | WGSL compute implementations of image operations |
| `moya-pipeline` | Glow, glare, spectrum, rays, chromatic aberration, X-Ray, tone mapping, and Draft/HQ effect graph |
| `moya-io` | PNG, JPEG, TIFF 8/16/32, EXR, flattened PSD/PSB composite |
| `moya-app` | Desktop app using eframe and a shared wgpu device |
| `moya-cli` | Headless rendering and benchmarks |
| `moya-calib` | Generic Photoshop operation calibration |

```sh
cargo test --workspace --release
node ps-plugin/check.cjs
cargo run -p moya-cli --release -- render in.png out.png --params p.json --hq
cargo run -p moya-cli --release -- bench in.png --params p.json --interactive --iters 9
```

`--params` takes a JSON object of `moya_core::Params` fields; omitted fields use defaults. Rendering also accepts `--preset presets.json --index N`, `--glow-only`, and `--depth 8|16|32`. Draft is the default; `--hq` selects HQ.

## App assets and bundles

`sh scripts/bundle-macos.sh [destination/Moya.app]` builds the release app. The default destination is `~/Applications/Moya.app`; the bundle ID is `local.moya.app`. Source artwork lives in `assets/`; packaged icons and their provenance are in `crates/moya-app/assets/`.

`sh scripts/install-ps-plugin.sh` installs the development UXP plugin `local.moya.bridge`. Quit Photoshop first. It backs up Photoshop's plugin registration file before changing it and preserves unrelated plugins.

## Photoshop bridge

Moya listens on loopback WebSocket port **47811**. On launch it writes `~/Library/Application Support/Moya/bridge.json`, containing the port and a per-launch random token, and removes it on exit. The plugin must send that token first, preventing unrelated browser pages from driving the app. A port conflict is reported in the app status bar.

The plugin connects to `ws://localhost:47811`: Photoshop 27.x rejects numeric-IP network permissions in v5 manifests ([Adobe developer forum](https://forums.creativeclouddeveloper.com/t/websocket-connection-not-working-in-manifestversion-5/6322)). The manifest allows `ws://localhost/`.

Source TIFF and result TIFF files live in the plugin's temporary folder and are removed after placement. On render or write errors, the session stays open so Send can be retried. The target document is tracked independently of Photoshop's current active document. Layer XMP stores effect settings for re-editing.

The opt-in integration test starts the real app with `--bridge-test-auto-send`, a test-only flag which sends a result after loading:

```sh
cargo test -p moya-app --test bridge_protocol -- --ignored --test-threads=1
```

## Masks and preview

Painting updates only the changed rectangle on the GPU. The preview re-renders at most about every 63 ms during a stroke and immediately when it ends. Mask edits remain full resolution; the overlay is capped at a 2048-pixel long side. Undo keeps sparse 16-bit mask diffs, bounded by 256 MiB and 200 steps, on the same timeline as parameter edits.

The shared renderer computes only extraction and the heatmap for X-Ray. Draft working grids are bounded to a 1500-pixel long side for landscape or 1300 for portrait. HQ uses up to 4000 pixels and resamples to original dimensions.

## Profiling and performance

`bench` reports the median of nine cold renders by default, after an untimed warmup. Timings include stage-cache clearing and synchronized composite readback, but exclude image loading and GPU setup. Prepared input remains warm. `--iters` changes the sample count.

`--interactive` measures individual edits to gamma, hue, CA distance, glare length, and threshold. Each iteration restores/renders the base outside the timer, then times the changed value including readback. Enable CA and glare in the input parameters to exercise them. Hue and threshold also report paired cold controls, rendering the same changed parameters after clearing caches.

Retained extraction/heatmap, exposure, glow, glare, tone/spectrum, CA, and output stages are keyed by parameters and upstream generations. Retained stages and prepared working textures share a **384 MiB unique-image budget**, evicted downstream first. This is not a total GPU-memory limit: inputs, in-progress intermediates, and caller-owned outputs are excluded. Gaussian glow uses a **96 MiB transient reduction DAG** for shared downsampled images; full-resolution blurs are streamed, not retained as a layer stack.

Idle GPU textures are pooled by size, bounded to **max(512 MiB, 64 × the largest requested texture), at most 2 GiB**, with least-recently-used eviction.

Historical measurements on Apple M4 Pro (20 GPU cores), release, 1086×1448 input, seven timed renders after warmup including readback:

| Configuration | Before median | After median |
|---|---:|---:|
| FX Draft | 195.030 ms | 106.422 ms |
| FX HQ | 240.363 ms | 132.371 ms |
| Defaults Draft | 25.987 ms | 19.962 ms |

Long-motion prefix scans fuse up to four Hillis–Steele steps per dispatch while retaining guarded f32 additions and tree order. Native quantization can fuse into the latest unsubmitted producer, avoiding an extra full-image pass. Already consumed, submitted, or rounded images use a separate quantization pass. GPU unit tests compare fused and separate implementations.

Set `MOYA_PROFILE_OPS=1` on `moya-cli bench` to emit `MOYA_PROFILE_OPS op=<id> w=<width> h=<height> ms=<milliseconds>` on stderr. This flushes and waits around dispatches, including CPU setup, submission, and GPU completion. It is not GPU timestamp profiling; do not sum its values as a normal asynchronous render latency estimate. Discard preparation/warmup when studying steady-state results.

## Generic Photoshop operation calibration

[Photoshop operation mathematics](photoshop-op-math.md) records public documentation, independent Photoshop measurements, and unresolved `CALIBRATE:` approximations. Gaussian uses measured finite box-spline kernels; large-radius downsampling remains approximate. Motion uses depth-aware raster/shear kernels and long-span prefix sums. Radial sampling remains approximate and Photoshop repeat measurements show nondeterministic residuals. CA offset repeats boundary pixels, not Wrap Around.

The corpus is optional and is not part of the source checkout. Generate the baseline patterns and manifest before capture. Photoshop access goes through `calib/ps-run.sh`, which serializes runs with `${TMPDIR:-/tmp}/moya-photoshop.lock`. Do not invoke it while already holding that lock.

```sh
cargo run --release -p moya-calib -- gen
cargo run --release -p moya-calib -- capture
cargo run --release -p moya-calib -- compare --json /tmp/moya-ops.json
# Focus individual measurements:
cargo run --release -p moya-calib -- compare --op gaussianBlur --root ../reference-corpus/ops
```

`compare` reports max error, RMSE, and PSNR per operation/depth against `calib/targets.json`. Operations Photoshop rejects at a depth are reported separately. Baseline data lives under `../reference-corpus/ops`; `--root` selects another corpus.

`Renderer::render_with_checkpoints` exposes borrowed native-depth images at `extraction`, `exposure_depth`, `merged_glow`, `tone`, `glare_composite`, `final_glow` (8-bit pre-CA), `ca`, and `output`. Ordinary rendering uses a no-op callback with no checkpoint readbacks or copies.
