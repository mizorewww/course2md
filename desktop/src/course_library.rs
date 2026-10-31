//! Course organization and presentation are independent, persistent choices.
use super::*;
use crate::theme::*;
use gpui_kit::component::{
    button::*,
    menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
};
use std::rc::Rc;

/// Section headings on the workbench ("正在处理", "最近的笔记").
const SECTION_TITLE: Rems = rems(22. / 14.);
const SECTION_TITLE_LINE: Rems = rems(28. / 14.);
/// Title of a workbench row.
const ROW_TITLE: Rems = rems(16. / 14.);
/// Cover column of workbench rows; narrow windows shrink it to a share of the row.
const ROW_COVER: Rems = rems(128. / 14.);
const ROW_COVER_SHARE: f32 = 0.28;

/// Hover tone of a note or task row on the surface: an on-surface overlay,
/// so dark palettes lighten the row as light ones darken it.
fn row_state_layer() -> Rgba {
    blend(color(SURFACE), color(INK), 0.05)
}

/// A row's main target when the row itself draws the state layer: no fill
/// of its own, so hover and press never paint a second, smaller shape.
fn row_target(button: Button, cx: &App) -> Button {
    let clear = Rgba {
        a: 0.,
        ..color(SURFACE)
    };
    button.custom(
        ButtonCustomVariant::new(cx)
            .color(clear.into())
            .foreground(color(INK).into())
            .hover(clear.into())
            .active(clear.into())
            .shadow(false),
    )
}

/// A whole row as one target on the surface, with the row state layer.
fn surface_row(button: Button, cx: &App) -> Button {
    let clear = Rgba {
        a: 0.,
        ..color(SURFACE)
    };
    button.custom(
        ButtonCustomVariant::new(cx)
            .color(clear.into())
            .foreground(color(INK).into())
            .hover(row_state_layer().into())
            .active(blend(color(SURFACE), color(INK), 0.08).into())
            .shadow(false),
    )
}

/// The cover slot of a workbench row.
fn row_cover() -> Div {
    div()
        .w(ROW_COVER)
        .max_w(relative(ROW_COVER_SHARE))
        .flex_shrink_0()
}

/// Hairline between workbench rows, inset past the cover column.
fn inset_divider() -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .pl_4()
        .gap_4()
        .child(row_cover())
        .child(div().flex_1().min_w_0().h(px(1.)).bg(color(HAIRLINE)))
}

/// A note picked up to file into a folder of the tree.
fn note_drag(course: &Course) -> crate::folder_tree::NoteDrag {
    crate::folder_tree::NoteDrag {
        storage: course.storage_dir(),
        title: course.title.clone().into(),
    }
}

fn incomplete_badge() -> Div {
    h_flex().child(badge(BadgeKind::Warning).child(course2md::tr!("部分内容待补全")))
}

/// Geometry shared by list and card layouts; rem-based spacing scales with text.
#[derive(Clone, Copy)]
struct LibraryLayout {
    columns: usize,
    /// One grid column's width in pixels: padding would otherwise make a
    /// short final row's cards wider than the full rows above it.
    card_width: f32,
    stacked: bool,
}

/// One row of the virtualized library page. Grid rows carry their courses for
/// the current column count; folder groups stay one item per disclosure so the
/// expand motion is unchanged.
enum LibraryItem {
    Checking,
    Recovery(crate::storage::LibraryAccess),
    Coverage(String),
    Issues(Vec<String>),
    Loading,
    Empty,
    /// The folders inside the selected library top or folder.
    Folders(std::path::PathBuf, Option<u64>),
    GroupHeader(Box<LibraryGroupHeader>),
    GroupBody(Box<LibraryGroupBody>),
    CardRow(LibraryRow),
    ListRow(LibraryRow),
}

/// Notes as indices into `Desktop::courses`; the key is fixed when the row is
/// built from them.
struct LibraryRow {
    key: String,
    courses: Vec<usize>,
    /// A list row's place in the one list surface: (first, last).
    edges: (bool, bool),
}

impl LibraryRow {
    fn cards(all: &[Rc<Course>], courses: &[usize], columns: usize) -> Vec<Self> {
        courses
            .chunks(columns)
            .map(|row| Self {
                key: format!("row-{columns}-{}-{}", row.len(), all[row[0]].dir.display()),
                courses: row.to_vec(),
                edges: (false, false),
            })
            .collect()
    }

    /// The surface's rounded ends change a row's height, so they are part
    /// of its identity.
    fn list(all: &[Rc<Course>], index: usize, first: bool, last: bool) -> Self {
        Self {
            key: format!("card-{}-{first}-{last}", all[index].dir.display()),
            courses: vec![index],
            edges: (first, last),
        }
    }
}

/// A folder group's header row.
struct LibraryGroupHeader {
    index: usize,
    key: String,
    name: String,
    count: usize,
    collapsed: bool,
}

/// A folder group's disclosure body: its card or list rows.
struct LibraryGroupBody {
    index: usize,
    key: String,
    collapsed: bool,
    courses: Vec<usize>,
}

impl LibraryItem {
    /// Identity for the list splice: stable while the row means the same
    /// content, so measured heights and the scroll anchor survive updates.
    fn key(&self) -> String {
        match self {
            LibraryItem::Checking => "checking".to_owned(),
            LibraryItem::Recovery(_) => "recovery".to_owned(),
            LibraryItem::Coverage(_) => "coverage".to_owned(),
            LibraryItem::Issues(_) => "issues".to_owned(),
            LibraryItem::Loading => "loading".to_owned(),
            LibraryItem::Empty => "empty".to_owned(),
            LibraryItem::Folders(root, parent) => {
                format!("folders-{}-{}", root.display(), parent.unwrap_or(0))
            }
            LibraryItem::GroupHeader(header) => format!("group-h-{}", header.key),
            LibraryItem::GroupBody(body) => format!("group-b-{}", body.key),
            LibraryItem::CardRow(row) | LibraryItem::ListRow(row) => row.key.clone(),
        }
    }
}

/// Per-row metadata for the library list's render closure.
struct LibraryRowContext<'a> {
    index: usize,
    last: bool,
    focus: Option<&'a FocusHandle>,
    layout: LibraryLayout,
}

/// A collapsed list of technical details under a short message.
#[derive(gpui::IntoElement)]
pub(crate) struct IssueDetails {
    id: SharedString,
    messages: Vec<String>,
}

impl IssueDetails {
    pub(crate) fn new(id: impl Into<SharedString>, messages: Vec<String>) -> Self {
        Self {
            id: id.into(),
            messages,
        }
    }
}

impl RenderOnce for IssueDetails {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(
            SharedString::from(format!("library-diagnostics-state:{}", self.id)),
            cx,
            |_, _| false,
        );
        let open = *state.read(cx);
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(
                quiet(SharedString::from(format!(
                    "library-diagnostics-toggle:{}",
                    self.id
                )))
                .self_start()
                .icon(if open {
                    crate::icons::chevron_up()
                } else {
                    crate::icons::info()
                })
                .label(if open {
                    course2md::tr!("收起详情")
                } else {
                    course2md::tr!("查看详情")
                })
                .on_click(move |_, _, cx| {
                    state.update(cx, |open, cx| {
                        *open = !*open;
                        cx.notify();
                    });
                }),
            )
            .child(crate::motion::disclosure(
                SharedString::from(format!("library-diagnostics-content:{}", self.id)),
                open,
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .p_3()
                    .bg(color(INSET))
                    .rounded(RADIUS_SMALL)
                    .children(
                        self.messages
                            .into_iter()
                            .enumerate()
                            .map(|(index, message)| {
                                accessible_text(
                                    SharedString::from(format!(
                                        "library-diagnostic:{}:{index}",
                                        self.id
                                    )),
                                    message,
                                )
                                .w_full()
                                .min_w_0()
                                .whitespace_normal()
                                .text_size(TEXT_AUX)
                                .font_weight(FontWeight::NORMAL)
                                .text_color(color(GRAY))
                            }),
                    ),
            ))
    }
}

#[derive(Clone)]
pub(super) struct CourseLocation {
    pub id: String,
    pub root: PathBuf,
    pub relative: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LibraryIssueKind {
    Scan,
    Folders,
    Titles,
}

/// A read failure in one registered library; recovery cards claim theirs by (root, kind).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LibraryIssue {
    pub root: PathBuf,
    pub kind: LibraryIssueKind,
    pub message: String,
}

/// Facts about the video a note came from, indexed by task id after each refresh.
#[derive(Clone)]
pub(super) struct NoteSource {
    author: String,
    duration: Option<f64>,
    local: bool,
    /// The video page an online note came from.
    link: Option<String>,
}

/// The pieces of a note's meta line.
struct CourseMeta {
    folder: Option<String>,
    source: Option<NoteSource>,
    date: Option<chrono::NaiveDate>,
}

impl CourseMeta {
    /// The recent-notes row shows the duration on the cover and a short date.
    fn short_line(&self) -> String {
        use chrono::Datelike;
        self.parts(false, |date| {
            crate::i18n::month_day(date.month(), date.day())
        })
    }

    fn parts(&self, duration: bool, date: impl Fn(&chrono::NaiveDate) -> String) -> String {
        let mut parts: Vec<String> = self.folder.iter().cloned().collect();
        if let Some(source) = &self.source {
            if !source.author.trim().is_empty() {
                parts.push(source.author.clone());
            }
            if duration && let Some(seconds) = source.duration {
                parts.push(course2md::render::fmt_ts(seconds));
            }
            if source.local {
                parts.push(course2md::tr!("本地视频").into());
            }
        }
        parts.extend(self.date.as_ref().map(date));
        parts.join(" · ")
    }
}

/// Filesystem facts are collected by the library worker and read by the UI.
#[derive(Default)]
pub(super) struct LibraryViewCache {
    pub locations: BTreeMap<PathBuf, CourseLocation>,
    /// Task id → its video, for the notes in the library.
    sources: std::collections::HashMap<String, NoteSource>,
    /// Source identity → indices into `Desktop::courses` of its notes.
    notes_by_source: std::collections::HashMap<String, Vec<usize>>,
    /// Course identity → index into `Desktop::courses` of its newest version.
    newest_by_course: std::collections::HashMap<String, usize>,
    pub recovery: BTreeMap<PathBuf, organize::Recovery>,
    pub title_recovery: BTreeMap<PathBuf, organize::Recovery>,
    aliases: BTreeMap<PathBuf, BTreeMap<PathBuf, String>>,
    alias_issues: Vec<LibraryIssue>,
}

impl LibraryViewCache {
    /// A newer version of the note `manifest` belongs to, when the library has one.
    pub fn newer_version(
        &self,
        courses: &[Rc<Course>],
        manifest: &course2md::artifact::Manifest,
    ) -> Option<Rc<Course>> {
        self.newest_by_course
            .get(&manifest.course_id)
            .and_then(|index| courses.get(*index))
            .filter(|course| {
                course
                    .manifest
                    .as_ref()
                    // Indices are rebuilt after each scan; until then an inserted
                    // note can shift them, so the identity is checked too.
                    .is_some_and(|newest| {
                        newest.course_id == manifest.course_id
                            && newest.revision > manifest.revision
                    })
            })
            .cloned()
    }

