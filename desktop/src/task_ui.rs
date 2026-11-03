//! The task page: one card for every task, and one table of what can be done to it.
use crate::{
    Desktop, Page, activity,
    import_ui::{self, video_cover, video_title},
    symbols, ui,
    workspace::{Intent, TaskRecord, TaskState},
};
use desktop_store::records::Records;
use desktop_tasks::job::Work;
use desktop_tasks::task_view::*;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::components::MaterialContextMenu;
use gpui_kit_material::{
    color::ColorRole,
    icons::{Icon, SymbolIcon},
    motion,
    prelude::*,
    tokens::shape::OuterCorners,
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
};

/// The record's revision and running-task progress the sorted tasks were copied at.
type TaskListKey = (u64, u64);

/// The task page's own view state.
pub(crate) struct TaskListUi {
    /// The virtualized queue: the list, and the identity and focus of what it describes.
    pub list: ListState,
    /// The sorted tasks, as of the record's (revision, progress).
    pub cache: Option<(TaskListKey, Rc<Vec<TaskRecord>>)>,
    pub keys: Vec<String>,
    pub focus: Vec<FocusHandle>,
    pub rem: f32,
    /// Disclosure open state that outlives a virtualized row's unmount.
    pub logs_open: std::collections::HashSet<String>,
    /// The task row showing its details on the task page.
    pub expanded: Option<String>,
}

impl TaskListUi {
    pub fn new() -> Self {
        Self {
            list: ListState::new(0, ListAlignment::Top, px(1000.)),
            cache: None,
            keys: Vec::new(),
            focus: Vec::new(),
            rem: f32::NAN,
            logs_open: Default::default(),
            expanded: None,
        }
    }
}

/// One row of the virtualized task queue. Task rows carry their index into
/// the task snapshot captured for the frame.
enum QueueItem {
    /// The page heading with its counts; it scrolls with the sections and
    /// shares their column.
    Header,
    Unavailable,
    Empty,
    GroupLabel {
        group: TaskGroup,
        count: usize,
        /// Members waiting to run, as opposed to paused.
        queued: usize,
    },
    Task {
        task: usize,
        place: CardPlace,
        /// The next item shares this one's surface.
        joined: bool,
    },
    /// Folds a long queue after its first rows.
    QueueFold {
        hidden: usize,
        open: Entity<bool>,
    },
    /// Folds finished work and earlier records behind one tile.
    CompletedToggle {
        count: usize,
        open: bool,
        state: Entity<bool>,
    },
}

impl QueueItem {
    /// Identity for the list splice: stable across content updates that keep
    /// the row in place, so measured heights and scroll anchors survive.
    fn key(&self, tasks: &[TaskRecord]) -> String {
        match self {
            QueueItem::Header => "header".to_owned(),
            QueueItem::Unavailable => "unavailable".to_owned(),
            QueueItem::Empty => "empty".to_owned(),
            QueueItem::GroupLabel { group, .. } => format!("group-{group:?}"),
            QueueItem::Task { task, .. } => format!("task-{}", tasks[*task].id),
            QueueItem::QueueFold { .. } => "queue-fold".to_owned(),
            QueueItem::CompletedToggle { .. } => "completed-toggle".to_owned(),
        }
    }
}

/// Where a task's card stands, which decides what it shows without being asked.
#[derive(Clone, Copy)]
pub(crate) enum CardPlace {
    /// A row on its group's surface that folds its details out; `number` is its
    /// place among the waiting tasks.
    Row {
        outer: OuterCorners,
        number: Option<usize>,
    },
    /// A task the person should watch or decide on: running or needing attention.
    Open { outer: OuterCorners },
    /// In the video card's place on the workbench, joined to its options.
    Home,
}

/// Queued rows shown before the rest of the queue folds.
const QUEUE_PREVIEW: usize = 5;
/// The task page's content column, inside the shell's gutters.
const TASK_COLUMN: Rems = rem(832.);
/// Between the page heading and each section.
const TASK_SECTION_GAP: Rems = rem(28.);

/// A queued task being dragged to a new place in the queue.
#[derive(Clone)]
struct QueueDrag {
    id: String,
    title: SharedString,
}

impl Render for QueueDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        AssistChip::new("queue-drag", self.title.clone())
            .elevated(true)
            .icon(symbols::drag_indicator())
    }
}
/// How a step's state draws.
trait StageStatusView {
    fn role(self) -> ColorRole;
    fn chip(self) -> StatusKind;
    fn icon(self) -> SymbolIcon;
}

/// The task page: the queue of every task and its cards, which the workbench and the
/// library also show. It reads the records and the running work, keeps what is open,
/// and asks the window for everything that changes a task.
pub(crate) struct TasksPage {
    records: Entity<Records>,
    work: Entity<Work>,
    this: WeakEntity<Self>,
    pub(crate) task_list: TaskListUi,
    /// What the window knows beyond the records and the work.
    facts: ShellFacts,
    /// Details already shown once; a virtualized row remounts without replaying them.
    entered: HashSet<String>,
}

/// What the cards show that neither the records nor the work hold. The window sets it
/// before it draws.
#[derive(Default, PartialEq)]
pub(crate) struct ShellFacts {
    /// The names of the AI services the tasks send to, by saved version.
    pub services: HashMap<String, String>,
    /// Libraries whose location must be associated again.
    pub reassociate: HashSet<String>,
    /// A storage operation is under way.
    pub storage_busy: bool,
}

/// What the task page asks of the window.
pub(crate) enum TasksEvent {
    /// Run one of a task's actions.
    Run(String, TaskAction),
    /// Select a task; `expand` then opens its details.
    Select {
        id: String,
        expand: bool,
    },
    /// Send a task's uncertain requests again, as its card's button says.
    Resend(String),
    /// Move a waiting task to this place in the queue.
    Move {
        id: String,
        position: usize,
    },
    PauseAll,
    ResumeAll,
    Navigate(Page),
    /// Import a video, from the empty page.
    Import,
}

impl EventEmitter<TasksEvent> for TasksPage {}

/// Runs `action` on task `id` through the page's window.
pub(crate) fn action_click(
    page: WeakEntity<TasksPage>,
    id: String,
    action: TaskAction,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, _, cx| {
        let _ = page.update(cx, |_, cx| {
            cx.emit(TasksEvent::Run(id.clone(), action.clone()))
        });
    }
}

/// Where a task's plan goes and what it does: a card's second line.
fn task_support_line(id: &str, support: String) -> Stateful<Div> {
    accessible_text(SharedString::from(format!("task-support-{id}")), support)
        .text_size(TEXT_AUX)
        .text_color(color(ColorRole::ON_SURFACE_VARIANT))
        .whitespace_normal()
}

