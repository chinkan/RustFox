//! Generic autonomous task supervisor.
//! See `docs/plans/2026-04-30-autopilot-supervisor-design.md`.

pub mod classifier;
pub mod intake;
pub mod job;
pub mod state;
pub mod store;
pub mod task;

#[allow(dead_code)]
pub struct Supervisor;

impl Supervisor {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self
    }
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}
