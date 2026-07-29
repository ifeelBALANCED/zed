//! Test runner: discovers test runnables in the active buffer, executes them
//! through the existing task infrastructure, parses runner output into
//! structured per-test results, and shows them in a dock panel.

mod adapter;
mod manager;
mod rust_adapter;
mod test_panel;

use std::sync::Arc;

use gpui::{App, actions};
use workspace::Workspace;

pub use adapter::{DiscoveredTest, RunKind, TestAdapter, TestStatus};
pub use manager::{TestRunEvent, TestRunnerManager};
pub use rust_adapter::RustAdapter;
pub use test_panel::{TestPanel, TestPanelSettings};

actions!(
    test_runner,
    [
        /// Toggles focus on the test panel.
        ToggleFocus,
        /// Runs all tests in the active file.
        RunFileTests,
        /// Reruns the last test run.
        Rerun,
        /// Stops the running tests.
        Stop
    ]
);

/// The test adapters bundled with Zed.
pub fn default_adapters() -> Vec<Arc<dyn TestAdapter>> {
    vec![Arc::new(RustAdapter)]
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<TestPanel>(window, cx);
        });
        workspace.register_action(|workspace, _: &RunFileTests, window, cx| {
            run_in_panel(workspace, window, cx, |manager, cx| manager.run_file(cx));
        });
        workspace.register_action(|workspace, _: &Rerun, window, cx| {
            run_in_panel(workspace, window, cx, |manager, cx| manager.rerun(cx));
        });
        workspace.register_action(|workspace, _: &Stop, _, cx| {
            if let Some(panel) = workspace.panel::<TestPanel>(cx) {
                let manager = panel.read(cx).manager().clone();
                manager.update(cx, |manager, cx| manager.stop(cx));
            }
        });
    })
    .detach();
}

fn run_in_panel(
    workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<Workspace>,
    run: impl FnOnce(&mut TestRunnerManager, &mut gpui::Context<TestRunnerManager>),
) {
    let Some(panel) = workspace.panel::<TestPanel>(cx) else {
        return;
    };
    workspace.open_panel::<TestPanel>(window, cx);
    let manager = panel.read(cx).manager().clone();
    manager.update(cx, run);
}