fn stage_row(stage: TaskStage, window: &mut Window, cx: &mut App) -> Div {
    let icon_id = SharedString::from(format!("{}-icon", stage.id));
    let status_id = SharedString::from(format!("{}-status", stage.id));
    let progress_id = SharedString::from(format!("{}-progress", stage.id));
    // A running or finished step reads its measurement; any other state names
    // itself, with what it measured so far.
    let (status, ink) = match stage.status {
        StageStatus::Running => (stage.detail, ColorRole::ON_SURFACE_VARIANT),
        StageStatus::Complete if !stage.detail.is_empty() => {
            (stage.detail, ColorRole::ON_SURFACE_VARIANT)
        }
        status if stage.detail.is_empty() => (status.label().to_owned(), status.role()),
        status => (
            format!("{} · {}", status.label(), stage.detail),
            status.role(),
        ),
    };
    let icon: AnyElement = if stage.status.running() && stage.fraction.is_none() {
        icon_spinner(icon_id, stage.title.clone(), ICON_SLOT, cx)
    } else {
        Icon::from(stage.status.icon())
            .size(ICON_SLOT)
            .color(color(stage.status.role()))
            .into_any_element()
    };
    v_flex()
        .w_full()
        .min_w_0()
        .gap(rem(8.))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap(rem(16.))
                .child(
                    semantic_label(SharedString::from(stage.id.clone()), stage.title, icon)
                        // The name keeps one line; the measurement wraps instead.
                        .whitespace_nowrap()
                        .flex_shrink_0(),
                )
                .child(
                    ui::status_line(status_id, &status)
                        .flex_1()
                        .justify_end()
                        .text_size(TEXT_BODY)
                        .text_color(color(ink)),
                ),
        )
        .when_some(stage.fraction, |row, fraction| {
            row.child(
                div()
                    .pl(LABEL_INSET)
                    .child(progress(progress_id, fraction, window, cx)),
            )
        })
}

/// The cover's width on a task card away from home.
const COVER_WIDTH: Rems = rem(160.);

