//! Decision tracing for the replay harness.
//!
//! The live features call [`emit`] at the points a replay wants to see (a
//! detector decision, a Copilot request's first word, a llama-helper queue
//! hand-off). In the app no sink is installed and `emit` returns after one
//! atomic load, without building the payload.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use serde_json::Value;

pub type Sink = Arc<dyn Fn(&'static str, Value) + Send + Sync>;

static ON: AtomicBool = AtomicBool::new(false);
static SINK: RwLock<Option<Sink>> = RwLock::new(None);

/// Whether a sink is listening.
pub fn enabled() -> bool {
    ON.load(Ordering::Acquire)
}

/// Records `name` with the payload `build` makes, when a sink is installed.
pub fn emit(name: &'static str, build: impl FnOnce() -> Value) {
    if !enabled() {
        return;
    }
    let sink = SINK.read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(sink) = sink {
        sink(name, build());
    }
}

pub fn install(sink: Sink) {
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    ON.store(true, Ordering::Release);
}

pub fn uninstall() {
    ON.store(false, Ordering::Release);
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_built_without_a_sink() {
        // No test installs a sink, so this holds even with tests in parallel.
        let mut built = false;
        emit("x", || {
            built = true;
            Value::Null
        });
        assert!(!built);
    }
}
