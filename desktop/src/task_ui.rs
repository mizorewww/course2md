//! The task page: 4 clean groups (`Running`, `Attention`, `Queued`, `Finished`),
//! in-place course task cards, stage rows, and copyable diagnostic logs.
use crate::{
    Desktop, Page, activity,
    import_ui::{self, video_cover, video_title},
    symbols, ui,
    workspace::{TaskRecord, TaskStatus},
};
use desktop_store::records::Records;
use desktop_tasks::job::Work;
use desktop_tasks::task_view::*;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit::component::{ActiveTheme as _, scroll::Scrollbar};
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
    pub logs_open: HashSet<String>,
    pub log_scrolls: HashMap<String, ScrollHandle>,
    pub copied_logs: HashSet<String>,
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
            log_scrolls: HashMap::new(),
            copied_logs: HashSet::new(),
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
    /// Inline page notice for an action error on the Tasks page.
    PageNotice(String),
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
    /// Folds finished work behind one tile.
    CompletedToggle {
        count: usize,
        open: bool,
        state: Entity<Option<bool>>,
    },
}

impl QueueItem {
    /// Identity for the list splice: stable across content updates that keep
    /// the row in place, so measured heights and scroll anchors survive.
    fn key(&self, tasks: &[TaskRecord]) -> String {
        match self {
            QueueItem::Header => "header".to_owned(),
            QueueItem::PageNotice(_) => "page-notice".to_owned(),
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
/// The task page's content column, inside the shell's gutters: home's column, so a
/// task reads at the same width in both places.
const TASK_COLUMN: Rems = rem(760.);
/// Between the page heading and each section.
const TASK_SECTION_GAP: Rems = rem(28.);
/// The cover's width on a task card away from home.
const COVER_WIDTH: Rems = rem(160.);

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

/// The task page: the queue of every task and its cards, which the workbench also shows.
pub(crate) struct TasksPage {
    records: Entity<Records>,
    work: Entity<Work>,
    this: WeakEntity<Self>,
    pub(crate) task_list: TaskListUi,
    /// What the window knows beyond the records and the work.
    facts: ShellFacts,
    /// Inline notice for task-page actions (such as opening a missing export folder).
    notice: Option<String>,
    /// Details already shown once; a virtualized row remounts without replaying them.
    entered: HashSet<String>,
    /// The workbench's running card: its actions, taken from its text column to run
    /// across the card's foot as one group, as the draft card's do.
    home_footer: Option<Div>,
}

/// What the cards show that neither the records nor the work hold.
#[derive(Default, PartialEq)]
pub(crate) struct ShellFacts {
    /// Libraries whose location must be associated again.
    pub reassociate: HashSet<String>,
    /// A storage operation is under way.
    pub storage_busy: bool,
    /// Where the model a failed task needs stands, once its card asked to prepare it.
    pub models: HashMap<String, ModelPreparation>,
}

/// The model preparation a task's card started.
#[derive(Clone, PartialEq)]
pub(crate) enum ModelPreparation {
    Preparing,
    /// It stopped with this message; the card's 下载并准备模型 tries again.
    Failed(String),
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
    /// Move a waiting task to this place in the queue.
    Move {
        id: String,
        position: usize,
    },
    PauseAll,
    ResumeAll,
    ClearCompleted,
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
            notice: None,
            entered: HashSet::new(),
            home_footer: None,
        }
    }

    /// Takes what the window knows; a change draws the page again.
    pub(crate) fn set_facts(&mut self, facts: ShellFacts, cx: &mut Context<Self>) {
        if self.facts != facts {
            self.facts = facts;
            cx.notify();
        }
    }

    pub(crate) fn set_notice(&mut self, notice: Option<String>, cx: &mut Context<Self>) {
        if self.notice != notice {
            self.notice = notice;
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
        task_stage_rows(task, self.work.read(cx))
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
                    // Running and queued work keeps its run order; attention and finished
                    // show the most recently updated first.
                    let mut tasks: Vec<_> =
                        workspace.state.tasks.iter().cloned().enumerate().collect();
                    tasks.sort_by_key(|(index, task)| {
                        let group = task_group(task);
                        let order = match group {
                            TaskGroup::Running | TaskGroup::Queued => *index as i64,
                            _ => -(task.updated.max(task.created) as i64),
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
            if let Some(notice) = &self.notice {
                items.insert(1, QueueItem::PageNotice(notice.clone()));
            }
        }
        let completed_state = window.use_keyed_state("task-history-open", cx, |_, _| None::<bool>);
        let queue_open = window.use_keyed_state("task-queue-open", cx, |_, _| false);
        let done = |task: &TaskRecord| task_group(task).settled();
        let count = tasks.iter().filter(|task| done(task)).count();
        let only_settled = count > 0 && count == tasks.len();
        // Finished work folds behind one tile while active/failed work is above it,
        // and opens by default when nothing else is on the page; a task the person
        // went to also stays visible.
        let expanded_done = self
            .task_list
            .expanded
            .as_ref()
            .is_some_and(|id| tasks.iter().any(|task| &task.id == id && done(task)));
        let completed_open = expanded_done || completed_state.read(cx).unwrap_or(only_settled);
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
                        .filter(|&&index| tasks[index].status == TaskStatus::Queued)
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

    /// The page title and the actions that stop, resume, or clear settled work.
    fn queue_page_header(&self, tasks: &[TaskRecord], cx: &mut Context<Self>) -> Div {
        let TaskCounts {
            resumable,
            pausable,
            clearable,
            ..
        } = task_counts(tasks);
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
                    .child(ui::page_title("tasks-page-title", course2md::tr!("任务"))),
            )
            .child(
                h_flex()
                    .gap(rem(8.))
                    .flex_wrap()
                    .when(pausable > 1, |row| {
                        row.child(
                            material_button("tasks-pause-all", ButtonVariant::Outlined)
                                .tone(ButtonTone::Accent)
                                .icon(symbols::pause())
                                .child(course2md::tr!("全部暂停"))
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(TasksEvent::PauseAll))),
                        )
                    })
                    .when(resumable > 1, |row| {
                        row.child(
                            material_button("tasks-resume-all", ButtonVariant::Outlined)
                                .tone(ButtonTone::Accent)
                                .icon(symbols::play_arrow())
                                .child(course2md::tr!("全部继续"))
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(TasksEvent::ResumeAll)),
                                ),
                        )
                    })
                    .when(clearable > 0, |row| {
                        row.child(
                            MaterialTooltip::plain(
                                "tasks-clear-completed-tip",
                                course2md::tr!("仅清空任务记录，不会删除已生成的笔记"),
                            )
                            .trigger(
                                material_button("tasks-clear-completed", ButtonVariant::Outlined)
                                    .tone(ButtonTone::Neutral)
                                    .icon(symbols::delete_sweep())
                                    .child(course2md::tr!("清空已完成"))
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(TasksEvent::ClearCompleted)
                                    })),
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
            QueueItem::Header => self.queue_page_header(tasks, cx).into_any_element(),
            QueueItem::PageNotice(message) => {
                notice_banner("task-page-notice", NoticeKind::Warning, message.clone())
                    .child(
                        IconButton::new(
                            "dismiss-task-page-notice",
                            symbols::close(),
                            course2md::tr!("关闭提示"),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        })),
                    )
                    .into_any_element()
            }
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
                        *state = Some(!open);
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
                    let danger = matches!(action, TaskAction::Cancel | TaskAction::Remove);
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
        self.task_list.log_scrolls.remove(id);
        self.task_list.copied_logs.remove(id);
    }

