#![allow(dead_code)]

//! Port of upstream/apps/web/src/components/RenderErrorBoundary.tsx.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderErrorBoundary {
    failed: bool,
    reset_generation: u64,
}

impl RenderErrorBoundary {
    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn reset_keys_changed(&mut self, reset_generation: u64) {
        if reset_generation != self.reset_generation {
            self.reset_generation = reset_generation;
            self.failed = false;
        }
    }

    pub fn record_error(&mut self) {
        self.failed = true;
    }
}
