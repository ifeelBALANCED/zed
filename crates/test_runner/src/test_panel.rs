use std::sync::Arc;

use anyhow::Result;
use editor::{Editor, SelectionEffects, scroll::Autoscroll};
use fs::Fs;
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    Pixels, Subscription, UniformListScrollHandle, WeakEntity, Window, px, relative, uniform_list,
};
use project::Project;
use settings::{DockSide, RegisterSetting, Settings};
use terminal_view::TerminalView;
use text::ToPoint as _;
use ui::{Divider, ListItem, Tooltip, prelude::*};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::adapter::TestStatus;
use crate::manager::{TestRunEvent, TestRunnerManager};
use crate::{RunFileTests, ToggleFocus};

const TEST_PANEL_KEY: &str = "TestPanel";

fn active_buffer(workspace: &Workspace, cx: &App) -> Option<Entity<language::Buffer>> {
    workspace
        .active_item(cx)
        .and_then(|item| item.act_as::<Editor>(cx))
        .filter(|editor| editor.read(cx).mode().is_full())
        .and_then(|editor| editor.read(cx).buffer().read(cx).as_singleton())
}

#[derive(Debug, Clone, Copy, PartialEq, RegisterSetting)]
pub struct TestPanelSettings {
    pub button: bool,
    pub default_width: Pixels,
    pub dock: DockSide,
}

impl Settings for TestPanelSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let panel = content.test_panel.as_ref().unwrap();
        Self {
            button: panel.button.unwrap(),
            default_width: panel.default_width.map(px).unwrap(),
            dock: panel.dock.unwrap(),
        }
    }
}

/// A dock panel listing the tests discovered in the active file, with
/// pass/fail statuses and the live output of the current run.
pub struct TestPanel {
    manager: Entity<TestRunnerManager>,
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    fs: Arc<dyn Fs>,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
    output_view: Option<Entity<TerminalView>>,
    selected_index: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

impl TestPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            Self::new(workspace, window, cx)
        })
    }

    fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let fs = workspace.app_state().fs.clone();
        let workspace_entity = cx.entity();
        let panel = cx.new(|cx| {
            let manager = cx.new(|_| {
                TestRunnerManager::new(project.clone(), crate::default_adapters())
            });
            let manager_subscription =
                cx.subscribe_in(&manager, window, Self::handle_manager_event);
            let workspace_subscription = cx.subscribe_in(
                &workspace_entity,
                window,
                |this: &mut Self, workspace, event, _, cx| {
                    if let workspace::Event::ActiveItemChanged = event {
                        let buffer = active_buffer(workspace.read(cx), cx);
                        this.manager
                            .update(cx, |manager, cx| manager.set_active_buffer(buffer, cx));
                    }
                },
            );
            Self {
                manager,
                workspace: workspace_entity.downgrade(),
                project,
                fs,
                focus_handle: cx.focus_handle(),
                scroll_handle: UniformListScrollHandle::new(),
                output_view: None,
                selected_index: None,
                _subscriptions: vec![manager_subscription, workspace_subscription],
            }
        });
        let buffer = active_buffer(workspace, cx);
        panel.update(cx, |panel, cx| {
            panel
                .manager
                .update(cx, |manager, cx| manager.set_active_buffer(buffer, cx));
        });
        panel
    }

    pub fn manager(&self) -> &Entity<TestRunnerManager> {
        &self.manager
    }

    fn handle_manager_event(
        &mut self,
        _: &Entity<TestRunnerManager>,
        event: &TestRunEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TestRunEvent::RunStarted => self.attach_output_view(window, cx),
            TestRunEvent::DiscoveryUpdated => self.selected_index = None,
            _ => {}
        }
        cx.notify();
    }

    fn attach_output_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(terminal) = self.manager.read(cx).run_terminal().cloned() else {
            return;
        };
        let workspace = self.workspace.clone();
        let project = self.project.downgrade();
        self.output_view = Some(cx.new(|cx| {
            TerminalView::new(terminal, workspace, None, project, window, cx)
        }));
    }

    fn select_test(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_index = Some(index);
        self.open_test_source(index, window, cx);
        cx.notify();
    }

    fn open_test_source(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let manager = self.manager.read(cx);
        let Some(test) = manager.discovered_tests().get(index) else {
            return;
        };
        let Some(buffer) = manager.buffer() else {
            return;
        };
        let point = test.range.start.to_point(&buffer.read(cx).snapshot());
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            let pane = workspace.active_pane().clone();
            let editor = workspace.open_project_item::<Editor>(
                pane, buffer, true, true, false, true, window, cx,
            );
            editor.update(cx, |editor, cx| {
                editor.change_selections(
                    SelectionEffects::scroll(Autoscroll::center()),
                    window,
                    cx,
                    |selections| selections.select_ranges([point..point]),
                );
            });
        });
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let manager = self.manager.read(cx);
        let is_running = manager.is_running(cx);
        let has_tests = !manager.discovered_tests().is_empty();
        let has_last_run = manager.has_last_run();
        h_flex()
            .px_2()
            .py_1()
            .justify_between()
            .child(Label::new("Tests"))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        IconButton::new("run-file-tests", IconName::PlayOutlined)
                            .disabled(!has_tests || is_running)
                            .tooltip(Tooltip::for_action_title(
                                "Run All Tests in File",
                                &RunFileTests,
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.manager.update(cx, |manager, cx| manager.run_file(cx));
                            })),
                    )
                    .child(
                        IconButton::new("rerun-tests", IconName::RotateCw)
                            .disabled(!has_last_run || is_running)
                            .tooltip(Tooltip::for_action_title("Rerun Last Run", &crate::Rerun))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.manager.update(cx, |manager, cx| manager.rerun(cx));
                            })),
                    )
                    .child(
                        IconButton::new("stop-tests", IconName::Stop)
                            .disabled(!is_running)
                            .tooltip(Tooltip::for_action_title("Stop Tests", &crate::Stop))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.manager.update(cx, |manager, cx| manager.stop(cx));
                            })),
                    ),
            )
    }

    fn render_test_entry(&self, index: usize, cx: &mut Context<Self>) -> Option<ListItem> {
        let manager = self.manager.read(cx);
        let test = manager.discovered_tests().get(index)?;
        let name = test.name.clone();
        let (icon, color) = match manager.status(&name) {
            TestStatus::NotRun => (IconName::Dash, Color::Muted),
            TestStatus::Running => (IconName::PlayFilled, Color::Accent),
            TestStatus::Passed => (IconName::Check, Color::Success),
            TestStatus::Failed => (IconName::XCircle, Color::Error),
            TestStatus::Ignored => (IconName::Dash, Color::Warning),
        };
        Some(
            ListItem::new(index)
                .toggle_state(self.selected_index == Some(index))
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(icon).color(color).size(IconSize::Small))
                        .child(Label::new(name)),
                )
                .end_slot(
                    IconButton::new(("run-test", index), IconName::PlayOutlined)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Run Test"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.manager
                                .update(cx, |manager, cx| manager.run_test_at(index, cx));
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_test(index, window, cx);
                })),
        )
    }
}