    pub(crate) fn forget_tasks(&mut self, ids: &[String]) {
        for id in ids {
            self.close_task_details(id);
            if self.task_list.expanded.as_deref() == Some(id.as_str()) {
                self.task_list.expanded = None;
            }
        }
    }

    fn copy_task_log(&mut self, id: String, text: String, cx: &mut Context<Self>) {
        crate::clipboard::write(ClipboardItem::new_string(text), cx);
        self.task_list.copied_logs.insert(id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.task_list.copied_logs.remove(&id) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn task_log_panel(&mut self, id: &str, text: String, cx: &mut Context<Self>) -> AnyElement {
        let scroll = self
            .task_list
            .log_scrolls
            .entry(id.to_owned())
            .or_default()
            .clone();
        let copied = self.task_list.copied_logs.contains(id);
        let mono_font = cx.theme().mono_font_family.clone();
        let lines: Vec<&str> = text.lines().collect();
        let line_count = lines.len().max(1);
        let gutter_digits = line_count.to_string().len().max(2);
        let gutter_width = rem((gutter_digits as f32) * 8. + 8.);
        let task_id = id.to_owned();
        let copy_text = text.clone();

        let header = h_flex()
            .w_full()
            .min_w_0()
            .px(rem(12.))
            .py(rem(6.))
            .gap(rem(8.))
            .items_center()
            .justify_between()
            .bg(color(ColorRole::SURFACE_CONTAINER))
            .border_b_1()
            .border_color(color(ColorRole::OUTLINE_VARIANT))
            .child(
                h_flex()
                    .min_w_0()
                    .gap(rem(8.))
                    .items_center()
                    .child(
                        Icon::from(symbols::terminal())
                            .size(ICON_SLOT)
                            .color(color(ColorRole::ON_SURFACE_VARIANT)),
                    )
                    .child(
                        accessible_text(
                            SharedString::from(format!("task-log-title-{id}")),
                            course2md::tr!("运行日志"),
                        )
                        .text_size(TEXT_AUX)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color(ColorRole::ON_SURFACE)),
                    )
                    .child(
                        accessible_text(
                            SharedString::from(format!("task-log-count-{id}")),
                            course2md::tr!("{} 行", line_count),
                        )
                        .text_size(TEXT_AUX)
                        .text_color(color(ColorRole::ON_SURFACE_VARIANT)),
                    ),
            )
            .child(
                material_button(
                    SharedString::from(format!("task-log-copy-{id}")),
                    ButtonVariant::Text,
                )
                .tone(if copied {
                    ButtonTone::Accent
                } else {
                    ButtonTone::Neutral
                })
                .icon(if copied {
                    symbols::check()
                } else {
                    symbols::content_copy()
                })
                .child(if copied {
                    course2md::tr!("已复制")
                } else {
                    course2md::tr!("复制日志")
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy_task_log(task_id.clone(), copy_text.clone(), cx);
                })),
            );

        let rendered_lines = lines.into_iter().enumerate().map(|(index, raw_line)| {
            let line_no = index + 1;
            let tone = log_line_color(raw_line);
            let display_line = preserve_leading_indent(raw_line);
            h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap(rem(10.))
                .child(
                    div()
                        .w(gutter_width)
                        .flex_shrink_0()
                        .text_right()
                        .font_family(mono_font.clone())
                        .text_size(TEXT_AUX)
                        .line_height(rems(1.45))
                        .text_color(color(ColorRole::OUTLINE))
                        .child(line_no.to_string()),
                )
                .child(
                    accessible_text(
                        SharedString::from(format!("task-log-line-{id}-{line_no}")),
                        display_line,
                    )
                    .flex_1()
                    .min_w_0()
                    .font_family(mono_font.clone())
                    .text_size(TEXT_AUX)
                    .line_height(rems(1.45))
                    .text_color(color(tone))
                    .whitespace_normal(),
                )
        });

        let viewport = div()
            .relative()
            .w_full()
            .min_w_0()
            .child(
                div()
                    .id(SharedString::from(format!("task-log-{id}")))
                    .w_full()
                    .min_w_0()
                    .max_h(rem(260.))
                    .track_scroll(&scroll)
                    .overflow_y_scroll()
                    .px(rem(12.))
                    .py(rem(10.))
                    .child(
                        v_flex()
                            .id(SharedString::from(format!("task-log-text-{id}")))
                            .w_full()
                            .min_w_0()
                            .gap(rem(2.))
                            .children(rendered_lines),
                    ),
            )
            .child(Scrollbar::vertical(&scroll));

        v_flex()
            .w_full()
            .min_w_0()
            .rounded(RADIUS_LARGE)
            .border_1()
            .border_color(color(ColorRole::OUTLINE_VARIANT))
            .bg(color(ColorRole::INSET))
            .overflow_hidden()
            .child(header)
            .child(viewport)
            .into_any_element()
    }

