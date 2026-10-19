# Development

## Workspace

| Crate | Role |
|---|---|
| `moya-core` | Parameter model, presets, document-size math |
| `moya-gpu` | WGSL compute implementations of image operations |
| `moya-pipeline` | Layered document rendering, per-layer glow/glare/spectrum/rays/CA caches, X-Ray, and Draft/HQ effect graphs |
| `moya-io` | PNG, JPEG, TIFF 8/16/32, EXR, flattened PSD/PSB composite |
| `moya-project` | Atomic `.moya` ZIP persistence, layer settings, lossless 16-bit masks, referenced or embedded source |
| `moya-app` | Desktop app using eframe and a shared wgpu device |
| `moya-cli` | Headless rendering and benchmarks |
| `moya-calib` | Generic Photoshop operation calibration |

```sh
cargo test --workspace --release
node ps-plugin/check.cjs
cargo run -p moya-cli --release -- render in.png out.png --params p.json --hq
cargo run -p moya-cli --release -- bench in.png --params p.json --interactive --iters 9
cargo run -p moya-cli --release -- render stack.moya out.tif --hq --depth 16
cargo run -p moya-cli --release -- bench stack.moya --iters 9
```

`--params` takes a JSON object of `moya_core::Params` fields; omitted fields use defaults. Rendering also accepts `--preset presets.json --index N`, `--glow-only`, and `--depth 8|16|32`. Draft is the default; `--hq` selects HQ.

Project input renders the complete bottom-to-top layer stack, including masks and Below inputs. `--glow-only` exports the project's combined glow rather than one layer. `.moya` files contain versioned `project.json`, `masks/<layer-id>.png` (16-bit grayscale), and optionally `source/<file-name>`. Referenced source paths may be absolute or relative to the project folder; missing sources are reported on load. Disabled masks keep their painted pixels and enabled state. Writes use a temporary file beside the destination and atomic replacement.

Project loading rejects unsupported versions, duplicate layer identities, unsafe ZIP names/symlinks, more than 4096 entries, more than 1 GiB of uncompressed archive content or decoded masks, and JSON larger than 4 MiB. Masks share the source decoder's 16384-pixel per-side ceiling. Unknown JSON fields are ignored for forward compatibility. The in-memory `from_bytes` API preserves source references without checking local file existence, for Photoshop's source-replacement round trip.

## CI and releases

GitHub Actions builds and tests the full workspace in release mode on Apple Silicon
`macos-14` and `windows-latest`, using stable Rust and a Cargo cache. The macOS job
also runs `node ps-plugin/check.cjs`; Windows parses the PowerShell scripts, builds the installer, and runs a real silent install/uninstall smoke (files, Start Menu shortcut, `.moya` association, uninstall entry, clean removal, `%APPDATA%\Moya` untouched; plugin task skipped).
Test output is visible with `--nocapture`. GPU tests require an adapter and fail
if none is available; they are not silently skipped on headless runners.

Pushing a `v*` tag packages both platforms and publishes the macOS ZIP and Windows setup `.exe` in a GitHub
release with SHA-256 checksums in the release notes. The tag must exactly match
`v<workspace.package.version>` in `Cargo.toml`. Packaging uses
`sh scripts/package-release.sh` and `pwsh -File scripts/package-windows.ps1`.
The Release workflow's manual `dry_run` input defaults to true: it builds both
release assets and a checksum artifact/summary without creating a public release.
Manual publication requires selecting a matching version tag and disabling
`dry_run`. Release notes can be edited afterward without changing the archives.

## App assets and bundles

`sh scripts/bundle-macos.sh [destination/Moya.app]` builds the release app. The default destination is `~/Applications/Moya.app`; the bundle ID is `local.moya.app`. Source artwork lives in `assets/`; packaged icons and their provenance are in `crates/moya-app/assets/`.

`sh scripts/install-ps-plugin.sh` installs the development UXP plugin `local.moya.bridge`. Quit Photoshop first. It backs up Photoshop's plugin registration file before changing it and preserves unrelated plugins.