    pub fn inspect(
        location: &workspace::LibraryLocation,
        scan: Option<&notes::LibraryScan>,
    ) -> Self {
        let mut cache = Self::default();
        if let Ok(Some(recovery)) = organize::recovery(&location.root) {
            cache.recovery.insert(location.root.clone(), recovery);
        }
        if let Ok(Some(recovery)) = organize::title_recovery(&location.root) {
            cache.title_recovery.insert(location.root.clone(), recovery);
        }
        match organize::title_aliases(&location.root) {
            Ok(names) => {
                cache.aliases.insert(location.root.clone(), names);
            }
            Err(error) => cache.alias_issues.push(LibraryIssue {
                root: location.root.clone(),
                kind: LibraryIssueKind::Titles,
                message: course2md::tr!(
                    "{}的显示名称尚未读取：{}",
                    location.name,
                    format!("{error:#}")
                ),
            }),
        }
        if let Some(scan) = scan {
            for course in &scan.courses {
                let storage = course.storage_dir();
                let Ok(relative) = organize::relative_key(&location.root, &storage) else {
                    continue;
                };
                let membership = CourseLocation {
                    id: location.id.clone(),
                    root: location.root.clone(),
                    relative: relative.clone(),
                };
                // Imported versions can use canonical paths while a registered
                // root uses a symlink or /tmp alias. Resolve once in this worker.
                let mut paths = vec![
                    course.dir.clone(),
                    storage.clone(),
                    location.root.join(relative),
                ];
                paths.extend(storage.canonicalize().ok());
                paths.extend(course.dir.canonicalize().ok());
                for path in paths {
                    cache.locations.insert(path, membership.clone());
                }
            }
        }
        cache
    }

    pub fn merge(&mut self, other: Self) {
        for (path, location) in other.locations {
            let replace = self.locations.get(&path).is_none_or(|previous| {
                location.root.components().count() > previous.root.components().count()
            });
            if replace {
                self.locations.insert(path, location);
            }
        }
        self.recovery.extend(other.recovery);
        self.title_recovery.extend(other.title_recovery);
        self.aliases.extend(other.aliases);
        self.alias_issues.extend(other.alias_issues);
    }
}

struct CourseRenameDialog {
    desktop: Entity<Desktop>,
    input: Entity<InputState>,
    course: Course,
    root: PathBuf,
    error: Option<String>,
    saving: bool,
    _subscription: Subscription,
}

