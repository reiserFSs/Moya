![Moya — luminous effects for your images](assets/readme-header.png)

# Moya

GPU-powered glow and light effects, with a standalone macOS app and a Photoshop round trip.

- Glow/bloom, glare and spectrum, rays, chromatic aberration, and tone mapping.
- Paintable masks with image-based builders, refinement, and undo/redo.
- Presets with live hover previews and import/export.
- Photoshop round trip with editable effect layers and saved settings.
- 8/16/32-bit images; PNG, TIFF, EXR, and flattened PSD/PSB input (JPEG too). Export PNG, TIFF, or EXR.

## Install

**Requirements:** macOS 13 or later on Apple Silicon. The optional plugin requires Adobe Photoshop 2024 or later and Creative Cloud for `.ccx` installation.

1. Download **Moya-&lt;version&gt;-macos-arm64.zip** from the [latest release](https://github.com/reiserFSs/Moya/releases/latest).
2. Unzip it and **quit Photoshop** before installing the plugin.
3. Double-click **Install Moya.command**. It copies the app to `/Applications` (or `~/Applications` if needed), clears quarantine on that copied app, and opens **Moya Photoshop Plugin.ccx** for installation. Approve installation in Creative Cloud.
4. Open **Moya** from Applications. For the plugin, start Photoshop and open **Plugins › Moya**.

**First launch / Gatekeeper:** the app is ad-hoc signed, not notarized. If macOS blocks the installer or app, right-click it and choose **Open**; on newer macOS versions, allow it in **System Settings › Privacy & Security** if prompted. Only override a warning for a release you trust. If Photoshop was running during installation, quit it and rerun the installer.

For manual installation, copy **Moya.app** to `/Applications` or `~/Applications`, then double-click **Moya Photoshop Plugin.ccx** and approve installation in Creative Cloud. The plugin discovers either app location; its **Settings** panel also accepts an explicit app path.

### Windows x64

Download **Moya-&lt;version&gt;-windows-x64-setup.exe** from the same release page. It requires an x64 Windows PC with a working GPU driver; the optional plugin requires Photoshop 2024 or later and Creative Cloud.

1. Quit Moya and Photoshop, then run the setup. The installer is unsigned, so Windows SmartScreen may say it "protected your PC": click **More info › Run anyway**, but only for a release you trust.
2. No administrator rights are needed. It installs Moya to `%LOCALAPPDATA%\Programs\Moya`, adds a Start Menu shortcut (optional desktop icon) and registers `.moya` projects for your user.
3. Leave **Install Photoshop plugin** checked to install the plugin through Creative Cloud (approve any prompt). If Photoshop is running, only the app installs; quit Photoshop and double-click `Moya Photoshop Plugin.ccx` in the install folder.
4. Launch **Moya** from Start; for the plugin, open **Plugins › Moya** in Photoshop. If another app is already the user-selected `.moya` default, choose Moya in **Settings › Apps › Default apps**. The installer contains the standalone app, not the CLI.

Windows settings and bridge discovery/token use `%APPDATA%\Moya` with inherited user-profile ACLs, not Unix `0600` permissions. Accept this only if the directory is private to your account, SYSTEM, and administrators; other processes running as you are still trusted. Do not share `bridge.json` or grant other users access.

Standard Windows profiles are supported. If Photoshop's UXP runtime does not expose environment variables, plugin discovery falls back to `AppData\Roaming` and `AppData\Local` under your home directory; redirected AppData locations may not be discovered. The plugin's **Settings** app-path override remains available, but does not relocate bridge discovery.

**Uninstall:** use **Settings › Apps › Installed apps › Moya**. It removes the app, shortcut and `.moya` association, but keeps `%APPDATA%\Moya` (settings/presets; delete it yourself if wanted) and your projects and exports. Remove the plugin in Creative Cloud. See the [Windows smoke checklist](docs/DEVELOPMENT.md#windows-packaging-and-smoke-checklist) before treating the port as validated.


## Using Moya

1. **Open or drop an image or project.** Use **Cmd+O**, or drop a supported image or `.moya` file anywhere in the window.
2. **Build your layer stack.** The **Layers** panel sits to the right of the preview. Add, duplicate, rename (double-click), reorder (drag a row, or Up/Down), hide, solo or delete effect layers; each has its own settings and mask. Choose a layer's input (original image or the composite below), blend mode and opacity under the list. Collapse the panel with **>>** to a narrow strip of layer swatches and widen it by dragging its edge; both are remembered. Select a layer and its settings appear in the left panels. Start with glow, then enable Glare, Spectrum, Chromatic Aberration, or Moya Rays as needed. Disabled sections collapse. Click a value to type it; right-click a slider to reset it. Changed values are tinted green. **Hide source** in the bottom bar shows the glow without the image.
3. **Inspect the preview.** Scroll or pinch to zoom around the cursor, drag to pan, and use **Fit**, **100%**, or **Split** for an original/result divider. Hold **Space** to see the original. Drag the crosshair to move the rays center. Preview uses Draft while it fits within Draft's pixel resolution; magnifying Draft pixels switches to the sharp viewport described below. Export renders HQ.
4. **Shape the mask.** White selects where light is taken from. Brush or erase directly in the preview; set size, hardness, and opacity. View the mask as an overlay or grayscale. Build from luminosity, transparency, outlines, or another image; combine with Replace, Add, Subtract, or Intersect. Feather, grow/shrink, invert, clear, and save masks from the same panel. Mask operations and parameter edits share undo/redo.
5. **Use presets.** Hover a preset to preview it; click to apply. Right-click to rename, duplicate, or delete. Import/export supports the `Oniric_Presets.json` / `userPresets` array format. No factory presets are bundled; import your own or save settings you create.
6. **Save the project.** **Cmd+S** saves editable layers and masks to `.moya`; **Cmd+Shift+S** saves a copy under a new name. Enable **Embed source** to include the original image for a portable project; otherwise keep the referenced source image available. The title shows the project name and an asterisk for unsaved edits. Opening another image/project or quitting offers Save, Discard, or Cancel.
7. **Export.** **Cmd+E** saves a full-quality PNG, TIFF, or EXR, with a choice of full composite or glow only. HQ works at up to a 4000-pixel long side and resamples to the original image dimensions.

On macOS, double-click a `.moya` project in Finder (or drag it onto Moya's Dock icon) to open it in the running app, with the same unsaved-changes prompt.

Window size, the last selected layer's parameters, and the last open folder are remembered. Opening a new standalone image or a Photoshop session without a saved project starts with one effect layer using those parameters, without painted masks; only opening a saved project restores a layer stack. User presets are stored in the platform config directory (`~/Library/Application Support/Moya/userPresets.json` on macOS).

Image and mask-file decoding runs on one background worker: newer requests replace queued work and stale results are discarded. Project loading also decodes embedded original images without changing their depth or profile. HQ encoding and file writes run in the background. Layer, parameter, and mask edits share a bounded undo/redo history.

If the preset file cannot be read, a persistent warning remains visible. Before the first subsequent preset write, Moya preserves the original as `userPresets.json.bak-<timestamp>`; if that backup fails, it does not overwrite the original. Cancelling a Photoshop send removes any result produced by that send.

### Preview and zoom

Fit uses the Draft preview while the fitted image stays within Draft's pixel resolution. The magnification threshold governs even in Fit, so a large or high-density viewport can cross it. Once zoom magnifies Draft pixels, the viewport samples the full-resolution source immediately and overlays bilinearly enlarged Draft glows; it does not enlarge a low-resolution composite. The original remains sharp in Split and while holding Space.

The status chip next to the zoom percentage describes the rendered result:

- **Preview** (grey): sharp source with Draft glows, or an approximate refined result below 100% zoom.
- **Refining n/N** (amber): after 0.5 seconds without an edit, pan, or zoom, visible layer glows refine to HQ in the background, one layer at a time, so painting and panning stay responsive; `n/N` counts ready/required layers. Editing cancels the layer in progress, and with plain glows a layer takes roughly a second (several for rays, glare and chromatic aberration) on a 92-megapixel photo.
- **Exact** (green): at 100% zoom or above, every visible layer's HQ glow is ready. The native-resolution region uses export's depth and quantization, so the pixels are identical to the export, and is displayed with nearest-neighbor sampling.

Edits restart refinement. Returning below the Draft-pixel magnification threshold, invalidating the result, or changing documents releases refined glows. Only a screen-sized region is composited for the zoomed viewport, not a full-size output image.

## Photoshop

1. Choose **Plugins › Moya › Edit in Moya**, or **Edit in Moya** in the plugin panel. The visible document is exported at its current bit depth with its color profile. Moya starts if needed; approve Photoshop's permission prompt on first use.
2. Build your layer stack in Moya, then click **Send to Photoshop** (**Cmd+Enter**). **Cancel** ends the session without placing a result.
3. The result is placed in the original document as a **Moya** group with one pixel layer per Moya layer, preserving each layer's name, visibility, opacity, and blend. Auto blend uses Screen at 8/16-bit and Linear Dodge at 32-bit. Enable **Return full composite** in the plugin for a single Normal layer instead.
4. To change it later, select the group or any layer inside it and choose **Re-edit selected Moya group**. Its project restores the full layer stack, settings, and masks. The returned group replaces the old one at the same stack position, retaining the group's visibility, opacity, and blend.

The panel shows connection status and an app-path override under **Settings**. Moya must be running for the local connection; no external server is needed.

## Shortcuts

| Shortcut | Action |
|---|---|
| Cmd+O | Open image or `.moya` project |
| Cmd+S | Save project |
| Cmd+Shift+S | Save project as |
| Cmd+E | Export HQ image |
| Cmd+Enter | Send to Photoshop |
| Cmd+Z / Cmd+Shift+Z | Undo / redo |
| Hold Space | Show original |
| F / 1 | Fit / 100% |
| [ / ] | Smaller / larger brush |
| Shift+[ / Shift+] | Decrease / increase hardness |
| X | Swap brush / eraser |
| Hold Option | Erase temporarily |
| Esc | Stop painting |

Painting shortcuts apply while painting. Shortcuts are ignored while a text field has focus.

## Build from source

Install Apple's command-line tools (`xcode-select --install`) and Rust using [rustup](https://rustup.rs/), then:

```sh
git clone https://github.com/reiserFSs/Moya.git
cd Moya
cargo run -p moya-app --release
# Or open an image directly:
cargo run -p moya-app --release -- path/to/image.png
```

To build the app bundle and install the development plugin (quit Photoshop first):

```sh
sh scripts/bundle-macos.sh       # installs ~/Applications/Moya.app
sh scripts/install-ps-plugin.sh  # registers the local Photoshop plugin
```

See [Development](docs/DEVELOPMENT.md) for crate layout, CLI rendering, calibration, performance, and the bridge protocol; [Photoshop operation mathematics](docs/photoshop-op-math.md) documents the measured operation models and remaining approximations.

## License

[MIT](LICENSE) — Copyright (c) 2026 reiserFSs.