### Windows packaging and smoke checklist

Run from an x64 PowerShell on Windows (including `windows-latest`):

```powershell
pwsh -File scripts/package-windows.ps1
# With an existing native/crossbuilt x64 exe at target/release/moya.exe:
pwsh -File scripts/package-windows.ps1 -SkipBuild
# Or select an existing crossbuilt executable without copying it:
pwsh -File scripts/package-windows.ps1 -SkipBuild -ExecutablePath target/x86_64-pc-windows-msvc/release/moya.exe
```

For a macOS cross-build, install Rust through rustup, LLVM, and `cargo-xwin`:

```sh
brew install llvm
cargo install cargo-xwin
rustup target add x86_64-pc-windows-msvc
env RUSTC="$(rustup which rustc)" RC_PATH="$(brew --prefix llvm)/bin/llvm-rc" \
  ~/.cargo/bin/cargo-xwin xwin build --release --target x86_64-pc-windows-msvc \
  -p moya-app -p moya-cli
```

Explicit `RUSTC` selects rustup's compiler and installed target standard library instead of Homebrew Rust, which may lack that target. `RC_PATH` selects LLVM's Windows resource compiler. Cross-built binaries land in `target/x86_64-pc-windows-msvc/release/`; transfer `moya.exe` to Windows and use `-SkipBuild -ExecutablePath` above. Packaging still runs natively on Windows; the installer does not ship the CLI. Shaders must stay compilable by FXC, wgpu's default DX12 compiler: no writes into a vector or local array at a runtime (non-unrollable) index; use `select` per component instead.

The script builds only `moya-app` in release mode, reads `[workspace.package].version` from `Cargo.toml`, builds the `.ccx` (only `manifest.json`, `index.html`, `index.js`, `icons/`, matching macOS), and compiles `installer/moya.iss` with Inno Setup 6 (`ISCC.exe`; preinstalled on `windows-latest`, otherwise the script runs `choco install innosetup -y`). Output: `target/release-dist/Moya-<version>-windows-x64-setup.exe`. No CLI is shipped.

