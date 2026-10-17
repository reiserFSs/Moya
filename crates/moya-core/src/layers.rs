//! Layer stack model shared by the pipeline, project files, the app and the Photoshop bridge.
use crate::Params;
use serde::{Deserialize, Serialize};

/// How a layer's glow is composited onto the layers below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LayerBlend {
    /// Screen on 8/16-bit hosts, Linear Dodge on 32-bit hosts (the single-layer behaviour).
    #[default]
    Auto,
    Screen,
    LinearDodge,
    Lighten,
    Normal,
}

/// What a layer extracts light from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LayerInput {
    /// The original document image (layers are independent; order only affects blending).
    #[default]
    Original,
    /// The composite of the visible layers below (glow-on-glow).
    Below,
}

/// One effect layer. Masks are stored separately (they are images, not settings).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerSettings {
    /// Stable identity within a document (cache keys, masks, undo, project files).
    pub id: u64,
    pub name: String,
    pub visible: bool,
    /// 0..=1, applied when compositing the layer's glow.
    pub opacity: f32,
    pub blend: LayerBlend,
    pub input: LayerInput,
    pub params: Params,
}

impl Default for LayerSettings {
    fn default() -> Self {
        Self { id: 1, name: "Layer 1".into(), visible: true, opacity: 1.0, blend: LayerBlend::Auto, input: LayerInput::Original, params: Params::default() }
    }
}

impl LayerSettings {
    /// Clamp opacity and sanitize params.
    pub fn sanitize(&mut self) {
        self.opacity = if self.opacity.is_finite() { self.opacity.clamp(0.0, 1.0) } else { 1.0 };
        self.params.sanitize();
    }
}