impl Render for TestPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let manager = self.manager.read(cx);
        let test_count = manager.discovered_tests().len();
        let error = manager.error().cloned();

        let tests: AnyElement = if test_count == 0 {
            v_flex()
                .flex_grow(1.)
                .items_center()
                .justify_center()
                .child(
                    Label::new("No tests discovered in the active file")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .into_any_element()
        } else {
            uniform_list(
                "tests",
                test_count,
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .filter_map(|index| this.render_test_entry(index, cx))
                        .collect()
                }),
            )
            .flex_grow(1.)
            .track_scroll(&self.scroll_handle)
            .into_any_element()
        };

        let output: AnyElement = match &self.output_view {
            Some(view) => view.clone().into_any_element(),
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(
                    Label::new("No test run yet")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .into_any_element(),
        };

        v_flex()
            .key_context("TestPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .child(self.render_header(cx))
            .when_some(error, |this, error| {
                this.child(
                    h_flex().px_2().py_1().child(
                        Label::new(error)
                            .color(Color::Error)
                            .size(LabelSize::Small),
                    ),
                )
            })
            .child(tests)
            .child(Divider::horizontal())
            .child(
                v_flex()
                    .h(relative(0.4))
                    .min_h(px(120.))
                    .flex_none()
                    .child(output),
            )
    }
}

impl EventEmitter<PanelEvent> for TestPanel {}

impl Focusable for TestPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for TestPanel {
    fn persistent_name() -> &'static str {
        "Test Panel"
    }

    fn panel_key() -> &'static str {
        TEST_PANEL_KEY
    }

    fn position(&self, _: &Window, cx: &App) -> DockPosition {
        match TestPanelSettings::get_global(cx).dock {
            DockSide::Left => DockPosition::Left,
            DockSide::Right => DockPosition::Right,
        }
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        settings::update_settings_file(self.fs.clone(), cx, move |settings, _| {
            let dock = match position {
                DockPosition::Left | DockPosition::Bottom => DockSide::Left,
                DockPosition::Right => DockSide::Right,
            };
            settings.test_panel.get_or_insert_default().dock = Some(dock);
        });
    }

    fn default_size(&self, _: &Window, cx: &App) -> Pixels {
        TestPanelSettings::get_global(cx).default_width
    }

    fn icon(&self, _: &Window, cx: &App) -> Option<IconName> {
        TestPanelSettings::get_global(cx)
            .button
            .then_some(IconName::ListTodo)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Test Panel")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        8
    }
}