    /// Where a task saves, as its supporting line.
    fn task_destination(&self, task: &TaskRecord, cx: &App) -> String {
        match self.task_library(task, cx) {
            Some(location) if task.note_dir.is_some() => {
                course2md::tr!("保存在「{}」", location.label())
            }
            Some(location) => course2md::tr!("将保存到「{}」", location.label()),
            None => course2md::tr!("保存位置暂时不可用").to_owned(),
        }
    }

    /// Destination details shown when a task row or card expands its details.
    pub(crate) fn task_detail_rows(&self, task: &TaskRecord, cx: &App) -> Vec<Div> {
        let library = self
            .workspace(cx)
            .and_then(|workspace| workspace.state.library(&task.library_id));
        let folder = task
            .target_folder
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
        let mut rows = vec![(
            format!("task-destination-{id}"),
            symbols::folder_open(),
            course2md::tr!("保存到"),
            destination,
        )];
        if !task.source.input.trim().is_empty() {
            rows.push((
                format!("task-source-{id}"),
                if task.source.online {
                    symbols::open_in_new()
                } else {
                    symbols::movie()
                },
                if task.source.online {
                    course2md::tr!("视频链接")
                } else {
                    course2md::tr!("本地视频")
                },
                task.source.input.clone(),
            ));
        }
        let scale = TileScale::Compact;
        rows.into_iter()
            .map(|(id, icon, label, value)| {
                scale
                    .row()
                    .child(Icon::from(icon).color(color(ColorRole::ON_SURFACE_VARIANT)))
                    .child(scale.text(ElementId::Name(id.into()), label, Some(value.into())))
            })
            .collect()
    }

