use crate::mask::Mask;
use moya_core::{LayerSettings, Params};
use moya_project::{Project, ProjectLayer, SourceRef};

pub struct Layer {
    pub settings: LayerSettings,
    pub mask: Mask,
}

impl Layer {
    pub fn new(id: u64, name: impl Into<String>) -> Self {
        Self { settings: LayerSettings { id, name: name.into(), ..LayerSettings::default() }, mask: Mask::default() }
    }

    pub fn duplicate(&self, id: u64) -> Self {
        let mut settings = self.settings.clone();
        settings.id = id;
        settings.name = format!("{} copy", settings.name);
        Self { settings, mask: self.mask.duplicate() }
    }
}

/// New sources inherit the selected look, never the previous document's stack or masks.
pub fn new_document(params: Params) -> Vec<Layer> {
    vec![Layer {
        settings: LayerSettings { params, ..LayerSettings::default() },
        mask: Mask::default(),
    }]
}

pub fn to_project(layers: &[Layer], selected: u64, source: SourceRef) -> Project {
    Project {
        version: 1,
        source,
        layers: layers.iter().map(|layer| ProjectLayer { settings: layer.settings.clone(), mask: layer.mask.export() }).collect(),
        selected: Some(selected),
    }
}

/// Project masks retain their own resolution on disk; source imports resize once here.
pub fn from_project(project: &Project, width: u32, height: u32) -> anyhow::Result<(Vec<Layer>, u64)> {
    anyhow::ensure!(!project.layers.is_empty(), "project has no layers");
    let mut ids = std::collections::HashSet::new();
    let mut layers = Vec::with_capacity(project.layers.len());
    for layer in &project.layers {
        anyhow::ensure!(ids.insert(layer.settings.id), "duplicate layer id");
        let mask = match &layer.mask {
            Some(mask) => Mask::import(mask, width, height)?,
            None => { let mut mask = Mask::default(); mask.reset(width, height); mask },
        };
        layers.push(Layer { settings: layer.settings.clone(), mask });
    }
    let selected = project.selected.filter(|id| ids.contains(id)).unwrap_or(layers[0].settings.id);
    Ok((layers, selected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use moya_project::MaskData;

    #[test]
    fn new_document_has_one_default_layer_and_only_inherits_params() {
        let mut params = Params::default();
        params.exposure = 3.0;
        let layers = new_document(params.clone());
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].settings, LayerSettings { params, ..LayerSettings::default() });
        assert!(layers[0].mask.is_empty());
        assert!(!layers[0].mask.enabled);
        assert!(layers[0].mask.export().is_none());
    }

    #[test]
    fn project_roundtrip_preserves_independent_disabled_masks_and_resizes_import() {
        let mask = MaskData { width: 2, height: 1, enabled: false, data: vec![0, 65535] };
        let mut first = Layer::new(7, "Painted");
        first.mask = Mask::import(&mask, 2, 1).unwrap();
        let mut second = first.duplicate(9);
        second.mask.enabled = true;
        second.settings.opacity = 0.25;
        let layers = vec![first, second];
        let project = to_project(&layers, 9, SourceRef::Path("image.png".into()));
        let bytes = moya_project::to_bytes(&project).unwrap();
        let decoded = moya_project::from_bytes(&bytes).unwrap();
        let (restored, selected) = from_project(&decoded, 2, 1).unwrap();
        assert_eq!(selected, 9);
        assert_eq!(to_project(&restored, selected, project.source.clone()), project);
        assert!(!restored[0].mask.enabled);
        assert!(restored[1].mask.enabled);
        let (resized, _) = from_project(&project, 4, 2).unwrap();
        let data = resized[0].mask.export().unwrap();
        assert_eq!((data.width, data.height, data.enabled), (4, 2, false));
        assert_eq!(data.data[0], 0);
        assert_eq!(data.data[3], 65535);
    }
}