impl CourseRenameDialog {
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.saving = true;
        let name = self.input.read(cx).value().to_string();
        let root = self.root.clone();
        let course = self.course.storage_dir();
        let task = crate::spawn_blocking_io(move || organize::rename_course(&root, &course, &name));
        cx.spawn_in(window, async move |this, cx| {
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::interrupted())));
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        this.desktop
                            .update(cx, |desktop, cx| desktop.refresh_library(cx));
                        window.close_dialog(cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("{error:#}"));
                        this.input.update(cx, |state, cx| state.focus(window, cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for CourseRenameDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("rename-note-dialog")
            .role(Role::Dialog)
            .aria_label(course2md::tr!("重命名笔记"))
            .gap_3()
            .child(accessible_text(
                "rename-note-label",
                course2md::tr!("笔记名称"),
            ))
            .child(text_input(&self.input).aria_label(course2md::tr!("笔记名称")))
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    accessible_text("rename-note-error", error)
                        .role(Role::Alert)
                        .text_color(color(DANGER)),
                )
            })
            .child(
                dialog_actions()
                    .child(
                        text_button("cancel-note-rename", cx)
                            .label(course2md::tr!("取消"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        primary_pill("save-note-rename")
                            .icon(icons::check())
                            .label(course2md::tr!("保存名称"))
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

impl Desktop {
    fn library_issue_messages(
        &self,
        root: &std::path::Path,
        kind: LibraryIssueKind,
    ) -> Vec<String> {
        self.library_issues
            .iter()
            .filter(|issue| issue.kind == kind && issue.root == root)
            .map(|issue| issue.message.clone())
            .collect()
    }

    /// Notes already made from the video with this identity.
    pub(crate) fn notes_for_source(&self, identity: &str) -> impl Iterator<Item = &Course> {
        self.library_view_cache
            .notes_by_source
            .get(identity)
            .into_iter()
            .flatten()
            .filter_map(|index| self.courses.get(*index).map(|course| &**course))
    }

    fn index_note_sources(&mut self) {
        let mut notes_by_source: std::collections::HashMap<String, Vec<usize>> = Default::default();
        for (index, course) in self.courses.iter().enumerate() {
            if let Some(manifest) = &course.manifest {
                notes_by_source
                    .entry(manifest.source_id.clone())
                    .or_default()
                    .push(index);
            }
        }
        self.library_view_cache.notes_by_source = notes_by_source;
        let mut newest: std::collections::HashMap<String, usize> = Default::default();
        for (index, course) in self.courses.iter().enumerate() {
            let Some(manifest) = &course.manifest else {
                continue;
            };
            let current = newest.entry(manifest.course_id.clone()).or_insert(index);
            let revision = |i: usize| self.courses[i].manifest.as_ref().map(|m| m.revision);
            if revision(*current) < Some(manifest.revision) {
                *current = index;
            }
        }
        self.library_view_cache.newest_by_course = newest;
        let wanted: std::collections::HashSet<&str> = self
            .courses
            .iter()
            .filter_map(|course| Some(course.manifest.as_ref()?.task_id.as_str()))
            .collect();
        self.library_view_cache.sources = self
            .workspace
            .as_ref()
            .map(|workspace| {
                workspace
                    .state
                    .tasks
                    .iter()
                    .filter(|task| wanted.contains(task.id.as_str()))
                    .map(|task| {
                        let source = &task.plan.source;
                        (
                            task.id.clone(),
                            NoteSource {
                                author: source.author.clone(),
                                duration: (source.duration.is_finite() && source.duration > 0.)
                                    .then_some(source.duration),
                                local: !source.online,
                                link: source.online.then(|| source.input.clone()),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
    }

    fn note_source(&self, course: &Course) -> Option<&NoteSource> {
        self.library_view_cache
            .sources
            .get(&course.manifest.as_ref()?.task_id)
    }

    pub fn course_display_title(&self, course: &Course) -> String {
        self.courses
            .iter()
            .find(|item| item.storage_dir() == course.storage_dir())
            .map(|item| item.title.clone())
            .unwrap_or_else(|| course.title.clone())
    }

    /// Apply the names already read by the library worker. The UI does no I/O.
    pub fn apply_course_title_aliases(&mut self) {
        self.index_note_sources();
        // 幂等合并：不重复追加同一条诊断（打开笔记路径会再次调用本函数）
        for issue in self.library_view_cache.alias_issues.clone() {
            if !self.library_issues.contains(&issue) {
                self.library_issues.push(issue);
            }
        }
        for course in &mut self.courses {
            if let Some(location) = self.library_view_cache.locations.get(&course.storage_dir())
                && let Some(name) = self
                    .library_view_cache
                    .aliases
                    .get(&location.root)
                    .and_then(|names| names.get(&location.relative))
                && course.title != *name
            {
                Rc::make_mut(course).title = name.clone();
            }
        }
        let title = self
            .preview
            .as_ref()
            .map(|preview| self.course_display_title(&preview.course));
        if let (Some(preview), Some(title)) = (self.preview.as_mut(), title) {
            Rc::make_mut(preview).course.title = title;
        }
    }

    pub fn begin_course_rename(
        &mut self,
        course: Course,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self
            .course_location(&course)
            .map(|library| library.root.clone())
        else {
            self.message = Some(
                course2md::tr!("这份笔记的保存位置暂时无法确认，请在笔记库菜单中重新扫描。").into(),
            );
            cx.notify();
            return;
        };
        let name = self.course_display_title(&course);
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |state, cx| state.set_value(name, window, cx));
        let desktop = cx.entity();
        let focus = input.clone();
        let content = cx.new(|cx| {
            let subscription = cx.subscribe_in(
                &input,
                window,
                |this: &mut CourseRenameDialog, _, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.save(window, cx);
                    }
                    if matches!(event, InputEvent::Change) {
                        this.error = None;
                        cx.notify();
                    }
                },
            );
            CourseRenameDialog {
                desktop,
                input,
                course,
                root,
                error: None,
                saving: false,
                _subscription: subscription,
            }
        });
        window.open_dialog(cx, move |dialog, window, _| {
            dialog
                .material("rename-note-title", course2md::tr!("重命名笔记"))
                .close_button(false)
                .w(crate::theme::dialog_width(
                    crate::theme::DIALOG_WIDTH,
                    window,
                ))
                .overlay_closable(false)
                .child(content.clone())
        });
        window.defer(cx, move |window, cx| {
            focus.update(cx, |state, cx| state.focus(window, cx))
        });
    }

    /// Asks before moving a note to the system trash. A note that a running
    /// task is still writing a version into cannot be deleted yet.
    pub fn confirm_course_delete(
        &mut self,
        course: Course,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((root, library_id)) = self
            .course_location(&course)
            .map(|library| (library.root.clone(), library.id.clone()))
        else {
            self.message = Some(
                course2md::tr!("这份笔记的保存位置暂时无法确认，请在笔记库菜单中重新扫描。").into(),
            );
            cx.notify();
            return;
        };
        let source = course
            .manifest
            .as_ref()
            .map(|manifest| manifest.source_id.as_str());
        let busy = self.workspace.as_ref().is_some_and(|workspace| {
            note_has_unfinished_task(&workspace.state.tasks, &library_id, source)
        });
        if busy {
            self.message =
                Some(course2md::tr!("这份笔记还有未完成的任务，请在任务结束或取消后再删除").into());
            cx.notify();
            return;
        }
        let title = self.course_display_title(&course);
        let trash = organize::trash_name();
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let weak = weak.clone();
            let course = course.clone();
            let root = root.clone();
            confirm_dialog(
                dialog,
                course2md::tr!("删除笔记"),
                course2md::tr!("保留笔记"),
                course2md::tr!("移到{}", trash),
                true,
                cx,
            )
            .child(accessible_text(
                "delete-note-consequence",
                course2md::tr!("《{}》将移到{}，可以从那里恢复。", title, trash),
            ))
            .on_ok(move |_, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.delete_course(root.clone(), course.clone(), cx)
                });
                true
            })
        });
    }

    fn delete_course(&mut self, root: PathBuf, course: Course, cx: &mut Context<Self>) {
        let title = self.course_display_title(&course);
        let dir = course.storage_dir();
        let worker_dir = dir.clone();
        let task = crate::spawn_blocking_io(move || organize::delete_course(&root, &worker_dir));
        cx.spawn(async move |this, cx| {
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::interrupted())));
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        let reading = this
                            .preview
                            .as_ref()
                            .is_some_and(|preview| preview.course.storage_dir() == dir);
                        if reading && this.page == Page::Result {
                            this.navigate(Page::Library, cx);
                        } else {
                            this.refresh_library(cx);
                        }
                        if reading {
                            this.preview = None;
                        }
                        this.message = Some(course2md::tr!(
                            "已将《{}》移到{}",
                            title,
                            organize::trash_name()
                        ));
                    }
                    Err(error) => {
                        this.message = Some(course2md::tr!("笔记尚未删除：{}", format!("{error}")))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn course_actions(
        &self,
        course: &Rc<Course>,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu = self.course_menu(course, false, cx);
        icon_button(("course-actions", index), CONTROL_HEIGHT, false, cx)
            .icon(crate::icons::more_vertical())
            .tooltip(course2md::tr!("笔记操作"))
            .accessibility_label(course2md::tr!("《{}》的笔记操作", course.title))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |popup, window, cx| {
                menu(popup, window, cx)
            })
            .into_any_element()
    }

    /// Every note offers the same commands wherever it appears, ordered
    /// 打开 → 处理 → 管理. A right-click menu also leads with opening the note;
    /// the ⋯ button sits on a row that already opens it.
    pub(crate) fn course_menu(
        &self,
        course: &Rc<Course>,
        with_open: bool,
        cx: &mut Context<Self>,
    ) -> MenuBuilder {
        let entity = cx.entity().downgrade();
        let course = Rc::clone(course);
        Rc::new(move |menu, window, cx| {
            // Read when the menu opens, not for every row on every frame.
            let Some(desktop) = entity.upgrade() else {
                return menu;
            };
            let (link, folders) = {
                let desktop = desktop.read(cx);
                (
                    desktop
                        .note_source(&course)
                        .and_then(|source| source.link.clone()),
                    desktop.folder_context(Some(course.dir.clone())),
                )
            };
            let path = course.storage_dir();
            let menu = if with_open {
                let open = entity.clone();
                let selected = course.clone();
                menu.item(
                    PopupMenuItem::new(course2md::tr!("阅读笔记"))
                        .icon(icons::book_open())
                        .on_click(move |_, _, cx| {
                            let _ =
                                open.update(cx, |this, cx| this.open_course(selected.clone(), cx));
                        }),
                )
            } else {
                menu
            };
            let mut menu = menu.item(
                PopupMenuItem::new(course2md::tr!("打开笔记保存位置"))
                    .icon(crate::icons::folder_open())
                    .on_click(move |_, _, cx| cx.open_with_system(&path)),
            );
            if let Some(link) = link {
                menu = menu.item(
                    PopupMenuItem::new(course2md::tr!("复制原视频链接"))
                        .icon(icons::link())
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(link.clone()))
                        }),
                );
            }
            let exports = {
                let entity = entity.clone();
                let course = course.clone();
                move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                    [
                        (
                            course2md::config::OutputFormat::Md,
                            course2md::tr!("Markdown 包…"),
                        ),
                        (
                            course2md::config::OutputFormat::Html,
                            course2md::tr!("网页文件…"),
                        ),
                        (
                            course2md::config::OutputFormat::Json,
                            course2md::tr!("结构化数据…"),
                        ),
                    ]
                    .into_iter()
                    .fold(menu, |menu, (format, label)| {
                        let entity = entity.clone();
                        let course = course.clone();
                        menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                this.export_course(course.clone(), format, window, cx)
                            });
                        }))
                    })
                }
            };
            menu = menu.separator().submenu_with_icon(
                Some(icons::download()),
                course2md::tr!("导出为"),
                window,
                cx,
                exports,
            );
            // 归属修改：与分组标题/筛选轨不重复，放进子菜单
            menu = menu.submenu_with_icon(
                Some(crate::icons::folder_input()),
                course2md::tr!("移动到"),
                window,
                cx,
                Desktop::folder_menu(
                    entity.clone(),
                    folders.origin,
                    folders.storage,
                    folders.folders,
                    folders.folder,
                ),
            );
            let rename = entity.clone();
            let selected = course.clone();
            let delete = entity.clone();
            let doomed = course.clone();
            menu.separator()
                .item(
                    PopupMenuItem::new(course2md::tr!("重命名笔记…"))
                        .icon(icons::edit())
                        .on_click(move |_, window, cx| {
                            let _ = rename.update(cx, |this, cx| {
                                this.begin_course_rename(Course::clone(&selected), window, cx)
                            });
                        }),
                )
                .item(
                    crate::theme::danger_menu_item(course2md::tr!("删除笔记…"), icons::delete())
                        .on_click(move |_, window, cx| {
                            let _ = delete.update(cx, |this, cx| {
                                this.confirm_course_delete(Course::clone(&doomed), window, cx)
                            });
                        }),
                )
        })
    }

    fn recover_library_folders(&mut self, root: PathBuf, rebuild: bool, cx: &mut Context<Self>) {
        let worker_root = root.clone();
        let task = crate::spawn_blocking_io(move || organize::recover(&worker_root, rebuild));
        cx.spawn(async move |this, cx| {
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::interrupted())));
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((library, _)) => {
                        this.store_folders(&root, library);
                        this.message = Some(if rebuild {
                            course2md::tr!("已重新开始整理文件夹，原损坏文件已另存").into()
                        } else {
                            course2md::tr!("文件夹已恢复，原损坏文件已另存").into()
                        });
                        this.refresh_library(cx);
                    }
                    Err(error) => {
                        this.message =
                            Some(course2md::tr!("文件夹尚未恢复：{}", format!("{error:#}")))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn recover_note_titles(&mut self, root: PathBuf, reset: bool, cx: &mut Context<Self>) {
        let task = crate::spawn_blocking_io(move || organize::recover_titles(&root, reset));
        cx.spawn(async move |this, cx| {
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::interrupted())));
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        this.message =
                            Some(course2md::tr!("笔记名称已恢复，原损坏文件已另存").into());
                        this.refresh_library(cx);
                    }
                    Err(error) => {
                        this.message =
                            Some(course2md::tr!("名称尚未恢复：{}", format!("{error:#}")))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn library_needs_recovery(&self, access: &crate::storage::LibraryAccess) -> bool {
        self.workspace.as_ref().is_some_and(|workspace| {
            workspace.state.libraries.iter().any(|location| {
                access.unavailable.contains(&location.root)
                    || self
                        .library_view_cache
                        .recovery
                        .contains_key(&location.root)
                    || self
                        .library_view_cache
                        .title_recovery
                        .contains_key(&location.root)
            })
        })
    }

    fn library_recovery_view(
        &self,
        access: &crate::storage::LibraryAccess,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let mut view = v_flex().gap_3();
        let workspace = self.workspace.as_ref()?;
        let mut has_notice = false;
        for (index, location) in workspace.state.libraries.iter().enumerate() {
            if access.unavailable.contains(&location.root) {
                has_notice = true;
                let name = location.name.clone();
                let root = location.root.clone();
                view = view.child(
                    v_flex()
                        .gap_3()
                        .p_4()
                        .bg(color(WARNING_BG))
                        .rounded(RADIUS_CARD)
                        .child(accessible_text(
                            ("library-unavailable", index),
                            course2md::tr!("{} 暂时无法访问", name),
                        ))
                        .child(
                            control(("retry-library-location", index))
                                .icon(icons::refresh())
                                .label(course2md::tr!("重新检查保存位置"))
                                .tooltip(
                                    course2md::config::simplify_verbatim(root.clone())
                                        .display()
                                        .to_string(),
                                )
                                .self_start()
                                .loading(self.loading)
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx))),
                        ),
                );
                continue;
            }
            if let Some(recovery) = self.library_view_cache.recovery.get(&location.root) {
                has_notice = true;
                let restore = location.root.clone();
                let rebuild = location.root.clone();
                let folder = location.root.clone();
                let diagnostics =
                    self.library_issue_messages(&location.root, LibraryIssueKind::Folders);
                let mut row = h_flex().gap_2().flex_wrap();
                if recovery.has_backup {
                    row = row.child(
                        control(("restore-classification", index))
                            .primary()
                            .icon(icons::history())
                            .label(course2md::tr!("恢复最近备份"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.recover_library_folders(restore.clone(), false, cx)
                            })),
                    );
                }
                row = row
                    .child(
                        control(("rebuild-classification", index))
                            .when(!recovery.has_backup, |button| button.primary())
                            .icon(icons::refresh())
                            .label(course2md::tr!("重新开始整理（保留损坏文件）"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.recover_library_folders(rebuild.clone(), true, cx)
                            })),
                    )
                    .child(
                        control(("show-classification-files", index))
                            .ghost()
                            .icon(crate::icons::folder_open())
                            .label(course2md::tr!("打开保存位置"))
                            .on_click(move |_, _, cx| cx.open_with_system(&folder)),
                    );
                view = view.child(
                    v_flex()
                        .gap_3()
                        .p_4()
                        .bg(color(WARNING_BG))
                        .rounded(RADIUS_CARD)
                        .child(
                            badge(BadgeKind::Warning).child(course2md::tr!("文件夹信息需要恢复")),
                        )
                        .child(accessible_text(
                            ("classification-recovery", index),
                            course2md::tr!("{} 的文件夹信息已损坏", location.name),
                        ))
                        .child(row)
                        .when(!diagnostics.is_empty(), |view| {
                            view.child(IssueDetails::new(
                                format!("folders:{}", location.id),
                                diagnostics,
                            ))
                        }),
                );
            }
            if let Some(recovery) = self.library_view_cache.title_recovery.get(&location.root) {
                has_notice = true;
                let diagnostics =
                    self.library_issue_messages(&location.root, LibraryIssueKind::Titles);
                let mut row = h_flex().gap_2().flex_wrap();
                for reset in [false, true] {
                    if !reset && !recovery.has_backup {
                        continue;
                    }
                    let root = location.root.clone();
                    row = row.child(
                        control((
                            if reset {
                                "reset-note-names"
                            } else {
                                "restore-note-names"
                            },
                            index,
                        ))
                        .icon(if reset {
                            icons::refresh()
                        } else {
                            icons::history()
                        })
                        .label(if reset {
                            course2md::tr!("恢复为原始标题")
                        } else {
                            course2md::tr!("恢复最近名称备份")
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.recover_note_titles(root.clone(), reset, cx)
                        })),
                    );
                }
                view = view.child(
                    v_flex()
                        .gap_3()
                        .p_4()
                        .bg(color(WARNING_BG))
                        .rounded(RADIUS_CARD)
                        .child(badge(BadgeKind::Warning).child(course2md::tr!("笔记名称需要恢复")))
                        .child(accessible_text(
                            ("title-recovery", index),
                            course2md::tr!("{} 的笔记名称记录无法读取", location.name),
                        ))
                        .child(row)
                        .when(!diagnostics.is_empty(), |view| {
                            view.child(IssueDetails::new(
                                format!("names:{}", location.id),
                                diagnostics,
                            ))
                        }),
                );
            }
        }
        has_notice.then_some(view)
    }

    pub fn library_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rem = f32::from(window.rem_size());
        let tree_visible = self.folder_sidebar_visible(window, cx);
        let content = crate::views::shell_content_width(Page::Library, window)
            - if tree_visible {
                f32::from(
                    (crate::folder_tree::FOLDER_SIDEBAR + crate::folder_tree::FOLDER_SIDEBAR_GAP)
                        .to_pixels(window.rem_size()),
                )
            } else {
                0.
            };
        // auto-fill minmax(240px, 1fr) with 12px gaps; both scale with text.
        let card_gap = 12. / 14. * rem;
        let columns = ((content + card_gap) / (240. / 14. * rem + card_gap))
            .floor()
            .max(1.) as usize;
        let layout = LibraryLayout {
            columns,
            card_width: ((content - card_gap * (columns as f32 - 1.)) / columns as f32).max(0.),
            stacked: content < 36. * rem + 24.,
        };
        let query = self.value(Field::Search, cx).to_lowercase();
        let mut items = Vec::new();
        let Some(all_access) = self.cached_library_access() else {
            items.push(LibraryItem::Checking);
            return self.library_list_page(items, layout, rem, tree_visible, None, window, cx);
        };
        let in_scope = |root: &&PathBuf| self.folder_filter.is_all() || **root == self.library_root;
        let scope = crate::storage::LibraryAccess {
            available: all_access
                .available
                .iter()
                .filter(in_scope)
                .cloned()
                .collect(),
            unavailable: all_access
                .unavailable
                .iter()
                .filter(in_scope)
                .cloned()
                .collect(),
        };
        let coverage = scope.coverage();
        let searched: Option<Vec<u64>> = match self.folder_filter {
            FolderFilter::Folder(id) if !query.is_empty() => self
                .library_indexes
                .get(&self.library_root)
                .map(|library| library.subtree(id)),
            _ => None,
        };
        let courses: Vec<usize> = self
            .courses
            .iter()
            .enumerate()
            .filter_map(|(index, course)| {
                let location = self.course_location(course);
                let in_scope = location
                    .as_ref()
                    .map(|location| scope.available.contains(&location.root))
                    .unwrap_or_else(|| {
                        self.workspace.is_none()
                            && self
                                .library_view_cache
                                .locations
                                .get(&course.storage_dir())
                                .is_some_and(|location| scope.available.contains(&location.root))
                    });
                if !in_scope || !course.title.to_lowercase().contains(&query) {
                    return None;
                }
                let matches_folder = match self.folder_filter {
                    crate::FolderFilter::All => true,
                    filter => {
                        let folder = self.course_folder(course);
                        location
                            .as_ref()
                            .is_some_and(|location| location.root == self.library_root)
                            && (!self.library_indexes.contains_key(&self.library_root)
                                || match filter {
                                    // Searching a place covers everything below it, as
                                    // in a file manager; browsing shows its own notes.
                                    crate::FolderFilter::Unfiled => folder.is_none(),
                                    crate::FolderFilter::Folder(id) => match &searched {
                                        Some(inside) => folder.is_some_and(|f| inside.contains(&f)),
                                        None => folder == Some(id),
                                    },
                                    crate::FolderFilter::All => true,
                                })
                    }
                };
                matches_folder.then_some(index)
            })
            .collect();
        if self.library_needs_recovery(&all_access) {
            items.push(LibraryItem::Recovery(all_access.clone()));
        }
        let coverage_message = match coverage {
            crate::storage::LibraryCoverage::Unavailable if query.is_empty() => {
                Some(course2md::tr!("保存位置暂时无法访问").to_owned())
            }
            crate::storage::LibraryCoverage::Unavailable => {
                Some(course2md::tr!("保存位置暂时无法访问，未能搜索").to_owned())
            }
            // 未连接的位置各有恢复卡片；仅搜索时提示结果不完整
            crate::storage::LibraryCoverage::Partial if !query.is_empty() => Some(course2md::tr!(
                "另有 {} 个位置未纳入搜索",
                scope.unavailable.len()
            )),
            _ => None,
        };
        if let Some(message) = coverage_message {
            items.push(LibraryItem::Coverage(message));
        }
        // Recovery cards own their matching diagnostics. Keep unrelated read
        // failures visible without repeating the same classification warning.
        let remaining_issues: Vec<_> = self
            .library_issues
            .iter()
            .filter(|issue| {
                let claimed = match issue.kind {
                    LibraryIssueKind::Scan => false,
                    LibraryIssueKind::Folders => {
                        self.library_view_cache.recovery.contains_key(&issue.root)
                    }
                    LibraryIssueKind::Titles => self
                        .library_view_cache
                        .title_recovery
                        .contains_key(&issue.root),
                };
                !(claimed && !all_access.unavailable.contains(&issue.root))
            })
            .map(|issue| issue.message.clone())
            .collect();
        if !remaining_issues.is_empty() && coverage != crate::storage::LibraryCoverage::Unavailable
        {
            items.push(LibraryItem::Issues(remaining_issues));
        }
        // 刷新期间保留已加载的列表：Loading 只是一行状态，不再整页替换；
        // 只有首次加载（没有任何已加载内容）时才让 Loading 独占页面
        if self.loading {
            items.push(LibraryItem::Loading);
        }
        if self.loading && courses.is_empty() {
            return self.library_list_page(items, layout, rem, tree_visible, None, window, cx);
        }
        if coverage == crate::storage::LibraryCoverage::Unavailable {
            return self.library_list_page(items, layout, rem, tree_visible, None, window, cx);
        }
        // Inside a folder, its folders come before its notes. Top-level
        // folders are in the navigation (or the places menu), not 未分类.
        let folders = match self.folder_filter {
            FolderFilter::All | FolderFilter::Unfiled => None,
            FolderFilter::Folder(id) => Some(Some(id)),
        }
        .filter(|parent| {
            query.is_empty()
                && self
                    .library_indexes
                    .get(&self.library_root)
                    .is_some_and(|library| !library.children(*parent).is_empty())
        });
        // The place's notes, its subfolders' included as in the navigation;
        // a search reports its matches on the same line, so nothing moves.
        let count = Some(if !query.is_empty() {
            found_notes(courses.len())
        } else if let crate::FolderFilter::Folder(id) = self.folder_filter {
            crate::folder_tree::note_count(self.folder_note_count(&self.library_root, id))
        } else {
            crate::folder_tree::note_count(courses.len())
        });
        if let Some(parent) = folders {
            items.push(LibraryItem::Folders(self.library_root.clone(), parent));
        }
        if courses.is_empty() && folders.is_some() {
            return self.library_list_page(items, layout, rem, tree_visible, count, window, cx);
        }
        if courses.is_empty() {
            if query.is_empty()
                && (coverage == crate::storage::LibraryCoverage::Partial
                    || !self.library_issues.is_empty())
            {
                return self.library_list_page(items, layout, rem, tree_visible, count, window, cx);
            }
            items.push(LibraryItem::Empty);
            return self.library_list_page(items, layout, rem, tree_visible, count, window, cx);
        }
        if self.desktop_settings.library_group_folders
            && self.folder_filter.is_all()
            && self.library_error.is_none()
        {
            let mut groups: BTreeMap<(PathBuf, u64), Vec<usize>> = BTreeMap::new();
            for index in courses {
                let course = &self.courses[index];
                let root = self
                    .course_location(course)
                    .map(|lib| lib.root.clone())
                    .unwrap_or_else(|| self.library_root.clone());
                groups
                    .entry((root, self.course_folder(course).unwrap_or(0)))
                    .or_default()
                    .push(index);
            }
            for (group_index, ((root, id), entries)) in groups.into_iter().enumerate() {
                let location = self
                    .workspace
                    .as_ref()
                    .and_then(|w| w.state.libraries.iter().find(|lib| lib.root == root));
                let key = format!(
                    "{}:{id}",
                    location
                        .map(|lib| lib.id.clone())
                        .unwrap_or_else(|| root.display().to_string())
                );
                let collapsed = query.is_empty()
                    && self
                        .workspace
                        .as_ref()
                        .is_some_and(|w| w.state.collapsed.contains(&key));
                let folder_name = self
                    .library_indexes
                    .get(&root)
                    .filter(|lib| lib.folders.contains_key(&id))
                    .map(|lib| lib.path(id).join(" / "))
                    .unwrap_or_else(|| {
                        if self.library_indexes.contains_key(&root) {
                            course2md::tr!("未分类")
                        } else {
                            course2md::tr!("文件夹暂不可用")
                        }
                        .into()
                    });
                let name = if self
                    .workspace
                    .as_ref()
                    .is_some_and(|w| w.state.libraries.len() > 1)
                {
                    format!(
                        "{} · {folder_name}",
                        location
                            .map(|lib| lib.name.as_str())
                            .unwrap_or(course2md::tr!("笔记库"))
                    )
                } else {
                    folder_name
                };
                items.push(LibraryItem::GroupHeader(Box::new(LibraryGroupHeader {
                    index: group_index,
                    key: key.clone(),
                    name,
                    count: entries.len(),
                    collapsed,
                })));
                items.push(LibraryItem::GroupBody(Box::new(LibraryGroupBody {
                    index: group_index,
                    key,
                    collapsed,
                    courses: entries,
                })));
            }
        } else if !self.desktop_settings.library_cards || layout.stacked {
            let count = courses.len();
            items.extend(courses.into_iter().enumerate().map(|(position, index)| {
                LibraryItem::ListRow(LibraryRow::list(
                    &self.courses,
                    index,
                    position == 0,
                    position + 1 == count,
                ))
            }));
        } else {
            items.extend(
                LibraryRow::cards(&self.courses, &courses, layout.columns)
                    .into_iter()
                    .map(LibraryItem::CardRow),
            );
        }
        if !items.iter().any(|item| matches!(item, LibraryItem::Empty)) {
            self.entered.remove("library-empty-state");
        }
        self.library_list_page(items, layout, rem, tree_visible, count, window, cx)
    }

    /// The library's variable-height list: notices, folder groups and card
    /// rows share one scroll region, spliced by identity so measured heights
    /// and the scroll anchor survive content updates. The title row and the
    /// toolbar stay above it; the folder navigation sits beside them.
    #[allow(clippy::too_many_arguments)]
    fn library_list_page(
        &mut self,
        items: Vec<LibraryItem>,
        layout: LibraryLayout,
        rem: f32,
        tree_visible: bool,
        count: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let notes = self.library_notes(items, layout, rem, cx);
        let pane = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .gap(rems(20. / 14.))
            .child(self.library_header(count, cx))
            .children(self.library_toolbar(tree_visible, layout, window, cx))
            .child(notes);
        let page = if tree_visible {
            h_flex()
                .size_full()
                .min_h_0()
                .items_start()
                .gap(crate::folder_tree::FOLDER_SIDEBAR_GAP)
                .child(self.folder_sidebar(cx))
                .child(pane)
        } else {
            h_flex().size_full().min_h_0().child(pane)
        };
        v_flex()
            .size_full()
            .min_h_0()
            .pt(rems(16. / 14.))
            .child(page)
            .into_any_element()
    }

    /// The selected place's name and item count, its folder commands and
    /// the page's main action.
    fn library_header(&self, count: Option<String>, cx: &mut Context<Self>) -> Div {
        let (place, library) = self.library_place();
        // An empty library's own empty state explains it; no "0 篇笔记".
        let count = count.filter(|_| self.library_controls_visible(cx));
        let supporting = match (library, count) {
            (Some(library), Some(count)) => Some(format!("{library} · {count}")),
            (Some(library), None) => Some(library),
            (None, Some(count)) => Some(count),
            (None, None) => None,
        };
        let folder_menu = self
            .folder_filter
            .folder_id()
            .map(|id| self.folder_node_menu(self.library_root.clone(), id, cx));
        h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap_4()
            .flex_wrap()
            .items_center()
            .min_h(BUTTON_LARGE)
            .child(
                v_flex()
                    .flex_1()
                    .min_w(rems(160. / 14.))
                    .gap(px(2.))
                    .child(page_heading("page-title", place))
                    .children(supporting.map(|text| {
                        accessible_text("library-place-count", text)
                            .text_size(TEXT_BODY)
                            .text_color(color(GRAY))
                    })),
            )
            .when_some(folder_menu, |row, menu| {
                row.child(
                    icon_button("manage-folder", CONTROL_HEIGHT, false, cx)
                        .icon(icons::more_vertical())
                        .accessibility_label(course2md::tr!("文件夹操作"))
                        .tooltip(course2md::tr!("文件夹操作"))
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |popup, window, cx| {
                            menu(popup, window, cx)
                        }),
                )
            })
            .when(self.library_controls_visible(cx), |row| {
                // M3 FAB-sized tonal action: 56px, 16px corners.
                row.child(
                    tonal_pill("library-new-note", cx)
                        .h(BUTTON_LARGE)
                        .min_h(BUTTON_LARGE)
                        .rounded(RADIUS_CARD)
                        .pl(rems(18. / 14.))
                        .pr(rems(24. / 14.))
                        .gap(rems(10. / 14.))
                        .label_text_size(rems(15. / 14.))
                        .icon(icons::plus().size(rems(22. / 14.)))
                        .label(course2md::tr!("导入视频"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.prepare_workbench_input(window, cx) {
                                this.navigate(Page::New, cx);
                            }
                        })),
                )
            })
    }

    fn library_notes(
        &mut self,
        items: Vec<LibraryItem>,
        layout: LibraryLayout,
        rem: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let flat = items.iter().find_map(|item| match item {
            LibraryItem::CardRow(row) => Some((true, row.courses[0])),
            LibraryItem::ListRow(row) => Some((false, row.courses[0])),
            _ => None,
        });
        let keys = items.iter().map(LibraryItem::key).collect();
        Self::reconcile_list_items(
            &self.library_list,
            &mut self.library_keys,
            &mut self.library_focus,
            keys,
            cx,
        );
        if self.library_rem != rem {
            self.library_list.remeasure();
            self.library_rem = rem;
        }
        let focus: Rc<Vec<FocusHandle>> = Rc::new(self.library_focus.clone());
        let state = self.library_list.clone();
        let items = Rc::new(items);
        let desktop = cx.weak_entity();
        let element = list(state, move |index, _window, cx| {
            let Some(item) = items.get(index) else {
                return div().into_any_element();
            };
            let last = index + 1 == items.len();
            let row = LibraryRowContext {
                index,
                last,
                focus: focus.get(index),
                layout,
            };
            desktop
                .update(cx, |this, cx| this.library_item(item, row, cx))
                .unwrap_or_else(|_| div().into_any_element())
        })
        .w_full()
        .h_full()
        .flex_1()
        .min_h_0()
        .pb_6();
        if let Some((grid, id)) = flat {
            crate::motion::enter(
                if grid {
                    ("library-grid", id)
                } else {
                    ("library-list", id)
                },
                element,
            )
        } else {
            element.into_any_element()
        }
    }

    fn library_item(
        &mut self,
        item: &LibraryItem,
        row: LibraryRowContext,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (index, last, focus, layout) = (row.index, row.last, row.focus, row.layout);
        let content = match item {
            LibraryItem::Checking => h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .py_6()
                .child(crate::motion::spinner("library-location-check-spinner", cx))
                .child(accessible_text(
                    "library-location-checking-label",
                    course2md::tr!("正在检查保存位置…"),
                ))
                .into_any_element(),
            LibraryItem::Recovery(access) => self
                .library_recovery_view(access, cx)
                .unwrap_or_else(div)
                .into_any_element(),
            LibraryItem::Coverage(message) => {
                accessible_text("library-search-coverage", message.clone())
                    .text_sm()
                    .text_color(color(GRAY))
                    .into_any_element()
            }
            LibraryItem::Issues(messages) => v_flex()
                .gap_2()
                .p_3()
                .bg(color(WARNING_BG))
                .rounded(RADIUS_CARD)
                .child(badge(BadgeKind::Warning).child(course2md::tr!("部分内容暂未读取")))
                .child(
                    control("retry-unread-library-content")
                        .self_start()
                        .icon(icons::refresh())
                        .label(course2md::tr!("重新检查"))
                        .loading(self.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx))),
                )
                .child(IssueDetails::new("unread-content", messages.clone()))
                .into_any_element(),
            LibraryItem::Loading => h_flex()
                .gap_2()
                .items_center()
                .py_6()
                .child(crate::motion::spinner("library-scan-spinner", cx))
                .child(accessible_text(
                    "library-loading",
                    course2md::tr!("正在读取笔记…"),
                ))
                .into_any_element(),
            LibraryItem::Empty => {
                let query = self.value(Field::Search, cx).to_lowercase();
                let (icon, heading) = if !query.is_empty() {
                    (icons::search(), course2md::tr!("没有匹配的笔记"))
                } else {
                    match self.folder_filter {
                        FolderFilter::All => (icons::notebook(), course2md::tr!("还没有笔记")),
                        FolderFilter::Unfiled => {
                            (icons::inbox(), course2md::tr!("没有未分类的笔记"))
                        }
                        FolderFilter::Folder(_) => {
                            (icons::folder(), course2md::tr!("这个文件夹还没有笔记"))
                        }
                    }
                };
                let content = v_flex()
                    .w_full()
                    .py_12()
                    .px_6()
                    .gap_4()
                    .items_center()
                    .text_center()
                    .rounded(RADIUS_HERO)
                    .bg(color(SURFACE))
                    .child(tinted_icon(icon, Tint::Blue, rems(56. / 14.)))
                    .child(
                        accessible_text("library-empty-heading", heading)
                            .role(Role::Heading)
                            .text_size(rems(22. / 14.))
                            .line_height(rems(28. / 14.))
                            .font_weight(FontWeight::NORMAL),
                    )
                    .when(!query.is_empty(), |empty| {
                        empty.child(
                            outline_pill("clear-search", cx)
                                .icon(crate::icons::close())
                                .label(course2md::tr!("清除搜索"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.inputs[&Field::Search]
                                        .update(cx, |state, cx| state.set_value("", window, cx));
                                    cx.notify();
                                })),
                        )
                    })
                    .when(query.is_empty() && self.folder_filter.is_all(), |empty| {
                        empty.child(
                            primary_pill("empty-library-add")
                                .icon(crate::icons::plus())
                                .label(course2md::tr!("导入视频"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.begin_add(window, cx)),
                                ),
                        )
                    })
                    .when(query.is_empty() && !self.folder_filter.is_all(), |empty| {
                        empty.child(
                            outline_pill("empty-folder-all-notes", cx)
                                .icon(crate::icons::notebook())
                                .label(course2md::tr!("浏览全部笔记"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.folder_filter = crate::FolderFilter::All;
                                    this.library_list.scroll_to(ListOffset {
                                        item_ix: 0,
                                        offset_in_item: px(0.),
                                    });
                                    cx.notify();
                                })),
                        )
                    });
                if self.enter_once("library-empty-state".to_owned()) {
                    crate::motion::enter("library-empty-state", content)
                } else {
                    content.into_any_element()
                }
            }
            LibraryItem::Folders(root, parent) => self
                .folder_tiles(
                    root,
                    *parent,
                    // Folder tiles take the card grid's columns.
                    (self.desktop_settings.library_cards && !layout.stacked)
                        .then_some(px(layout.card_width)),
                    cx,
                )
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element()),
            LibraryItem::GroupHeader(header) => {
                let LibraryGroupHeader {
                    index,
                    key,
                    name,
                    count,
                    collapsed,
                } = header.as_ref();
                let key = key.clone();
                let collapsed = *collapsed;
                let index = *index;
                // A section title over its notes: quiet medium text on the
                // list's 20px text inset, the chevron in the shared icon slot.
                let clear = Rgba {
                    a: 0.,
                    ..color(CANVAS)
                };
                control(("library-group", index))
                    .custom(
                        ButtonCustomVariant::new(cx)
                            .color(clear.into())
                            .foreground(color(GRAY).into())
                            .hover(blend(color(CANVAS), color(INK), 0.08).into())
                            .active(blend(color(CANVAS), color(INK), 0.10).into())
                            .shadow(false),
                    )
                    .w_full()
                    .justify_start()
                    .px(SETTING_ITEM_INSET)
                    .accessibility_label(course2md::tr!(
                        "{} {}，{} 篇笔记",
                        if collapsed {
                            course2md::tr!("展开")
                        } else {
                            course2md::tr!("收起")
                        },
                        name,
                        count
                    ))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap(ICON_GAP)
                            .items_center()
                            .child(
                                h_flex()
                                    .size(ICON_SLOT)
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        Icon::new(if collapsed {
                                            crate::icons::chevron_right()
                                        } else {
                                            crate::icons::chevron_down()
                                        })
                                        .size(rems(18. / 14.))
                                        .text_color(color(GRAY)),
                                    ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(TEXT_BODY)
                                    .line_height(rems(20. / 14.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(color(GRAY))
                                    .child(name.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(rems(13. / 14.))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(color(GRAY))
                                    .child(count.to_string()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(workspace) = &mut this.workspace {
                            match workspace.transaction(|state| {
                                let became = !state.collapsed.remove(&key);
                                if became {
                                    state.collapsed.insert(key.clone());
                                }
                                Ok(became)
                            }) {
                                Ok(true) => {
                                    this.entered.remove(&format!("folder-disclosure-{index}"));
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    this.workspace_error = Some(course2md::tr!(
                                        "分组展开状态尚未保存：{}",
                                        format!("{error:#}")
                                    ));
                                }
                            }
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            LibraryItem::GroupBody(body) => {
                let collapsed = body.collapsed;
                let index = body.index;
                let animate = !collapsed && self.enter_once(format!("folder-disclosure-{index}"));
                let collection = self.course_collection(&body.courses, layout, animate, cx);
                if animate {
                    crate::motion::disclosure(("folder-disclosure", index), true, collection)
                } else if collapsed {
                    div().hidden().into_any_element()
                } else {
                    collection.into_any_element()
                }
            }
            LibraryItem::CardRow(row) => self
                .library_card_row(&row.courses, layout, cx)
                .into_any_element(),
            LibraryItem::ListRow(row) => self
                .library_list_row(row.courses[0], row.edges, layout, cx)
                .into_any_element(),
        };
        // Each item carries its trailing spacing as padding: the virtual
        // list measures an item's padding but not its outer margin, so a
        // margin would let the next item draw over the gap.
        let mut wrapper = v_flex().w_full().min_w_0();
        if !last {
            wrapper = match item {
                // The header's own height leaves the section-title gap below its text.
                LibraryItem::GroupHeader { .. } => wrapper,
                LibraryItem::GroupBody { .. } => wrapper.pb(px(20.)),
                LibraryItem::CardRow(_) => wrapper.pb(rems(12. / 14.)),
                // List rows join into one surface.
                LibraryItem::ListRow(row) if !row.edges.1 => wrapper,
                _ => wrapper.pb_6(),
            };
        }
        let mut wrapper = wrapper
            .child(content)
            .id(("library-item", index))
            .tab_stop(false);
        if let Some(focus) = focus {
            wrapper = wrapper.track_focus(focus);
        }
        wrapper.into_any_element()
    }

    pub(super) fn library_controls_visible(&self, cx: &App) -> bool {
        !(self.courses.is_empty()
            && self.folder_filter.is_all()
            && self.value(Field::Search, cx).is_empty()
            && self.library_error.is_none()
            && self.library_issues.is_empty()
            && !self.loading)
    }

    /// The filled M3 search bar above the notes.
    pub fn library_search(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        theme::search_field(
            &self.inputs[&Field::Search],
            course2md::tr!("搜索笔记标题"),
            BUTTON_LARGE,
            window,
            cx,
        )
    }

    /// Search, the places menu on narrow windows, grouping, the card/list
    /// choice and a manual rescan, above the notes.
    fn library_toolbar(
        &self,
        tree_visible: bool,
        layout: LibraryLayout,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.library_controls_visible(cx) {
            return None;
        }
        let group_on = self.desktop_settings.library_group_folders;
        let cards_on = self.desktop_settings.library_cards;
        let can_group = self.folder_filter.is_all() && self.library_error.is_none();
        let entity = cx.entity().downgrade();
        let loading = self.loading;
        // Inside one folder there is nothing to group, but the chip keeps its
        // place (disabled) so the search field does not change width.
        let group = filter_chip("library-group-folders", group_on && can_group, cx)
            .h(CONTROL_HEIGHT)
            .min_h(CONTROL_HEIGHT)
            .pl(rems(10. / 14.))
            .pr(rems(16. / 14.))
            .gap(ICON_GAP)
            .when(!group_on || !can_group, |chip| chip.icon(icons::folder()))
            .label(course2md::tr!("按文件夹分组"))
            .disabled(!can_group)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.desktop_settings.library_group_folders = !group_on;
                this.save_library_presentation(cx);
            }));
        // Narrow windows always list notes, so the choice only shows where it applies.
        let view = SingleChoiceGroup::new("library-view", course2md::tr!("显示"))
            .options([
                ("cards", course2md::tr!("卡片")),
                ("list", course2md::tr!("列表")),
            ])
            .icon("cards", icons::layout_grid())
            .icon("list", icons::list())
            .selected(if cards_on { "cards" } else { "list" })
            .on_change(move |value, _, cx| {
                let cards = value.as_ref() == "cards";
                let _ = entity.update(cx, |this, cx| {
                    this.desktop_settings.library_cards = cards;
                    this.save_library_presentation(cx);
                });
            });
        // The library refreshes itself; a manual rescan is a rare command.
        let rescan = icon_button("library-rescan", CONTROL_HEIGHT, false, cx)
            .icon(icons::refresh())
            .accessibility_label(course2md::tr!("重新扫描笔记库"))
            .tooltip(course2md::tr!("重新扫描笔记库"))
            .loading(loading)
            .disabled(loading)
            .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx)));
        Some(
            h_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .gap_3()
                .flex_wrap()
                .items_center()
                .child(
                    self.library_search(window, cx)
                        .flex_1()
                        .flex_basis(rems(280. / 14.))
                        .min_w(rems(200. / 14.)),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .flex_wrap()
                        .items_center()
                        .when(!tree_visible, |row| row.child(self.library_places_menu(cx)))
                        .child(group)
                        .when(!layout.stacked, |row| row.child(view))
                        .child(rescan),
                ),
        )
    }

    /// Source identity helps readers recognize a note; routine revision numbers
    /// remain in the reader's version history.
    fn course_meta(&self, course: &Course) -> CourseMeta {
        let ms = course
            .manifest
            .as_ref()
            .map(|manifest| manifest.created_at_ms)
            .unwrap_or_else(|| {
                course
                    .modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs() * 1000)
                    .unwrap_or(0)
            });
        let date = i64::try_from(ms)
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|instant| instant.with_timezone(&chrono::Local).date_naive());
        // Grouped views name the folder in the group header instead.
        let folder = if self.desktop_settings.library_group_folders {
            None
        } else {
            self.course_folder(course)
                .filter(|id| *id != 0)
                .and_then(|id| {
                    self.course_location(course)
                        .and_then(|location| self.library_indexes.get(&location.root))
                        .and_then(|library| library.folders.get(&id))
                })
                .cloned()
        };
        CourseMeta {
            folder,
            source: self.note_source(course).cloned(),
            date,
        }
    }

    pub fn save_library_presentation(&mut self, cx: &mut Context<Self>) {
        let mut preferences = self.application_edit_base();
        preferences.desktop.library_cards = self.desktop_settings.library_cards;
        preferences.desktop.library_group_folders = self.desktop_settings.library_group_folders;
        self.commit_application(preferences, cx);
        cx.notify();
    }

    fn course_has_reading_position(&self, course: &Course) -> bool {
        course.manifest.as_ref().is_some_and(|manifest| {
            let key = crate::reader_ui::manifest_reading_key(
                &manifest.course_id,
                &manifest.version_id,
                0,
            );
            self.workspace
                .as_ref()
                .and_then(|workspace| workspace.state.positions.get(&key))
                .is_some_and(crate::reader_ui::has_reading_position)
        })
    }

    fn course_read_error(
        &self,
        course: &Course,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let (_, message) = self
            .reader_course_error
            .as_ref()
            .filter(|(path, _)| path == &course.dir)?;
        let retry = course.clone();
        let path = course.storage_dir();
        Some(
            v_flex()
                .w_full()
                .gap_2()
                .px_4()
                .pb_3()
                .child(
                    theme::accessible_text(("course-read-error", index), message.clone())
                        .text_size(TEXT_AUX)
                        .text_color(color(WARNING)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            quiet(("retry-course-read", index))
                                .icon(icons::refresh())
                                .label(course2md::tr!("重试打开"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_course(retry.clone(), cx)
                                })),
                        )
                        .child(
                            quiet(("reveal-unread-course", index))
                                .icon(icons::folder_open())
                                .label(course2md::tr!("检查保存位置"))
                                .on_click(move |_, _, cx| cx.reveal_path(&path)),
                        ),
                ),
        )
    }

    /// A folder group's notes, as one list surface or card rows like the flat view.
    fn course_collection(
        &self,
        courses: &[usize],
        layout: LibraryLayout,
        animate: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let collection_id = courses.first().copied().unwrap_or(0);
        let collection = v_flex().w_full();
        if !self.desktop_settings.library_cards || layout.stacked {
            let count = courses.len();
            let rows = v_flex().children(courses.iter().enumerate().map(|(position, index)| {
                self.library_list_row(*index, (position == 0, position + 1 == count), layout, cx)
            }));
            return if animate {
                collection.child(crate::motion::enter(("library-list", collection_id), rows))
            } else {
                collection.child(rows)
            };
        }
        let cards = v_flex().gap(rems(12. / 14.)).children(
            courses
                .chunks(layout.columns)
                .map(|row| self.library_card_row(row, layout, cx)),
        );
        if animate {
            collection.child(crate::motion::enter(("library-grid", collection_id), cards))
        } else {
            collection.child(cards)
        }
    }

    /// One row of note cards: equal-height columns of the grid's width.
    fn library_card_row(
        &self,
        row: &[usize],
        layout: LibraryLayout,
        cx: &mut Context<Self>,
    ) -> Div {
        h_flex()
            .gap(rems(12. / 14.))
            .items_stretch()
            .children(row.iter().filter_map(|index| {
                let course = self.courses.get(*index)?;
                Some(
                    div()
                        .flex()
                        .flex_none()
                        .w(px(layout.card_width))
                        .child(self.note_card(course, *index, "library", true, cx)),
                )
            }))
    }

    /// A note's real state worth flagging on its cover or row: a task still
    /// writing a version of it, or content that still needs completing.
    fn note_status(&self, course: &Course) -> Option<Div> {
        let busy = self
            .course_location(course)
            .zip(self.workspace.as_ref())
            .is_some_and(|(location, workspace)| {
                note_has_unfinished_task(
                    &workspace.state.tasks,
                    &location.id,
                    course
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.source_id.as_str()),
                )
            });
        if busy {
            Some(badge(BadgeKind::Progress).child(course2md::tr!("正在处理")))
        } else if course.incomplete() {
            Some(badge(BadgeKind::Warning).child(course2md::tr!("部分内容待补全")))
        } else {
            None
        }
    }

    /// Folder · date. The folder is named only where the place does not
    /// already say it: all notes, ungrouped.
    fn library_meta(&self, course: &Course) -> String {
        let meta = self.course_meta(course);
        let folder = (self.folder_filter.is_all() && !self.desktop_settings.library_group_folders)
            .then(|| {
                meta.folder.clone().or_else(|| {
                    self.course_location(course)
                        .is_some_and(|location| self.library_indexes.contains_key(&location.root))
                        .then(|| course2md::tr!("未分类").to_owned())
                })
            })
            .flatten();
        folder
            .into_iter()
            .chain(meta.date.map(|date| date.format("%Y-%m-%d").to_string()))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// A note's cover, or a tonal placeholder with a document icon.
    fn note_cover_content(course: &Course, radius: Pixels, icon: Rems) -> AnyElement {
        match &course.thumbnail {
            Some(thumbnail) => img(thumbnail.clone())
                .size_full()
                .rounded(radius)
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => icons::file_text()
                .size(icon)
                .text_color(color(ON_SECONDARY_CONTAINER))
                .into_any_element(),
        }
    }

    /// The library grid's note card: a 16:9 cover carrying the note's status,
    /// the title, folder · date and the note menu, on a 24px surface. Cover
    /// and title both open the note; the card needs no separate read button.
    pub(crate) fn note_card(
        &self,
        course: &Rc<Course>,
        index: usize,
        scope: &'static str,
        with_actions: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu = self.course_menu(course, true, cx);
        let open = |course: &Rc<Course>| {
            let course = Rc::clone(course);
            cx.listener(move |this: &mut Self, _, _, cx| this.open_course(course.clone(), cx))
        };
        let opening = self.opening_course.as_ref() == Some(&course.dir);
        let cover = control(SharedString::from(format!("{scope}-cover-{index}")))
            .ghost()
            .relative()
            .w_full()
            .h_auto()
            .p_0()
            .rounded(RADIUS_CARD)
            .aspect_ratio(16. / 9.)
            .tab_stop(false)
            .accessibility_label(course2md::tr!("阅读 {}", course.title))
            .bg(color(SECONDARY_CONTAINER))
            .child(Self::note_cover_content(
                course,
                RADIUS_CARD,
                rems(28. / 14.),
            ))
            .children(
                self.note_status(course)
                    .map(|status| div().absolute().left(px(8.)).top(px(8.)).child(status)),
            )
            .when(opening, |cover| {
                cover.child(
                    div()
                        .absolute()
                        .right(px(8.))
                        .top(px(8.))
                        .p(px(4.))
                        .rounded_full()
                        .bg(color(SURFACE))
                        .child(crate::motion::spinner(("library-card-opening", index), cx)),
                )
            })
            .on_click(open(course));
        let title = control(SharedString::from(format!("{scope}-title-{index}")))
            .ghost()
            .w_full()
            .h_auto()
            .min_h(px(0.))
            .p_0()
            .justify_start()
            .rounded(px(4.))
            .accessibility_label(course2md::tr!("阅读 {}", course.title))
            .child(
                div()
                    .w_full()
                    .whitespace_normal()
                    .text_ellipsis()
                    .line_clamp(2)
                    .text_size(rems(16. / 14.))
                    .line_height(rems(22. / 14.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color(INK))
                    .child(course.title.clone()),
            )
            .on_click(open(course));
        v_flex()
            .id(SharedString::from(format!("{scope}-card-{index}")))
            .flex_1()
            .min_w_0()
            .p(rems(10. / 14.))
            .pb(rems(1.))
            .gap(rems(12. / 14.))
            .rounded(TILE_RADIUS)
            .bg(color(SURFACE))
            .child(cover)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .pl(rems(6. / 14.))
                    .gap_1()
                    .items_start()
                    .child(
                        v_flex().flex_1().min_w_0().gap_1().child(title).child(
                            div()
                                .w_full()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(rems(13. / 14.))
                                .text_color(color(GRAY))
                                .child(self.library_meta(course)),
                        ),
                    )
                    .when(with_actions, |row| {
                        row.child(
                            div()
                                .mt(rems(-6. / 14.))
                                .child(self.course_actions(course, index, cx)),
                        )
                    }),
            )
            .when(with_actions, |view| {
                view.children(self.course_read_error(course, index, cx))
                    .children(self.course_export_feedback(course, cx))
            })
            .on_drag(note_drag(course), |drag, _, _, cx| cx.new(|_| drag.clone()))
            .context_menu(move |popup, window, cx| menu(popup, window, cx))
            .into_any_element()
    }

    /// One note in the list presentation: a segment of the single list
    /// surface, with a 128×72 cover, title, folder · date, status and the
    /// note menu, and a divider inset past the cover from the previous note.
    fn library_list_row(
        &self,
        index: usize,
        (first, last): (bool, bool),
        layout: LibraryLayout,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(course) = self.courses.get(index) else {
            return div().into_any_element();
        };
        let cover_w = rems(128. / 14.);
        let status = self.note_status(course);
        let opening = self.opening_course.as_ref() == Some(&course.dir);
        let cover = div()
            .w(cover_w)
            .max_w(relative(ROW_COVER_SHARE))
            .aspect_ratio(16. / 9.)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .rounded(FIELD_RADIUS)
            .bg(color(SECONDARY_CONTAINER))
            .child(Self::note_cover_content(
                course,
                FIELD_RADIUS,
                rems(24. / 14.),
            ));
        let text = v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .child(
                div()
                    .w_full()
                    .whitespace_normal()
                    .text_ellipsis()
                    .line_clamp(2)
                    .text_size(rems(16. / 14.))
                    .line_height(rems(22. / 14.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color(INK))
                    .child(course.title.clone()),
            )
            .child(
                div()
                    .w_full()
                    .whitespace_normal()
                    .text_size(rems(13. / 14.))
                    .text_color(color(GRAY))
                    .child(self.library_meta(course)),
            )
            // Narrow rows keep the status with the text instead of squeezing it.
            .when(layout.stacked, |text| {
                text.children(
                    self.note_status(course)
                        .map(|status| h_flex().pt_1().child(status)),
                )
            });
        // The row draws the one state layer for the whole note, so the read
        // target itself stays transparent under hover and press.
        let read = row_target(control(("read-course", index)), cx)
            .flex_1()
            .min_w_0()
            .h_auto()
            .p_0()
            .justify_start()
            .rounded(FIELD_RADIUS)
            .accessibility_label(course2md::tr!("阅读 {}", course.title))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_4()
                    .items_center()
                    .child(cover)
                    .child(text),
            )
            .on_click({
                let course = course.clone();
                cx.listener(move |this, _, _, cx| this.open_course(course.clone(), cx))
            });
        let menu = self.course_menu(course, true, cx);
        let read_error = self.course_read_error(course, index, cx);
        let export_feedback = self
            .course_export_feedback(course, cx)
            .map(|feedback| feedback.px_4().pb_3());
        let has_details = read_error.is_some() || export_feedback.is_some();
        v_flex()
            .id(("library-row", index))
            .w_full()
            .min_w_0()
            .bg(color(SURFACE))
            .when(first, |row| row.rounded_t(RADIUS_HERO))
            .when(last, |row| row.rounded_b(RADIUS_HERO))
            .when(!first, |row| {
                row.child(
                    div()
                        .h(px(1.))
                        .ml(rems(160. / 14.))
                        .mr(rems(16. / 14.))
                        .bg(color(HAIRLINE)),
                )
            })
            .child(
                // One state layer covers the cover, text, status and menu;
                // the list's top and bottom padding belong to the edge rows
                // so the layer follows the surface's corners.
                h_flex()
                    .id(("library-row-layer", index))
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .pl_4()
                    .pr_2()
                    .py_3()
                    .when(first, |row| row.pt(rems(20. / 14.)).rounded_t(RADIUS_HERO))
                    .when(last && !has_details, |row| {
                        row.pb(rems(20. / 14.)).rounded_b(RADIUS_HERO)
                    })
                    .gap_4()
                    .hover(|style| style.bg(row_state_layer()))
                    .child(read)
                    .when(!layout.stacked, |row| row.children(status))
                    .when(opening, |row| {
                        row.child(crate::motion::spinner(("library-row-opening", index), cx))
                    })
                    .child(self.course_actions(course, index, cx)),
            )
            .children(read_error)
            .children(export_feedback)
            .when(last && has_details, |row| row.pb_2())
            .on_drag(note_drag(course), |drag, _, _, cx| cx.new(|_| drag.clone()))
            .context_menu(move |popup, window, cx| menu(popup, window, cx))
            .into_any_element()
    }

    /// Every queued/running task remains visible without replacing the source input.
    pub fn workbench_active_tasks_section(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        use crate::workspace::{Intent, TaskState};
        let linked = self.current_input_task(cx).map(|task| task.id.clone());
        let tasks: Vec<_> = self
            .workspace
            .as_ref()?
            .state
            .tasks
            .iter()
            .filter(|task| {
                Some(&task.id) != linked.as_ref()
                    && task.handled_by.is_none()
                    && (matches!(
                        task.state,
                        TaskState::Queued | TaskState::Running | TaskState::Pausing
                    ) || self.is_running_task(&task.id))
            })
            .collect();
        if tasks.is_empty() {
            return None;
        }
        let mut list = v_flex()
            .w_full()
            .min_w_0()
            .rounded(RADIUS_CARD)
            .bg(color(SURFACE));
        for (index, task) in tasks.into_iter().enumerate() {
            let id = task.id.clone();
            let queued = task.state == TaskState::Queued && !self.is_running_task(&id);
            let cancelling = task.intent == Intent::Cancel;
            if index > 0 {
                list = list.child(inset_divider());
            }
            let mut detail = v_flex().flex_1().min_w_0().gap_2().child(
                quiet(SharedString::from(format!("workbench-task-open-{id}")))
                    .w_full()
                    .h_auto()
                    .p_0()
                    .justify_start()
                    // The component centers its internal content wrapper; a full-width
                    // text child owns the shared title/status leading edge.
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_left()
                            .text_color(color(INK))
                            .text_size(ROW_TITLE)
                            .font_weight(FontWeight::MEDIUM)
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(task.plan.title.clone()),
                    )
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            this.select_task(&id, cx);
                            this.navigate(Page::Task, cx);
                        }
                    })),
            );
            detail = detail.children(self.task_status_lines(
                task,
                &format!("workbench-task-{id}"),
                window,
                cx,
            ));
            let cover = cover_frame(
                RADIUS_SMALL,
                task.plan.source.cover.clone().filter(|_| !queued),
            )
            .w(ROW_COVER)
            .max_w(relative(ROW_COVER_SHARE))
            .when(queued, |cover| {
                cover.child(icons::schedule().size_6().text_color(color(GRAY)))
            })
            .when(!queued && task.plan.source.cover.is_none(), |cover| {
                cover.child(crate::import_ui::cover_placeholder(rems(24. / 14.)))
            });
            let menu = self.task_menu(task, cx);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("workbench-task-{id}")))
                    .w_full()
                    .min_w_0()
                    .p_4()
                    .gap_4()
                    .items_center()
                    .child(cover)
                    .child(detail)
                    .child(
                        quiet(SharedString::from(format!("workbench-task-cancel-{id}")))
                            .flex_shrink_0()
                            .icon(icons::close())
                            // Labelled as on the running card and the Tasks page.
                            .label(course2md::tr!("取消任务"))
                            .disabled(cancelling)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_task_intent(id.clone(), Intent::Cancel, cx);
                            })),
                    )
                    .context_menu(move |popup, window, cx| menu(popup, window, cx)),
            );
        }
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rems(20. / 14.))
                .child(
                    h_flex().min_h(rems(48. / 14.)).pl(rems(4. / 14.)).child(
                        theme::accessible_text(
                            "workbench-active-title",
                            course2md::tr!("正在处理"),
                        )
                        .role(Role::Heading)
                        .text_size(SECTION_TITLE)
                        .line_height(SECTION_TITLE_LINE)
                        .font_weight(FontWeight::NORMAL),
                    ),
                )
                .child(list),
        )
    }

    /// A queued or running task's live status: the status line (a spinner
    /// on its first line for unmeasured work) and, for measured work, a bar.
    pub(crate) fn task_status_lines(
        &self,
        task: &crate::workspace::TaskRecord,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        use crate::workspace::{Intent, TaskState};
        let id = &task.id;
        let queued = task.state == TaskState::Queued && !self.is_running_task(id);
        let cancelling = task.intent == Intent::Cancel;
        let progress = self.compact_task_progress(task);
        let status = if cancelling {
            course2md::tr!("正在取消…").to_owned()
        } else if task.state == TaskState::Pausing {
            course2md::tr!("正在保存进度…").to_owned()
        } else if queued {
            course2md::tr!("排队中").to_owned()
        } else {
            progress
                .as_ref()
                .map(|(label, _)| label.clone())
                .unwrap_or_else(|| {
                    if self.is_running_task(id) {
                        self.with_job_clock(course2md::tr!("正在准备…"))
                    } else {
                        course2md::tr!("正在准备…").into()
                    }
                })
        };
        // Measured work gets a bar under the status; unmeasured work a
        // spinner leading the status line, never one on a line of its own.
        let active = !queued && !cancelling;
        let fraction = progress.and_then(|(_, fraction)| fraction);
        let line = rems(20. / 14.);
        let mut lines = vec![
            h_flex()
                .min_w_0()
                .gap_2()
                .items_start()
                .when(active && fraction.is_none(), |row| {
                    // Centred on the first line when the status wraps.
                    row.child(h_flex().h(line).flex_shrink_0().items_center().child(
                        crate::motion::spinner(SharedString::from(format!("{key}-spinner")), cx),
                    ))
                })
                .child(
                    crate::ui::status_line(SharedString::from(format!("{key}-status")), &status)
                        .text_size(TEXT_BODY)
                        .line_height(line)
                        .text_color(color(GRAY)),
                )
                .into_any_element(),
        ];
        if let Some(fraction) = fraction.filter(|_| active) {
            lines.push(crate::motion::progress(
                SharedString::from(format!("{key}-progress")),
                fraction,
                window,
                cx,
            ));
        }
        lines
    }

    /// Workbench "最近笔记": attention tasks first, then recent readable notes.
    /// First use keeps a compact explanation below the source input.
    /// Notes waiting on the person stay listed even when recent notes are
    /// turned off; only finished notes and the empty state follow the setting.
    pub fn recent_notes_section(&self, cx: &mut Context<Self>) -> Option<Div> {
        let show_notes = self.desktop_settings.home_recent_notes;
        let linked = self.current_input_task(cx).map(|task| task.id.clone());
        let mut attention: Vec<_> = self
            .workspace
            .as_ref()
            .into_iter()
            .flat_map(|w| w.state.tasks.iter())
            .filter(|task| {
                Some(&task.id) != linked.as_ref()
                    && task.handled_by.is_none()
                    && (matches!(
                        task.state,
                        crate::workspace::TaskState::NeedsAttention
                            | crate::workspace::TaskState::Uncertain
                            | crate::workspace::TaskState::Paused
                    ) || (task.state == crate::workspace::TaskState::Partial
                        && task.artifact.is_some()
                        && !crate::task_ui::task_component_failures(task).is_empty()))
            })
            .collect::<Vec<_>>();
        attention.sort_by_key(|task| std::cmp::Reverse(task.updated));
        attention.truncate(2);
        let attention: Vec<crate::workspace::TaskRecord> = attention.into_iter().cloned().collect();
        let notes: Vec<Rc<Course>> = self
            .courses
            .iter()
            .filter(|course| {
                !attention
                    .iter()
                    .any(|task| task.artifact.as_ref() == Some(&course.dir))
            })
            .take(if show_notes { 4 } else { 0 })
            .cloned()
            .collect();
        if attention.is_empty() && !show_notes {
            return None;
        }
        if attention.is_empty() && notes.is_empty() {
            let access = self.cached_library_access();
            let checking = self.loading || access.is_none();
            let unavailable = !self.library_issues.is_empty()
                || access
                    .as_ref()
                    .is_some_and(|access| !access.unavailable.is_empty());
            let title = if checking {
                course2md::tr!("正在读取笔记库")
            } else if unavailable {
                course2md::tr!("暂时无法读取笔记库")
            } else {
                course2md::tr!("还没有笔记")
            };
            let description = if checking {
                None
            } else if unavailable {
                Some(course2md::tr!("检查保存位置后，即可继续查看已有笔记"))
            } else {
                Some(course2md::tr!("整理好的笔记会出现在这里"))
            };
            let mut empty = v_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .justify_center()
                .py(rems(40. / 14.))
                .px_4()
                .gap(rems(16. / 14.))
                .rounded(RADIUS_HERO)
                .bg(color(SURFACE))
                .child(
                    h_flex()
                        .size(rems(72. / 14.))
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(color(SECONDARY_CONTAINER))
                        .child(
                            icons::book_open()
                                .size(rems(32. / 14.))
                                .text_color(color(ON_SECONDARY_CONTAINER)),
                        ),
                )
                .child(
                    v_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(rems(22. / 14.))
                                .line_height(rems(28. / 14.))
                                .font_weight(FontWeight::NORMAL)
                                .child(title),
                        )
                        .children(description.map(|description| {
                            div()
                                .text_center()
                                .text_size(TEXT_BODY)
                                .text_color(color(GRAY))
                                .child(description)
                        })),
                );
            if unavailable && !checking {
                empty = empty.child(
                    text_button("recent-unavailable-library", cx)
                        .label(course2md::tr!("查看笔记库"))
                        .on_click(cx.listener(|this, _, _, cx| this.navigate(Page::Library, cx))),
                );
            } else if !checking {
                empty = empty.child(
                    h_flex().justify_center().flex_wrap().gap_3().children(
                        [
                            course2md::tr!("粘贴链接或拖入视频"),
                            course2md::tr!("读取字幕或识别语音"),
                            course2md::tr!("AI 校对并生成笔记"),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, label)| {
                            h_flex()
                                .gap_2()
                                .pl_1()
                                .pr_3()
                                .py_1()
                                .rounded(RADIUS_SMALL)
                                .border_1()
                                .border_color(color(HAIRLINE))
                                .text_size(TEXT_BODY)
                                .child(
                                    h_flex()
                                        .size(rems(24. / 14.))
                                        .justify_center()
                                        .items_center()
                                        .rounded_full()
                                        .bg(color(SECONDARY_CONTAINER))
                                        .text_color(color(ACCENT))
                                        .child((index + 1).to_string()),
                                )
                                .child(label)
                        }),
                    ),
                );
            }
            return Some(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rems(20. / 14.))
                    .child(
                        h_flex().min_h(rems(48. / 14.)).pl(rems(4. / 14.)).child(
                            theme::accessible_text("recent-title", course2md::tr!("最近的笔记"))
                                .role(Role::Heading)
                                .text_size(SECTION_TITLE)
                                .line_height(SECTION_TITLE_LINE)
                                .font_weight(FontWeight::NORMAL),
                        ),
                    )
                    .child(empty),
            );
        }
        let mut section = v_flex().w_full().min_w_0().gap(rems(20. / 14.));
        section = section.child(
            h_flex()
                .w_full()
                .min_h(rems(48. / 14.))
                .pl(rems(4. / 14.))
                .items_center()
                .gap(rems(12. / 14.))
                .child(
                    theme::accessible_text(
                        "recent-title",
                        if show_notes {
                            course2md::tr!("最近的笔记")
                        } else {
                            course2md::tr!("需要处理")
                        },
                    )
                    .role(Role::Heading)
                    .text_size(SECTION_TITLE)
                    .line_height(SECTION_TITLE_LINE)
                    .font_weight(FontWeight::NORMAL)
                    .flex_1(),
                )
                .child(
                    text_button("recent-all", cx)
                        .pl(rems(1.))
                        .pr(rems(8. / 14.))
                        .gap(rems(4. / 14.))
                        .label(course2md::tr!("笔记库"))
                        .child(crate::icons::chevron_right().size(rems(18. / 14.)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.folder_filter = crate::FolderFilter::All;
                            this.navigate(Page::Library, cx);
                        })),
                ),
        );
        // One surface; rows keep an 8px inset so their hover shape stays
        // inside its corners, and dividers start on the title column.
        let mut list = v_flex()
            .w_full()
            .min_w_0()
            .p(rems(8. / 14.))
            .rounded(RADIUS_HERO)
            .bg(color(SURFACE));
        let divider = || {
            div()
                .h(px(1.))
                .ml(rems(152. / 14.))
                .mr(rems(8. / 14.))
                .bg(color(HAIRLINE))
        };
        let has_attention = !attention.is_empty();
        for task in attention {
            let id = task.id.clone();
            let uncertain = task.state == crate::workspace::TaskState::Uncertain
                || task
                    .events
                    .blocked
                    .iter()
                    .any(|request| request.is_uncertain());
            let requires_repair = !uncertain && crate::task_ui::task_requires_service_repair(&task);
            let partial_components = if task.state == crate::workspace::TaskState::Partial {
                task.artifact
                    .as_ref()
                    .map(|_| {
                        crate::task_ui::task_component_failures(&task)
                            .into_iter()
                            .filter_map(|(component, _, _)| {
                                course2md::execution::Component::parse(&component)
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let mut actions = h_flex().gap_2().flex_wrap();
            if let Some(course) = task.artifact.as_ref().and_then(|path| {
                self.courses
                    .iter()
                    .find(|course| &course.dir == path)
                    .cloned()
            }) {
                actions = actions.child(
                    outline_pill(
                        SharedString::from(format!("recent-attention-read-{id}")),
                        cx,
                    )
                    .icon(icons::book_open())
                    .label(if self.course_has_reading_position(&course) {
                        course2md::tr!("继续阅读")
                    } else {
                        course2md::tr!("阅读笔记")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_course(course.clone(), cx);
                    })),
                );
            }
            if requires_repair {
                let repair_id = id.clone();
                actions = actions.child(
                    quiet(SharedString::from(format!("recent-repair-{id}")))
                        .icon(icons::settings())
                        .label(course2md::tr!("修复 AI 服务并补做"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.repair_task_service(repair_id.clone(), window, cx);
                        })),
                );
            } else if !uncertain && !partial_components.is_empty() {
                let retry_id = id.clone();
                let labels = partial_components
                    .iter()
                    .map(|component| component.as_str().to_owned())
                    .collect::<Vec<_>>();
                let label = crate::task_ui::retry_components_label(&labels);
                actions = actions.child(
                    quiet(SharedString::from(format!("recent-retry-{id}")))
                        .icon(icons::refresh())
                        .label(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reprocess_task(
                                retry_id.clone(),
                                partial_components.clone(),
                                Vec::new(),
                                cx,
                            );
                        })),
                );
            } else if !uncertain && task.state == crate::workspace::TaskState::Paused {
                let resume_id = id.clone();
                actions = actions.child(
                    outline_pill(SharedString::from(format!("recent-resume-{id}")), cx)
                        .icon(icons::play_arrow())
                        .label(course2md::tr!("继续生成"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_task_intent(
                                resume_id.clone(),
                                crate::workspace::Intent::Run,
                                cx,
                            );
                        })),
                );
            } else {
                actions = actions.child(
                    outline_pill(SharedString::from(format!("recent-task-{id}")), cx)
                        .icon(icons::task())
                        .label(course2md::tr!("查看任务"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // 统一走 navigate()：保存 New 草稿、阅读位置、跟随清理
                            this.select_task(&id, cx);
                            this.navigate(Page::Task, cx);
                        })),
                );
            }
            let summary = crate::task_ui::task_attention_summary(&task);
            list = list.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .py(rems(12. / 14.))
                    .px(rems(8. / 14.))
                    .gap_2()
                    // Same order as every task and note row: title, status, actions.
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            .items_center()
                            .flex_wrap()
                            .child(
                                div()
                                    .min_w(rems(12.))
                                    .flex_1()
                                    .whitespace_normal()
                                    .text_size(rems(16. / 14.))
                                    .line_height(rems(22. / 14.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(task.plan.title.clone()),
                            )
                            .child(badge(BadgeKind::Warning).child(task.state.label()))
                            .child(actions),
                    )
                    .when(summary != task.state.label(), |card| {
                        card.child(
                            div()
                                .text_sm()
                                .text_color(color(GRAY))
                                .whitespace_normal()
                                .child(summary),
                        )
                    }),
            );
        }
        for (index, course) in notes.iter().enumerate() {
            let selected = course.clone();
            let meta = self.course_meta(course);
            let duration = meta
                .source
                .as_ref()
                .and_then(|source| source.duration)
                .map(course2md::render::fmt_ts);
            let metadata = meta.short_line();
            if index > 0 || has_attention {
                list = list.child(divider());
            }
            let row = surface_row(control(("recent-note", index)), cx)
                .w_full()
                .h_auto()
                .py(rems(12. / 14.))
                .pl(rems(8. / 14.))
                .pr(rems(4. / 14.))
                .rounded(px(20.))
                .accessibility_label(course2md::tr!("阅读 {}", course.title))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap(rems(16. / 14.))
                        .child(
                            cover_frame(px(12.), course.thumbnail.clone())
                                .w(ROW_COVER)
                                .max_w(relative(ROW_COVER_SHARE))
                                .relative()
                                .when(course.thumbnail.is_none(), |cover| {
                                    cover.child(
                                        icons::file_text()
                                            .size(rems(24. / 14.))
                                            .text_color(color(ON_SECONDARY_CONTAINER)),
                                    )
                                })
                                .when_some(duration, |cover, duration| {
                                    cover.child(
                                        div()
                                            .absolute()
                                            .right(px(4.))
                                            .bottom(px(4.))
                                            .px(px(4.))
                                            .py(px(1.))
                                            .rounded(px(3.))
                                            .bg(color(INK))
                                            .text_color(color(CANVAS))
                                            .text_size(rems(10. / 14.))
                                            .font_weight(FontWeight::NORMAL)
                                            .child(duration),
                                    )
                                }),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(rems(4. / 14.))
                                .child(
                                    div()
                                        .w_full()
                                        .text_size(rems(16. / 14.))
                                        .line_height(rems(22. / 14.))
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(course.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(rems(13. / 14.))
                                        .font_weight(FontWeight::NORMAL)
                                        .text_color(color(GRAY))
                                        .child(metadata),
                                )
                                .when(course.incomplete(), |view| view.child(incomplete_badge())),
                        )
                        .child(
                            crate::icons::chevron_right()
                                .size(rems(20. / 14.))
                                .flex_shrink_0()
                                .text_color(color(GRAY)),
                        ),
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| this.open_course(selected.clone(), cx)),
                );
            let menu = self.course_menu(course, true, cx);
            list = list
                .child(
                    div()
                        .id(("recent-note-menu", index))
                        .w_full()
                        .child(row)
                        .context_menu(move |popup, window, cx| menu(popup, window, cx)),
                )
                .children(self.course_read_error(course, index, cx));
        }
        Some(section.child(list))
    }
}