    fn task_cover(&self, task: &TaskRecord, cx: &App) -> Option<PathBuf> {
        task.source.cover.clone().or_else(|| {
            self.library(cx)
                .cover_by_source
                .get(&task.source_id)
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
        let on_home_running = matches!(place, CardPlace::Home) && runs_in_card(task);
        let generating = task.status == TaskStatus::Running;
        let named_by_group = matches!(place, CardPlace::Open { .. })
            && (generating || task_status_label(task) == task_group(task).label());
        let chip = (!on_home_running
            && !named_by_group
            && (task.status != TaskStatus::Queued || !matches!(place, CardPlace::Row { .. })))
        .then(|| status_chip(task_status_kind(task)).child(task_status_label(task)));
        let context_menu = self.task_menu(task, cx);
        let in_column = on_home_running && !needs_person(task);
        let inner = in_column.then(|| self.task_body(task, place, window, cx));
        let (outer, head, body) = match place {
            CardPlace::Row { outer, number } => {
                let corners = if expanded {
                    [outer[0], outer[1], false, false]
                } else {
                    outer
                };
                let head = self.task_row_head(task, number, corners, chip, more, cx);
                (outer, head.into_any_element(), expanded)
            }
            CardPlace::Open { outer } => {
                let stages = self.task_stage_rows(task, cx);
                let chips = stages.len() > 1
                    && (stages.iter().any(|stage| stage.status.running())
                        || self.is_running_task(&task.id, cx));
                let facts = (
                    self.task_destination(task, cx),
                    self.task_cover(task, cx),
                    COVER_WIDTH,
                    (!chips && task.status == TaskStatus::Running)
                        .then(|| self.task_step_line(task, false, cx))
                        .flatten(),
                );
                let head = self.task_card_head(task, place, chip, more, None, facts);
                (outer, head.into_any_element(), true)
            }
            CardPlace::Home => {
                let facts = (
                    self.task_destination(task, cx),
                    self.task_cover(task, cx),
                    import_ui::cover_width(window),
                    (task.status == TaskStatus::Running)
                        .then(|| self.task_step_line(task, true, cx))
                        .flatten(),
                );
                let head = self.task_card_head(task, place, chip, more, inner, facts);
                ([true; 4], head.into_any_element(), !in_column)
            }
        };
        let body = body.then(|| self.task_body(task, place, window, cx));
        let footer = self.home_footer.take();
        let menu_focus = window
            .use_keyed_state(
                SharedString::from(format!("task-menu-focus-{id}")),
                cx,
                |_, cx| cx.focus_handle(),
            )
            .read(cx)
            .clone();
        tile(outer)
            .map(|card| match place {
                CardPlace::Home => card.rounded(RADIUS_HERO),
                CardPlace::Open { .. } if outer == [true; 4] => card.rounded(RADIUS_HERO),
                _ => card,
            })
            .id(SharedString::from(format!("task-menu-{id}")))
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .track_focus(&menu_focus)
            .child(
                MaterialContextMenu::new(
                    SharedString::from(format!("task-context-{id}")),
                    context_menu,
                )
                .trigger(&menu_focus),
            )
            .map(|card| match place {
                CardPlace::Row { .. } => card
                    .child(head)
                    .children(body.map(|body| div().px(rem(16.)).pb(rem(16.)).child(body))),
                CardPlace::Open { .. } => card.p(rem(20.)).gap(rem(16.)).child(head).children(body),
                _ => card
                    .p(rem(16.))
                    .gap(rem(16.))
                    .child(head)
                    .children(body)
                    .children(footer),
            })
            .into_any_element()
    }

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
        let waiting = number.map(|_| task.status);
        let scale = TileScale::Regular;
        let cover = crate::course_library::row_cover(
            self.task_cover(task, cx),
            match waiting {
                Some(TaskStatus::Paused) => symbols::pause(),
                Some(_) => symbols::schedule(),
                None => symbols::movie(),
            },
        );
        let display_title = if task.title.trim().is_empty() {
            task.source.title.clone()
        } else {
            task.title.clone()
        };
        let row = scale
            .button(SharedString::from(format!("select-{id}")), corners)
            .accessibility_label(display_title.clone())
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| this.toggle_task_row(&id, cx)
            }))
            .child(
                scale
                    .row()
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
                                        display_title.clone(),
                                    )
                                    .whitespace_normal()
                                    .line_clamp(2)
                                    .text_ellipsis(),
                            )
                            .child(scale.support(
                                SharedString::from(format!("task-row-place-{id}")),
                                self.task_destination(task, cx),
                            ))
                            .children(task_kept_edits(task).map(|text| {
                                scale.support(
                                    SharedString::from(format!("task-row-kept-edits-{id}")),
                                    text,
                                )
                            })),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .ml_auto()
                            .gap(rem(8.))
                            .items_center()
                            .children(chip)
                            .when(
                                task.note_dir.is_some() && task_group(task) != TaskGroup::Queued,
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
                                                TaskAction::OpenNote,
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
                    title: display_title.into(),
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
        (destination, cover, cover_width, step): (
            String,
            Option<PathBuf>,
            Rems,
            Option<(String, bool)>,
        ),
    ) -> Div {
        let id = &task.id;
        let home = matches!(place, CardPlace::Home);
        let title = if task.title.trim().is_empty() {
            task.source.title.clone()
        } else {
            task.title.clone()
        };
        let support = if home {
            desktop_tasks::import::source_metadata(&task.source)
        } else {
            destination
        };
        let (title_chip, line_chip) = if home || needs_person(task) {
            (None, chip)
        } else {
            (chip, None)
        };
        let duration_secs = if task.duration.is_finite() && task.duration > 0. {
            Some(task.duration)
        } else if task.source.duration.is_finite() && task.source.duration > 0. {
            Some(task.source.duration)
        } else {
            None
        };
        h_flex()
            .w_full()
            .min_w_0()
            .items_start()
            .flex_wrap()
            .gap(rem(16.))
            .child({
                let duration = duration_secs.map(course2md::render::fmt_ts);
                video_cover(cover, cover_width).when_some(duration, |cover, duration| {
                    cover.child(
                        media_label(format!("task-duration-{id}"), duration)
                            .absolute()
                            .right(rem(8.))
                            .bottom(rem(8.)),
                    )
                })
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
                            .child(video_title(format!("task-title-{id}"), title).flex_1())
                            .children(title_chip)
                            .child(more),
                    )
                    .when(home, |column| {
                        column.when(!support.is_empty(), |column| {
                            column
                                .child(task_support_line(id, support.clone()).text_size(TEXT_BODY))
                        })
                    })
                    .when_some(step, |column, (step, side_by_side)| {
                        let line =
                            accessible_text(SharedString::from(format!("task-step-{id}")), step)
                                .role(Role::Status)
                                .text_size(TEXT_BODY)
                                .text_color(color(ColorRole::ON_SURFACE_VARIANT));
                        column.child(if side_by_side {
                            line.w_full().min_w_0().truncate()
                        } else {
                            line.whitespace_normal()
                        })
                    })
                    .when(!home && !support.is_empty(), |column| {
                        column.child(task_support_line(id, support))
                    })
                    .child(h_flex().pt(rem(4.)).children(line_chip))
                    .children(inner),
            )
    }

    fn task_step_line(&self, task: &TaskRecord, home: bool, cx: &App) -> Option<(String, bool)> {
        let stages = self.task_stage_rows(task, cx);
        let running: Vec<String> = stages
            .iter()
            .enumerate()
            .filter(|(_, stage)| stage.status.running())
            .map(|(index, stage)| {
                if !home {
                    course2md::tr!("第 {} 步 · {}", index + 1, stage.title)
                } else if stage.detail.is_empty() {
                    stage.title.clone()
                } else {
                    format!("{} · {}", stage.title, stage.detail)
                }
            })
            .collect();
        (!running.is_empty())
            .then(|| (running.join(course2md::tr!("；")), running.len() > 1))
            .or_else(|| {
                self.task_wait(task, cx)
                    .map(|wait| (wait.label().to_owned(), false))
            })
    }

    fn task_wait(&self, task: &TaskRecord, cx: &App) -> Option<WorkerWait> {
        worker_wait_state(
            self.is_running_task(&task.id, cx),
            task.events.done.is_some(),
            task.events
                .stages
                .iter()
                .map(|(name, item)| (name, item.done)),
        )
    }

    fn task_progress(
        &self,
        task: &TaskRecord,
        stages: &[TaskStage],
        surface: TaskSurface,
        cx: &App,
    ) -> Option<Div> {
        let id = &task.id;
        let active = self.is_running_task(id, cx);
        let running: Vec<&TaskStage> = stages
            .iter()
            .filter(|stage| stage.status.running())
            .collect();
        let current = running.first().copied();
        if !active || (current.is_none() && self.task_wait(task, cx).is_none()) {
            return None;
        }
        let elapsed = || {
            self.work
                .read(cx)
                .job_started
                .filter(|_| self.work.read(cx).job.is_some())
                .map(|started| activity::elapsed_label(started.elapsed()))
        };
        let side_by_side = running.len() > 1;
        let lines = measure_lines(&running, surface == TaskSurface::Home, elapsed);
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
                    .value(current.and_then(|stage| stage.fraction)),
                )
                .children(lines.iter().enumerate().map(|(index, line)| {
                    let key = if index == 0 {
                        format!("task-measure-{id}")
                    } else {
                        format!("task-measure-{id}-{index}")
                    };
                    let key = SharedString::from(key);
                    if side_by_side {
                        div().w_full().min_w_0().child(
                            accessible_text(key, line.clone())
                                .w_full()
                                .min_w_0()
                                .truncate(),
                        )
                    } else {
                        ui::status_line(key, line)
                    }
                    .text_size(TEXT_AUX)
                    .text_color(color(ColorRole::ON_SURFACE_VARIANT))
                })),
        )
    }

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
        actions
            .retain(|action| !row || !matches!(action, TaskAction::OpenNote | TaskAction::Details));
        if task.status == TaskStatus::Failed
            && task_log_text(task).is_some()
            && !actions.contains(&TaskAction::Log)
        {
            actions.push(TaskAction::Log);
        }

        // On `Failed` (`Attention`) cards, `[重试]` is the primary action (`Filled`);
        // on other cards `lead_action` chooses the single primary button.
        let primary_action =
            if task.status == TaskStatus::Failed && actions.contains(&TaskAction::Retry) {
                Some(TaskAction::Retry)
            } else {
                lead_action(&actions, task, surface).cloned()
            };

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
        let running = stages.iter().any(|stage| stage.status.running())
            || self.is_running_task(&task.id, cx) && !stages.is_empty();
        let progress = self.task_progress(task, &stages, surface, cx);
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

        // One clean `notice_banner` on `Failed` (`Attention`) cards:
        // `NoticeKind::Warning` when `task.note_dir.is_some()`, `NoticeKind::Danger` when `None`.
        let notice = (!row && task.status == TaskStatus::Failed).then(|| {
            let reasons = task_reasons(task);
            let message = if reasons.is_empty() {
                task_attention_summary(task)
            } else {
                reasons.join("\n")
            };
            notice_banner(
                SharedString::from(format!("task-reason-{id}")),
                if task.note_dir.is_some() {
                    NoticeKind::Warning
                } else {
                    NoticeKind::Danger
                },
                message,
            )
            .rounded(RADIUS_LARGE)
        });

        let show_failed_stages_inline =
            !row && task.status == TaskStatus::Failed && !stages.is_empty();
        let failed_stage_rows = show_failed_stages_inline.then(|| {
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(10.))
                .children(stages.into_iter().map(|stage| stage_row(stage, window, cx)))
        });

        let buttons = actions.iter().map(|action| {
            let (label, icon, disabled) = self.present(action, task, cx);
            let variant = if primary_action.as_ref() == Some(action) {
                ButtonVariant::Filled
            } else {
                match action {
                    TaskAction::Pause | TaskAction::PrepareModel => ButtonVariant::Tonal,
                    TaskAction::Details
                    | TaskAction::Log
                    | TaskAction::Cancel
                    | TaskAction::Remove
                    | TaskAction::ViewTask => ButtonVariant::Text,
                    _ => ButtonVariant::Outlined,
                }
            };
            material_button(SharedString::from(format!("task-{label}-{id}")), variant)
                .when(variant == ButtonVariant::Outlined, |b| {
                    b.tone(ButtonTone::Accent)
                })
                .when(
                    matches!(
                        action,
                        TaskAction::Cancel
                            | TaskAction::Remove
                            | TaskAction::Details
                            | TaskAction::Log
                    ),
                    |b| b.tone(ButtonTone::Neutral),
                )
                .icon(icon)
                .child(label)
                .disabled(disabled)
                .on_click(action_click(cx.weak_entity(), id.clone(), action.clone()))
        });
        let mut buttons: Vec<Option<MaterialButton>> = buttons.map(Some).collect();
        let home_running = surface == TaskSurface::Home && runs_in_card(task);
        let leading_cancel = home_running
            .then(|| {
                actions
                    .iter()
                    .position(|action| *action == TaskAction::Cancel)
            })
            .flatten()
            .and_then(|index| buttons[index].take());
        let buttons: Vec<MaterialButton> = buttons.into_iter().flatten().collect();
        let buttons = h_flex()
            .w_full()
            .min_w_0()
            .gap(rem(8.))
            .flex_wrap()
            .justify_end()
            .children(leading_cancel)
            .children(buttons);

        let details = (self.task_list.expanded.as_ref() == Some(&id)).then(|| {
            let content = v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(12.))
                .child(MaterialDivider::new(SharedString::from(format!(
                    "task-detail-divider-{id}"
                ))))
                .children(self.task_detail_rows(task, cx))
                .when(!show_failed_stages_inline, |view| {
                    view.children(
                        self.task_stage_rows(task, cx)
                            .into_iter()
                            .map(|stage| stage_row(stage, window, cx)),
                    )
                });
            let key = format!("task-detail-{id}");
            if self.enter_once(key.clone()) {
                motion::enter(SharedString::from(key), content)
            } else {
                content.into_any_element()
            }
        });
        let log = task_log_text(task)
            .filter(|_| self.task_list.logs_open.contains(&id))
            .map(|text| self.task_log_panel(&id, text, cx));

        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(12.))
            .children(notice)
            .children(
                consequence
                    .map(|text| ("consequence", text))
                    .into_iter()
                    .chain(
                        task_kept_edits(task)
                            .filter(|_| !row)
                            .map(|text| ("kept-edits", text)),
                    )
                    .map(|(kind, text)| {
                        accessible_text(SharedString::from(format!("task-{kind}-{id}")), text)
                            .text_size(TEXT_AUX)
                            .text_color(color(ColorRole::ON_SURFACE_VARIANT))
                            .whitespace_normal()
                    }),
            )
            .children(self.facts.models.get(&id).map(|preparation| {
                let key = SharedString::from(format!("task-model-{id}"));
                match preparation {
                    ModelPreparation::Preparing => {
                        ui::status_line(key, course2md::tr!("正在准备识别模型"))
                            .text_size(TEXT_AUX)
                            .into_any_element()
                    }
                    ModelPreparation::Failed(message) => error_text(key, message.clone())
                        .text_size(TEXT_BODY)
                        .whitespace_normal()
                        .into_any_element(),
                }
            }))
            .children(failed_stage_rows)
            .children(progress)
            .children(chips)
            .when(
                actions.contains(&TaskAction::PrepareModel) && self.work.read(cx).job.is_some(),
                |body| {
                    body.child(
                        accessible_text(
                            SharedString::from(format!("task-model-wait-{id}")),
                            course2md::tr!("其他任务结束后才能下载模型"),
                        )
                        .text_size(TEXT_AUX)
                        .text_color(color(ColorRole::ON_SURFACE_VARIANT))
                        .whitespace_normal(),
                    )
                },
            )
            .map(|body| {
                if home_running {
                    self.home_footer = Some(buttons);
                    body
                } else {
                    body.child(buttons)
                }
            })
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
        let details = self.task_list.expanded.as_ref() == Some(&task.id);
        let log = self.task_list.logs_open.contains(&task.id);
        use course2md::tr;
        match action {
            TaskAction::OpenNote => (tr!("阅读笔记"), symbols::menu_book(), false),
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
            TaskAction::Move { label, up, to } => (
                label,
                if *up {
                    symbols::arrow_upward()
                } else {
                    symbols::arrow_downward()
                },
                to.is_none(),
            ),
            TaskAction::Pause => (tr!("暂停"), symbols::pause(), false),
            TaskAction::Resume => (tr!("继续生成"), symbols::play_arrow(), false),
            TaskAction::Retry => (task_retry_action_label(task), symbols::refresh(), false),
            TaskAction::Regenerate => (tr!("重新生成"), symbols::restart_alt(), false),
            TaskAction::PrepareModel => (
                tr!("下载并准备模型"),
                symbols::download(),
                self.work.read(cx).job.is_some(),
            ),
            TaskAction::OpenSettings => (tr!("打开设置"), symbols::settings(), false),
            TaskAction::Reassociate(_) => (
                tr!("重新关联此保存位置"),
                symbols::database(),
                self.facts.storage_busy,
            ),
            TaskAction::Cancel => (tr!("取消任务"), symbols::close(), false),
            TaskAction::Remove => (tr!("移除任务记录"), symbols::delete(), false),
            TaskAction::ViewTask => (tr!("查看任务"), symbols::task(), false),
        }
    }
}

