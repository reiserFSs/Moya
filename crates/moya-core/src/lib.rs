//! Parameter model, preset (de)serialization, and document-size math.

use serde::{Deserialize, Serialize};

pub mod presets;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Transition { #[default] Soft, Precise }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Highlights { #[default] Preserve, Flatten }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GlareRays { #[default] Two, Four, Six }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GlareBlend { #[default] ColorDodge, LinearDodge }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SpectrumBlend { #[default] Overlay, SoftLight, Multiply }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CaType { #[default] Offset, Center }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CaChannels { #[default] Rgb, Bgr, MagentaGreen, YellowBlue, RedCyan }

/// Every look parameter, in numeric preset-slot order.
/// `Default` supplies the initial UI values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    pub threshold: f32,          // 0
    pub dr_compression: f32,     // 1
    pub main_opacity: f32,       // 2
    pub core_opacity: f32,       // 3
    pub radius: f32,             // 4
    pub exposure: f32,           // 5
    pub gamma: f32,              // 6
    pub glare_length: f32,       // 7
    pub glare_opacity: f32,      // 8
    pub glare_rotation: f32,     // 9
    pub glare_detail: f32,       // 10
    pub hue: f32,                // 11
    pub saturation: f32,         // 12
    pub spectrum_mix: f32,       // 13
    pub spectrum_frequency: f32, // 14
    pub spectrum_blur: f32,      // 15
    pub spectrum_saturation: f32,// 16
    pub ca_distance: f32,        // 17
    pub glare_rays: GlareRays,   // 18
    pub glare_on: bool,          // 19
    pub colorize: bool,          // 20
    pub glare_blend: GlareBlend, // 21
    pub ca_channels: CaChannels, // 22
    pub spectrum_blend: SpectrumBlend, // 23
    pub ca_type: CaType,         // 24
    pub protect_core: bool,      // 25
    pub transition: Transition,  // 26
    pub unmult: bool,            // 27
    pub ca_blur: f32,            // 28
    pub ca_direction: f32,       // 29
    pub highlights: Highlights,  // 30
    pub glare_sharpness: f32,    // 31
    pub aspect_ratio: f32,       // 32
    pub ca_on: bool,             // 33
    pub ca_mix: bool,            // 34
    pub ca_intensity: f32,       // 35
    pub aspect_rotation: f32,    // 36
    pub rays_on: bool,           // 37
    pub rays_center: [f32; 2],   // 38, 39 (normalized)
    pub rays_spacing: f32,       // 40
    pub rays_samples: f32,       // 41
    pub rays_falloff: f32,       // 42
    pub hdr32: bool,             // 43
    pub tonemap: bool,           // 44
    pub glare_gamma: f32,        // 45
    pub glow_iterations: f32,    // 46
}

const DEFAULT_SLOTS: [f64; 47] = [
    0.15, 30., 100., 100., 1000., 1., 1.3, 1.5, 100., 0., 0., 0., 0., 0.,
    4., 4., 100., 2., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 45., 0.,
    0., 1., 0., 0., 0., 0., 0., 0.5, 0.5, 5., 20., 0.5, 0., 0., 1., 11.,
];

impl Default for Params {
    fn default() -> Self { Self::decode(DEFAULT_SLOTS) }
}

/// Non-preset view state (spec: helper switches not stored in presets).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewState { pub auto_update: bool, pub xray: bool, pub solo: bool }

impl Default for ViewState {
    fn default() -> Self { Self { auto_update: true, xray: false, solo: false } }
}

/// Slider metadata for UI: (slot, label, min, max, default, step, decimals).
#[derive(Clone, Copy, Debug)]
pub struct SliderSpec { pub slot: usize, pub label: &'static str, pub min: f32, pub max: f32, pub default: f32, pub step: f32, pub decimals: u8, pub tooltip: &'static str }