/// Whether an unfinished task is still writing a version of the note with
/// `source_id` into the library `library_id`. Tasks name their library by
/// `LibraryLocation::id`, never by its path.
pub(crate) fn note_has_unfinished_task(
    tasks: &[crate::workspace::TaskRecord],
    library_id: &str,
    source_id: Option<&str>,
) -> bool {
    source_id.is_some_and(|source| {
        tasks.iter().any(|task| {
            !task.state.finished()
                && task.plan.source_id == source
                && task.plan.library_id == library_id
        })
    })
}

/// "找到 1 篇笔记" / "找到 {} 篇笔记", with the singular English form.
fn found_notes(count: usize) -> String {
    if count == 1 {
        course2md::tr!("找到 1 篇笔记").to_owned()
    } else {
        course2md::tr!("找到 {} 篇笔记", count)
    }
}

#[cfg(test)]
mod tests {
    use super::{LibraryGroupBody, LibraryGroupHeader, LibraryItem, LibraryRow};
    use crate::notes::Course;
    use std::time::SystemTime;

    #[test]
    fn a_note_with_a_running_task_is_not_trashed() {
        use crate::workspace::{Intent, TaskPlan, TaskRecord, TaskState};
        let task = |state: TaskState| TaskRecord {
            id: "task-1".into(),
            plan: TaskPlan {
                operation: Default::default(),
                source: Default::default(),
                source_id: "video-a".into(),
                title: "Course A".into(),
                library_id: "library-1".into(),
                folder: None,
                subtitle: None,
                config: Default::default(),
                asr_service: None,
                ai_service: None,
            },
            state,
            intent: Intent::Run,
            created: 0,
            updated: 0,
            parent: None,
            handled_by: None,
            work_dir: "/tmp/work".into(),
            events: crate::activity::JobEvents {
                error: None,
                ..Default::default()
            },
            artifact: None,
            unread: false,
            resend: vec![],
        };
        let busy = super::note_has_unfinished_task;
        let running = [task(TaskState::Running)];
        assert!(busy(&running, "library-1", Some("video-a")));
        // The old guard compared the id with the library's root path.
        assert!(!busy(&running, "/Users/someone/Notes", Some("video-a")));
        assert!(!busy(&running, "library-2", Some("video-a")));
        assert!(!busy(&running, "library-1", Some("video-b")));
        assert!(!busy(&running, "library-1", None));
        assert!(!busy(
            &[task(TaskState::Complete)],
            "library-1",
            Some("video-a")
        ));
    }