pub(crate) fn task_status_kind(task: &TaskRecord) -> StatusKind {
    match task.status {
        TaskStatus::Completed => StatusKind::Success,
        TaskStatus::Running => StatusKind::Progress,
        TaskStatus::Failed if task.note_dir.is_some() => StatusKind::Warning,
        TaskStatus::Failed => StatusKind::Danger,
        TaskStatus::Queued | TaskStatus::Paused => StatusKind::Neutral,
    }
}

impl StageStatusView for StageStatus {
    fn role(self) -> ColorRole {
        match self {
            Self::Running => ColorRole::PRIMARY,
            Self::Complete => ColorRole::SUCCESS,
            Self::Failed => ColorRole::ERROR,
            _ => ColorRole::ON_SURFACE_VARIANT,
        }
    }

    fn chip(self) -> StatusKind {
        match self {
            Self::Complete => StatusKind::Success,
            Self::Failed => StatusKind::Danger,
            _ if self.running() => StatusKind::Progress,
            _ => StatusKind::Neutral,
        }
    }

    fn icon(self) -> SymbolIcon {
        match self {
            Self::Running => symbols::refresh(),
            Self::Complete => symbols::check_circle(),
            Self::Paused => symbols::pause(),
            Self::Failed => symbols::warning(),
            Self::Waiting => symbols::schedule(),
        }
    }
}