impl Params {
    /// 47 numeric slots in preset order, rounded to each slot's save precision.
    pub fn to_slots(&self) -> [f64; 47] {
        let mut slots = self.raw_slots();
        for spec in SLIDERS {
            let value = &mut slots[spec.slot];
            if spec.decimals == 0 || matches!(spec.slot, 15 | 17 | 31) {
                *value = value.trunc();
            } else {
                let scale = 10_f64.powi(spec.decimals as i32);
                *value = (*value * scale).round() / scale;
            }
        }
        slots
    }
    /// Missing/null slots use loader fallbacks; non-finite values use slider defaults.
    /// Preset strings are represented as `Some(NaN)` by the JSON loader.
    pub fn from_slots(slots: &[Option<f64>]) -> Self {
        let mut values = DEFAULT_SLOTS;
        values[29] = -35.;
        values[34] = 1.;
        for (i, value) in values.iter_mut().enumerate() {
            if let Some(Some(v)) = slots.get(i) {
                *value = if v.is_finite() {
                    v.clamp(f32::MIN as f64, f32::MAX as f64)
                } else { DEFAULT_SLOTS[i] };
            }
        }
        if values[20] != 0. && slots.get(12).and_then(|v| *v).filter(|v| v.is_finite()).is_none() {
            values[12] = 100.;
        }
        let mut params = Self::decode(values);
        params.sanitize();
        params
    }
    /// All sliders, including the colorize-dependent hue/sat ranges and tonemap-dependent exposure min.
    pub fn slider_specs(&self) -> Vec<SliderSpec> {
        SLIDERS.map(|spec| self.current_slider(spec)).to_vec()
    }

    fn current_slider(&self, mut spec: SliderSpec) -> SliderSpec {
        match spec.slot {
            5 if self.tonemap => spec.min = 0.,
            11 if self.colorize => { spec.min = 0.; spec.max = 360.; }
            12 if self.colorize => { spec.min = 0.; spec.default = 100.; }
            _ => {}
        }
        spec
    }
    /// Clamp every field into its current valid range (honoring colorize/tonemap range switches, aspect==1 => rotation 0).
    pub fn sanitize(&mut self) {
        let mut slots = self.raw_slots();
        for spec in SLIDERS.map(|spec| self.current_slider(spec)) {
            let value = &mut slots[spec.slot];
            *value = if value.is_finite() {
                value.clamp(spec.min as f64, spec.max as f64)
            } else { spec.default as f64 };
        }
        if self.tonemap && slots[5] <= 0. { slots[5] = 0.1; }
        if slots[32] == 1. { slots[36] = 0.; }
        *self = Self::decode(slots);
    }
    /// Dynamic-range dropdown side effects (Flatten: thr .25, exp 3, DR 100; Preserve: .15, 1, 30).
    pub fn set_highlights(&mut self, h: Highlights) {
        self.highlights = h;
        (self.threshold, self.exposure, self.dr_compression) = match h {
            Highlights::Flatten => (0.25, 3., 100.),
            Highlights::Preserve => (0.15, 1., 30.),
        };
    }
    /// Tonemap toggle side effect: exposure min 0, E<=0 -> 0.1.
    pub fn set_tonemap(&mut self, on: bool) { self.tonemap = on; self.sanitize(); }
    /// Colorize toggle: hue/sat range switch.
    pub fn set_colorize(&mut self, on: bool) {
        self.colorize = on;
        self.hue = 0.;
        self.saturation = if on { 100. } else { 0. };
        self.sanitize();
    }

    fn raw_slots(&self) -> [f64; 47] {
        [
            self.threshold as f64, self.dr_compression as f64, self.main_opacity as f64,
            self.core_opacity as f64, self.radius as f64, self.exposure as f64,
            self.gamma as f64, self.glare_length as f64, self.glare_opacity as f64,
            self.glare_rotation as f64, self.glare_detail as f64, self.hue as f64,
            self.saturation as f64, self.spectrum_mix as f64, self.spectrum_frequency as f64,
            self.spectrum_blur as f64, self.spectrum_saturation as f64, self.ca_distance as f64,
            self.glare_rays as u8 as f64, self.glare_on as u8 as f64,
            self.colorize as u8 as f64, self.glare_blend as u8 as f64,
            self.ca_channels as u8 as f64, self.spectrum_blend as u8 as f64,
            self.ca_type as u8 as f64, self.protect_core as u8 as f64,
            self.transition as u8 as f64, self.unmult as u8 as f64,
            self.ca_blur as f64, self.ca_direction as f64, self.highlights as u8 as f64,
            self.glare_sharpness as f64, self.aspect_ratio as f64, self.ca_on as u8 as f64,
            self.ca_mix as u8 as f64, self.ca_intensity as f64, self.aspect_rotation as f64,
            self.rays_on as u8 as f64, self.rays_center[0] as f64, self.rays_center[1] as f64,
            self.rays_spacing as f64, self.rays_samples as f64, self.rays_falloff as f64,
            self.hdr32 as u8 as f64, self.tonemap as u8 as f64, self.glare_gamma as f64,
            self.glow_iterations as f64,
        ]
    }

