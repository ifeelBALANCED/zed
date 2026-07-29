use std::process::ExitStatus;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use collections::HashMap;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, SharedString, Subscription,
    Task, WeakEntity,
};
use language::{Buffer, BufferEvent, Location};
use project::Project;
use task::{SpawnInTerminal, TaskVariables, VariableName};
use terminal::{TaskStatus, Terminal};

use crate::adapter::{DiscoveredTest, RunKind, TestAdapter, TestStatus as TestResult};

/// Events emitted by [`TestRunnerManager`]. The UI observes these instead of
/// reading terminal output.
#[derive(Debug, Clone)]
pub enum TestRunEvent {
    DiscoveryUpdated,
    RunStarted,
    TestStarted(SharedString),
    TestPassed(SharedString),
    TestFailed(SharedString),
    RunFinished { success: bool },
}

/// Discovers tests in the active buffer, executes them through the existing
/// task infrastructure, and tracks per-test results of the current run.
pub struct TestRunnerManager {
    project: Entity<Project>,
    adapters: Vec<Arc<dyn TestAdapter>>,
    buffer: Option<WeakEntity<Buffer>>,
    discovered: Vec<DiscoveredTest>,
    statuses: HashMap<SharedString, TestResult>,
    current_run: Option<CurrentRun>,
    last_run: Option<PreparedRun>,
    error: Option<SharedString>,
    _buffer_subscription: Option<Subscription>,
    _discovery_task: Option<Task<()>>,
    _run_task: Option<Task<()>>,
}

struct CurrentRun {
    terminal: Entity<Terminal>,
    adapter: Arc<dyn TestAdapter>,
    targets: Vec<DiscoveredTest>,
    _terminal_subscription: Subscription,
}

#[derive(Clone)]
struct PreparedRun {
    spawn: SpawnInTerminal,
    targets: Vec<DiscoveredTest>,
    adapter: Arc<dyn TestAdapter>,
}

impl EventEmitter<TestRunEvent> for TestRunnerManager {}

impl TestRunnerManager {
    pub fn new(project: Entity<Project>, adapters: Vec<Arc<dyn TestAdapter>>) -> Self {
        Self {
            project,
            adapters,
            buffer: None,
            discovered: Vec::new(),
            statuses: HashMap::default(),
            current_run: None,
            last_run: None,
            error: None,
            _buffer_subscription: None,
            _discovery_task: None,
            _run_task: None,
        }
    }

    pub fn discovered_tests(&self) -> &[DiscoveredTest] {
        &self.discovered
    }

    pub fn status(&self, name: &SharedString) -> TestResult {
        self.statuses.get(name).copied().unwrap_or_default()
    }