    fn course(dir: &str) -> Course {
        Course {
            dir: dir.into(),
            title: dir.into(),
            modified: SystemTime::UNIX_EPOCH,
            slides: 0,
            thumbnail: None,
            manifest: None,
            warning: None,
        }
    }

    #[test]
    fn library_item_keys_follow_content_identity_and_columns() {
        let all = [course("/lib/a"), course("/lib/b"), course("/lib/c")].map(std::rc::Rc::new);
        let row = |columns| LibraryRow::cards(&all, &[0, 1], columns).remove(0);
        let items = [
            LibraryItem::Coverage("m".into()),
            LibraryItem::CardRow(row(3)),
            LibraryItem::ListRow(LibraryRow::list(&all, 0, true, false)),
            LibraryItem::GroupHeader(Box::new(LibraryGroupHeader {
                index: 0,
                key: "lib:0".into(),
                name: "未分类".into(),
                count: 2,
                collapsed: false,
            })),
            LibraryItem::GroupBody(Box::new(LibraryGroupBody {
                index: 0,
                key: "lib:0".into(),
                collapsed: false,
                courses: vec![0, 1],
            })),
        ];
        let keys: Vec<String> = items.iter().map(LibraryItem::key).collect();
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len(), "library rows need distinct keys");
        // Regrouping for a new column count replaces the affected rows.
        assert_ne!(row(3).key, row(2).key);
        // Content edits keep the row identity keyed by its first course; the
        // row re-measures when it is visible.
        assert_eq!(LibraryRow::cards(&all, &[0, 2], 3)[0].key, row(3).key);
    }
}