impl TasksPage {
    pub(crate) fn new(
        records: Entity<Records>,
        work: Entity<Work>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&records, |_, _, cx| cx.notify()).detach();
        cx.observe(&work, |_, _, cx| cx.notify()).detach();
        Self {
            records,
            work,
            this: cx.weak_entity(),
            task_list: TaskListUi::new(),
            facts: ShellFacts::default(),
            entered: HashSet::new(),
        }
    }

    /// Takes what the window knows; a change draws the page again.
    pub(crate) fn set_facts(&mut self, facts: ShellFacts, cx: &mut Context<Self>) {
        if self.facts != facts {
            self.facts = facts;
            cx.notify();
        }
    }

    fn workspace<'a>(&self, cx: &'a App) -> Option<&'a crate::workspace::Workspace> {
        self.records.read(cx).workspace.as_ref()
    }

    fn library<'a>(&self, cx: &'a App) -> &'a desktop_store::library::LibraryState {
        &self.records.read(cx).library
    }

    fn task<'a>(&self, id: &str, cx: &'a App) -> Option<&'a TaskRecord> {
        self.records.read(cx).task(id)
    }

    fn task_library<'a>(
        &self,
        task: &TaskRecord,
        cx: &'a App,
    ) -> Option<&'a crate::workspace::LibraryLocation> {
        self.records.read(cx).task_library(task)
    }

    fn is_running_task(&self, id: &str, cx: &App) -> bool {
        self.work.read(cx).is_running(id)
    }

    fn enter_once(&mut self, id: String) -> bool {
        self.entered.insert(id)
    }

    /// What the task's actions depend on beyond its own record.
    fn task_facts<'a>(&'a self, task: &TaskRecord, cx: &'a App) -> TaskFacts<'a> {
        TaskFacts {
            active: self.is_running_task(&task.id, cx),
            expanded: self.task_list.expanded.as_ref() == Some(&task.id),
            library: self.task_library(task, cx),
            needs_reassociation: self
                .task_library(task, cx)
                .is_some_and(|library| self.facts.reassociate.contains(&library.id)),
            queue: self
                .workspace(cx)
                .map(|workspace| {
                    workspace
                        .state
                        .waiting_tasks()
                        .map(|task| task.id.as_str())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub(crate) fn task_actions(
        &self,
        task: &TaskRecord,
        surface: TaskSurface,
        cx: &App,
    ) -> Vec<TaskAction> {
        task_actions(task, surface, &self.task_facts(task, cx))
    }

    fn task_stage_rows(&self, task: &TaskRecord, cx: &App) -> Vec<TaskStage> {
        task_stage_rows(task, self.is_running_task(&task.id, cx))
    }

    fn queue_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rem_size = f32::from(window.rem_size());
        let mut items = Vec::new();
        let tasks: Rc<Vec<TaskRecord>> = match &self.records.read(cx).workspace {
            None => {
                items.push(QueueItem::Unavailable);
                self.task_list.cache = None;
                Rc::new(Vec::new())
            }
            Some(workspace) if workspace.state.tasks.is_empty() => {
                items.push(QueueItem::Empty);
                self.task_list.cache = None;
                Rc::new(Vec::new())
            }
            Some(workspace) => {
                // A running task's progress changes the record without its revision.
                let key = (workspace.state.revision, workspace.state.progress);
                if let Some((cached, tasks)) = &self.task_list.cache
                    && *cached == key
                {
                    Rc::clone(tasks)
                } else {
                    // Running and queued work keeps its run order; the rest
                    // shows the newest first.
                    let mut tasks: Vec<_> =
                        workspace.state.tasks.iter().cloned().enumerate().collect();
                    tasks.sort_by_key(|(index, task)| {
                        let group = task_group(task);
                        let order = match group {
                            TaskGroup::Running | TaskGroup::Queued => *index as i64,
                            _ => -(task.created as i64),
                        };
                        (group, order)
                    });
                    let tasks: Vec<_> = tasks.into_iter().map(|(_, task)| task).collect();
                    let tasks = Rc::new(tasks);
                    self.task_list.cache = Some((key, Rc::clone(&tasks)));
                    tasks
                }
            }
        };
        if self.records.read(cx).workspace.is_some() {
            items.insert(0, QueueItem::Header);
        }
        let completed_state = window.use_keyed_state("task-history-open", cx, |_, _| false);
        let queue_open = window.use_keyed_state("task-queue-open", cx, |_, _| false);
        let done = |task: &TaskRecord| task_group(task).settled();
        // Finished work and earlier records fold behind one tile; a task the
        // person went to (from the library or a notice) stays visible.
        let expanded_done = self
            .task_list
            .expanded
            .as_ref()
            .is_some_and(|id| tasks.iter().any(|task| &task.id == id && done(task)));
        let completed_open = *completed_state.read(cx) || expanded_done;
        let count = tasks.iter().filter(|task| done(task)).count();
        for group in TaskGroup::ORDER {
            let members: Vec<usize> = tasks
                .iter()
                .enumerate()
                .filter(|(_, task)| task_group(task) == group)
                .map(|(index, _)| index)
                .collect();
            if group == TaskGroup::Finished && count > 0 {
                items.push(QueueItem::CompletedToggle {
                    count,
                    open: completed_open,
                    state: completed_state.clone(),
                });
            }
            if members.is_empty() || (group.settled() && !completed_open) {
                continue;
            }
            // The fold tile just above already names finished work.
            if group != TaskGroup::Finished {
                items.push(QueueItem::GroupLabel {
                    group,
                    count: members.len(),
                    queued: members
                        .iter()
                        .filter(|&&index| tasks[index].state == TaskState::Queued)
                        .count(),
                });
            }
            // Folding one row would save nothing.
            let folds = group == TaskGroup::Queued && members.len() > QUEUE_PREVIEW + 1;
            let shown = if folds && !*queue_open.read(cx) {
                QUEUE_PREVIEW
            } else {
                members.len()
            };
            // The tile order is the run order; paused tasks keep their place.
            let tiles = shown + usize::from(folds);
            for (position, &task) in members.iter().take(shown).enumerate() {
                let outer = list_corners(position, tiles);
                let place = match group {
                    TaskGroup::Running => CardPlace::Open { outer: [true; 4] },
                    TaskGroup::Attention => CardPlace::Open { outer },
                    _ => CardPlace::Row {
                        outer,
                        number: (group == TaskGroup::Queued).then_some(position + 1),
                    },
                };
                items.push(QueueItem::Task {
                    task,
                    place,
                    joined: group != TaskGroup::Running && position + 1 < tiles,
                });
            }
            if folds {
                items.push(QueueItem::QueueFold {
                    hidden: members.len() - shown,
                    open: queue_open.clone(),
                });
            }
        }
        let keys = items.iter().map(|item| item.key(&tasks)).collect();
        Desktop::reconcile_list_items(
            &self.task_list.list,
            &mut self.task_list.keys,
            &mut self.task_list.focus,
            keys,
            cx,
        );
        if self.task_list.rem != rem_size {
            self.task_list.list.remeasure();
            self.task_list.rem = rem_size;
        }
        let focus = Rc::new(self.task_list.focus.clone());
        let state = self.task_list.list.clone();
        let items = Rc::new(items);
        let page = cx.weak_entity();
        let queue = list(state, move |index, window, cx| {
            let Some(item) = items.get(index) else {
                return div().into_any_element();
            };
            let last = index + 1 == items.len();
            page.update(cx, |this, cx| {
                let gap = match item {
                    QueueItem::GroupLabel { .. } => rems(SECTION_TITLE_GAP.as_f32() / 14.),
                    QueueItem::Task { joined: true, .. } => rems(TILE_GAP.as_f32() / 14.),
                    _ => TASK_SECTION_GAP,
                };
                let mut wrapper = div()
                    .w_full()
                    .min_w_0()
                    .max_w(TASK_COLUMN)
                    .mx_auto()
                    .flex()
                    .flex_col()
                    .when(!last, |view| view.pb(gap))
                    .child(this.queue_item(item, &tasks, window, cx))
                    .id(("queue-item", index))
                    .tab_stop(false);
                if let Some(focus) = focus.get(index) {
                    wrapper = wrapper.track_focus(focus);
                }
                wrapper.into_any_element()
            })
            .unwrap_or_else(|_| div().into_any_element())
        })
        .w_full()
        .h_full()
        .flex_1()
        .min_h_0()
        .pb(rem(72.));
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .child(queue)
            .into_any_element()
    }

    /// The page title with a chip for each kind of work there is, and the
    /// action that stops or resumes all of it.
    fn queue_page_header(&self, tasks: &[TaskRecord], cx: &mut Context<Self>) -> Div {
        let TaskCounts {
            running,
            attention,
            queued,
            resumable,
            pausable,
        } = task_counts(tasks);
        let chips = [
            (
                running,
                StatusKind::Progress,
                course2md::tr!("{} 个正在处理", running),
            ),
            (
                attention,
                StatusKind::Danger,
                course2md::tr!("{} 个需要处理", attention),
            ),
            (
                queued,
                StatusKind::Neutral,
                course2md::tr!("{} 个排队中", queued),
            ),
            (
                resumable,
                StatusKind::Neutral,
                course2md::tr!("{} 个已暂停", resumable),
            ),
        ]
        .into_iter()
        .filter(|(count, ..)| *count > 0)
        .map(|(_, kind, text)| status_chip(kind).child(text));
        h_flex()
            .w_full()
            .min_w_0()
            .pt(rem(16.))
            .gap(rem(16.))
            .items_center()
            .flex_wrap()
            .child(
                v_flex()
                    .flex_1()
                    .flex_basis(rem(320.))
                    .min_w_0()
                    .gap(rem(8.))
                    .child(ui::page_title("tasks-page-title", course2md::tr!("任务")))
                    .child(h_flex().gap(rem(8.)).flex_wrap().children(chips)),
            )
            .child(
                h_flex()
                    .gap(rem(8.))
                    .when(pausable > 0, |row| {
                        row.child(
                            material_button("tasks-pause-all", ButtonVariant::Outlined)
                                .tone(ButtonTone::Accent)
                                .icon(symbols::pause())
                                .child(course2md::tr!("全部暂停"))
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(TasksEvent::PauseAll))),
                        )
                    })
                    .when(resumable > 0, |row| {
                        row.child(
                            material_button("tasks-resume-all", ButtonVariant::Outlined)
                                .tone(ButtonTone::Accent)
                                .icon(symbols::play_arrow())
                                .child(course2md::tr!("全部继续"))
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(TasksEvent::ResumeAll)),
                                ),
                        )
                    }),
            )
    }

    fn queue_item(
        &mut self,
        item: &QueueItem,
        tasks: &[TaskRecord],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match item {
            // The page heading summarizes the counts; group headings stay names.
            QueueItem::Header => self.queue_page_header(tasks, cx).into_any_element(),
            QueueItem::Unavailable => notice_banner(
                "task-records-unavailable",
                NoticeKind::Danger,
                course2md::tr!("任务记录暂时无法读取"),
            )
            .child(
                material_button("task-records-open-library", ButtonVariant::Outlined)
                    .tone(ButtonTone::Accent)
                    .icon(symbols::menu_book())
                    .child(course2md::tr!("前往笔记库"))
                    .on_click(
                        cx.listener(|_, _, _, cx| cx.emit(TasksEvent::Navigate(Page::Library))),
                    ),
            )
            .into_any_element(),
            QueueItem::Empty => tile([true; 4])
                .flex()
                .flex_col()
                .gap(rem(16.))
                .px(rem(20.))
                .py(rem(40.))
                .items_center()
                .child(tinted_icon(symbols::checklist(), Tint::Teal, rem(64.)))
                .child(
                    accessible_text("task-empty-title", course2md::tr!("还没有生成任务"))
                        .role(Role::Heading)
                        .text_size(TEXT_SECTION),
                )
                .child(
                    material_button("task-new", ButtonVariant::Filled)
                        .icon(symbols::add())
                        .child(course2md::tr!("导入视频"))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(TasksEvent::Import))),
                )
                .into_any_element(),
            QueueItem::GroupLabel {
                group,
                count,
                queued,
            } => {
                // Paused work keeps its place in the queue; when nothing in it
                // is waiting to run, the section says so.
                let label = if *group == TaskGroup::Queued && *queued == 0 {
                    course2md::tr!("已暂停")
                } else {
                    group.label()
                };
                h_flex()
                    .w_full()
                    .min_w_0()
                    .px(ITEM_INSET)
                    .gap_x(rem(12.))
                    .items_baseline()
                    .flex_wrap()
                    .child(section_title(
                        SharedString::from(format!("task-group-{group:?}")),
                        label,
                    ))
                    .when(
                        *group == TaskGroup::Queued && *count > 1 && *queued > 0,
                        |row| {
                            row.child(
                                accessible_text(
                                    "task-queue-order-hint",
                                    course2md::tr!("拖动可以调整顺序"),
                                )
                                .text_size(TEXT_AUX)
                                .text_color(color(ColorRole::ON_SURFACE_VARIANT)),
                            )
                        },
                    )
                    .into_any_element()
            }
            QueueItem::Task { task, place, .. } => {
                self.task_card(&tasks[*task], *place, window, cx)
            }
            QueueItem::QueueFold { hidden, open } => {
                let toggle = open.clone();
                let opened = *open.read(cx);
                ListItem::new(
                    "toggle-task-queue",
                    if opened {
                        course2md::tr!("收起").to_owned()
                    } else {
                        course2md::tr!("显示另外 {} 个", hidden)
                    },
                )
                .trailing(ListTrailing::Icon(disclosure_chevron(opened)))
                .segment([false, false, true, true])
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |open, cx| {
                        *open = !*open;
                        cx.notify();
                    });
                })
                .into_any_element()
            }
            QueueItem::CompletedToggle { count, open, state } => {
                let toggle = state.clone();
                let open = *open;
                // The tile names what it folds; the count is the record's size.
                ListItem::new(
                    "toggle-task-history",
                    if open {
                        course2md::tr!("收起已完成的处理")
                    } else {
                        course2md::tr!("查看已完成的处理")
                    },
                )
                .leading(ListLeading::Icon(symbols::check_circle().into()))
                .trailing_text(count.to_string())
                .trailing(ListTrailing::Icon(disclosure_chevron(open)))
                .segment([true; 4])
                // Folding also closes a finished task that was opened
                // from elsewhere, or the fold could not close.
                .on_click(cx.listener(move |this, _, _, cx| {
                    if open
                        && let Some(id) = this.task_list.expanded.clone()
                        && this
                            .task(&id, cx)
                            .is_some_and(|task| task_group(task).settled())
                    {
                        this.close_task_details(&id);
                        this.task_list.expanded = None;
                    }
                    toggle.update(cx, |state, cx| {
                        *state = !open;
                        cx.notify();
                    });
                }))
                .into_any_element()
            }
        }
    }

    /// Every command for a task, on a right press and behind the card's ⋯ button.
    pub(crate) fn task_menu(
        &self,
        task: &TaskRecord,
        cx: &App,
    ) -> impl Fn(&mut Window, &mut App) -> MaterialMenu + 'static {
        let entity = self.this.clone();
        let id = task.id.clone();
        let actions = self.task_actions(task, TaskSurface::Menu, cx);
        // A divider ends each menu section.
        let items: Vec<_> = actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let ends = actions
                    .get(index + 1)
                    .is_some_and(|next| next.group() != action.group());
                (self.present(action, task, cx), ends, action.clone())
            })
            .collect();
        move |_, _| {
            MaterialMenu::new(SharedString::from(format!("task-more-items-{id}"))).items(
                items.iter().map(|((text, icon, disabled), ends, action)| {
                    let danger = *action == TaskAction::Cancel;
                    MaterialMenuItem::new(*text)
                        .leading(
                            Icon::from(*icon)
                                .when(danger, |icon| icon.color(color(ColorRole::ERROR))),
                        )
                        .disabled(*disabled)
                        .divider(*ends)
                        .on_click(action_click(entity.clone(), id.clone(), action.clone()))
                }),
            )
        }
    }

    /// The same commands behind the card's ⋯ button.
    fn task_menu_button(
        &self,
        task: &TaskRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = &task.id;
        let label = course2md::tr!("更多操作");
        let items = self.task_menu(task, cx)(window, cx);
        let trigger =
            MaterialTooltip::plain(SharedString::from(format!("task-more-tip-{id}")), label)
                .trigger(IconButton::new(
                    SharedString::from(format!("task-more-{id}")),
                    symbols::more_vert(),
                    label,
                ));
        ui::menu(
            SharedString::from(format!("task-more-menu-{id}")),
            trigger,
            items,
            window,
            cx,
        )
        .anchor(Anchor::TopRight)
        .into_any_element()
    }

    /// Expand a row's details, or fold the expanded one.
    pub(crate) fn toggle_task_row(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.task_list.expanded.as_deref() == Some(id) {
            self.close_task_details(id);
            self.task_list.expanded = None;
            cx.notify();
            return;
        }
        cx.emit(TasksEvent::Select {
            id: id.to_owned(),
            expand: true,
        });
    }

    /// Show or hide a task's log; a stopped task opens its details to show it.
    pub(crate) fn toggle_task_log(&mut self, task: &TaskRecord, cx: &mut Context<Self>) {
        if !self.task_list.logs_open.remove(&task.id) {
            self.task_list.logs_open.insert(task.id.clone());
            if task_group(task) != TaskGroup::Running {
                cx.emit(TasksEvent::Select {
                    id: task.id.clone(),
                    expand: false,
                });
            }
        }
        cx.notify();
    }

    /// Folding a card unmounts its details; opening it later starts fresh.
    pub(crate) fn close_task_details(&mut self, id: &str) {
        self.entered.remove(&format!("task-detail-{id}"));
        self.task_list.logs_open.remove(id);
    }

    /// Where a task saves, as its supporting line.
    fn task_destination(&self, task: &TaskRecord, cx: &App) -> String {
        match self.task_library(task, cx) {
            Some(location) => course2md::tr!("保存到 {}", location.label()),
            None => course2md::tr!("保存位置暂时不可用").to_owned(),
        }
    }

    /// Which service receives the text, when one does.
    fn task_ai_destination(&self, task: &TaskRecord) -> Option<String> {
        let name = self.facts.services.get(
            task.plan
                .ai_service
                .as_ref()
                .filter(|_| task.plan.config.llm.uses_service())?,
        )?;
        Some(course2md::tr!(
            "{}发送到「{}」",
            if task.plan.options().vision {
                course2md::tr!("文字与截图")
            } else {
                course2md::tr!("文字")
            },
            name,
        ))
    }

    /// What a task was planned with, read from its saved plan: a submitted task's
    /// options on the workbench, and the record in its details.
    pub(crate) fn task_plan_rows(&self, task: &TaskRecord, flat: bool, cx: &App) -> Vec<Div> {
        let options = task.plan.options();
        let mut ai: Vec<String> = [
            desktop_tasks::import::ConversionAiOption::Proofread,
            desktop_tasks::import::ConversionAiOption::Summary,
            desktop_tasks::import::ConversionAiOption::Vision,
        ]
        .into_iter()
        .filter(|option| desktop_tasks::import::conversion_ai_option_enabled(&options, *option))
        .map(|option| desktop_tasks::import::conversion_ai_option_label(option).to_owned())
        .collect();
        if options.translate.is_some() {
            ai.push(course2md::tr!("双语翻译").to_owned());
        }
        let ai = if ai.is_empty() {
            course2md::tr!("不使用 AI").to_owned()
        } else {
            ai.join(course2md::tr!("、"))
        };
        let ai = match self.task_ai_destination(task) {
            Some(destination) => format!("{ai} · {destination}"),
            None => ai,
        };
        let library = self
            .workspace(cx)
            .and_then(|workspace| workspace.state.library(&task.plan.library_id));
        let folder = task
            .plan
            .folder
            .and_then(|id| {
                self.library(cx)
                    .indexes
                    .get(&library?.root)?
                    .folders
                    .get(&id)
                    .cloned()
            })
            .unwrap_or_else(|| course2md::tr!("未分类").to_owned());
        let destination = match library {
            Some(library) => format!("{} · {folder}", library.label()),
            None => folder,
        };
        let id = &task.id;
        [
            (
                format!("task-destination-{id}"),
                symbols::folder_open(),
                course2md::tr!("保存到"),
                destination,
            ),
            (
                format!("task-text-source-{id}"),
                symbols::subtitles(),
                course2md::tr!("文字来源"),
                task_text_source(task).to_owned(),
            ),
            (
                format!("task-ai-{id}"),
                symbols::auto_fix_high(),
                course2md::tr!("AI 处理"),
                ai,
            ),
        ]
        .into_iter()
        .map(|(id, icon, label, value)| ui::fact_row(id, (!flat).then_some(icon), label, value))
        .collect()
    }

    fn task_cover(&self, task: &TaskRecord, cx: &App) -> Option<PathBuf> {
        task.plan.source.cover.clone().or_else(|| {
            self.library(cx)
                .cover_by_source
                .get(&task.plan.source_id)
                .cloned()
        })
    }

    /// One task, wherever it stands.
    pub(crate) fn task_card(
        &mut self,
        task: &TaskRecord,
        place: CardPlace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = &task.id;
        let expanded = self.task_list.expanded.as_ref() == Some(id);
        let more = self.task_menu_button(task, window, cx);
        // On the workbench the running card's status is its step; Tasks owns the rest.
        let on_home_running = matches!(place, CardPlace::Home) && runs_in_card(task);
        let chip = (!on_home_running
            && (task.state != TaskState::Queued || !matches!(place, CardPlace::Row { .. })))
        .then(|| {
            // A running title already says what is being generated; the chip only
            // carries the state.
            let generating = task.state == TaskState::Running && task.intent == Intent::Run;
            status_chip(task_status_kind(task.state)).child(
                if generating && matches!(place, CardPlace::Open { .. }) {
                    course2md::tr!("处理中").to_owned()
                } else {
                    task_status_label(task)
                },
            )
        });
        let context_menu = self.task_menu(task, cx);
        // The running card's progress and actions stay in its text column.
        let inner = on_home_running.then(|| self.task_body(task, place, window, cx));
        let (outer, head, body) = match place {
            CardPlace::Row { outer, number } => {
                // An open row's details continue its surface below it.
                let corners = if expanded {
                    [outer[0], outer[1], false, false]
                } else {
                    outer
                };
                let head = self.task_row_head(task, number, corners, chip, more, cx);
                (outer, head.into_any_element(), expanded)
            }
            CardPlace::Open { outer } => {
                let facts = (
                    self.task_destination(task, cx),
                    self.task_cover(task, cx),
                    COVER_WIDTH,
                    self.task_step_line(task, false, cx),
                );
                let head = self.task_card_head(task, place, chip, more, None, facts);
                (outer, head.into_any_element(), true)
            }
            CardPlace::Home => {
                let facts = (
                    self.task_destination(task, cx),
                    self.task_cover(task, cx),
                    import_ui::cover_width(window),
                    self.task_step_line(task, true, cx),
                );
                let head = self.task_card_head(task, place, chip, more, inner, facts);
                (
                    [true, true, false, false],
                    head.into_any_element(),
                    !on_home_running,
                )
            }
        };
        let body = body.then(|| self.task_body(task, place, window, cx));
        tile(outer)
            .map(|card| match place {
                CardPlace::Home => card.rounded_t(RADIUS_HERO),
                CardPlace::Open { .. } if outer == [true; 4] => card.rounded(RADIUS_HERO),
                _ => card,
            })
            .id(SharedString::from(format!("task-menu-{id}")))
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(MaterialContextMenu::new(
                SharedString::from(format!("task-context-{id}")),
                context_menu,
            ))
            .map(|card| match place {
                CardPlace::Row { .. } => card
                    .child(head)
                    .children(body.map(|body| div().px(rem(16.)).pb(rem(16.)).child(body))),
                // A card of its own on Tasks is roomy, as its large corners are (T6).
                CardPlace::Open { .. } => card.p(rem(20.)).gap(rem(16.)).child(head).children(body),
                _ => card.p(rem(16.)).gap(rem(16.)).child(head).children(body),
            })
            .into_any_element()
    }

    /// A row of its group: the grip for queued work, the row that folds details out,
    /// and what it says and offers at its trailing edge. The whole row is one tile
    /// button, grip and commands included, with the inset cover of a note row (U21, V6).
    fn task_row_head(
        &self,
        task: &TaskRecord,
        number: Option<usize>,
        corners: OuterCorners,
        chip: Option<Div>,
        more: AnyElement,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = task.id.clone();
        let waiting = number.map(|_| task.state);
        let scale = TileScale::Regular;
        let cover = crate::course_library::row_cover(
            self.task_cover(task, cx),
            match waiting {
                Some(TaskState::Paused) => symbols::pause(),
                Some(_) => symbols::schedule(),
                None => symbols::movie(),
            },
        );
        let row = scale
            .button(SharedString::from(format!("select-{id}")), corners)
            .accessibility_label(task.plan.title.clone())
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| this.toggle_task_row(&id, cx)
            }))
            .child(
                scale
                    .row()
                    // At narrow widths or large text the commands move under the identity.
                    .flex_wrap()
                    .when(number.is_some(), |row| {
                        row.child(h_flex().flex_shrink_0().cursor_grab().child(
                            Icon::from(symbols::drag_indicator()).color(color(ColorRole::OUTLINE)),
                        ))
                    })
                    .child(cover)
                    .child(
                        v_flex()
                            .flex_1()
                            .flex_basis(rem(200.))
                            .min_w_0()
                            .gap(rem(2.))
                            .child(
                                scale
                                    .label(
                                        SharedString::from(format!("task-row-title-{id}")),
                                        task.plan.title.clone(),
                                    )
                                    .whitespace_normal()
                                    .line_clamp(2)
                                    .text_ellipsis(),
                            )
                            .child(scale.support(
                                SharedString::from(format!("task-row-place-{id}")),
                                self.task_destination(task, cx),
                            )),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .ml_auto()
                            .gap(rem(8.))
                            .items_center()
                            .children(chip)
                            .when(
                                task.artifact.is_some() && task_group(task) != TaskGroup::Queued,
                                |row| {
                                    row.child(
                                        material_button(
                                            SharedString::from(format!("task-read-{id}")),
                                            ButtonVariant::Text,
                                        )
                                        .child(course2md::tr!("阅读笔记"))
                                        .on_click(
                                            action_click(
                                                cx.weak_entity(),
                                                id.clone(),
                                                TaskAction::Read,
                                            ),
                                        ),
                                    )
                                },
                            )
                            .child(more),
                    ),
            );
        let head = div()
            .id(SharedString::from(format!("task-row-{id}")))
            .w_full()
            .min_w_0()
            .child(row);
        match number {
            Some(number) => {
                let drag = QueueDrag {
                    id,
                    title: task.plan.title.clone().into(),
                };
                head.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                    .drag_over::<QueueDrag>(|style, _, _, _| {
                        style.bg(color(ColorRole::SECONDARY_CONTAINER))
                    })
                    .on_drop(cx.listener(move |_, drag: &QueueDrag, _, cx| {
                        cx.emit(TasksEvent::Move {
                            id: drag.id.clone(),
                            position: number - 1,
                        })
                    }))
            }
            None => head,
        }
    }

    /// A card's identity: its cover, title and what is known so far.
    fn task_card_head(
        &self,
        task: &TaskRecord,
        place: CardPlace,
        chip: Option<Div>,
        more: AnyElement,
        inner: Option<Div>,
        // Where it saves, its cover and the cover's width, and the step it is on.
        (destination, cover, cover_width, step): (String, Option<PathBuf>, Rems, Option<String>),
    ) -> Div {
        let id = &task.id;
        let home = matches!(place, CardPlace::Home);
        let title = if task.plan.title.trim().is_empty() {
            task.plan.source.title.clone()
        } else {
            task.plan.title.clone()
        };
        let generating = task.state == TaskState::Running && task.intent == Intent::Run;
        let title = if generating && !home {
            course2md::tr!("正在生成《{}》", title)
        } else {
            title
        };
        // Where it saves, where its text comes from and which service
        // receives it, on one line; on the workbench, whose video it is.
        let support = if home {
            desktop_tasks::import::source_metadata(&task.plan.source)
        } else {
            [destination, task_text_source(task).to_owned()]
                .into_iter()
                .chain(self.task_ai_destination(task))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        // A running task's status ends its title; one that waits says so below it.
        let (title_chip, line_chip) = if home || needs_person(task) {
            (None, chip)
        } else {
            (chip, None)
        };
        h_flex()
            .w_full()
            .min_w_0()
            .items_start()
            .flex_wrap()
            .gap(rem(16.))
            .map(|head| {
                // A task that needs the person leads with that, not its video. Only a
                // failure is red: a paused, partial or uncertain task is a warning.
                if !home && needs_person(task) {
                    head.child(tinted_icon(
                        symbols::warning(),
                        if task.state == TaskState::NeedsAttention {
                            Tint::Danger
                        } else {
                            Tint::Amber
                        },
                        ui::RESOURCE_ICON,
                    ))
                } else {
                    head.child(video_cover(cover, cover_width))
                }
            })
            .child(
                v_flex()
                    .flex_1()
                    .flex_basis(rem(260.))
                    .min_w_0()
                    .gap(rem(4.))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_start()
                            .gap(rem(8.))
                            .map(|row| {
                                if !home && needs_person(task) {
                                    row.child(
                                        // It shrinks before the menu beside it, and a long
                                        // title ends in an ellipsis on its second line.
                                        TileScale::Regular
                                            .label(format!("task-title-{id}"), title.clone())
                                            .flex_1()
                                            .min_w_0()
                                            .whitespace_normal()
                                            .text_ellipsis()
                                            .line_clamp(2),
                                    )
                                } else {
                                    row.child(
                                        video_title(format!("task-title-{id}"), title.clone())
                                            .flex_1(),
                                    )
                                }
                            })
                            .children(title_chip)
                            .child(more),
                    )
                    // Whose video first on the workbench, where the step belongs to the bar.
                    .when(home, |column| {
                        column.when(!support.is_empty(), |column| {
                            column.child(task_support_line(id, support.clone()))
                        })
                    })
                    .when_some(step, |column, step| {
                        column.child(
                            accessible_text(SharedString::from(format!("task-step-{id}")), step)
                                .role(Role::Status)
                                .text_size(TEXT_BODY)
                                .text_color(color(ColorRole::ON_SURFACE_VARIANT))
                                .whitespace_normal(),
                        )
                    })
                    .when(!home && !support.is_empty(), |column| {
                        column.child(task_support_line(id, support.clone()))
                    })
                    .child(h_flex().pt(rem(4.)).children(line_chip))
                    .children(inner),
            )
    }

    /// The step a running task is on. Later steps depend on what the video turns out
    /// to need, so the position counts the steps reached so far, without a total. The
    /// workbench says the step and its measure instead, above the bar.
    fn task_step_line(&self, task: &TaskRecord, home: bool, cx: &App) -> Option<String> {
        let stages = self.task_stage_rows(task, cx);
        let index = stages.iter().position(|stage| stage.status.running());
        index
            .map(|index| {
                let stage = &stages[index];
                if !home {
                    course2md::tr!("第 {} 步 · {}", index + 1, stage.title)
                } else if stage.detail.is_empty() {
                    stage.title.clone()
                } else {
                    format!("{} · {}", stage.title, stage.detail)
                }
            })
            .or_else(|| self.task_wait(task, cx).map(|wait| wait.label().to_owned()))
    }

    fn task_wait(&self, task: &TaskRecord, cx: &App) -> Option<WorkerWait> {
        worker_wait_state(
            self.is_running_task(&task.id, cx),
            task.events.done.is_some(),
            task.intent,
            task.events
                .stages
                .iter()
                .map(|(name, item)| (name, item.done)),
        )
    }

    /// One indicator for the step in progress: its measured fraction when the
    /// worker reports one, motion otherwise. The line under it carries the step's
    /// own measurement and clock, never an estimate.
    fn task_progress(
        &self,
        task: &TaskRecord,
        stages: &[TaskStage],
        surface: TaskSurface,
        cx: &App,
    ) -> Option<Div> {
        let id = &task.id;
        let active = self.is_running_task(id, cx);
        let current = stages.iter().find(|stage| stage.status.running());
        if !active || (current.is_none() && self.task_wait(task, cx).is_none()) {
            return None;
        }
        let detail = current
            .map(|stage| stage.detail.clone())
            .filter(|detail| !detail.is_empty())
            .or_else(|| {
                self.work
                    .read(cx)
                    .job_started
                    .filter(|_| self.work.read(cx).job.is_some())
                    .map(|started| activity::elapsed_label(started.elapsed()))
            })
            .filter(|_| surface != TaskSurface::Home || current.is_none());
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .child(
                    LinearProgress::new(
                        SharedString::from(format!("task-progress-{id}")),
                        current.map_or("", |stage| stage.title.as_str()).to_owned(),
                    )
                    .value(current.and_then(|stage| stage.fraction))
                    .muted(task.state == TaskState::Pausing || task.intent != Intent::Run),
                )
                .when_some(detail, |view, detail| {
                    view.child(
                        ui::status_line(SharedString::from(format!("task-measure-{id}")), &detail)
                            .text_size(TEXT_AUX)
                            .text_color(color(ColorRole::ON_SURFACE_VARIANT)),
                    )
                }),
        )
    }

    /// The uncertain-outcome decision: exactly what may be billed again, beside the
    /// one button that sends it. Nothing here runs without that press.
    fn uncertain_block(&self, task: &TaskRecord, cx: &mut Context<Self>) -> Option<Div> {
        let id = &task.id;
        let uncertain: Vec<_> = task
            .events
            .blocked
            .iter()
            .filter(|blocked| blocked.is_uncertain())
            .collect();
        if uncertain.is_empty() || task.handled_by.is_some() {
            return None;
        }
        let active = self.is_running_task(id, cx);
        Some(
            notice_banner(
                SharedString::from(format!("unknown-effect-{id}")),
                NoticeKind::Warning,
                course2md::tr!("服务可能已处理这些内容，重新发送可能产生额外费用"),
            )
            .rounded(RADIUS_LARGE)
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(8.))
                    .text_size(TEXT_BODY)
                    .children(uncertain.iter().enumerate().map(|(index, blocked)| {
                        accessible_text(
                            SharedString::from(format!("unknown-scope-{id}-{index}")),
                            request_scope(blocked),
                        )
                        .whitespace_normal()
                    }))
                    .when(active, |view| {
                        view.child(accessible_text(
                            SharedString::from(format!("unknown-saving-{id}")),
                            course2md::tr!("正在保存当前结果…"),
                        ))
                    })
                    .child(
                        material_button(
                            SharedString::from(format!("resend-{id}")),
                            ButtonVariant::Filled,
                        )
                        .self_start()
                        .icon(symbols::refresh())
                        .child(course2md::tr!("重新发送以上内容"))
                        .disabled(active)
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |_, _, _, cx| cx.emit(TasksEvent::Resend(id.clone()))
                        })),
                    ),
            ),
        )
    }

    /// What a card shows under its identity: why it stopped, what is decided next,
    /// how far it got, and the actions; its record once its details are open.
    fn task_body(
        &mut self,
        task: &TaskRecord,
        place: CardPlace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = task.id.clone();
        let row = matches!(place, CardPlace::Row { .. });
        let surface = if matches!(place, CardPlace::Home) {
            TaskSurface::Home
        } else {
            TaskSurface::Card
        };
        let mut actions = self.task_actions(task, surface, cx);
        // A row carries reading at its edge and folds itself.
        actions.retain(|action| !row || !matches!(action, TaskAction::Read | TaskAction::Details));
        let lead = lead_action(&actions, task, surface).cloned();
        let failures = task_note_failures(task);
        let consequence = (!failures.is_empty()).then(|| {
            course2md::tr!(
                "{}尚未完成，笔记的其余部分已保存",
                failures
                    .iter()
                    .map(|(_, part, _)| part.label())
                    .collect::<Vec<_>>()
                    .join(course2md::tr!("、"))
            )
        });
        let stages = self.task_stage_rows(task, cx);
        // Between two steps the job still runs: the steps it has reached stay in view.
        let running = stages.iter().any(|stage| stage.status.running())
            || self.is_running_task(&task.id, cx) && !stages.is_empty();
        let progress = self.task_progress(task, &stages, surface, cx);
        // A single reached step is already the step line above.
        let chips = (running && stages.len() > 1 && surface != TaskSurface::Home).then(|| {
            h_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .flex_wrap()
                .children(stages.iter().map(|stage| {
                    status_fact_chip(
                        SharedString::from(format!("{}-chip", stage.id)),
                        course2md::tr!("{}：{}", stage.title, stage.status.label()),
                        stage.status.chip(),
                    )
                    .when(stage.status.running(), |chip| {
                        chip.child(icon_spinner(
                            SharedString::from(format!("{}-chip-spinner", stage.id)),
                            stage.title.clone(),
                            rem(16.),
                            cx,
                        ))
                    })
                    .child(stage.title.clone())
                }))
        });
        let buttons = actions.iter().map(|action| {
            let (label, icon, disabled) = self.present(action, task, cx);
            let variant = if lead.as_ref() == Some(action) {
                ButtonVariant::Filled
            } else {
                match action {
                    TaskAction::Pause => ButtonVariant::Tonal,
                    TaskAction::Read
                    | TaskAction::Details
                    | TaskAction::Log
                    | TaskAction::Cancel
                    | TaskAction::ViewTask => ButtonVariant::Text,
                    _ => ButtonVariant::Outlined,
                }
            };
            material_button(SharedString::from(format!("task-{label}-{id}")), variant)
                .when(variant == ButtonVariant::Outlined, |b| {
                    b.tone(ButtonTone::Accent)
                })
                // What only shows more of the task is quieter than what acts on it (G4),
                // and stopping the work is never the saturated action, on any page.
                .when(
                    matches!(
                        action,
                        TaskAction::Cancel | TaskAction::Details | TaskAction::Log
                    ),
                    |b| b.tone(ButtonTone::Neutral),
                )
                .icon(icon)
                .child(label)
                .disabled(disabled)
                .when(
                    disabled && matches!(action, TaskAction::Pause | TaskAction::Cancel),
                    |b| b.loading(true, label),
                )
                .on_click(action_click(cx.weak_entity(), id.clone(), action.clone()))
        });
        let mut buttons: Vec<MaterialButton> = buttons.collect();
        let home_running = surface == TaskSurface::Home && runs_in_card(task);
        // Stopping stands apart from what goes on; those wrap as one trailing group.
        let leading_cancel = (home_running)
            .then(|| {
                actions
                    .iter()
                    .position(|action| *action == TaskAction::Cancel)
            })
            .flatten()
            .map(|index| buttons.remove(index));
        let buttons = if home_running {
            h_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .flex_wrap()
                .children(leading_cancel)
                .child(
                    h_flex()
                        .ml_auto()
                        .gap(rem(8.))
                        .flex_wrap()
                        .justify_end()
                        .children(buttons),
                )
        } else {
            h_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .flex_wrap()
                .when(
                    !needs_person(task) || matches!(place, CardPlace::Home),
                    |row| row.justify_end(),
                )
                .children(buttons)
        };
        let details = (self.task_list.expanded.as_ref() == Some(&id)).then(|| {
            let content = v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(12.))
                .child(MaterialDivider::new(SharedString::from(format!(
                    "task-detail-divider-{id}"
                ))))
                .children(self.task_plan_rows(task, false, cx))
                .children(
                    self.task_stage_rows(task, cx)
                        .into_iter()
                        .map(|stage| stage_row(stage, window, cx)),
                );
            let key = format!("task-detail-{id}");
            if self.enter_once(key.clone()) {
                motion::enter(SharedString::from(key), content)
            } else {
                content.into_any_element()
            }
        });
        let log = task_log_text(task)
            .filter(|_| self.task_list.logs_open.contains(&id))
            .map(|text| ui::log_box(&id, text));
        // A task that waits for the person keeps its explanation on the text's ruler too.
        let ruler = if needs_person(task) && !matches!(place, CardPlace::Home) {
            rem(ui::RESOURCE_ICON_SIZE + 16.)
        } else {
            rem(0.)
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(12.))
            .pl(ruler)
            .children(
                task_reasons(task)
                    .into_iter()
                    .enumerate()
                    .map(|(index, reason)| {
                        error_text(
                            SharedString::from(format!("task-reason-{id}-{index}")),
                            reason,
                        )
                        .text_size(TEXT_BODY)
                        .whitespace_normal()
                    }),
            )
            .children(consequence.map(|consequence| {
                accessible_text(
                    SharedString::from(format!("task-consequence-{id}")),
                    consequence,
                )
                .text_size(TEXT_AUX)
                .text_color(color(ColorRole::ON_SURFACE_VARIANT))
                .whitespace_normal()
            }))
            .children(self.uncertain_block(task, cx))
            .children(progress)
            .children(chips)
            .child(buttons)
            .children(details)
            .children(log)
    }

    /// A waiting line with the running job's clock, so a pause between
    /// stages never looks stalled.
    pub(crate) fn with_job_clock(&self, label: &str, cx: &App) -> String {
        match self
            .work
            .read(cx)
            .job_started
            .filter(|_| self.work.read(cx).job.is_some())
        {
            Some(started) => format!("{label} · {}", activity::elapsed_label(started.elapsed())),
            None => label.to_owned(),
        }
    }

    /// Compact status uses the same measured progress as task details.
    pub(crate) fn compact_task_progress(
        &self,
        task: &TaskRecord,
        cx: &App,
    ) -> Option<(String, Option<f32>)> {
        self.task_stage_rows(task, cx)
            .into_iter()
            .find(|stage| stage.status.running())
            .map(|stage| {
                let label = if stage.compact_detail.is_empty() {
                    stage.title
                } else {
                    format!("{} · {}", stage.title, stage.compact_detail)
                };
                (label, stage.fraction)
            })
    }
}

