use crate::layers::Layer;
use moya_core::LayerSettings;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Debug, PartialEq)]
struct State {
    layers: Vec<(LayerSettings, bool)>,
    selected: u64,
}
impl State {
    fn capture(layers: &[Layer], selected: u64) -> Self {
        Self { layers: layers.iter().map(|l| (l.settings.clone(), l.mask.enabled)).collect(), selected }
    }
    fn matches(&self, layers: &[Layer], selected: u64) -> bool {
        self.selected == selected && self.layers.len() == layers.len()
            && self.layers.iter().zip(layers).all(|((settings, enabled), layer)|
                *settings == layer.settings && *enabled == layer.mask.enabled)
    }
}

enum Edit { Stack(State), Mask(u64) }

/// Settings snapshots never clone image buffers. Removed layers retain their mask engines here.
pub(super) struct History {
    past: VecDeque<Edit>,
    future: Vec<Edit>,
    current: State,
    pending: Option<State>,
    removed: HashMap<u64, Layer>,
    next_id: u64,
    mask_limit: usize,
}

impl History {
    pub fn new(layers: &[Layer], selected: u64) -> Self {
        Self { past: VecDeque::new(), future: Vec::new(), current: State::capture(layers, selected),
            pending: None, removed: HashMap::new(), next_id: layers.iter().map(|l| l.settings.id).max().unwrap_or(0).saturating_add(1),
            mask_limit: 256 * 1024 * 1024 }
    }

    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = id.checked_add(1).expect("layer IDs exhausted");
        id
    }

    fn branch(&mut self, layers: &mut [Layer]) {
        self.future.clear();
        for layer in layers.iter_mut().chain(self.removed.values_mut()) { layer.mask.clear_redo(); }
    }

    /// Coalesce pointer drags; selection and the complete settings stack share one timeline.
    /// Also drains every active mask engine's new-step and eviction markers.
    pub fn observe(&mut self, layers: &mut Vec<Layer>, selected: u64, pointer_down: bool) {
        let changed = !self.current.matches(layers, selected);
        self.next_id = self.next_id.max(layers.iter().map(|l| l.settings.id).max().unwrap_or(0).saturating_add(1));
        if changed {
            self.pending.get_or_insert_with(|| self.current.clone());
            self.current = State::capture(layers, selected);
            self.branch(layers);
        }
        if !pointer_down { self.commit(); }
        for index in 0..layers.len() {
            let id = layers[index].settings.id;
            let (steps, evicted) = layers[index].mask.take_history_steps();
            let prior_steps = self.past.iter().filter(|e| matches!(e, Edit::Mask(owner) if *owner == id)).count();
            self.drop_oldest_mask(id, evicted);
            if steps > 0 {
                self.commit(); self.branch(layers);
                for _ in 0..steps.saturating_sub(evicted.saturating_sub(prior_steps)) { self.past.push_back(Edit::Mask(id)); }
            }
        }
        self.trim(layers);
    }

    fn commit(&mut self) {
        if let Some(before) = self.pending.take() {
            if before != self.current { self.past.push_back(Edit::Stack(before)); }
        }
    }

    pub fn remove_layer(&mut self, layers: &mut Vec<Layer>, selected: &mut u64, id: u64) -> bool {
        if layers.len() <= 1 { return false; }
        let Some(index) = layers.iter().position(|l| l.settings.id == id) else { return false; };
        layers[index].mask.end_stroke();
        self.observe(layers, *selected, false);
        let mut layer = layers.remove(index);
        layer.mask.release_gpu();
        self.removed.insert(id, layer);
        if *selected == id { *selected = layers[index.min(layers.len() - 1)].settings.id; }
        self.observe(layers, *selected, false);
        true
    }

    fn drop_oldest_mask(&mut self, id: u64, mut count: usize) {
        self.past.retain(|edit| {
            if count > 0 && matches!(edit, Edit::Mask(layer) if *layer == id) { count -= 1; false } else { true }
        });
    }

    fn trim(&mut self, layers: &mut [Layer]) {
        while self.past.iter().filter(|e| matches!(e, Edit::Stack(_))).count() > 200 {
            let index = self.past.iter().position(|e| matches!(e, Edit::Stack(_))).unwrap();
            self.past.remove(index);
        }
        loop {
            let bytes: usize = layers.iter().chain(self.removed.values()).map(|l| l.mask.history_bytes()).sum();
            let count = self.past.iter().chain(&self.future).filter(|e| matches!(e, Edit::Mask(_))).count();
            if bytes <= self.mask_limit && count <= 200 { break; }
            if let Some(index) = self.past.iter().position(|e| matches!(e, Edit::Mask(_))) {
                let Edit::Mask(id) = self.past.remove(index).unwrap() else { unreachable!() };
                if let Some(layer) = layers.iter_mut().find(|l| l.settings.id == id).or_else(|| self.removed.get_mut(&id)) {
                    layer.mask.drop_oldest_history();
                }
            } else if let Some(index) = self.future.iter().position(|e| matches!(e, Edit::Mask(_))) {
                let Edit::Mask(id) = self.future.remove(index) else { unreachable!() };
                if let Some(layer) = layers.iter_mut().find(|l| l.settings.id == id).or_else(|| self.removed.get_mut(&id)) {
                    layer.mask.drop_farthest_redo();
                }
            } else { break; }
        }
        self.prune_removed();
        // Deleted masks retain two-byte CPU pixels; retention shares the global diff budget.
        while layers.iter().map(|l| l.mask.history_bytes()).sum::<usize>()
            + self.removed.values().map(|l| l.mask.retained_bytes()).sum::<usize>() > self.mask_limit
        {
            let (edit, redo) = if let Some(edit) = self.past.pop_front() { (edit, false) }
                else if !self.future.is_empty() { (self.future.remove(0), true) }
                else { break; };
            if let Edit::Mask(id) = edit {
                if let Some(layer) = layers.iter_mut().find(|l| l.settings.id == id).or_else(|| self.removed.get_mut(&id)) {
                    if redo { layer.mask.drop_farthest_redo(); } else { layer.mask.drop_oldest_history(); }
                }
            }
            self.prune_removed();
        }
    }

    fn prune_removed(&mut self) {
        if self.removed.is_empty() { return; }
        let mut retained: HashSet<u64> = self.current.layers.iter().map(|(l, _)| l.id).collect();
        for edit in self.past.iter().chain(&self.future) {
            match edit {
                Edit::Stack(state) => retained.extend(state.layers.iter().map(|(l, _)| l.id)),
                Edit::Mask(id) => { retained.insert(*id); }
            }
        }
        if let Some(state) = &self.pending { retained.extend(state.layers.iter().map(|(l, _)| l.id)); }
        self.removed.retain(|id, _| retained.contains(id));
    }

    pub fn clear(&mut self, layers: &[Layer], selected: u64) {
        *self = Self::new(layers, selected);
    }
    pub fn can_undo(&self) -> bool { !self.past.is_empty() || self.pending.as_ref().is_some_and(|p| *p != self.current) }
    pub fn can_redo(&self) -> bool { !self.future.is_empty() }

    fn apply(&mut self, edit: Edit, layers: &mut Vec<Layer>, selected: &mut u64, redo: bool) -> Edit {
        match edit {
            Edit::Stack(state) => {
                let previous = std::mem::replace(&mut self.current, state);
                for mut layer in layers.drain(..) {
                    if !self.current.layers.iter().any(|(settings, _)| settings.id == layer.settings.id) { layer.mask.release_gpu(); }
                    self.removed.insert(layer.settings.id, layer);
                }
                for (settings, enabled) in &self.current.layers {
                    let mut layer = self.removed.remove(&settings.id).expect("history retains removed layers");
                    layer.settings = settings.clone(); layer.mask.enabled = *enabled;
                    layers.push(layer);
                }
                *selected = self.current.selected;
                Edit::Stack(previous)
            }
            Edit::Mask(id) => {
                let layer = layers.iter_mut().find(|l| l.settings.id == id).or_else(|| self.removed.get_mut(&id)).expect("history retains mask owner");
                if redo { layer.mask.redo(); } else { layer.mask.undo(); }
                Edit::Mask(id)
            }
        }
    }

    pub fn undo(&mut self, layers: &mut Vec<Layer>, selected: &mut u64) -> bool {
        for layer in layers.iter_mut() { layer.mask.end_stroke(); }
        self.observe(layers, *selected, false);
        let Some(edit) = self.past.pop_back() else { return false; };
        let inverse = self.apply(edit, layers, selected, false);
        self.future.push(inverse); true
    }
    pub fn redo(&mut self, layers: &mut Vec<Layer>, selected: &mut u64) -> bool {
        self.observe(layers, *selected, false);
        let Some(edit) = self.future.pop() else { return false; };
        let inverse = self.apply(edit, layers, selected, true);
        self.past.push_back(inverse); true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_operations_and_selection_undo_as_one_stack() {
        let mut layers = vec![Layer::new(1, "First")];
        let mut selected = 1;
        let mut history = History::new(&layers, selected);
        let added = history.next_id();
        layers.push(Layer::new(added, "Added")); selected = added;
        history.observe(&mut layers, selected, false);
        let duplicated = history.next_id();
        layers.push(layers[1].duplicate(duplicated)); selected = duplicated;
        history.observe(&mut layers, selected, false);
        layers.swap(0, 2);
        layers[0].settings.name = "Renamed".into();
        layers[0].settings.opacity = 0.4;
        layers[0].settings.params.exposure = 3.0;
        history.observe(&mut layers, selected, false);
        assert!(history.remove_layer(&mut layers, &mut selected, duplicated));
        assert_eq!(layers.len(), 2);
        assert_ne!(selected, duplicated);
        assert!(history.undo(&mut layers, &mut selected));
        assert_eq!(selected, duplicated);
        assert_eq!(layers[0].settings.name, "Renamed");
        assert_eq!(layers[0].settings.params.exposure, 3.0);
        assert!(history.undo(&mut layers, &mut selected));
        assert_eq!(layers[0].settings.id, 1);
        assert_eq!(layers[2].settings.name, "Added copy");
        assert!(history.undo(&mut layers, &mut selected));
        assert_eq!(selected, added);
        assert_eq!(layers.len(), 2);
        assert!(history.undo(&mut layers, &mut selected));
        assert_eq!(selected, 1);
        assert_eq!(layers.len(), 1);
        for _ in 0..4 { assert!(history.redo(&mut layers, &mut selected)); }
        assert_eq!(layers.len(), 2);
        assert!(!history.remove_layer(&mut vec![Layer::new(1, "Only")], &mut 1, 1));
    }

    #[test]
    fn mask_timeline_owns_deleted_masks_and_trims_globally() {
        let mut layers = vec![Layer::new(1, "One"), Layer::new(2, "Two")];
        for layer in &mut layers { layer.mask.reset(4, 1); }
        let mut selected = 1;
        let mut history = History::new(&layers, selected);
        layers[0].mask.invert(); history.observe(&mut layers, selected, false);
        let painted = layers[0].mask.fingerprint();
        layers[1].mask.invert(); history.observe(&mut layers, selected, false);
        assert!(history.remove_layer(&mut layers, &mut selected, 1));
        assert!(history.undo(&mut layers, &mut selected));
        assert_eq!(layers[0].mask.fingerprint(), painted);
        assert!(history.undo(&mut layers, &mut selected));
        assert!(layers[1].mask.is_empty());
        assert!(history.undo(&mut layers, &mut selected));
        assert!(layers[0].mask.is_empty());
        assert!(history.redo(&mut layers, &mut selected));
        assert_eq!(layers[0].mask.fingerprint(), painted);
        history.mask_limit = 0;
        history.observe(&mut layers, selected, false);
        assert_eq!(layers.iter().map(|l| l.mask.history_bytes()).sum::<usize>(), 0);
    }

    #[test]
    fn drag_coalesces_and_branch_discards_mask_redo() {
        let mut layers = vec![Layer::new(1, "One")]; layers[0].mask.reset(2, 1);
        let mut selected = 1;
        let mut history = History::new(&layers, selected);
        for value in [2.0, 3.0, 4.0] {
            layers[0].settings.params.exposure = value;
            history.observe(&mut layers, selected, true);
        }
        history.observe(&mut layers, selected, false);
        layers[0].mask.invert(); history.observe(&mut layers, selected, false);
        assert!(history.undo(&mut layers, &mut selected));
        assert!(history.undo(&mut layers, &mut selected));
        assert!(!history.can_undo());
        assert!(history.redo(&mut layers, &mut selected));
        layers[0].settings.name = "New branch".into();
        history.observe(&mut layers, selected, false);
        assert!(!history.can_redo());
        assert!(!layers[0].mask.redo());
    }

    #[test]
    fn deleted_u16_mask_fits_exact_pixel_budget_and_restores_exactly() {
        let saved=moya_project::MaskData { width:4,height:1,enabled:false,data:vec![0,1,32768,65535] };
        let mut layers=vec![Layer::new(1,"Masked"),Layer::new(2,"Other")];
        layers[0].mask=crate::mask::Mask::import(&saved,4,1).unwrap();
        let mut selected=1;
        let mut history=History::new(&layers,selected);
        history.mask_limit=8;
        assert!(history.remove_layer(&mut layers,&mut selected,1));
        assert_eq!(history.removed[&1].mask.retained_bytes(),8);
        assert!(history.undo(&mut layers,&mut selected));
        assert_eq!(layers[0].mask.export().unwrap(),saved);
        assert!(layers[0].mask.gpu_image().is_none());
        assert!(history.redo(&mut layers,&mut selected));
        history.mask_limit=7;history.observe(&mut layers,selected,false);
        assert!(history.removed.is_empty());assert!(!history.can_undo());
    }

    #[test]
    fn enabled_toggle_and_deleted_image_retention_are_bounded() {
        let mut layers = vec![Layer::new(1, "One"), Layer::new(2, "Two")];
        layers[0].mask.reset(16, 16);
        let mut selected = 1;
        let mut history = History::new(&layers, selected);
        layers[0].mask.enabled = true;
        history.observe(&mut layers, selected, false);
        assert!(history.undo(&mut layers, &mut selected));
        assert!(!layers[0].mask.enabled);
        assert!(history.redo(&mut layers, &mut selected));
        assert!(layers[0].mask.enabled);
        history.mask_limit = 1;
        assert!(history.remove_layer(&mut layers, &mut selected, 1));
        assert!(history.removed.is_empty());
        assert!(!history.can_undo());
    }
}