fn log_line_color(line: &str) -> ColorRole {
    let trimmed = line.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("error:")
        || lower.starts_with("[error]")
        || lower.starts_with("fatal:")
        || lower.starts_with("traceback")
        || lower.contains(" error: ")
        || trimmed.starts_with("错误")
        || trimmed.starts_with("失败")
    {
        ColorRole::ERROR
    } else if lower.starts_with("warning:")
        || lower.starts_with("warn:")
        || lower.starts_with("[warn")
        || lower.contains(" warning: ")
        || trimmed.starts_with("警告")
    {
        ColorRole::WARNING
    } else {
        ColorRole::ON_SURFACE
    }
}

fn preserve_leading_indent(line: &str) -> String {
    if line.is_empty() {
        return "\u{00A0}".into();
    }
    let mut prefix = String::new();
    let mut rest_start = 0;
    for (idx, ch) in line.char_indices() {
        match ch {
            ' ' => {
                prefix.push('\u{00A0}');
                rest_start = idx + 1;
            }
            '\t' => {
                prefix.push_str("\u{00A0}\u{00A0}\u{00A0}\u{00A0}");
                rest_start = idx + 1;
            }
            _ => break,
        }
    }
    if prefix.is_empty() {
        line.to_owned()
    } else {
        prefix.push_str(&line[rest_start..]);
        prefix
    }
}