impl Render for TasksPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.queue_page(window, cx)
    }
}

impl TasksPage {
    /// Its label and icon, and whether it is unavailable right now.
    pub(crate) fn present(
        &self,
        action: &TaskAction,
        task: &TaskRecord,
        cx: &App,
    ) -> (&'static str, SymbolIcon, bool) {
        let cancelling = task.state == TaskState::Pausing && task.intent == Intent::Cancel;
        let pausing = task.state == TaskState::Pausing && !cancelling;
        let details = self.task_list.expanded.as_ref() == Some(&task.id);
        let log = self.task_list.logs_open.contains(&task.id);
        use course2md::tr;
        match action {
            TaskAction::Read => (tr!("阅读笔记"), symbols::menu_book(), false),
            TaskAction::OpenNoteFolder => (tr!("打开笔记保存位置"), symbols::folder_open(), false),
            TaskAction::OpenExports => (tr!("打开导出位置"), symbols::folder_open(), false),
            TaskAction::OpenSource => (tr!("打开原视频"), symbols::open_in_new(), false),
            TaskAction::CopyLink => (tr!("复制视频链接"), symbols::content_copy(), false),
            TaskAction::Details if details => {
                (tr!("收起详情"), symbols::keyboard_arrow_up(), false)
            }
            TaskAction::Details => (tr!("查看详情"), symbols::keyboard_arrow_down(), false),
            TaskAction::Log if log => (tr!("收起日志"), symbols::description(), false),
            TaskAction::Log => (tr!("查看日志"), symbols::description(), false),
            TaskAction::Followup(_) => (tr!("查看后续处理任务"), symbols::arrow_forward(), false),
            TaskAction::Move { label, up, to } => (
                label,
                if *up {
                    symbols::arrow_upward()
                } else {
                    symbols::arrow_downward()
                },
                to.is_none(),
            ),
            TaskAction::Pause if pausing => (tr!("正在暂停…"), symbols::pause(), true),
            TaskAction::Pause => (tr!("暂停"), symbols::pause(), false),
            TaskAction::Resume => (tr!("继续生成"), symbols::play_arrow(), false),
            TaskAction::PrepareModel => (
                tr!("下载并准备模型"),
                symbols::download(),
                self.work.read(cx).job.is_some(),
            ),
            TaskAction::Repair => (tr!("修复 AI 服务并补做"), symbols::settings(), false),
            TaskAction::Retry(component) => {
                (retry_component_label(component), symbols::refresh(), false)
            }
            TaskAction::Reassociate(_) => (
                tr!("重新关联此保存位置"),
                symbols::database(),
                self.facts.storage_busy,
            ),
            TaskAction::Resend => (tr!("重新发送以上内容"), symbols::refresh(), false),
            TaskAction::Adjust => (adjust_label(task), symbols::tune(), false),
            TaskAction::Cancel if cancelling => (tr!("正在取消…"), symbols::close(), true),
            TaskAction::Cancel => (tr!("取消任务"), symbols::close(), false),
            TaskAction::ViewTask => (tr!("查看任务"), symbols::task(), false),
            TaskAction::NextVideo => (tr!("设置下一个视频"), symbols::add(), false),
        }
    }
}

