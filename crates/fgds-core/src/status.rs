use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct PluginStatus {
    state: String,
    fault: Option<String>,
    counters: BTreeMap<String, u64>,
}

#[derive(Default)]
struct Data {
    link: String,
    peer: Option<String>,
    link_error: Option<String>,
    storage: String,
    gap: Option<(u64, u64)>,
    warnings: Vec<String>,
    counters: BTreeMap<String, u64>,
    plugins: BTreeMap<String, PluginStatus>,
}

/// Link, storage and plugin state with counters (LNK-12). Kept in memory, cheap to clone.
#[derive(Clone, Default)]
pub struct Status(Arc<Mutex<Data>>);

impl Status {
    fn with<T>(&self, f: impl FnOnce(&mut Data) -> T) -> T {
        // A poisoned lock only means a panic elsewhere; the data is still usable.
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn set_link(&self, state: &str, peer: Option<String>) {
        self.set_link_state(state, peer, None);
    }

    /// The link state with the reason of the last failure. True when anything changed.
    pub fn set_link_state(&self, state: &str, peer: Option<String>, error: Option<String>) -> bool {
        self.with(|d| {
            let changed = d.link != state || d.peer != peer || d.link_error != error;
            d.link = state.to_string();
            d.peer = peer;
            d.link_error = error;
            changed
        })
    }

    pub fn link(&self) -> String {
        self.with(|d| d.link.clone())
    }

    pub fn set_storage(&self, state: &str) {
        self.with(|d| d.storage = state.to_string());
    }

    /// The sequence numbers lost while storage is failed (STO-9).
    pub fn set_storage_gap(&self, gap: Option<(u64, u64)>) {
        self.with(|d| d.gap = gap);
    }

    /// A condition the operator should know about, shown until the GDS stops.
    pub fn warn(&self, message: &str) {
        self.with(|d| {
            if !d.warnings.iter().any(|w| w == message) {
                d.warnings.push(message.to_string());
            }
        });
    }

    /// Add to a core counter.
    pub fn count(&self, counter: &str, n: u64) {
        self.with(|d| *d.counters.entry(counter.to_string()).or_default() += n);
    }

    pub fn handle(&self, plugin: &str) -> StatusHandle {
        StatusHandle { status: self.clone(), plugin: plugin.to_string() }
    }

    pub fn snapshot(&self) -> Value {
        self.with(|d| {
            let plugins: BTreeMap<_, _> = d
                .plugins
                .iter()
                .map(|(name, p)| {
                    let value = json!({"state": p.state, "fault": p.fault, "counters": p.counters});
                    (name.clone(), value)
                })
                .collect();
            json!({
                "link": {"state": d.link, "peer": d.peer, "error": d.link_error},
                "storage": {
                    "state": d.storage,
                    "gap": d.gap.map(|(first, last)| json!({"first_seq": first.to_string(), "last_seq": last.to_string()})),
                },
                "warnings": d.warnings,
                "counters": d.counters,
                "plugins": plugins,
            })
        })
    }
}

/// One plugin's view of the status.
#[derive(Clone)]
pub struct StatusHandle {
    status: Status,
    plugin: String,
}

impl StatusHandle {
    fn with(&self, f: impl FnOnce(&mut PluginStatus)) {
        self.status.with(|d| f(d.plugins.entry(self.plugin.clone()).or_default()));
    }

    /// The link state (`connected`, `disconnected`, `disabled`), for a plugin that acts on it (FIL-4).
    pub fn link(&self) -> String {
        self.status.link()
    }

    pub fn set_state(&self, state: &str) {
        self.with(|p| p.state = state.to_string());
    }

    pub fn set_fault(&self, message: &str) {
        self.with(|p| {
            p.state = "faulted".to_string();
            p.fault = Some(message.to_string());
        });
    }

    pub fn count(&self, counter: &str, n: u64) {
        self.with(|p| *p.counters.entry(counter.to_string()).or_default() += n);
    }
}