/// The lines under a running task's bar. Steps that run side by side each say
/// where they are, by name, on one line (drawn without wrapping), so the card
/// keeps its height when a second step starts; with no measure yet, how long the
/// job has run. On the workbench the line above the bar already says the steps.
fn measure_lines(
    running: &[&TaskStage],
    home: bool,
    elapsed: impl FnOnce() -> Option<String>,
) -> Vec<String> {
    let current = running.first();
    let line = if running.len() > 1 {
        let measures = running
            .iter()
            .filter(|stage| !stage.detail.is_empty())
            .map(|stage| format!("{} · {}", stage.title, stage.detail))
            .collect::<Vec<_>>()
            .join(course2md::tr!("；"));
        Some(measures)
            .filter(|measures| !measures.is_empty())
            .or_else(elapsed)
            .filter(|_| !home)
    } else {
        current
            .map(|stage| stage.detail.clone())
            .filter(|detail| !detail.is_empty())
            .or_else(elapsed)
            .filter(|_| !home || current.is_none())
    };
    line.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::{QueueItem, TaskGroup};

    #[test]
    fn steps_side_by_side_share_one_line_and_fall_back_to_the_clock() {
        use desktop_tasks::task_view::TaskStage;
        let stage = |title: &str, detail: &str| TaskStage {
            id: title.into(),
            title: title.into(),
            status: desktop_tasks::task_view::StageStatus::Running,
            detail: detail.into(),
            compact_detail: String::new(),
            fraction: None,
        };
        let clock = || Some("12s".to_owned());
        let (scan, speech) = (stage("Scan", "1,234 frames"), stage("Speech", "3 / 9"));
        assert_eq!(
            super::measure_lines(&[&scan, &speech], false, clock),
            [format!(
                "Scan · 1,234 frames{}Speech · 3 / 9",
                course2md::tr!("；")
            )]
        );
        // Neither measures itself yet: the clock, not nothing.
        let (scan, speech) = (stage("Scan", ""), stage("Speech", ""));
        assert_eq!(
            super::measure_lines(&[&scan, &speech], false, clock),
            ["12s"]
        );
        // The workbench says the steps above the bar.
        assert!(super::measure_lines(&[&scan, &speech], true, clock).is_empty());
        assert_eq!(super::measure_lines(&[&scan], false, clock), ["12s"]);
    }

    #[gpui::test]
    fn queue_item_keys_track_identity_not_content(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        cx.update(|cx| {
            let tasks = vec![task_record("task-a"), task_record("task-b")];
            let open = cx.new(|_| None);
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
            let tasks = vec![task_record("task-a"), task_record("task-b")];
            let again: Vec<String> = items.iter().map(|item| item.key(&tasks)).collect();
            assert_eq!(keys, again);
            let mut moved = tasks;
            moved.swap(0, 1);
            assert_eq!(row(0).key(&moved), "task-task-b");
        });
    }

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
            library_id: "library".into(),
            course_id: format!("course-{id}"),
            source_id: "online:video".into(),
            source: source("https://example.test/video", "课程"),
            title: "课程".into(),
            title_from_probe: false,
            author: String::new(),
            duration: 0.,
            subtitle: None,
            subtitle_events: None,
            target_folder: None,
            note_dir: None,
            work_dir: "work".into(),
            legacy_work_dir: None,
            status: crate::workspace::TaskStatus::Paused,
            run_mode: crate::workspace::RunMode::Resume,
            stages: Default::default(),
            error: None,
            logs: Vec::new(),
            unread: false,
            created: 0,
            updated: 0,
            outcomes: None,
            kept_video: false,
            kept_edits: Vec::new(),
            events: crate::activity::JobEvents::default(),
        }
    }

    #[test]
    fn log_panel_helpers_classify_severity_and_preserve_indentation() {
        assert_eq!(
            super::log_line_color("ERROR: Unable to download"),
            super::ColorRole::ERROR
        );
        assert_eq!(
            super::log_line_color("WARNING: [youtube] Retrying"),
            super::ColorRole::WARNING
        );
        assert_eq!(
            super::log_line_color("[download] 100% of 12.4MiB"),
            super::ColorRole::ON_SURFACE
        );
        assert_eq!(
            super::preserve_leading_indent("  frame=42"),
            "\u{00A0}\u{00A0}frame=42"
        );
        assert_eq!(super::preserve_leading_indent(""), "\u{00A0}");
    }
}