pub(crate) fn task_status_kind(state: TaskState) -> StatusKind {
    match state {
        TaskState::Complete => StatusKind::Success,
        TaskState::Running | TaskState::Pausing => StatusKind::Progress,
        TaskState::NeedsAttention => StatusKind::Danger,
        TaskState::Uncertain | TaskState::Partial => StatusKind::Warning,
        _ => StatusKind::Neutral,
    }
}

impl StageStatusView for StageStatus {
    fn role(self) -> ColorRole {
        match self {
            Self::Running => ColorRole::PRIMARY,
            Self::Complete => ColorRole::SUCCESS,
            Self::Failed => ColorRole::ERROR,
            Self::Uncertain => ColorRole::WARNING,
            _ => ColorRole::ON_SURFACE_VARIANT,
        }
    }

    fn chip(self) -> StatusKind {
        match self {
            Self::Complete => StatusKind::Success,
            Self::Failed => StatusKind::Danger,
            Self::Uncertain => StatusKind::Warning,
            _ if self.running() => StatusKind::Progress,
            _ => StatusKind::Neutral,
        }
    }

    fn icon(self) -> SymbolIcon {
        match self {
            Self::Running => symbols::refresh(),
            Self::Complete => symbols::check_circle(),
            Self::Paused | Self::Pausing => symbols::pause(),
            Self::Stopping | Self::Cancelled => symbols::close(),
            Self::Failed | Self::Uncertain => symbols::warning(),
            Self::Waiting => symbols::schedule(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{QueueItem, TaskGroup};

    #[gpui::test]
    fn queue_item_keys_track_identity_not_content(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        cx.update(|cx| {
            let tasks = vec![task_record("task-a"), task_record("task-b")];
            let open = cx.new(|_| false);
            let row = |task: usize| QueueItem::Task {
                task,
                place: super::CardPlace::Row {
                    outer: [false; 4],
                    number: Some(task + 1),
                },
                joined: false,
            };
            let items = [
                QueueItem::GroupLabel {
                    group: TaskGroup::Queued,
                    count: 1,
                    queued: 1,
                },
                row(0),
                QueueItem::CompletedToggle {
                    count: 1,
                    open: false,
                    state: open,
                },
                row(1),
            ];
            let keys: Vec<String> = items.iter().map(|item| item.key(&tasks)).collect();
            let unique: std::collections::HashSet<_> = keys.iter().collect();
            assert_eq!(unique.len(), keys.len(), "queue rows need distinct keys");
            // Content updates with the same identities leave keys untouched.
            let tasks = vec![task_record("task-a"), task_record("task-b")];
            let again: Vec<String> = items.iter().map(|item| item.key(&tasks)).collect();
            assert_eq!(keys, again);
            // Task rows key on the task id, not the position in the queue.
            let mut moved = tasks;
            moved.swap(0, 1);
            assert_eq!(row(0).key(&moved), "task-task-b");
        });
    }

    /// Opening a row asks the window to select the task, which then opens it; folding the
    /// open row is the page's own and closes its log with it.
    #[gpui::test]
    fn a_row_opens_through_the_window_and_folds_on_its_own(cx: &mut gpui::TestAppContext) {
        use super::{TasksEvent, TasksPage};
        use gpui::AppContext as _;
        let records = cx.new(|_| {
            desktop_store::records::Records::new(
                None,
                desktop_store::library::LibraryState::new(std::env::temp_dir()),
            )
        });
        let work = cx.new(|_| desktop_tasks::job::Work::new());
        let page = cx.new(|cx| TasksPage::new(records, work, cx));
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|cx| {
            let asked = asked.clone();
            cx.subscribe(&page, move |_, event: &TasksEvent, _| {
                if let TasksEvent::Select { id, expand } = event {
                    asked.borrow_mut().push((id.clone(), *expand));
                }
            })
            .detach();
        });
        page.update(cx, |page, cx| page.toggle_task_row("a", cx));
        cx.run_until_parked();
        assert_eq!(*asked.borrow(), [("a".to_owned(), true)]);
        assert_eq!(
            page.read_with(cx, |page, _| page.task_list.expanded.clone()),
            None
        );
        page.update(cx, |page, cx| {
            page.task_list.expanded = Some("a".into());
            page.task_list.logs_open.insert("a".into());
            page.toggle_task_row("a", cx);
        });
        cx.run_until_parked();
        assert_eq!(asked.borrow().len(), 1, "folding asks nothing");
        page.read_with(cx, |page, _| {
            assert_eq!(page.task_list.expanded, None);
            assert!(page.task_list.logs_open.is_empty());
        });
    }

    fn source(input: &str, title: &str) -> crate::source::Source {
        crate::source::Source {
            input: input.into(),
            title: title.into(),
            identity: format!("online:{input}"),
            online: true,
            ..Default::default()
        }
    }

    fn task_record(id: &str) -> crate::workspace::TaskRecord {
        crate::workspace::TaskRecord {
            id: id.into(),
            plan: crate::workspace::TaskPlan {
                operation: Default::default(),
                source: source("https://example.test/video", "课程"),
                source_id: "online:video".into(),
                title: "课程".into(),
                library_id: "library".into(),
                folder: None,
                subtitle: None,
                config: Default::default(),
                asr_service: None,
                ai_service: None,
            },
            state: crate::workspace::TaskState::Paused,
            intent: crate::workspace::Intent::Run,
            created: 0,
            updated: 0,
            parent: None,
            handled_by: None,
            held: false,
            work_dir: "work".into(),
            events: crate::activity::JobEvents {
                error: None,
                ..Default::default()
            },
            artifact: None,
            unread: false,
            resend: Vec::new(),
        }
    }
}
