use std::ops::Range;

use collections::HashMap;
use gpui::SharedString;
use task::SpawnInTerminal;
use text::Anchor;

/// What a test run targets, which controls how the adapter shapes the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    SingleTest,
    File,
}

/// Outcome of a single test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TestStatus {
    #[default]
    NotRun,
    Running,
    Passed,
    Failed,
    Ignored,
}

/// A test discovered in a buffer through the language's `runnables.scm` tags.
#[derive(Debug, Clone)]
pub struct DiscoveredTest {
    /// Test name as written in the source, e.g. the test function name.
    pub name: SharedString,
    /// The runnable tag this test was discovered through, e.g. `rust-test`.
    pub tag: SharedString,
    /// Position of the runnable in the buffer, for jumping to the source.
    pub range: Range<Anchor>,
    /// Extra tree-sitter captures, fed into task context resolution.
    pub extra_captures: HashMap<String, String>,
}

/// Language-specific test running logic: which runnables are tests, how to
/// shape a task command for a run, and how to interpret the runner's output.
///
/// Everything else (discovery mechanics, task resolution, process management,
/// state, UI) is language-agnostic and lives outside the adapter.
pub trait TestAdapter: Send + Sync {
    fn name(&self) -> &'static str;

    /// Runnable tags (from the language's `runnables.scm`) that mark individual
    /// tests. The first tag is the preferred one to resolve multi-test runs
    /// through, so it must identify a plain test function.
    fn test_tags(&self) -> &'static [&'static str];

    /// Shape the resolved task command for the given run.
    /// `base` was resolved from `targets[0]`'s task template.
    fn prepare_run(
        &self,
        kind: RunKind,
        targets: &[DiscoveredTest],
        base: SpawnInTerminal,
    ) -> SpawnInTerminal;

    /// Parse accumulated runner output into per-test results. Called with the
    /// full output every time more output arrives, so it must be idempotent.
    fn parse_output(&self, output: &str) -> Vec<(String, TestStatus)>;

    /// Whether a runner-reported test name refers to the given discovered test.
    fn matches_test(&self, discovered: &str, reported: &str) -> bool;
}
