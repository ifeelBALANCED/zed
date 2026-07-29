use task::SpawnInTerminal;

use crate::adapter::{DiscoveredTest, RunKind, TestAdapter, TestStatus};

/// Runs Rust tests through the `cargo test` task templates provided by
/// `RustContextProvider` and parses libtest's human-readable output.
pub struct RustAdapter;

const NO_CAPTURE_FLAG: &str = "--nocapture";

impl TestAdapter for RustAdapter {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn test_tags(&self) -> &'static [&'static str] {
        &["rust-test", "rust-doc-test"]
    }

    fn prepare_run(
        &self,
        kind: RunKind,
        targets: &[DiscoveredTest],
        mut base: SpawnInTerminal,
    ) -> SpawnInTerminal {
        // With capture enabled, libtest prints `test <name> ... <result>` lines
        // atomically, which keeps them parseable; output of failing tests still
        // appears in the failures section.
        base.args.retain(|arg| arg != NO_CAPTURE_FLAG);
        if kind == RunKind::File && targets.len() > 1 {
            // The resolved template carries the base test's name as the libtest
            // filter; widen it to every test in the file (libtest accepts
            // multiple filter arguments).
            let base_name = targets[0].name.as_ref();
            let names = targets.iter().map(|target| target.name.to_string());
            match base.args.iter().rposition(|arg| arg == base_name) {
                Some(position) => {
                    base.args.splice(position..position + 1, names);
                }
                None => base.args.extend(names),
            }
            base.label = format!("cargo test ({} tests)", targets.len());
            base.full_label = base.label.clone();
        }
        base
    }

    fn parse_output(&self, output: &str) -> Vec<(String, TestStatus)> {
        output.lines().filter_map(parse_result_line).collect()
    }

    fn matches_test(&self, discovered: &str, reported: &str) -> bool {
        reported == discovered
            || reported
                .rsplit("::")
                .next()
                .is_some_and(|last| last == discovered)
    }
}

/// Parses a libtest result line: `test module::name ... ok|FAILED|ignored`.
fn parse_result_line(line: &str) -> Option<(String, TestStatus)> {
    let rest = line.trim().strip_prefix("test ")?;
    let (name, outcome) = rest.split_once(" ... ")?;
    let status = match outcome.split_whitespace().next()?.trim_end_matches(',') {
        "ok" => TestStatus::Passed,
        "FAILED" => TestStatus::Failed,
        "ignored" => TestStatus::Ignored,
        _ => return None,
    };
    Some((name.to_string(), status))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovered(name: &str) -> DiscoveredTest {
        DiscoveredTest {
            name: name.to_string().into(),
            tag: "rust-test".into(),
            range: text::Anchor::min_min_range_for_buffer(text::BufferId::new(1).unwrap()),
            extra_captures: Default::default(),
        }
    }

    #[test]
    fn parses_libtest_output() {
        let output = "\
   Compiling foo v0.1.0 (/tmp/foo)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.52s
     Running unittests src/lib.rs (target/debug/deps/foo-abc123)

running 4 tests
test tests::parser ... ok
test tests::lexer ... FAILED
test tests::slow ... ignored, requires network
test benches::tokenize ... bench:       1,000 ns/iter (+/- 10)

failures:

---- tests::lexer stdout ----
thread 'tests::lexer' panicked at src/lib.rs:5:9

failures:
    tests::lexer

test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out
";
        assert_eq!(
            RustAdapter.parse_output(output),
            vec![
                ("tests::parser".to_string(), TestStatus::Passed),
                ("tests::lexer".to_string(), TestStatus::Failed),
                ("tests::slow".to_string(), TestStatus::Ignored),
            ]
        );
    }

    #[test]
    fn widens_file_runs_to_all_test_filters() {
        let base = SpawnInTerminal {
            args: vec![
                "test".into(),
                "-p".into(),
                "foo".into(),
                "--".into(),
                "--nocapture".into(),
                "--include-ignored".into(),
                "alpha".into(),
            ],
            ..Default::default()
        };
        let targets = [discovered("alpha"), discovered("beta")];
        let prepared = RustAdapter.prepare_run(RunKind::File, &targets, base);
        assert_eq!(
            prepared.args,
            vec!["test", "-p", "foo", "--", "--include-ignored", "alpha", "beta"]
        );
    }

    #[test]
    fn single_runs_only_drop_nocapture() {
        let base = SpawnInTerminal {
            args: vec!["test".into(), "--".into(), "--nocapture".into(), "alpha".into()],
            ..Default::default()
        };
        let targets = [discovered("alpha")];
        let prepared = RustAdapter.prepare_run(RunKind::SingleTest, &targets, base);
        assert_eq!(prepared.args, vec!["test", "--", "alpha"]);
    }

    #[test]
    fn matches_reported_module_paths() {
        assert!(RustAdapter.matches_test("alpha", "alpha"));
        assert!(RustAdapter.matches_test("alpha", "tests::alpha"));
        assert!(RustAdapter.matches_test("alpha", "a::b::alpha"));
        assert!(!RustAdapter.matches_test("alpha", "tests::alphabet"));
        assert!(!RustAdapter.matches_test("alpha", "alpha::beta"));
    }
}
