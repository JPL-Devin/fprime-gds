use crate::{NewRecord, Records, Result, Status, KIND_PLUGIN_FAULT};
use serde_json::json;
use std::any::Any;
use std::future::Future;

pub(crate) fn panic_message(panic: &Box<dyn Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic without a message".into())
}

/// Record a caught panic: a `gds.plugin_fault` record and the plugin's status (MNT-7).
pub async fn report_fault(records: &Records, status: &Status, plugin: &str, message: &str) {
    tracing::error!(plugin, message, "plugin fault");
    status.handle(plugin).set_fault(message);
    let body = json!({"plugin": plugin, "message": message});
    let _ = records.publish(NewRecord::new(KIND_PLUGIN_FAULT, body)).await;
}

/// Run plugin code in its own task. A panic is caught and reported; the task is not restarted.
pub fn spawn_supervised<F>(plugin: &str, records: Records, status: Status, task: F)
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    let plugin = plugin.to_string();
    status.handle(&plugin).set_state("running");
    let inner = tokio::spawn(task);
    tokio::spawn(async move {
        match inner.await {
            Ok(Ok(())) => status.handle(&plugin).set_state("stopped"),
            Ok(Err(e)) => status.handle(&plugin).set_state(&format!("failed: {e}")),
            Err(e) if e.is_panic() => {
                let message = panic_message(&e.into_panic());
                report_fault(&records, &status, &plugin, &message).await;
            }
            Err(_) => status.handle(&plugin).set_state("cancelled"),
        }
    });
}
