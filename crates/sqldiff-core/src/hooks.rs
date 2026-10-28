//! What a caller watching long work gets: progress, and a way to stop it.
//! Conversions, instance checks, drift checks and branch reviews all take
//! the same hooks, so the app drives each of them the same way.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Progress and cancel. The default has neither; a conversion with no
/// progress callback prints `checked n/total` to stderr every 50 files, as
/// the command line shows it.
#[derive(Default)]
pub struct Hooks {
    /// Called after each check (a file, a routine's paired templates, an
    /// instance, a changed file) with the number checked so far and the total.
    pub progress: Option<Box<dyn Fn(usize, usize) + Send + Sync>>,
    /// Once set, checks not yet started are skipped and the work fails with
    /// [`CANCELLED`], before anything more is written.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// The error of work stopped through [`Hooks::cancel`].
pub const CANCELLED: &str = "cancelled";

impl Hooks {
    pub fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    pub fn stop_if_cancelled(&self) -> Result<(), String> {
        match self.cancelled() {
            true => Err(CANCELLED.to_string()),
            false => Ok(()),
        }
    }

    /// Reports `checked` of `total` to the progress callback, if any.
    pub fn report(&self, checked: usize, total: usize) {
        if let Some(p) = &self.progress {
            p(checked, total);
        }
    }
}