The installer is per-user and never elevates (`PrivilegesRequired=lowest`). It installs `Moya.exe` and `Moya Photoshop Plugin.ccx` to `%LOCALAPPDATA%\Programs\Moya` (fixed: the Photoshop plugin's default app path points there), adds a Start Menu shortcut (optional unchecked desktop icon), and sets `HKCU\Software\Classes\.moya` to `Moya.Project` with open command `"<installed exe>" "%1"`. It does not touch protected `UserChoice` defaults. A running Moya is closed on upgrade/uninstall (Restart Manager). The checked **Install Photoshop plugin** task runs `%CommonProgramFiles%\Adobe\Adobe Desktop Common\RemoteComponents\UPI\UnifiedPluginInstallerAgent\UnifiedPluginInstallerAgent.exe /install "<ccx>"` (Windows UPIA takes `/install`; `--install` is the macOS form); if the agent is missing or returns non-zero it opens the `.ccx` so Creative Cloud installs it. A running Photoshop skips the step with a message and never fails the install. Uninstall (Settings > Apps) removes the app files, shortcut and the keys Moya owns (`Moya.Project`; `.moya` only if empty after removing our value), keeps `%APPDATA%\Moya`, and reminds the user to remove the plugin in Creative Cloud.

`AppId` in `installer/moya.iss` is a fixed GUID and must never change; it identifies the install for upgrades and uninstall. Silent install for automation: `Moya-<version>-windows-x64-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /CURRENTUSER /MERGETASKS="!plugin"` (drop the `MERGETASKS` to install the plugin).

Upgrading over 0.3.0's script-based install: that install used the same directory and the same HKCU keys but has no uninstall entry, so the installer simply overwrites `Moya.exe` and reuses the keys.

**Not yet verified on a real Windows desktop:** packaging/CI success does not establish GPU, Explorer, Adobe installer, or Photoshop runtime correctness. Before accepting a release, record the Windows version, GPU/driver, Photoshop version, and results for all of these:

- Run the setup `.exe` as a normal non-admin user from a path with spaces (expect a SmartScreen warning: unsigned). Check the wizard pages (license, tasks, finish), installed path, Start Menu shortcut, optional desktop icon, quoted HKCU open command, and Settings > Apps entry. Test both Adobe UPI and `.ccx` fallback when available; with Photoshop running, confirm the plugin-skip message and that the app still installs.
- Upgrade over a 0.3.0 script install and over an older setup with Moya running; confirm Moya is closed/replaced and the `.moya` association still works.
- Launch from Start; open an image from a path with spaces, edit an effect/mask, export PNG/TIFF/EXR, and reopen the exports to inspect size/depth and visible output.
- Save an embedded-source `.moya` in a path with spaces/non-ASCII characters. Close Moya and double-click it in Explorer; then double-click another project while Moya is running. Confirm the intended project opens and unsaved changes still offer Save/Discard/Cancel. If Windows has a prior user-selected default, select Moya through Default apps first.
- In Photoshop, **Plugins › Moya › Edit in Moya**, edit and send back, then re-edit the returned group. Check layers/masks/settings and repeat at 8/16/32-bit. Test cancellation as well as completion, and both a stopped and already-running Moya.
- Inspect `%APPDATA%\Moya` permissions. Windows accepts inherited user-profile ACLs instead of Unix mode `0600`; accept only a private profile accessible to the user, SYSTEM, and administrators, not other ordinary users. The token does not protect against processes running as the same user. Do not share `bridge.json`. No custom Windows ACL hardening is claimed.
- Standard Windows profile paths are supported. The plugin uses `APPDATA`/`LOCALAPPDATA` when its UXP environment exposes them; otherwise it falls back to `os.homedir()/AppData/Roaming` and `/Local`. UXP's `os` API has no `getenv`, so redirected AppData may not be discoverable without exposed environment variables. The Settings executable-path override is available but does not override bridge discovery. Test redirected profiles separately rather than claiming support from standard-profile results.
- Uninstall from Settings > Apps with Moya running. Confirm the install directory, shortcut and Moya's registry keys are removed, `%APPDATA%\Moya` and user projects/exports are kept, and the reminder to remove the plugin in Creative Cloud appears. Resolve any user-selected default through Windows Settings.

### macOS release install checklist

The packaging step only runs the installer's `--dry-run`; a real install from the downloaded ZIP must be checked by hand before a release counts as installable. Use a second Mac or a fresh macOS user account (no `~/Applications/Moya.app`, no Moya plugin, no `~/Library/Application Support/Moya`) with Photoshop 2024 or later and Creative Cloud signed in. Record the macOS, Photoshop and Creative Cloud versions with the results.

- Download the ZIP from the GitHub release page **in a browser** (so it carries the quarantine flag; `curl` downloads don't). Compare `shasum -a 256` with the release notes.
- Quit Photoshop. Unzip by double-clicking in Finder and double-click **Install Moya.command**. Expected: Gatekeeper blocks it because the files are ad-hoc signed; right-click › **Open** › **Open** runs it. Note exactly which prompts appear and what they say.
- Confirm `Moya.app` landed in `/Applications` (or `~/Applications` when `/Applications` isn't writable) and that `xattr -l` on the installed app shows no `com.apple.quarantine`, while the downloaded folder keeps it.
- Confirm Creative Cloud's plugin installer ran (or the `.ccx` opened in Creative Cloud) and that **Plugins › Moya** appears after starting Photoshop.
- Launch Moya from Launchpad/Finder: it opens without a Gatekeeper prompt. Open an image, change a setting, export PNG, and save a `.moya` project.
- Quit Moya, then double-click the `.moya` in Finder: Moya launches and opens it. With Moya running and unsaved changes, double-click another project: the in-app Save / Discard / Cancel dialog appears; Cancel keeps the current document.
- In Photoshop: **Plugins › Moya › Edit in Moya**, accept the launch permission prompt, tweak, **Send to Photoshop**, then **Re-edit selected Moya group**. Repeat with Moya already running.
- Run the installer again over the existing install (upgrade path): the app is replaced, the previous plugin version folder is removed, and `PS.json` gets a timestamped backup.
- Remove: drag `Moya.app` to the Trash, remove the plugin through Creative Cloud (or Photoshop's **Plugins › Manage Plugins**), and optionally delete `~/Library/Application Support/Moya`. Confirm projects and exports are untouched.


## macOS document opening

The bundle exports `local.moya.project` for `.moya` and registers as its Editor/Owner; supported image types use Alternate rank. `macos_open` registers an `NSAppleEventManager` open-documents (`aevt`/`odoc`) handler before eframe starts, without replacing winit 0.30's application delegate. A retained handler forwards file URLs through a channel, including cold-launch events received before `App` exists; attaching egui's context wakes the UI on later events. The app polls the channel and uses `App::open`, preserving the unsaved-changes guard shared with argv and window drag/drop. No last document is restored at startup. `open path/to/project.moya` exercises the same LaunchServices route as Finder double-click and Dock drops.

## Photoshop bridge

Moya binds both `127.0.0.1:47811` and `[::1]:47811` before publishing `~/Library/Application Support/Moya/bridge.json` (mode 0600), containing the port and a per-launch random token. IPv6 is skipped only when unavailable on the system; a conflict on either address aborts startup without replacing discovery. Discovery is removed on exit.

The plugin connects to `ws://localhost:47811`: Photoshop 27.x rejects numeric-IP network permissions in v5 manifests ([Adobe developer forum](https://forums.creativeclouddeveloper.com/t/websocket-connection-not-working-in-manifestversion-5/6322)). The manifest allows `ws://localhost/`.

The first WebSocket message must be a JSON `hello` with `version: 2`, `client: "moya-bridge"`, the discovery `token`, and a cryptographically random 64-hex-character `nonce`. Discovery also has `version: 2`. The server replies with `type: "hello"`, `app: "moya"`, `version: 2`, `appVersion`, and `proof`: lowercase hex SHA-256 of the UTF-8 concatenation `token || nonce`. The plugin verifies this proof before sending any document data. The hello deadline is one second, with a 4 KiB message/frame limit before authentication and 1 MiB afterward. At most eight unauthenticated candidates are retained; a new connection evicts the oldest candidate. Accepted streams are explicitly blocking during the bounded HTTP upgrade, then nonblocking for message processing.

The plugin sends `edit` with a UUID `id`, absolute `source` TIFF path, optional absolute `projectPath`, `document: {name, id, depth, width, height}`, and `returnMode: "layers" | "composite"`. Depth is 8, 16, or 32; dimensions must be nonzero. On re-edit the project is restored from the selected Moya group's XMP into a temporary `.moya` file; selecting a descendant resolves to that group. The newly exported TIFF replaces the project's source, and masks are resized if document geometry changed.

The app returns `result` with `id`, `depth`, `returnMode`, `groupName: "Moya"`, `layers` in bottom-to-top order, `composite` (a path or null), and an absolute `projectPath`. Each layer has a glow-only TIFF `path`, `name`, `blend` (`screen`, `linearDodge`, `lighten`, or `normal`), percentage `opacity`, and `visible`. Auto resolves to Screen for 8/16-bit hosts and Linear Dodge for float hosts. Layer mode builds a Moya group of pixel layers. Composite mode returns one Normal pixel layer named Moya. Re-edit replaces the previous target at the same stack position, preserving the group's visibility, opacity, and blend.

The returned project excludes embedded source. Its ZIP bytes are base64-encoded in the target's existing Moya XMP namespace as `{"moya":2,"project":"<base64>"}`. WebSocket messages carry paths, never project bytes. Source/result TIFFs and project files live in the plugin's temporary folder and are removed after placement or cancellation. On render or write errors the session stays open so Send can be retried. The target document is tracked independently of Photoshop's current active document; successful placement replies `placed` with the session `id`.

Temporary artifacts are named `<id>.tif` (source), `<id>.input.moya` (re-edit input), `<id>.moya` (returned project), `<id>.moya-layer-N.tif` (one-based layer outputs), and `<id>.moya.tif` (composite output). Import and returned project paths must differ because output publication refuses to overwrite an existing file. The plugin validates returned paths against the current session before opening or deleting them. Group insertion uses UXP `PLACEINSIDE` plus explicit child ordering; replacement deletes the old group with `deleteContained: true` inside the rollback-capable history transaction.

The opt-in integration test starts the real app with `--bridge-test-auto-send`, a test-only flag which sends a result after loading:

```sh
cargo test -p moya-app --test bridge_protocol -- --ignored --test-threads=1
```

## Masks and preview

Painting updates only the changed rectangle on the GPU. The preview re-renders at most about every 63 ms during a stroke and immediately when it ends. For layers with rays, glare or chromatic aberration on, each mid-stroke preview is slow enough to stall the brush, so their glow re-renders only when the brush rests for 120 ms and when the stroke ends; the overlay still follows every dab. The pipeline snapshots each mask at the working size extraction samples it (same copy-or-resize as extraction, so results are bit-identical), keeping large-image masks inside the stage-cache budget. Mask edits remain full resolution and are stored on the CPU as u16 (0..65535); the overlay is capped at a 2048-pixel long side. A disabled or empty mask is never uploaded (the pipeline gets `None`); enabled masks upload through `Gpu::upload_gray_u16`. Luminosity/Transparency/Outlines masks are built on the GPU from the source texture (`Gpu::mask_from_source`, bit-identical to the former CPU builder), so the app drops the decoded CPU pixels right after upload and keeps only dimensions, depth and ICC. Undo stores each mask step as sparse 16-bit diffs or a compact 16-bit snapshot, whichever is smaller. History is bounded by exact bytes (256 MiB) and 200 steps, and the newest step is always kept even if it alone exceeds the budget. It shares one timeline with parameter edits.

`moya_pipeline::Document` renders a bottom-to-top stack of borrowed `LayerInputs` and a `RenderRequest`. Each input supplies settings, an optional GPU mask, and its mask generation. Original-input layers extract from the shared source; Below-input layers extract from the visible composite beneath them. Blend mode and opacity apply when compositing, while requested `layer_glows` retain each layer's uncomposited glow, including unmult alpha. X-Ray computes only extraction and the heatmap for the selected layer. Draft working grids are bounded to a 1500-pixel long side for landscape or 1300 for portrait. HQ uses up to 4000 pixels and resamples to original dimensions.

### Sharp zoom viewport

Fit uses Draft while the fitted image stays within Draft's pixel resolution; it is not a forced-Draft mode. The sharp viewport activates strictly when zoom exceeds `draft_width / original_width`, including Fit on large or high-density viewports. Above that Draft-pixel magnification threshold, the app composites only the screen-sized visible region from the full-resolution source and bilinearly sampled per-layer Draft glows. It must not allocate a full-size composite merely to display a zoomed crop. Split and Space use the full-resolution source rather than the Draft composite.

Editing, panning, or zooming restarts a 0.5-second idle delay. After that delay the app refines the required HQ layers in the background, one layer at a time, retaining completed layer glows for subsequent viewport draws; the UI thread never waits for a refinement (see "Background refinement" below). The overlay chip shows **Preview** (grey; result still approximate), **Refining n/N** (amber; ready/required visible HQ layer glows) or **Exact** (green). Exact requires zoom of at least 100% and all visible HQ glows ready: composite a native-resolution region with the same depth and quantization as export, then display it with nearest-neighbor sampling. A completed refinement below 100% remains Preview, not Exact. Measured on ciel2 (8313×11084, two layers): refinement ~254 ms of GPU time per layer when run in one piece (the stall the background worker removes); footprint fit 2.38 GB → zoomed Preview 2.45 GB → refined 2.70 GB (exactly two 3000×4000 RGBA32F glows retained) → zoomed out 2.23 GB.

Viewport refinement is separate from the pipeline's Draft stage cache. `Document` retains final HQ layer-glow textures, not persistent HQ intermediate stages. Release them when returning below the magnification threshold, invalidating the rendered result, or replacing the document. Pan/zoom resets the idle timer; it is not an effect-parameter change. Keep the ordinary Draft path available for Fit and interaction.

#### Background refinement

Refining a layer in one piece occupies the GPU for ~270 ms (plain glow) to ~1.3 s (rays + glare + CA), and pressing the brush while it runs waits that long. So the app runs refinement on one worker thread (`moya-app/src/refine.rs`) that owns a second GPU **lane**, with at most one job in flight:

- `Gpu::lane()` returns a second handle on the same device, queue and compiled pipelines with its own pending command encoder and its own texture pool. Work recorded on one lane is visible to another only after the producing lane flushes (queue submission order); `refine_job` flushes the document's lane before hand-off and `RefineJob::run` flushes the worker lane before it releases captured inputs. The lane pool recycles textures only on the thread that allocates from it, so a texture dropped elsewhere (whose unsubmitted commands may still read it) is freed, never handed to the other lane for overwriting. A lane never fuses owned store rounding into a producer.
- `Gpu::set_chunking(Some(Chunking { max_pixels, target, duty, cancel }))` makes a lane throttled and cancellable. Every compute dispatch is cut into row tiles (multiples of 16 rows; `ops.wgsl` adds the tile origin in `Params.head.xy` to the invocation id, so tiled output is bit-identical to one dispatch) and small dispatches are batched, each chunk sized to `target` GPU time (cost = output pixels × a per-operation ns/pixel rate × kernel taps, learned from the measured chunk time and corrected immediately when underestimated) and at most `max_pixels`. After each chunk the lane submits, waits for that submission and sleeps `busy × (1 − duty) / duty`, so it keeps the GPU at most `duty` busy and a UI frame waits for roughly one chunk. When `cancel` is set the lane stops recording at the next dispatch or chunk boundary; the caller discards the result (`RefineJob::run` returns `None` and calls `Gpu::discard_pending`), and the lane stays usable.
- The app uses `max_pixels` 16 M, `target` 3 ms, `duty` 0.5. A job is cancelled, without the UI waiting, when its ticket stops matching (any edit, mask stroke, hover preview, layer change), when the view drops below the magnification threshold, and on document replacement and exit (the worker is detached and exits by itself). Cancellation is acknowledged asynchronously; a stale result that still arrives is rejected by `accept_refine`.

Measured on ciel2 (92 MP, M-series), 100% zoom, brush 100 px, seven 500 ms strokes with 0.2/0.4/0.7/1/1.5/3/1 s pauses, frame-start gaps spanning each press-to-release interval: the synchronous refinement stalled a press for 252–258 ms (plain glow, one or two layers) and 548–558 ms (rays + glare + CA); with the worker the worst such gap is 36–55 ms at every stroke start, the same as strokes that overlap no refinement (the first stroke after loading is ~70 ms either way). Time from the last edit to Exact grows: plain glow 0.5 s → 1.35 s, two layers (only the painted layer re-refines) 0.5 s → 1.35 s, rays + glare + CA 0.5 s → 2.7 s; refining from scratch (key `1` at fit → Exact) is 0.6/0.8/0.6/1.1 s synchronous (plain, 2 layers, FX, 2 FX layers; frame starts only, the GPU still finishes afterwards) versus 1.4/2.2/2.8/4.9 s in the background.

`moya_pipeline` exports `ViewRegion { x, y, w, h }` (a nonempty source-pixel rectangle), `RegionQuality::{Preview, Exact}`, and `RefineState::{Idle, Pending { done, total }, Ready}`. The viewport entry points on `Document` are:

- `refine_state(layers, view)` queries matching visible-layer progress without rendering.
- `refine_job(layers, view) -> Option<RefineJob>` invalidates stale entries and captures the next missing visible layer, bottom-to-top so Below inputs can use preceding refined glows: settings, a working-size mask snapshot (taken on the document's lane, which is flushed before hand-off), the preceding refined glows and the shared source. `RefineJob::run(self, lane: &Arc<Gpu>) -> Option<RefinedGlow>` renders the HQ working-grid glow on any lane (typically a worker thread's chunked lane; `None` if its cancel flag fired). It does not allocate an original-size effect image, drops temporary HQ stages/outputs and trims the lane's idle pool, retaining only the working-grid glow rather than the layer's peak intermediates. `refine_current(ticket, layers, view)` says whether a job's captured inputs still match, and `accept_refine(glow, layers, view)` retains the result only if they do (and returns whether it did). `refine_step` is the synchronous composition of the three on the document's own lane, for tests and tools. Results are bit-identical on every lane, chunked or not. The app owns idle scheduling and the one-job limit.
- `render_region(layers, request, region, out_w, out_h, export_depth) -> (Image, RegionQuality)` composites the requested region. Preview output is encoded at 8-bit depth; Exact uses `export_depth` and requires all glows ready and output dimensions equal to the region's native dimensions. The app uses this native-grid output with nearest display at zoom ≥100%. X-Ray always returns Preview.
- `refine_bytes()` reports retained HQ glow payload, excluded from the Draft cache budget. `drop_refine()` releases those glows and trims idle GPU textures; `clear_caches()` also clears refinement.

Refinement keys include effect parameters, input mode, mask generation/presence, and relevant view/depth flags. Below-input keys additionally include the preceding visible blend chain, so edits below invalidate dependent HQ glows. Pan/zoom can reuse matching HQ working-grid glows; they do not key the effect cache.

The GPU region primitives use `Region { x, y, w, h }` in the full output's pixel grid. `resize_bicubic_region` preserves the full-grid resampling/reduction convention; region blend, depth-conversion, and quantization operations accept full-grid or already cropped pointwise inputs. `Gpu::sample_view` produces a viewport directly using `ViewMap { origin_x, origin_y, src_per_dst }` and `Filter::{Nearest, Bilinear}`. Coordinates refer to source outer edges: destination pixel centers map to `origin + (pixel + 0.5) * src_per_dst`.

## Robustness limits

- **Decoding:** `moya_io::load` checks header dimensions and the decoded size before allocating. Defaults are at most 16384 px per side and 4 GiB of RGBA-f32 storage; `load_with_limits(LoadLimits { .. })` overrides them. PSD/PSB files are streamed: the layer data is skipped by seeking, and only the composite section is read, after a budget check. 16/32-bit PSDs read the merged-transparency count from the global `Lr16`/`Lr32` blocks.
- **Saving:** `moya_io::save` and `save_streaming` encode into a sibling temp file, sync it, and then rename it over the destination; failures, including streaming pixel callbacks, preserve the existing file. `save_streaming(path, width, height, depth, icc, fill)` pulls RGBA-f32 rows/strips/blocks instead of retaining a full-image f32 readback. PNG uses a temporary compressed spool to preserve the existing encoder's byte framing with bounded RAM; TIFF keeps its existing strips and BigTIFF threshold near the 4 GiB offset ceiling. JPEG still buffers RGB8 for its whole-image encoder. EXR uses canonical tile order in both APIs: pixels are unchanged from the former parallel encoder, whose completion-order tile emission made its file bytes nondeterministic.
- **GPU size limits:** long-span motion blur processes minor-axis strips of at most min(device limit, 4096) px, with overlap, so intermediates always fit the device. The output is bit-identical to the unstripped path. Uploads, region writes and readback are split into bands within `max_buffer_size`, so any image up to `max_texture_dimension_2d` per side round-trips. Resizers reject zero-sized targets.
- **Non-finite input:** the motion-blur prefix scan zeroes non-finite channels first, so a stray inf/NaN only affects its own kernel window. Float exposure handles ±inf without NaN.

## Profiling and performance

`bench` reports the median of nine cold renders by default, after an untimed warmup. Timings include `Document::clear_stage_caches` and synchronized composite readback, but exclude image loading and GPU setup. Prepared input remains warm; `clear_caches` also removes prepared working grids. `--iters` changes the sample count.
The idle texture pool stays warm between benchmark iterations: inter-iteration trimming adds texture-allocation work to later timings, changing what the benchmark measures. CLI file exports instead trim idle textures to three Draft working images before encoding and after readback completes.

`--interactive` measures individual edits to gamma, hue, CA distance, glare length, and threshold. Each iteration restores/renders the base outside the timer, then times the changed value including readback. Enable CA and glare in the input parameters to exercise them. Hue and threshold also report paired cold controls, rendering the same changed parameters after clearing caches.

Each layer retains extraction/heatmap, exposure, glow, glare, tone/spectrum, CA, and output stages keyed by parameters and upstream generations — **Draft only**. Mask generations invalidate extraction; changes below a Below-input layer invalidate that layer's upstream input. Layer blend/opacity edits reuse its effect stages. Retained stages and shared prepared working textures use a document-wide **384 MiB unique-image budget**, evicted downstream first. This is not a total GPU-memory limit: inputs, in-progress intermediates, and caller-owned outputs are excluded. Gaussian glow uses a **96 MiB transient reduction DAG** for shared downsampled images; full-resolution blurs are streamed, not retained as a layer stack.

**HQ intermediate stages are transient:** they don't populate stage caches, release each stage as soon as it's consumed, and share immutable textures between outputs instead of copying them. The zoom viewport may retain final HQ layer-glow outputs for the lifetime described above; that separate refinement cache is not the Draft stage cache. `RenderRequest` selects which outputs to produce (`want_composite`, `want_glow_composite`, `want_layer_glows`, `want_xray`), so an export builds only what it writes. Exports read back through `Gpu::download_row_range` / `download_rows` (bounded, reused staging) into `moya_io::save_streaming`, so no full-image float buffer exists on the CPU.

Idle GPU textures are pooled by size, bounded to **max(512 MiB, 64 × the largest requested texture), at most 2 GiB**, with least-recently-used eviction, which keeps HQ renders from re-allocating mid-render. The app and CLI then call `Gpu::trim_idle` down to three Draft working images after an HQ export or Photoshop send completes, and the app does the same after ~5 s without rendering. `Gpu::memory_stats()` reports live/idle bytes and counts plus `peak_live_bytes` (reset with `reset_peak()`); it covers pooled textures and explicitly owned buffers, not driver/queue staging.

Memory before → after these policies (release, physical footprint; ciel2 = 8313×11084 JPEG, FX parameters):

| Scenario | Before | After |
|---|---:|---:|
| CLI ciel2 HQ export, peak | 22.7 GB | 9.5 GB |
| ciel2 HQ peak live GPU payload | 17.6 GB | 5.0 GB |
| App, Embers 1086×1448, 1 layer idle | 877 MB | 708 MB |
| App, Embers, 3 layers idle | 1.32 GB | 884 MB |
| App, ciel2, 1 layer idle | 6.51 GB | 2.19 GB |

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

`Document::render_with_checkpoints` exposes borrowed native-depth images at `extraction`, `exposure_depth`, `merged_glow`, `tone`, `glare_composite`, `final_glow` (8-bit pre-CA), `ca`, and `output`, retaining the single-layer replay checkpoint names. Ordinary rendering uses a no-op callback with no checkpoint readbacks or copies. CLI parameter/preset renders and replay cases use one default `LayerSettings` with the supplied parameters; glow-only export uses the first requested `layer_glows` image rather than the composited glow.