    fn decode(s: [f64; 47]) -> Self {
        Self {
            threshold: s[0] as f32, dr_compression: s[1] as f32,
            main_opacity: s[2] as f32, core_opacity: s[3] as f32,
            radius: s[4] as f32, exposure: s[5] as f32, gamma: s[6] as f32,
            glare_length: s[7] as f32, glare_opacity: s[8] as f32,
            glare_rotation: s[9] as f32, glare_detail: s[10] as f32,
            hue: s[11] as f32, saturation: s[12] as f32, spectrum_mix: s[13] as f32,
            spectrum_frequency: s[14] as f32, spectrum_blur: s[15] as f32,
            spectrum_saturation: s[16] as f32, ca_distance: s[17] as f32,
            glare_rays: match s[18] { 1. => GlareRays::Four, 2. => GlareRays::Six, _ => GlareRays::Two },
            glare_on: s[19] != 0., colorize: s[20] != 0.,
            glare_blend: if s[21] == 1. { GlareBlend::LinearDodge } else { GlareBlend::ColorDodge },
            ca_channels: match s[22] { 1. => CaChannels::Bgr, 2. => CaChannels::MagentaGreen, 3. => CaChannels::YellowBlue, 4. => CaChannels::RedCyan, _ => CaChannels::Rgb },
            spectrum_blend: match s[23] { 1. => SpectrumBlend::SoftLight, 2. => SpectrumBlend::Multiply, _ => SpectrumBlend::Overlay },
            ca_type: if s[24] == 1. { CaType::Center } else { CaType::Offset },
            protect_core: s[25] != 0.,
            transition: if s[26] == 1. { Transition::Precise } else { Transition::Soft },
            unmult: s[27] != 0., ca_blur: s[28] as f32, ca_direction: s[29] as f32,
            highlights: if s[30] == 1. { Highlights::Flatten } else { Highlights::Preserve },
            glare_sharpness: s[31] as f32, aspect_ratio: s[32] as f32,
            ca_on: s[33] != 0., ca_mix: s[34] != 0., ca_intensity: s[35] as f32,
            aspect_rotation: s[36] as f32, rays_on: s[37] != 0.,
            rays_center: [s[38] as f32, s[39] as f32],
            rays_spacing: s[40] as f32, rays_samples: s[41] as f32,
            rays_falloff: s[42] as f32, hdr32: s[43] != 0., tonemap: s[44] != 0.,
            glare_gamma: s[45] as f32, glow_iterations: s[46] as f32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode { Draft, Hq }

/// Draft bounds of 1500x1300 and a 4000-pixel HQ long-side cap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocGeometry { pub orig_w: u32, pub orig_h: u32 }
impl DocGeometry {
    /// Working size for a mode (Draft: landscape width->min(W,1500) / else height->min(H,1300), aspect kept; Hq: long side capped at 4000).
    pub fn working_size(&self, mode: RenderMode) -> (u32, u32) {
        let w = self.orig_w.max(1);
        let h = self.orig_h.max(1);
        let (side, limit) = match mode {
            RenderMode::Draft if w > h => (w, 1500),
            RenderMode::Draft => (h, 1300),
            RenderMode::Hq => (w.max(h), 4000),
        };
        let scale = f64::from(side.min(limit)) / f64::from(side);
        // CALIBRATE: Photoshop resize's integer-pixel rounding is undocumented; use nearest.
        (((w as f64 * scale).round() as u32).max(1),
         ((h as f64 * scale).round() as u32).max(1))
    }
    /// Blur-radius scale: 1 in Draft; HQ working size relative to Draft bounds.
    pub fn radius_scale(&self, mode: RenderMode) -> f32 {
        if mode == RenderMode::Draft { return 1.; }
        let (w, h) = self.working_size(mode);
        if self.orig_w > self.orig_h {
            w as f32 / self.orig_w.clamp(1, 1500) as f32
        } else {
            h as f32 / self.orig_h.clamp(1, 1300) as f32
        }
    }
    /// Working long side normalized to 3000 pixels.
    pub fn spectrum_scale(&self, mode: RenderMode) -> f32 {
        let (w, h) = self.working_size(mode);
        w.max(h) as f32 / 3000.
    }
    /// Smart-sharpen scale relative to the Draft dimension bounds.
    pub fn sharpen_scale(&self, mode: RenderMode) -> f32 {
        let (w, h) = self.working_size(mode);
        if w > h { w as f32 / 1500. } else { h as f32 / 1300. }
    }
    pub fn ca_scale(&self, mode: RenderMode) -> f32 {
        let (w, h) = self.working_size(mode);
        // Both orientations normalize against the bounded source width.
        w.max(h) as f32 / self.orig_w.clamp(1, 1500) as f32
    }
    pub fn ca_blur_base(&self, mode: RenderMode, blur: f32) -> f32 {
        let (w, h) = self.working_size(mode);
        let denominator = if self.orig_h >= 1300 { 1300 } else { self.orig_w.clamp(1, 1500) };
        0.35 * blur * w.max(h) as f32 / denominator as f32
    }
}

const fn slider(slot: usize, label: &'static str, min: f32, max: f32, default: f32, step: f32, decimals: u8, tooltip: &'static str) -> SliderSpec {
    SliderSpec { slot, label, min, max, default, step, decimals, tooltip }
}

const SLIDERS: [SliderSpec; 31] = [
    slider(0, "Threshold", 0.01, 2., 0.15, 0.01, 2, "Choose which highlights emit glow."),
    slider(1, "DR compression", 1., 254., 30., 1., 0, "Expand the extracted highlight range."),
    slider(2, "Main opacity", 1., 100., 100., 1., 0, "Set overall glow strength."),
    slider(3, "Core opacity", 1., 100., 100., 1., 0, "Set the strength of the tight glow layers."),
    slider(4, "Radius", 1., 1000., 1000., 1., 0, "Set glow spread in preview pixels."),
    slider(5, "Exposure", -10., 10., 1., 0.01, 2, "Brighten or darken the glow."),
    slider(6, "Gamma", 0.01, 2.2, 1.3, 0.01, 2, "Shape the glow's tonal response."),
    slider(7, "Glare length", 0.01, 3., 1.5, 0.01, 2, "Extend glare streaks."),
    slider(8, "Glare opacity", 1., 100., 100., 1., 0, "Set glare strength."),
    slider(9, "Glare rotation", -90., 90., 0., 1., 0, "Turn the glare streaks."),
    slider(10, "Glare detail", 0., 100., 0., 1., 0, "Break glare into finer streaks."),
    slider(11, "Hue", -180., 180., 0., 1., 0, "Shift hue or choose a colorized hue."),
    slider(12, "Saturation", -100., 100., 0., 1., 0, "Adjust glow color strength."),
    slider(13, "Spectrum mix", 0., 100., 0., 1., 0, "Mix a colored spectrum into glare."),
    slider(14, "Spectrum frequency", 1., 10., 4., 1., 0, "Set spectrum band spacing."),
    slider(15, "Spectrum blur", 0., 10., 4., 0.1, 1, "Soften spectrum bands."),
    slider(16, "Spectrum saturation", 0., 100., 100., 1., 0, "Set spectrum color strength."),
    slider(17, "CA distance", 0., 200., 2., 0.01, 2, "Separate chromatic channels."),
    slider(28, "CA blur", 0., 100., 0., 1., 0, "Blur separated channels."),
    slider(29, "CA direction", -180., 180., 45., 1., 0, "Choose the channel offset direction."),
    slider(31, "Glare sharpness", 0., 10., 0., 0.1, 1, "Sharpen glare edges."),
    slider(32, "Aspect ratio", 1., 2., 1., 0.01, 2, "Stretch glow along one axis."),
    slider(35, "CA intensity", -100., 100., 0., 0.5, 1, "Adjust separated-channel contrast."),
    slider(36, "Aspect rotation", -90., 90., 0., 1., 0, "Turn the stretched glow."),
    slider(38, "Rays center X", -1., 1., 0.5, 0.01, 2, "Move the ray origin horizontally."),
    slider(39, "Rays center Y", -1., 1., 0.5, 0.01, 2, "Move the ray origin vertically."),
    slider(40, "Rays spacing", 0.01, 100., 5., 0.01, 2, "Set spacing between ray samples."),
    slider(41, "Rays samples", 1., 100., 20., 1., 0, "Set how many ray samples accumulate."),
    slider(42, "Rays falloff", 0.01, 1., 0.5, 0.01, 2, "Fade successive ray samples."),
    slider(45, "Glare gamma", 0.01, 9., 1., 0.01, 2, "Shape glare brightness."),
    slider(46, "Glow iterations", 1., 11., 11., 1., 0, "Set the number of additional glow layers."),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) { assert!((a - b).abs() < 0.00001, "{a} != {b}"); }

    #[test]
    fn default_slots_roundtrip_and_save_precision() {
        let p = Params::default();
        assert_eq!(p.to_slots(), DEFAULT_SLOTS);
        assert_eq!(Params::from_slots(&p.to_slots().map(Some)), p);
        let p = Params { spectrum_blur: 4.9, ca_distance: 2.99, glare_sharpness: 9.9,
            ca_intensity: -1.26, exposure: -1.236, ..p };
        let s = p.to_slots();
        assert_eq!((s[15], s[17], s[31]), (4., 2., 9.));
        assert_eq!((s[35], s[5]), (-1.3, -1.24));
    }

    #[test]
    fn loader_fallbacks_and_dynamic_ranges() {
        let p = Params::from_slots(&[]);
        assert_eq!((p.ca_direction, p.ca_mix), (-35., true));
        let mut slots = Params::default().to_slots().map(Some);
        slots[0] = Some(f64::NAN);
        slots[20] = Some(1.);
        slots[11] = Some(300.);
        slots[12] = Some(80.);
        slots[36] = Some(45.);
        let mut p = Params::from_slots(&slots);
        assert_eq!((p.threshold, p.hue, p.saturation, p.aspect_rotation), (0.15, 300., 80., 0.));
        p.exposure = -2.;
        p.set_tonemap(true);
        assert_eq!(p.exposure, 0.1);
        p.set_colorize(false);
        assert_eq!((p.hue, p.saturation), (0., 0.));
        p.set_highlights(Highlights::Flatten);
        assert_eq!((p.threshold, p.exposure, p.dr_compression), (0.25, 3., 100.));
    }

    #[test]
    fn document_geometry_scales() {
        let landscape = DocGeometry { orig_w: 3000, orig_h: 2000 };
        assert_eq!(landscape.working_size(RenderMode::Draft), (1500, 1000));
        assert_eq!(landscape.working_size(RenderMode::Hq), (3000, 2000));
        near(landscape.radius_scale(RenderMode::Hq), 2.);
        near(landscape.spectrum_scale(RenderMode::Draft), 0.5);
        near(landscape.sharpen_scale(RenderMode::Draft), 1.);
        let portrait = DocGeometry { orig_w: 1000, orig_h: 3000 };
        assert_eq!(portrait.working_size(RenderMode::Draft), (433, 1300));
        near(portrait.radius_scale(RenderMode::Hq), 3000. / 1300.);
        near(portrait.ca_scale(RenderMode::Draft), 1.3);
        near(portrait.ca_blur_base(RenderMode::Draft, 2.), 0.7);
        let large = DocGeometry { orig_w: 9000, orig_h: 6000 };
        assert_eq!(large.working_size(RenderMode::Hq), (4000, 2667));
        near(large.radius_scale(RenderMode::Hq), 4000. / 1500.);
        let square = DocGeometry { orig_w: 5000, orig_h: 5000 };
        assert_eq!(square.working_size(RenderMode::Draft), (1300, 1300));
        assert_eq!(square.working_size(RenderMode::Hq), (4000, 4000));
    }
}
