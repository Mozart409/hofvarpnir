//! A mailbox-free liveness handle shared between the watchdog and `/live`.
//!
//! `GetActorHealth` is a kameo `ask()` — it can only ever answer once the
//! root supervisor's mailbox loop gets around to it, which is a guarantee
//! about ordering, not about time: a wedged actor system, or the root
//! supervisor itself mid-restart, can leave that `ask()` pending
//! indefinitely. A `/live` (Kubernetes/Docker liveness) probe must never be
//! able to hang behind that — see the doc comment on
//! `hof_api::routes::health::check_actors` for the full argument, which
//! applies here identically. [`LivenessFlag`] sidesteps the mailbox
//! entirely: the watchdog task (`crate::watchdog`) writes it after every
//! poll of the root supervisor, and the `/live` handler only ever reads it —
//! a lock-free atomic load, never an `.await`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the process should currently report itself alive to `/live`.
///
/// Cheap to clone (an `Arc`-backed handle, like [`crate::runtime_config::DrainToken`]) —
/// one instance is created in `startup::initialize` and shared between the
/// watchdog task and `AppState`. `Ordering::Relaxed` is sufficient on both
/// the read and write side: this flag gates an HTTP status code, not memory
/// safety, and there is no second variable whose visibility needs to be
/// ordered against it.
#[derive(Debug, Clone)]
pub struct LivenessFlag(Arc<AtomicBool>);

impl LivenessFlag {
    /// Start alive: at process start nothing has been observed unrecoverable
    /// yet, since the watchdog has not polled anything.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    /// Read the current value.
    ///
    /// Never blocks and never touches an actor mailbox — safe to call from a
    /// probe handler on every request, however often the orchestrator polls.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Set the current value.
    ///
    /// Called by the watchdog task after each poll of the root supervisor's
    /// health — not exclusively on the way down: a manual restart
    /// (`RestartActor`) can clear a previously-`unrecoverable` actor, and the
    /// next poll should flip this back to alive rather than leaving `/live`
    /// permanently tripped.
    pub fn set_alive(&self, alive: bool) {
        self.0.store(alive, Ordering::Relaxed);
    }
}

impl Default for LivenessFlag {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::LivenessFlag;

    #[test]
    fn starts_alive() {
        assert!(LivenessFlag::new().is_alive());
    }

    #[test]
    fn clone_shares_state() {
        let flag = LivenessFlag::new();
        let clone = flag.clone();

        clone.set_alive(false);

        assert!(
            !flag.is_alive(),
            "clones must share the same underlying flag"
        );
    }
}
