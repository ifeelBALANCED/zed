//! Test runner core: discovers test runnables in the active buffer, executes
//! them through the existing task infrastructure, and parses runner output
//! into structured per-test results that the UI can observe.

mod adapter;
mod manager;
mod rust_adapter;

use std::sync::Arc;

pub use adapter::{DiscoveredTest, RunKind, TestAdapter, TestStatus};
pub use manager::{TestRunEvent, TestRunnerManager};
pub use rust_adapter::RustAdapter;

/// The test adapters bundled with Zed.
pub fn default_adapters() -> Vec<Arc<dyn TestAdapter>> {
    vec![Arc::new(RustAdapter)]
}
