use moya_core::Params;
use std::collections::VecDeque;

#[derive(Debug, PartialEq)]
pub enum Edit {
    Params(Params),
    Mask,
}

pub(super) struct History {
    past: VecDeque<Edit>,
    future: Vec<Edit>,
    past_params: usize,
    current: Params,
    pending: Option<Params>,
}

impl History {
    pub fn new(params: &Params) -> Self {
        Self {
            past: VecDeque::new(),
            future: Vec::new(),
            past_params: 0,
            current: params.clone(),
            pending: None,
        }
    }

    /// Pointer edits form one transaction, committed when the last button is released.
    pub fn observe(&mut self, params: &Params, pointer_down: bool) {
        if *params != self.current {
            self.pending.get_or_insert_with(|| self.current.clone());
            self.current = params.clone();
            self.future.clear();
        }
        if !pointer_down {
            self.commit();
        }
    }

    fn commit(&mut self) {
        if let Some(before) = self.pending.take() {
            if before != self.current {
                self.push_params(before);
            }
        }
    }

    fn push_params(&mut self, params: Params) {
        // Mask retention belongs to the mask engine, not the parameter history cap.
        if self.past_params == 200 {
            let oldest = self.past.iter().position(|edit| matches!(edit, Edit::Params(_))).unwrap();
            self.past.remove(oldest);
        } else {
            self.past_params += 1;
        }
        self.past.push_back(Edit::Params(params));
    }

    /// Commit parameter edits before recording a mask-engine-owned undo step.
    pub fn mask_step(&mut self) {
        self.commit();
        self.past.push_back(Edit::Mask);
        self.future.clear();
    }

    pub fn clear(&mut self, params: &Params) {
        self.past.clear();
        self.future.clear();
        self.past_params = 0;
        self.current = params.clone();
        self.pending = None;
    }

    /// Remove applied mask steps whose diffs the mask engine has evicted.
    pub fn drop_oldest_mask(&mut self, mut count: usize) {
        self.past.retain(|edit| {
            if count > 0 && matches!(edit, Edit::Mask) {
                count -= 1;
                false
            } else {
                true
            }
        });
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty() || self.pending.as_ref().is_some_and(|p| *p != self.current)
    }

    pub fn can_redo(&self) -> bool { !self.future.is_empty() }

    pub fn undo(&mut self) -> Option<Edit> {
        self.commit();
        match self.past.pop_back()? {
            Edit::Params(previous) => {
                self.past_params -= 1;
                self.future.push(Edit::Params(std::mem::replace(&mut self.current, previous)));
                Some(Edit::Params(self.current.clone()))
            }
            Edit::Mask => {
                self.future.push(Edit::Mask);
                Some(Edit::Mask)
            }
        }
    }

    pub fn redo(&mut self) -> Option<Edit> {
        self.commit();
        match self.future.pop()? {
            Edit::Params(next) => {
                let previous = std::mem::replace(&mut self.current, next);
                self.push_params(previous);
                Some(Edit::Params(self.current.clone()))
            }
            Edit::Mask => {
                self.past.push_back(Edit::Mask);
                Some(Edit::Mask)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_coalesces_and_new_edit_discards_redo() {
        let mut params = Params::default();
        let initial = params.clone();
        let mut history = History::new(&params);
        for value in [1.0, 2.0, 3.0] {
            params.exposure = value;
            history.observe(&params, true);
        }
        history.observe(&params, false);
        assert_eq!(history.undo(), Some(Edit::Params(initial)));
        assert_eq!(history.redo(), Some(Edit::Params(params.clone())));
        history.undo();
        params.exposure = 4.0;
        history.observe(&params, false);
        assert!(!history.can_redo());
        for value in 5..250 {
            params.exposure = value as f32;
            history.observe(&params, false);
        }
        assert_eq!(history.past.len(), 200);
    }

    #[test]
    fn mixed_timeline_commits_pending_params_and_branches() {
        let initial = Params::default();
        let mut params = initial.clone();
        let mut history = History::new(&params);
        params.exposure += 1.0;
        history.observe(&params, true);
        history.mask_step();
        let first = params.clone();
        params.exposure += 1.0;
        history.observe(&params, false);
        history.mask_step();

        assert_eq!(history.undo(), Some(Edit::Mask));
        assert_eq!(history.undo(), Some(Edit::Params(first.clone())));
        assert_eq!(history.undo(), Some(Edit::Mask));
        assert_eq!(history.undo(), Some(Edit::Params(initial)));
        assert!(!history.can_undo());
        assert_eq!(history.redo(), Some(Edit::Params(first)));
        assert_eq!(history.redo(), Some(Edit::Mask));
        assert_eq!(history.redo(), Some(Edit::Params(params.clone())));
        assert_eq!(history.redo(), Some(Edit::Mask));
        assert!(!history.can_redo());

        history.undo();
        params.exposure += 1.0;
        history.observe(&params, true);
        assert!(history.can_undo());
        assert!(!history.can_redo());
        history.mask_step();
        assert_eq!(history.undo(), Some(Edit::Mask));
        params.exposure -= 1.0;
        assert_eq!(history.undo(), Some(Edit::Params(params.clone())));
        history.mask_step();
        assert!(!history.can_redo());
    }

    #[test]
    fn mask_eviction_preserves_params_and_clear_resets_pending() {
        let initial = Params::default();
        let mut params = initial.clone();
        let mut history = History::new(&params);
        history.mask_step();
        params.exposure += 1.0;
        history.observe(&params, false);
        history.mask_step();
        history.drop_oldest_mask(1);
        assert_eq!(history.undo(), Some(Edit::Mask));
        assert_eq!(history.undo(), Some(Edit::Params(initial)));
        assert!(!history.can_undo());
        history.redo();
        history.redo();
        history.drop_oldest_mask(10);
        assert_eq!(history.undo(), Some(Edit::Params(Params::default())));
        params.exposure += 1.0;
        history.observe(&params, true);
        history.clear(&params);
        assert!(!history.can_undo());
        assert!(!history.can_redo());
        assert_eq!(history.undo(), None);
        history.observe(&params, false);
        assert!(!history.can_undo());
    }

    #[test]
    fn parameter_limit_does_not_evict_mask_markers() {
        let mut params = Params::default();
        let mut history = History::new(&params);
        history.mask_step();
        for value in 1..250 {
            params.exposure = value as f32;
            history.observe(&params, false);
        }
        assert_eq!(history.past.len(), 201);
        assert_eq!(history.past.front(), Some(&Edit::Mask));
        history.drop_oldest_mask(1);
        assert_eq!(history.past.len(), 200);
    }

    #[test]
    fn observing_parameter_undo_preserves_mask_redo() {
        let initial = Params::default();
        let mut params = initial.clone();
        let mut history = History::new(&params);
        params.exposure += 1.0;
        history.observe(&params, false);
        history.mask_step();
        assert_eq!(history.undo(), Some(Edit::Mask));
        assert_eq!(history.undo(), Some(Edit::Params(initial.clone())));
        history.observe(&initial, false);
        assert!(history.can_redo());
        assert_eq!(history.redo(), Some(Edit::Params(params)));
        assert_eq!(history.redo(), Some(Edit::Mask));
    }
}