    pub fn buffer(&self) -> Option<Entity<Buffer>> {
        self.buffer.as_ref()?.upgrade()
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    pub fn run_terminal(&self) -> Option<&Entity<Terminal>> {
        self.current_run.as_ref().map(|run| &run.terminal)
    }

    pub fn has_last_run(&self) -> bool {
        self.last_run.is_some()
    }

    pub fn is_running(&self, cx: &App) -> bool {
        self.current_run.as_ref().is_some_and(|run| {
            run.terminal
                .read(cx)
                .task()
                .is_some_and(|task| task.status == TaskStatus::Running)
        })
    }

    /// Points discovery at the given buffer; tests are (re)discovered on every
    /// reparse until another buffer becomes active.
    pub fn set_active_buffer(&mut self, buffer: Option<Entity<Buffer>>, cx: &mut Context<Self>) {
        let current_id = self.buffer().map(|buffer| buffer.entity_id());
        if buffer.as_ref().map(|buffer| buffer.entity_id()) == current_id {
            return;
        }
        match buffer {
            Some(buffer) => {
                self._buffer_subscription =
                    Some(cx.subscribe(&buffer, |this, buffer, event, cx| {
                        if matches!(event, BufferEvent::Reparsed) {
                            this.discover(&buffer, cx);
                        }
                    }));
                self.buffer = Some(buffer.downgrade());
                self.statuses.clear();
                self.discover(&buffer, cx);
            }
            None => {
                self._buffer_subscription = None;
                self.buffer = None;
                self.discovered.clear();
                self.statuses.clear();
                cx.emit(TestRunEvent::DiscoveryUpdated);
                cx.notify();
            }
        }
    }

    pub fn run_test_at(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(test) = self.discovered.get(index).cloned() else {
            return;
        };
        self.start_run(RunKind::SingleTest, vec![test], cx);
    }

    pub fn run_file(&mut self, cx: &mut Context<Self>) {
        self.start_run(RunKind::File, self.discovered.clone(), cx);
    }

    pub fn rerun(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.last_run.clone() else {
            return;
        };
        self.error = None;
        self._run_task = Some(cx.spawn(async move |this, cx| {
            if let Err(error) = Self::execute(&this, run, cx).await {
                this.update(cx, |this, cx| this.set_error(error, cx)).ok();
            }
        }));
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = &self.current_run
            && run
                .terminal
                .read(cx)
                .task()
                .is_some_and(|task| task.status == TaskStatus::Running)
        {
            run.terminal
                .update(cx, |terminal, _| terminal.kill_active_task());
        }
    }

    fn adapter_for_tag(&self, tag: &str) -> Option<Arc<dyn TestAdapter>> {
        self.adapters
            .iter()
            .find(|adapter| adapter.test_tags().contains(&tag))
            .cloned()
    }

    fn discover(&mut self, buffer: &Entity<Buffer>, cx: &mut Context<Self>) {
        let snapshot = buffer.read(cx).snapshot();
        let adapters = self.adapters.clone();
        self._discovery_task = Some(cx.spawn(async move |this, cx| {
            let tests = cx
                .background_spawn(async move {
                    let mut tests = Vec::new();
                    for runnable in snapshot.runnable_ranges(0..snapshot.len()) {
                        let tag = runnable.runnable.tags.iter().find(|tag| {
                            adapters
                                .iter()
                                .any(|adapter| adapter.test_tags().contains(&tag.0.as_ref()))
                        });
                        let Some(tag) = tag else {
                            continue;
                        };
                        let name: String =
                            snapshot.text_for_range(runnable.run_range.clone()).collect();
                        tests.push(DiscoveredTest {
                            name: name.into(),
                            tag: tag.0.clone(),
                            range: snapshot.anchor_before(runnable.run_range.start)
                                ..snapshot.anchor_after(runnable.run_range.end),
                            extra_captures: runnable.extra_captures,
                        });
                    }
                    tests
                })
                .await;
            this.update(cx, |this, cx| {
                this.discovered = tests;
                cx.emit(TestRunEvent::DiscoveryUpdated);
                cx.notify();
            })
            .ok();
        }));
    }

    fn start_run(&mut self, kind: RunKind, targets: Vec<DiscoveredTest>, cx: &mut Context<Self>) {
        let Some(buffer) = self.buffer() else {
            return;
        };
        let Some(first) = targets.first() else {
            return;
        };
        let Some(adapter) = self.adapter_for_tag(first.tag.as_ref()) else {
            return;
        };
        self.error = None;
        self._run_task = Some(cx.spawn(async move |this, cx| {
            let mut targets = targets;
            let run = async {
                let spawn = Self::resolve(&this, kind, buffer, &mut targets, &adapter, cx).await?;
                let run = PreparedRun {
                    spawn,
                    targets,
                    adapter,
                };
                Self::execute(&this, run, cx).await
            }
            .await;
            if let Err(error) = run {
                this.update(cx, |this, cx| this.set_error(error, cx)).ok();
            }
        }));
    }

    /// Resolves the targets into a spawnable command by reusing the task
    /// pipeline the editor's gutter runnables go through: tag-matched task
    /// templates from the inventory, resolved against the task context built
    /// for the test's location.
    async fn resolve(
        this: &WeakEntity<Self>,
        kind: RunKind,
        buffer: Entity<Buffer>,
        targets: &mut Vec<DiscoveredTest>,
        adapter: &Arc<dyn TestAdapter>,
        cx: &mut AsyncApp,
    ) -> Result<SpawnInTerminal> {
        // The adapter's first tag marks plain test functions, which are the only
        // runnables whose template can be widened into a multi-test run.
        if let Some(primary_tag) = adapter.test_tags().first()
            && let Some(index) = targets
                .iter()
                .position(|target| target.tag.as_ref() == *primary_tag)
        {
            targets.swap(0, index);
        }
        let base = targets.first().context("no tests to run")?.clone();

        let (task_store, worktree_id, language) = this.read_with(cx, |this, cx| {
            let buffer = buffer.read(cx);
            (
                this.project.read(cx).task_store().clone(),
                buffer.file().map(|file| file.worktree_id(cx)),
                buffer.language().cloned(),
            )
        })?;
        let inventory = task_store
            .read_with(cx, |task_store, _| task_store.task_inventory().cloned())
            .context("project has no task inventory")?;
        let templates = inventory
            .update(cx, |inventory, cx| {
                inventory.list_tasks(Some(buffer.clone()), language, worktree_id, cx)
            })
            .await;
        let (source_kind, template) = templates
            .into_iter()
            .find(|(_, template)| {
                template
                    .tags
                    .iter()
                    .any(|tag| tag.as_str() == base.tag.as_ref())
            })
            .with_context(|| format!("no task template found for tag `{}`", base.tag))?;

        let mut captured_variables = TaskVariables::default();
        for (name, value) in &base.extra_captures {
            captured_variables.insert(VariableName::Custom(name.clone().into()), value.clone());
        }
        let location = Location {
            buffer,
            range: base.range.start..base.range.start,
        };
        let task_context = task_store
            .update(cx, |task_store, cx| {
                task_store.task_context_for_location(captured_variables, location, cx)
            })
            .await?
            .context("no task context for the test's location")?;

        let resolved = template
            .resolve_task(&source_kind.to_id_base(), &task_context)
            .context("failed to resolve the test task template")?;
        Ok(adapter.prepare_run(kind, targets, resolved.resolved))
    }

    async fn execute(this: &WeakEntity<Self>, run: PreparedRun, cx: &mut AsyncApp) -> Result<()> {
        let terminal_task = this.update(cx, |this, cx| {
            this.stop(cx);
            for status in this.statuses.values_mut() {
                if *status == TestResult::Running {
                    *status = TestResult::NotRun;
                }
            }
            this.last_run = Some(run.clone());
            this.project.update(cx, |project, cx| {
                project.create_terminal_task(run.spawn.clone(), cx)
            })
        })?;
        let terminal = terminal_task.await?;
        let completion = this.update(cx, |this, cx| {
            let subscription = cx.subscribe(&terminal, |this, _, event, cx| {
                if matches!(event, terminal::Event::Wakeup) {
                    this.parse_current_output(cx);
                }
            });
            for target in &run.targets {
                this.statuses.insert(target.name.clone(), TestResult::Running);
                cx.emit(TestRunEvent::TestStarted(target.name.clone()));
            }
            let completion = terminal.read(cx).wait_for_completed_task(cx);
            this.current_run = Some(CurrentRun {
                terminal,
                adapter: run.adapter,
                targets: run.targets,
                _terminal_subscription: subscription,
            });
            cx.emit(TestRunEvent::RunStarted);
            cx.notify();
            completion
        })?;
        let exit_status = completion.await;
        this.update(cx, |this, cx| this.finish_run(exit_status, cx))?;
        Ok(())
    }

    fn parse_current_output(&mut self, cx: &mut Context<Self>) {
        let Some(run) = &self.current_run else {
            return;
        };
        // Re-parsing the full output on every wakeup is O(output size), but it
        // is idempotent and immune to terminal grid reflows; test output is
        // small enough that incremental parsing isn't worth the fragility.
        let content = run.terminal.read(cx).get_content();
        let mut updates = Vec::new();
        for (reported, status) in run.adapter.parse_output(&content) {
            let matching = run
                .targets
                .iter()
                .find(|target| run.adapter.matches_test(&target.name, &reported));
            if let Some(target) = matching
                && self.statuses.get(&target.name) != Some(&status)
            {
                updates.push((target.name.clone(), status));
            }
        }
        if updates.is_empty() {
            return;
        }
        for (name, status) in updates {
            self.statuses.insert(name.clone(), status);
            match status {
                TestResult::Passed => cx.emit(TestRunEvent::TestPassed(name)),
                TestResult::Failed => cx.emit(TestRunEvent::TestFailed(name)),
                _ => {}
            }
        }
        cx.notify();
    }

    fn finish_run(&mut self, exit_status: Option<ExitStatus>, cx: &mut Context<Self>) {
        self.parse_current_output(cx);
        let success = exit_status.is_some_and(|status| status.success());
        if let Some(run) = &self.current_run {
            // Exit status is the only signal for tests whose result lines could
            // not be parsed (e.g. doc-tests), and it's only unambiguous when a
            // single test was targeted.
            let fallback = if run.targets.len() == 1 {
                if success {
                    TestResult::Passed
                } else {
                    TestResult::Failed
                }
            } else {
                TestResult::NotRun
            };
            let unresolved: Vec<SharedString> = run
                .targets
                .iter()
                .filter(|target| self.statuses.get(&target.name) == Some(&TestResult::Running))
                .map(|target| target.name.clone())
                .collect();
            for name in unresolved {
                self.statuses.insert(name.clone(), fallback);
                match fallback {
                    TestResult::Passed => cx.emit(TestRunEvent::TestPassed(name)),
                    TestResult::Failed => cx.emit(TestRunEvent::TestFailed(name)),
                    _ => {}
                }
            }
        }
        cx.emit(TestRunEvent::RunFinished { success });
        cx.notify();
    }

    fn set_error(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        self.error = Some(SharedString::from(format!("{error:#}")));
        cx.notify();
    }
}
