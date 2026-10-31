//! The note library and the workbench's lists of notes and tasks.
use super::*;
use crate::folder_tree::{Command, NoteDrag, command_menu, command_popup, note_count};
use crate::library_ui::{Moving, NameTarget};
use crate::{symbols, ui};
use gpui_kit::component::{menu::ContextMenuExt, scroll::Scrollbar};
use gpui_kit_material::{
    color::ColorRole,
    icons::{Icon, SymbolIcon},
    motion,
    prelude::{
        ButtonIconPosition, ButtonSize, ButtonVariant, CardVariant, ConnectedButtonGroup,
        FilterChip, ITEM_INSET, IconButton, IconButtonVariant, ListDensity, ListItem, ListLeading,
        MaterialCard, MaterialSideSheet, MaterialTooltip, NoticeKind, RADIUS_SMALL, SearchField,
        StatusKind, TEXT_AUX, TEXT_BODY, TILE_GAP, TileScale, Tint, TypeRole, accessible_text,
        color, disclosure_chevron, list_corners, material_button, notice_banner, page_heading,
        progress, rem, spinner, status_chip, tile, tinted_icon,
    },
    tokens::shape::OuterCorners,
};
use std::sync::Arc;

/// Space between note cards and between the rows of their grid.
const GRID_GAP: f32 = 12.;
/// A grid column is at least this wide; both scale with text.
const CARD_MIN_WIDTH: f32 = 240.;
/// The scrollbar's lane beside the notes, inside the page's gutter.
const SCROLLBAR_GUTTER: f32 = 16.;
/// Cover column of a note or task row; narrow windows shrink it to a share of the row.
const ROW_COVER: f32 = 128.;
const ROW_COVER_SHARE: f32 = 0.28;

/// A 16:9 cover: the image, or a tonal placeholder with `icon`.
fn cover(image: Option<PathBuf>, icon: SymbolIcon) -> Div {
    div()
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .w_full()
        .aspect_ratio(16. / 9.)
        .overflow_hidden()
        .bg(color(ColorRole::SECONDARY_CONTAINER))
        .child(match image {
            Some(path) => img(path)
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => Icon::from(icon)
                .size(rem(24.))
                .color(color(ColorRole::ON_SECONDARY_CONTAINER))
                .into_any_element(),
        })
}

/// The cover of a note or task row.
fn row_cover(image: Option<PathBuf>, icon: SymbolIcon) -> Div {
    cover(image, icon)
        .w(rem(ROW_COVER))
        .max_w(relative(ROW_COVER_SHARE))
        .flex_shrink_0()
        .rounded(RADIUS_SMALL)
}

/// A note picked up to file into a folder of the tree.
fn note_drag(course: &Course) -> NoteDrag {
    NoteDrag {
        storage: course.storage_dir(),
        title: course.title.clone().into(),
    }
}

/// One row of the virtualized library page. Folder groups stay one item per
/// disclosure, so a group arrives as one unit.
enum LibraryItem {
    /// Work in progress, with its message.
    Status(&'static str),
    Recovery(crate::storage::LibraryAccess),
    Coverage(String),
    Issues(Vec<String>),
    Empty,
    /// The folders inside the selected folder.
    Folders(std::path::PathBuf, Option<u64>),
    GroupHeader {
        key: String,
        name: String,
        count: usize,
        collapsed: bool,
    },
    GroupBody {
        key: String,
        collapsed: bool,
        courses: Vec<usize>,
    },
    /// One row of cards: notes as indices into `Desktop::courses`.
    Cards {
        key: String,
        courses: Vec<usize>,
    },
    /// One note; `outer` places it in the connected list.
    Note {
        key: String,
        course: usize,
        outer: OuterCorners,
    },
}

impl LibraryItem {
    /// Identity for the list splice: stable while the row means the same
    /// content, so measured heights and the scroll anchor survive updates.
    fn key(&self) -> String {
        match self {
            LibraryItem::Status(message) => format!("status-{message}"),
            LibraryItem::Recovery(_) => "recovery".to_owned(),
            LibraryItem::Coverage(_) => "coverage".to_owned(),
            LibraryItem::Issues(_) => "issues".to_owned(),
            LibraryItem::Empty => "empty".to_owned(),
            LibraryItem::Folders(root, parent) => {
                format!("folders-{}-{}", root.display(), parent.unwrap_or(0))
            }
            LibraryItem::GroupHeader { key, .. } => format!("group-h-{key}"),
            LibraryItem::GroupBody { key, .. } => format!("group-b-{key}"),
            LibraryItem::Cards { key, .. } | LibraryItem::Note { key, .. } => key.clone(),
        }
    }

    /// The rows of a run of card or list notes.
    fn notes(all: &[Rc<Course>], courses: &[usize], columns: Option<usize>) -> Vec<Self> {
        let Some(columns) = columns else {
            let count = courses.len();
            return courses
                .iter()
                .enumerate()
                .map(|(position, &course)| {
                    let outer = list_corners(position, count);
                    LibraryItem::Note {
                        // The surface's rounded ends are part of a row's identity.
                        key: format!("note-{}-{outer:?}", all[course].dir.display()),
                        course,
                        outer,
                    }
                })
                .collect();
        };
        courses
            .chunks(columns)
            .map(|row| LibraryItem::Cards {
                key: format!("row-{columns}-{}-{}", row.len(), all[row[0]].dir.display()),
                courses: row.to_vec(),
            })
            .collect()
    }
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
        let id = &self.id;
        let state = window.use_keyed_state(
            SharedString::from(format!("library-diagnostics-state:{id}")),
            cx,
            |_, _| false,
        );
        let open = *state.read(cx);
        let messages = self.messages.iter().enumerate().map(|(index, message)| {
            TileScale::Regular.support(
                SharedString::from(format!("library-diagnostic:{id}:{index}")),
                message.clone(),
            )
        });
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(8.))
            .child(
                material_button(
                    SharedString::from(format!("library-diagnostics-toggle:{id}")),
                    ButtonVariant::Text,
                )
                .self_start()
                .icon(if open {
                    symbols::keyboard_arrow_up()
                } else {
                    symbols::info()
                })
                .child(if open {
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
            .child(motion::disclosure(
                SharedString::from(format!("library-diagnostics-content:{id}")),
                open,
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(8.))
                    .p(rem(12.))
                    .bg(color(ColorRole::INSET))
                    .rounded(RADIUS_SMALL)
                    .children(messages),
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

/// A note or task's commands behind a ⋮ button, as one element so it can reach the window.
#[derive(gpui::IntoElement)]
pub(crate) struct CommandsButton {
    id: ElementId,
    /// The tooltip, and what the button is called; `subject` says whose commands.
    label: SharedString,
    subject: Option<SharedString>,
    commands: Vec<Command>,
    tonal: bool,
}

impl RenderOnce for CommandsButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let button = ElementId::NamedChild(Arc::new(self.id.clone()), "button".into());
        let tip = ElementId::NamedChild(Arc::new(self.id.clone()), "tip".into());
        let items = ElementId::NamedChild(Arc::new(self.id.clone()), "items".into());
        ui::menu(
            self.id,
            MaterialTooltip::plain(tip, self.label.clone()).trigger(
                IconButton::new(
                    button,
                    symbols::more_vert(),
                    self.subject.unwrap_or(self.label),
                )
                .when(self.tonal, |button| {
                    button.variant(IconButtonVariant::Tonal)
                }),
            ),
            command_menu(items, self.commands),
            window,
            cx,
        )
        .anchor(Anchor::TopRight)
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
        let storage = course.storage_dir();
        self.begin_naming(NameTarget::Note { root, storage }, window, cx);
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
        ui::Confirm::new(
            course2md::tr!("删除笔记"),
            course2md::tr!("保留笔记"),
            course2md::tr!("移到{}", trash),
        )
        .danger()
        .text(course2md::tr!(
            "《{}》将移到{}，可以从那里恢复。",
            title,
            trash
        ))
        .ask(
            cx.listener(move |this, _, _, cx| this.delete_course(root.clone(), course.clone(), cx)),
            window,
            cx,
        );
    }

    fn delete_course(&mut self, root: PathBuf, course: Course, cx: &mut Context<Self>) {
        let title = self.course_display_title(&course);
        let dir = course.storage_dir();
        let worker_dir = dir.clone();
        self.in_background(
            move || organize::delete_course(&root, &worker_dir),
            move |this, result, cx| match result {
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
            },
            cx,
        );
    }

    /// Every note offers the same commands wherever it appears, ordered
    /// 打开 → 处理 → 管理: behind its ⋮ button and on right-click.
    fn note_commands(&self, course: &Rc<Course>, cx: &mut Context<Self>) -> Vec<Command> {
        let desktop = cx.weak_entity();
        let path = course.storage_dir();
        let link = self
            .note_source(course)
            .and_then(|source| source.link.clone());
        let on_course =
            |label: &'static str,
             icon,
             run: fn(&mut Desktop, Rc<Course>, &mut Window, &mut Context<Desktop>)| {
                let course = course.clone();
                Command::new(label, icon, &desktop, move |this, window, cx| {
                    run(this, course.clone(), window, cx)
                })
            };
        let export = |label: &'static str, format: course2md::config::OutputFormat| {
            let course = course.clone();
            Command::new(
                label,
                symbols::download(),
                &desktop,
                move |this, window, cx| this.export_course(course.clone(), format, window, cx),
            )
        };
        let mut commands = vec![
            on_course(
                course2md::tr!("阅读笔记"),
                symbols::menu_book(),
                |this, course, _, cx| this.open_course(course, cx),
            ),
            Command::global(
                course2md::tr!("打开笔记保存位置"),
                symbols::folder_open(),
                move |_, cx| cx.open_with_system(&path),
            ),
        ];
        commands.extend(link.map(|link| {
            Command::global(
                course2md::tr!("复制原视频链接"),
                symbols::link(),
                move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(link.clone())),
            )
        }));
        if let Some(last) = commands.pop() {
            commands.push(last.ends_group());
        }
        use course2md::config::OutputFormat;
        commands.extend([
            export(course2md::tr!("导出为 Markdown 包…"), OutputFormat::Md),
            export(course2md::tr!("导出为网页文件…"), OutputFormat::Html),
            export(course2md::tr!("导出为结构化数据…"), OutputFormat::Json).ends_group(),
            on_course(
                course2md::tr!("移动到…"),
                symbols::drive_file_move(),
                |this, course, _, cx| {
                    let root = this.folder_context(Some(course.dir.clone())).root;
                    this.begin_move(root, Moving::Note(course.storage_dir()), cx)
                },
            ),
            on_course(
                course2md::tr!("重命名笔记…"),
                symbols::edit(),
                |this, course, window, cx| {
                    this.begin_course_rename(Course::clone(&course), window, cx)
                },
            ),
            on_course(
                course2md::tr!("删除笔记…"),
                symbols::delete(),
                |this, course, window, cx| {
                    this.confirm_course_delete(Course::clone(&course), window, cx)
                },
            )
            .danger(),
        ]);
        commands
    }

    fn note_commands_button(
        &self,
        course: &Rc<Course>,
        index: usize,
        tonal: bool,
        cx: &mut Context<Self>,
    ) -> CommandsButton {
        CommandsButton {
            id: ("note-commands", index).into(),
            label: course2md::tr!("笔记操作").into(),
            subject: Some(course2md::tr!("《{}》的笔记操作", course.title).into()),
            commands: self.note_commands(course, cx),
            tonal,
        }
    }

    fn recover_library_folders(&mut self, root: PathBuf, rebuild: bool, cx: &mut Context<Self>) {
        let worker_root = root.clone();
        self.in_background(
            move || organize::recover(&worker_root, rebuild),
            move |this, result, cx| match result {
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
                    this.message = Some(course2md::tr!("文件夹尚未恢复：{}", format!("{error:#}")))
                }
            },
            cx,
        );
    }

    fn recover_note_titles(&mut self, root: PathBuf, reset: bool, cx: &mut Context<Self>) {
        self.in_background(
            move || organize::recover_titles(&root, reset),
            |this, result, cx| match result {
                Ok(_) => {
                    this.message = Some(course2md::tr!("笔记名称已恢复，原损坏文件已另存").into());
                    this.refresh_library(cx);
                }
                Err(error) => {
                    this.message = Some(course2md::tr!("名称尚未恢复：{}", format!("{error:#}")))
                }
            },
            cx,
        );
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

    /// One library's obstacle: what is wrong, what can be done and the details.
    fn recovery_notice(
        id: SharedString,
        kind: NoticeKind,
        message: String,
        actions: Vec<AnyElement>,
        details: Option<IssueDetails>,
    ) -> Div {
        notice_banner(id, kind, message)
            .children(actions)
            .children(details)
    }

    fn library_recovery_view(
        &self,
        access: &crate::storage::LibraryAccess,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let workspace = self.workspace.as_ref()?;
        let details = |prefix: &str, id: &str, kind, root: &std::path::Path| {
            let messages = self.library_issue_messages(root, kind);
            (!messages.is_empty()).then(|| IssueDetails::new(format!("{prefix}:{id}"), messages))
        };
        let mut notices = Vec::new();
        for (index, location) in workspace.state.libraries.iter().enumerate() {
            if access.unavailable.contains(&location.root) {
                notices.push(Self::recovery_notice(
                    format!("library-unavailable-{index}").into(),
                    NoticeKind::Info,
                    course2md::tr!("{} 暂时无法访问", location.name),
                    vec![
                        material_button(("retry-library-location", index), ButtonVariant::Tonal)
                            .icon(symbols::refresh())
                            .child(course2md::tr!("重新检查保存位置"))
                            .loading(self.loading, course2md::tr!("重新检查保存位置"))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx)))
                            .into_any_element(),
                    ],
                    None,
                ));
                continue;
            }
            if let Some(recovery) = self.library_view_cache.recovery.get(&location.root) {
                let mut actions = Vec::new();
                let rebuild = location.root.clone();
                if recovery.has_backup {
                    let restore = location.root.clone();
                    actions.push(
                        material_button(("restore-classification", index), ButtonVariant::Tonal)
                            .icon(symbols::history())
                            .child(course2md::tr!("恢复最近备份"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.recover_library_folders(restore.clone(), false, cx)
                            }))
                            .into_any_element(),
                    );
                }
                actions.push(
                    material_button(
                        ("rebuild-classification", index),
                        if recovery.has_backup {
                            ButtonVariant::Text
                        } else {
                            ButtonVariant::Tonal
                        },
                    )
                    .icon(symbols::refresh())
                    .child(course2md::tr!("重新开始整理（保留损坏文件）"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.recover_library_folders(rebuild.clone(), true, cx)
                    }))
                    .into_any_element(),
                );
                let folder = location.root.clone();
                actions.push(
                    material_button(("show-classification-files", index), ButtonVariant::Text)
                        .icon(symbols::folder_open())
                        .child(course2md::tr!("打开保存位置"))
                        .on_click(move |_, _, cx| cx.open_with_system(&folder))
                        .into_any_element(),
                );
                notices.push(Self::recovery_notice(
                    format!("classification-recovery-{index}").into(),
                    NoticeKind::Danger,
                    course2md::tr!("{} 的文件夹信息已损坏", location.name),
                    actions,
                    details(
                        "folders",
                        &location.id,
                        LibraryIssueKind::Folders,
                        &location.root,
                    ),
                ));
            }
            if let Some(recovery) = self.library_view_cache.title_recovery.get(&location.root) {
                let actions = [false, true]
                    .into_iter()
                    .filter(|reset| *reset || recovery.has_backup)
                    .map(|reset| {
                        let root = location.root.clone();
                        material_button(
                            (
                                if reset {
                                    "reset-note-names"
                                } else {
                                    "restore-note-names"
                                },
                                index,
                            ),
                            ButtonVariant::Tonal,
                        )
                        .icon(if reset {
                            symbols::refresh()
                        } else {
                            symbols::history()
                        })
                        .child(if reset {
                            course2md::tr!("恢复为原始标题")
                        } else {
                            course2md::tr!("恢复最近名称备份")
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.recover_note_titles(root.clone(), reset, cx)
                        }))
                        .into_any_element()
                    })
                    .collect();
                notices.push(Self::recovery_notice(
                    format!("title-recovery-{index}").into(),
                    NoticeKind::Danger,
                    course2md::tr!("{} 的笔记名称记录无法读取", location.name),
                    actions,
                    details(
                        "names",
                        &location.id,
                        LibraryIssueKind::Titles,
                        &location.root,
                    ),
                ));
            }
        }
        (!notices.is_empty()).then(|| v_flex().gap(rem(12.)).children(notices))
    }

    pub fn library_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rem_px = f32::from(window.rem_size());
        let scale = rem_px / 14.;
        let controls = self.library_controls_visible(cx);
        let sidebar = controls && self.folder_sidebar_fits(window);
        let content = crate::views::shell_content_width(Page::Library, window)
            - if sidebar {
                (crate::folder_tree::FOLDER_SIDEBAR + crate::folder_tree::FOLDER_SIDEBAR_GAP)
                    * scale
            } else {
                0.
            };
        // Narrow windows always list notes.
        let columns = (self.desktop_settings.library_cards && content >= 528. * scale).then(|| {
            let gap = GRID_GAP * scale;
            (((content + gap) / (CARD_MIN_WIDTH * scale + gap)).floor() as usize).max(1)
        });
        let (items, count) = self.library_items(columns, cx);
        let count = count.filter(|_| controls);
        let notes = self.library_notes(items, columns, rem_px, cx);
        let pane = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .gap(rem(20.))
            .child(self.library_header(count, cx))
            .children(controls.then(|| self.library_toolbar(sidebar, columns.is_some(), cx)))
            .child(notes);
        v_flex()
            .size_full()
            .min_h_0()
            .pt(rem(16.))
            .child(
                h_flex()
                    .size_full()
                    .min_h_0()
                    .items_start()
                    .gap(rem(crate::folder_tree::FOLDER_SIDEBAR_GAP))
                    .when(sidebar, |page| page.child(self.folder_sidebar(cx)))
                    .child(pane),
            )
            .when(controls && !sidebar, |page| {
                page.child(self.places_sheet(cx))
            })
            .into_any_element()
    }

    /// What the library list shows for the current place and search, and how many
    /// notes that is.
    fn library_items(
        &self,
        columns: Option<usize>,
        cx: &mut Context<Self>,
    ) -> (Vec<LibraryItem>, Option<String>) {
        let query = self.value(Field::Search, cx).to_lowercase();
        let mut items = Vec::new();
        let Some(all_access) = self.cached_library_access() else {
            items.push(LibraryItem::Status(course2md::tr!("正在检查保存位置…")));
            return (items, None);
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
                    FolderFilter::All => true,
                    filter => {
                        let folder = self.course_folder(course);
                        location
                            .as_ref()
                            .is_some_and(|location| location.root == self.library_root)
                            && (!self.library_indexes.contains_key(&self.library_root)
                                || match filter {
                                    // Searching a place covers everything below it, as
                                    // in a file manager; browsing shows its own notes.
                                    FolderFilter::Unfiled => folder.is_none(),
                                    FolderFilter::Folder(id) => match &searched {
                                        Some(inside) => folder.is_some_and(|f| inside.contains(&f)),
                                        None => folder == Some(id),
                                    },
                                    FolderFilter::All => true,
                                })
                    }
                };
                matches_folder.then_some(index)
            })
            .collect();
        if self.library_needs_recovery(&all_access) {
            items.push(LibraryItem::Recovery(all_access.clone()));
        }
        use crate::storage::LibraryCoverage;
        match coverage {
            LibraryCoverage::Unavailable if query.is_empty() => items.push(LibraryItem::Coverage(
                course2md::tr!("保存位置暂时无法访问").to_owned(),
            )),
            LibraryCoverage::Unavailable => items.push(LibraryItem::Coverage(
                course2md::tr!("保存位置暂时无法访问，未能搜索").to_owned(),
            )),
            // 未连接的位置各有恢复卡片；仅搜索时提示结果不完整
            LibraryCoverage::Partial if !query.is_empty() => items.push(LibraryItem::Coverage(
                course2md::tr!("另有 {} 个位置未纳入搜索", scope.unavailable.len()),
            )),
            _ => {}
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
        if !remaining_issues.is_empty() && coverage != LibraryCoverage::Unavailable {
            items.push(LibraryItem::Issues(remaining_issues));
        }
        // 刷新期间保留已加载的列表：Loading 只是一行状态，不再整页替换
        if self.loading {
            items.push(LibraryItem::Status(course2md::tr!("正在读取笔记…")));
        }
        if coverage == LibraryCoverage::Unavailable || (self.loading && courses.is_empty()) {
            return (items, None);
        }
        // Inside a folder, its folders come before its notes.
        let folders = self.folder_filter.folder_id().map(Some).filter(|parent| {
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
        } else if let FolderFilter::Folder(id) = self.folder_filter {
            note_count(self.folder_note_count(&self.library_root, id))
        } else {
            note_count(courses.len())
        });
        if let Some(parent) = folders {
            items.push(LibraryItem::Folders(self.library_root.clone(), parent));
        }
        if courses.is_empty() {
            let obstructed = query.is_empty()
                && (coverage == LibraryCoverage::Partial || !self.library_issues.is_empty());
            if folders.is_none() && !obstructed {
                items.push(LibraryItem::Empty);
            }
        } else if self.desktop_settings.library_group_folders
            && self.folder_filter.is_all()
            && self.library_error.is_none()
        {
            self.push_groups(&mut items, courses, query.is_empty());
        } else {
            items.extend(LibraryItem::notes(&self.courses, &courses, columns));
        }
        (items, count)
    }

    /// The notes grouped by folder, each group its header and its body.
    fn push_groups(&self, items: &mut Vec<LibraryItem>, courses: Vec<usize>, collapsible: bool) {
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
        let state = self.workspace.as_ref().map(|workspace| &workspace.state);
        for ((root, id), entries) in groups {
            let location = state.and_then(|s| s.libraries.iter().find(|lib| lib.root == root));
            let key = format!(
                "{}:{id}",
                location
                    .map(|lib| lib.id.clone())
                    .unwrap_or_else(|| root.display().to_string())
            );
            let collapsed = collapsible && state.is_some_and(|s| s.collapsed.contains(&key));
            let folder_name = match self.library_indexes.get(&root) {
                Some(library) if library.folders.contains_key(&id) => library.path(id).join(" / "),
                Some(_) => course2md::tr!("未分类").into(),
                None => course2md::tr!("文件夹暂不可用").into(),
            };
            let name = if state.is_some_and(|s| s.libraries.len() > 1) {
                let library = location
                    .map(|lib| lib.name.as_str())
                    .unwrap_or(course2md::tr!("笔记库"));
                format!("{library} · {folder_name}")
            } else {
                folder_name
            };
            items.push(LibraryItem::GroupHeader {
                key: key.clone(),
                name,
                count: entries.len(),
                collapsed,
            });
            items.push(LibraryItem::GroupBody {
                key,
                collapsed,
                courses: entries,
            });
        }
    }

    fn toggle_group(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(workspace) = &mut self.workspace
            && let Err(error) = workspace.transaction(|state| {
                if !state.collapsed.remove(key) {
                    state.collapsed.insert(key.to_owned());
                }
                Ok(())
            })
        {
            self.workspace_error = Some(course2md::tr!(
                "分组展开状态尚未保存：{}",
                format!("{error:#}")
            ));
        }
        self.entered.remove(&format!("folder-disclosure-{key}"));
        cx.notify();
    }

    /// The narrow window's folder navigation: the same tree, in a sheet.
    fn places_sheet(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let desktop = cx.weak_entity();
        MaterialSideSheet::new("library-places-sheet")
            .modal(true)
            .open(self.places_open)
            .headline(course2md::tr!("文件夹"))
            .close_button(course2md::tr!("关闭"))
            .on_dismiss(move |_, cx| {
                let _ = desktop.update(cx, |this, cx| {
                    this.places_open = false;
                    cx.notify();
                });
            })
            .child(
                div()
                    .pl(rem(8.))
                    .pr(rem(16.))
                    .child(self.folder_tree(ColorRole::SURFACE_CONTAINER_LOW, cx)),
            )
    }

    /// The selected place's name and item count, its folder commands and
    /// the page's main action.
    fn library_header(&self, count: Option<String>, cx: &mut Context<Self>) -> Div {
        let (place, library) = self.library_place();
        let supporting = match (library, count) {
            (Some(library), Some(count)) => Some(format!("{library} · {count}")),
            (Some(library), None) => Some(library),
            (None, count) => count,
        };
        let folder_commands = self.folder_filter.folder_id().map(|id| CommandsButton {
            id: "manage-folder".into(),
            label: course2md::tr!("文件夹操作").into(),
            subject: None,
            commands: self.folder_commands(&self.library_root.clone(), id, cx),
            tonal: false,
        });
        h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap(rem(16.))
            .flex_wrap()
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w(rem(160.))
                    .gap(rem(2.))
                    .child(page_heading("page-title", place))
                    .children(
                        supporting
                            .map(|text| TileScale::Regular.support("library-place-count", text)),
                    ),
            )
            .children(folder_commands)
            .when(self.library_controls_visible(cx), |row| {
                row.child(
                    material_button("library-new-note", ButtonVariant::Tonal)
                        .icon(symbols::add())
                        .child(course2md::tr!("导入视频"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.prepare_workbench_input(window, cx) {
                                this.navigate(Page::New, cx);
                            }
                        })),
                )
            })
    }

    /// The library's variable-height list: notices, folder groups and card rows share
    /// one scroll region, spliced by identity so measured heights and the scroll
    /// anchor survive content updates.
    fn library_notes(
        &mut self,
        items: Vec<LibraryItem>,
        columns: Option<usize>,
        rem_px: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let keys = items.iter().map(LibraryItem::key).collect();
        let first_items = self.library_keys.is_empty();
        Self::reconcile_list_items(
            &self.library_list,
            &mut self.library_keys,
            &mut self.library_focus,
            keys,
            cx,
        );
        // A scroll offset set while the list was empty would land past these items.
        if first_items {
            self.library_list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
        }
        if self.library_rem != rem_px {
            self.library_list.remeasure();
            self.library_rem = rem_px;
        }
        let focus: Rc<Vec<FocusHandle>> = Rc::new(self.library_focus.clone());
        let state = self.library_list.clone();
        let items = Rc::new(items);
        let desktop = cx.weak_entity();
        let element = list(state.clone(), move |index, _window, cx| {
            let Some(item) = items.get(index) else {
                return div().into_any_element();
            };
            let last = index + 1 == items.len();
            let focus = focus.get(index).cloned();
            desktop
                .update(cx, |this, cx| {
                    this.library_item(item, index, last, focus, columns, cx)
                })
                .unwrap_or_else(|_| div().into_any_element())
        })
        .size_full()
        .pb(rem(24.));
        // The scrollbar sits in the gutter, clear of the rows' trailing actions.
        motion::enter(
            ("library-view", usize::from(columns.is_some())),
            div()
                .relative()
                .w_full()
                .flex_1()
                .min_h_0()
                .child(element)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(rem(-SCROLLBAR_GUTTER))
                        .w(rem(SCROLLBAR_GUTTER))
                        .child(Scrollbar::vertical(&state).viewport_from_layout()),
                ),
        )
    }

    fn library_item(
        &mut self,
        item: &LibraryItem,
        index: usize,
        last: bool,
        focus: Option<FocusHandle>,
        columns: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = match item {
            LibraryItem::Status(message) => ui::async_status(
                ("library-status", index),
                &ui::AsyncStatus::Busy((*message).into()),
                None,
                cx,
            )
            .unwrap_or_else(div)
            .py(rem(24.))
            .into_any_element(),
            LibraryItem::Recovery(access) => self
                .library_recovery_view(access, cx)
                .unwrap_or_else(div)
                .into_any_element(),
            LibraryItem::Coverage(message) => TileScale::Regular
                .support("library-search-coverage", message.clone())
                .into_any_element(),
            LibraryItem::Issues(messages) => notice_banner(
                "library-unread-content",
                NoticeKind::Danger,
                course2md::tr!("部分内容暂未读取"),
            )
            .child(
                material_button("retry-unread-library-content", ButtonVariant::Tonal)
                    .icon(symbols::refresh())
                    .child(course2md::tr!("重新检查"))
                    .loading(self.loading, course2md::tr!("重新检查"))
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx))),
            )
            .child(IssueDetails::new("unread-content", messages.clone()))
            .into_any_element(),
            LibraryItem::Empty => self.library_empty(cx).into_any_element(),
            LibraryItem::Folders(root, parent) => self
                .folder_rows(root, *parent, columns.unwrap_or(1), cx)
                .unwrap_or_else(div)
                .into_any_element(),
            LibraryItem::GroupHeader {
                key,
                name,
                count,
                collapsed,
            } => {
                let key = key.clone();
                ListItem::new(("library-group", index), name.clone())
                    .density(ListDensity::Compact)
                    .container(ColorRole::SURFACE_CONTAINER)
                    .segment([true; 4])
                    .leading(ListLeading::Icon(disclosure_chevron(!collapsed)))
                    .trailing_text(count.to_string())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(&key, cx)))
                    .into_any_element()
            }
            LibraryItem::GroupBody {
                key,
                collapsed,
                courses,
            } => {
                let animate = !collapsed && self.enter_once(format!("folder-disclosure-{key}"));
                let rows = self.note_collection(courses, columns, cx);
                if *collapsed {
                    div().hidden().into_any_element()
                } else if animate {
                    motion::disclosure(("folder-disclosure", index), true, rows)
                } else {
                    rows.into_any_element()
                }
            }
            LibraryItem::Cards { courses, .. } => self
                .note_grid_row(courses, columns.unwrap_or(1), cx)
                .into_any_element(),
            LibraryItem::Note { course, outer, .. } => self.note_row(*course, *outer, cx),
        };
        // Each item carries its trailing spacing as padding: the virtual list
        // measures an item's padding but not its outer margin.
        let wrapper = v_flex().w_full().min_w_0();
        let wrapper = match item {
            _ if last => wrapper,
            LibraryItem::GroupHeader { .. } => wrapper.pb(rem(8.)),
            LibraryItem::Note { .. } => wrapper.pb(TILE_GAP),
            LibraryItem::Cards { .. } => wrapper.pb(rem(GRID_GAP)),
            _ => wrapper.pb(rem(24.)),
        };
        wrapper
            .child(content)
            .id(("library-item", index))
            .tab_stop(false)
            .when_some(focus, |wrapper, focus| wrapper.track_focus(&focus))
            .into_any_element()
    }

    /// What an empty place says, and the way forward from it.
    fn library_empty(&self, cx: &mut Context<Self>) -> Div {
        let query = self.value(Field::Search, cx);
        let (icon, heading) = match self.folder_filter {
            _ if !query.is_empty() => (symbols::search(), course2md::tr!("没有匹配的笔记")),
            FolderFilter::All => (symbols::book(), course2md::tr!("还没有笔记")),
            FolderFilter::Unfiled => (symbols::inbox(), course2md::tr!("没有未分类的笔记")),
            FolderFilter::Folder(_) => (symbols::folder(), course2md::tr!("这个文件夹还没有笔记")),
        };
        let action = if !query.is_empty() {
            material_button("clear-search", ButtonVariant::Outlined)
                .icon(symbols::close())
                .child(course2md::tr!("清除搜索"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.inputs[&Field::Search]
                        .update(cx, |state, cx| state.set_value("", window, cx));
                    cx.notify();
                }))
        } else if self.folder_filter.is_all() {
            material_button("empty-library-add", ButtonVariant::Filled)
                .icon(symbols::add())
                .child(course2md::tr!("导入视频"))
                .on_click(cx.listener(|this, _, window, cx| this.begin_add(window, cx)))
        } else {
            material_button("empty-folder-all-notes", ButtonVariant::Outlined)
                .icon(symbols::book())
                .child(course2md::tr!("浏览全部笔记"))
                .on_click(cx.listener(|this, _, _, cx| {
                    let root = this.library_root.clone();
                    this.select_library_scope(root, FolderFilter::All, cx);
                }))
        };
        empty_state(icon, heading, None).child(action)
    }

    pub(super) fn library_controls_visible(&self, cx: &App) -> bool {
        !(self.courses.is_empty()
            && self.folder_filter.is_all()
            && self.value(Field::Search, cx).is_empty()
            && self.library_error.is_none()
            && self.library_issues.is_empty()
            && !self.loading)
    }

    /// Search, the folders on narrow windows, grouping, the card/list choice
    /// and a manual rescan, above the notes.
    fn library_toolbar(&self, sidebar: bool, can_view: bool, cx: &mut Context<Self>) -> Div {
        let group_on = self.desktop_settings.library_group_folders;
        let can_group = self.folder_filter.is_all() && self.library_error.is_none();
        let desktop = cx.weak_entity();
        let places = self.library_place().0;
        let view = ConnectedButtonGroup::new("library-view", course2md::tr!("显示"))
            .options([
                ("cards", course2md::tr!("卡片")),
                ("list", course2md::tr!("列表")),
            ])
            .icon("cards", symbols::grid_view())
            .icon("list", symbols::list())
            .selected(if self.desktop_settings.library_cards {
                "cards"
            } else {
                "list"
            })
            .button_size(ButtonSize::Small)
            .on_activate(move |value, _, cx| {
                let cards = value.as_ref() == "cards";
                let _ = desktop.update(cx, |this, cx| {
                    this.desktop_settings.library_cards = cards;
                    this.save_library_presentation(cx);
                });
            });
        h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap(rem(12.))
            .flex_wrap()
            .items_center()
            .child(
                div().flex_1().flex_basis(rem(280.)).min_w(rem(200.)).child(
                    SearchField::new(&self.inputs[&Field::Search])
                        .leading(Icon::from(symbols::search()))
                        .when(!self.value(Field::Search, cx).is_empty(), |field| {
                            field.trailing(
                                IconButton::new(
                                    "library-search-clear",
                                    symbols::close(),
                                    course2md::tr!("清除搜索"),
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.inputs[&Field::Search].update(cx, |state, cx| {
                                            state.set_value("", window, cx)
                                        });
                                        cx.notify();
                                    },
                                )),
                            )
                        }),
                ),
            )
            .child(
                h_flex()
                    .gap(rem(12.))
                    .flex_wrap()
                    .items_center()
                    .when(!sidebar, |row| {
                        row.child(
                            material_button("library-places", ButtonVariant::Tonal)
                                .icon(symbols::folder())
                                .child(places)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.places_open = true;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        FilterChip::new("library-group-folders", course2md::tr!("按文件夹分组"))
                            .selected(group_on && can_group)
                            .disabled(!can_group)
                            .on_toggle(cx.listener(|this, on: &bool, _, cx| {
                                this.desktop_settings.library_group_folders = *on;
                                this.save_library_presentation(cx);
                            })),
                    )
                    .when(can_view, |row| row.child(view))
                    .child(
                        MaterialTooltip::plain(
                            "library-rescan-tip",
                            course2md::tr!("重新扫描笔记库"),
                        )
                        .trigger(
                            IconButton::new(
                                "library-rescan",
                                symbols::refresh(),
                                course2md::tr!("重新扫描笔记库"),
                            )
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx))),
                        ),
                    ),
            )
    }

    /// Where a note lives and came from, on one line. Its folder is named only where
    /// the place does not already say it; source identity helps recognize a note;
    /// routine revision numbers remain in the reader's version history.
    fn note_meta(&self, course: &Course) -> String {
        use chrono::Datelike;
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
        let home = self.page != Page::Library;
        let names_folder =
            home || (self.folder_filter.is_all() && !self.desktop_settings.library_group_folders);
        let mut parts = Vec::new();
        if names_folder {
            let location = self.course_location(course);
            let library = location.and_then(|location| self.library_indexes.get(&location.root));
            match (self.course_folder(course).filter(|id| *id != 0), library) {
                (Some(id), Some(library)) => parts.extend(library.folders.get(&id).cloned()),
                (None, Some(_)) if !home => parts.push(course2md::tr!("未分类").to_owned()),
                _ => {}
            }
        }
        if let Some(source) = self.note_source(course) {
            if !source.author.trim().is_empty() {
                parts.push(source.author.clone());
            }
            parts.extend(source.duration.map(course2md::render::fmt_ts));
            if source.local {
                parts.push(course2md::tr!("本地视频").into());
            }
        }
        parts.extend(date.map(|date| {
            if home {
                crate::i18n::month_day(date.month(), date.day())
            } else {
                date.format("%Y-%m-%d").to_string()
            }
        }));
        parts.join(" · ")
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

    /// Why a note did not open, with ways to retry and to look at its files.
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
                .gap(rem(8.))
                .px(rem(16.))
                .pb(rem(12.))
                .child(
                    accessible_text(("course-read-error", index), message.clone())
                        .text_size(TEXT_AUX)
                        .text_color(color(ColorRole::WARNING)),
                )
                .child(
                    h_flex()
                        .gap(rem(8.))
                        .flex_wrap()
                        .child(
                            material_button(("retry-course-read", index), ButtonVariant::Text)
                                .icon(symbols::refresh())
                                .child(course2md::tr!("重试打开"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_course(retry.clone(), cx)
                                })),
                        )
                        .child(
                            material_button(("reveal-unread-course", index), ButtonVariant::Text)
                                .icon(symbols::folder_open())
                                .child(course2md::tr!("检查保存位置"))
                                .on_click(move |_, _, cx| cx.reveal_path(&path)),
                        ),
                ),
        )
    }

    /// A folder group's notes, as a connected list or card rows like the flat view.
    fn note_collection(
        &self,
        courses: &[usize],
        columns: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Div {
        let rows = v_flex().w_full();
        match columns {
            Some(columns) => rows.gap(rem(GRID_GAP)).children(
                courses
                    .chunks(columns)
                    .map(|row| self.note_grid_row(row, columns, cx)),
            ),
            None => {
                let count = courses.len();
                rows.gap(TILE_GAP)
                    .children(courses.iter().enumerate().map(|(position, course)| {
                        self.note_row(*course, list_corners(position, count), cx)
                    }))
            }
        }
    }

    /// One row of note cards: equal-height columns of the grid's width.
    fn note_grid_row(&self, row: &[usize], columns: usize, cx: &mut Context<Self>) -> Div {
        div()
            .grid()
            .grid_cols(columns as u16)
            .w_full()
            .gap(rem(GRID_GAP))
            .children(row.iter().filter_map(|index| {
                let course = self.courses.get(*index)?;
                Some(self.note_card(course, *index, cx))
            }))
    }

    /// A note's real state worth flagging on its cover or row: opening now, a task
    /// still writing a version of it, or content that still needs completing.
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
        if self.opening_course.as_ref() == Some(&course.dir) {
            Some(status_chip(StatusKind::Progress).child(course2md::tr!("正在打开笔记…")))
        } else if busy {
            Some(status_chip(StatusKind::Progress).child(course2md::tr!("正在处理")))
        } else if course.incomplete() {
            Some(status_chip(StatusKind::Warning).child(course2md::tr!("部分内容待补全")))
        } else {
            None
        }
    }

    /// The note grid's card: a 16:9 cover carrying the note's status and commands,
    /// then the title and its line. The card is one action: reading the note.
    fn note_card(&self, course: &Rc<Course>, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let scale = TileScale::Compact;
        let commands = self.note_commands(course, cx);
        let open = course.clone();
        let cover = cover(course.thumbnail.clone(), symbols::description()).children(
            self.note_status(course)
                .map(|status| div().absolute().left(rem(8.)).top(rem(8.)).child(status)),
        );
        let card = MaterialCard::new(("note-card", index), CardVariant::Filled)
            .accessibility_label(course2md::tr!("阅读 {}", course.title))
            .on_click(cx.listener(move |this, _, _, cx| this.open_course(open.clone(), cx)))
            .child(cover)
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(4.))
                    .p(rem(12.))
                    .child(
                        scale
                            .label(("note-card-title", index), course.title.clone())
                            // Room for two lines, so a row's cards end together.
                            .min_h(rem(44.))
                            .whitespace_normal()
                            .line_clamp(2)
                            .text_ellipsis(),
                    )
                    .child(
                        scale
                            .support(("note-card-meta", index), self.note_meta(course))
                            .truncate(),
                    ),
            );
        v_flex()
            .id(("note", index))
            .relative()
            .min_w_0()
            .gap(rem(4.))
            .child(card)
            .child(
                div()
                    .absolute()
                    .top(rem(4.))
                    .right(rem(4.))
                    .child(self.note_commands_button(course, index, true, cx)),
            )
            .children(self.course_read_error(course, index, cx))
            .children(self.course_export_feedback(course, cx))
            .on_drag(note_drag(course), |drag, _, _, cx| cx.new(|_| drag.clone()))
            .context_menu(move |menu, _, _| command_popup(menu, &commands))
            .into_any_element()
    }

    /// One note as a row: its cover, title, line and status, the row as one action
    /// and its commands at the end. Library lists and the workbench's recent notes
    /// show the same row.
    fn note_row(&self, index: usize, outer: OuterCorners, cx: &mut Context<Self>) -> AnyElement {
        let Some(course) = self.courses.get(index) else {
            return div().into_any_element();
        };
        let scale = TileScale::Regular;
        let commands = self.note_commands(course, cx);
        let details: Vec<Div> = self
            .course_read_error(course, index, cx)
            .into_iter()
            .chain(
                self.course_export_feedback(course, cx)
                    .map(|feedback| feedback.px(rem(16.)).pb(rem(12.))),
            )
            .collect();
        let open = course.clone();
        let row = scale
            .button(
                ("note-row", index),
                if details.is_empty() {
                    outer
                } else {
                    [outer[0], outer[1], false, false]
                },
            )
            .accessibility_label(course2md::tr!("阅读 {}", course.title))
            .on_click(cx.listener(move |this, _, _, cx| this.open_course(open.clone(), cx)))
            .child(
                scale
                    .row()
                    .child(row_cover(course.thumbnail.clone(), symbols::description()))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(rem(2.))
                            .child(
                                scale
                                    .label(("note-title", index), course.title.clone())
                                    .whitespace_normal()
                                    .line_clamp(2)
                                    .text_ellipsis(),
                            )
                            .child(scale.support(("note-meta", index), self.note_meta(course)))
                            .children(self.note_status(course).map(|status| status.self_start())),
                    )
                    .child(self.note_commands_button(course, index, false, cx)),
            );
        tile(outer)
            .id(("note", index))
            .child(row)
            .children(details)
            .on_drag(note_drag(course), |drag, _, _, cx| cx.new(|_| drag.clone()))
            .context_menu(move |menu, _, _| command_popup(menu, &commands))
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
        let scale = TileScale::Regular;
        let count = tasks.len();
        let mut list = v_flex().w_full().min_w_0().gap(TILE_GAP);
        for (index, task) in tasks.into_iter().enumerate() {
            let id = task.id.clone();
            let queued = task.state == TaskState::Queued && !self.is_running_task(&id);
            let cancel = id.clone();
            let menu = self.task_menu(task, cx);
            let row = scale
                .button(
                    SharedString::from(format!("workbench-task-open-{id}")),
                    list_corners(index, count),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_task(&id, cx);
                    this.navigate(Page::Task, cx);
                }))
                .child(
                    scale
                        .row()
                        .child(row_cover(
                            task.plan.source.cover.clone().filter(|_| !queued),
                            if queued {
                                symbols::schedule()
                            } else {
                                symbols::movie()
                            },
                        ))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(rem(8.))
                                .child(
                                    scale
                                        .label(
                                            SharedString::from(format!(
                                                "workbench-task-title-{cancel}"
                                            )),
                                            task.plan.title.clone(),
                                        )
                                        .truncate(),
                                )
                                .children(self.task_status_lines(
                                    task,
                                    &format!("workbench-task-{cancel}"),
                                    window,
                                    cx,
                                )),
                        )
                        .child(
                            // Labelled as on the running card and the Tasks page.
                            material_button(
                                SharedString::from(format!("workbench-task-cancel-{cancel}")),
                                ButtonVariant::Text,
                            )
                            .icon(symbols::close())
                            .child(course2md::tr!("取消任务"))
                            .disabled(task.intent == Intent::Cancel)
                            .on_click({
                                let id = cancel.clone();
                                cx.listener(move |this, _, _, cx| {
                                    this.set_task_intent(id.clone(), Intent::Cancel, cx);
                                })
                            }),
                        ),
                );
            list = list.child(
                div()
                    .id(SharedString::from(format!("workbench-task-{cancel}")))
                    .w_full()
                    .child(row)
                    .context_menu(move |popup, window, cx| menu(popup, window, cx)),
            );
        }
        Some(home_section(
            "workbench-active-title",
            course2md::tr!("正在处理"),
            None,
            list,
        ))
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
        let progress_now = self.compact_task_progress(task);
        let status = if cancelling {
            course2md::tr!("正在取消…").to_owned()
        } else if task.state == TaskState::Pausing {
            course2md::tr!("正在保存进度…").to_owned()
        } else if queued {
            course2md::tr!("排队中").to_owned()
        } else {
            progress_now
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
        let fraction = progress_now.and_then(|(_, fraction)| fraction);
        let line = rem(20.);
        let mut lines = vec![
            h_flex()
                .min_w_0()
                .gap(rem(8.))
                .items_start()
                .when(active && fraction.is_none(), |row| {
                    // Centred on the first line when the status wraps.
                    row.child(
                        h_flex()
                            .h(line)
                            .flex_shrink_0()
                            .items_center()
                            .child(spinner(
                                SharedString::from(format!("{key}-spinner")),
                                status.clone(),
                                cx,
                            )),
                    )
                })
                .child(
                    ui::status_line(SharedString::from(format!("{key}-status")), &status)
                        .text_size(TEXT_BODY)
                        .line_height(line)
                        .text_color(color(ColorRole::ON_SURFACE_VARIANT)),
                )
                .into_any_element(),
        ];
        if let Some(fraction) = fraction.filter(|_| active) {
            lines.push(progress(
                SharedString::from(format!("{key}-progress")),
                fraction,
                window,
                cx,
            ));
        }
        lines
    }

    /// One task waiting on the person: title, status, summary and what can be done.
    fn attention_row(
        &self,
        task: crate::workspace::TaskRecord,
        outer: OuterCorners,
        cx: &mut Context<Self>,
    ) -> Div {
        use crate::workspace::{Intent, TaskState};
        let id = task.id.clone();
        let action = |kind: &str, variant| {
            material_button(SharedString::from(format!("recent-{kind}-{id}")), variant)
        };
        let uncertain = task.state == TaskState::Uncertain
            || task
                .events
                .blocked
                .iter()
                .any(|request| request.is_uncertain());
        let requires_repair = !uncertain && crate::task_ui::task_requires_service_repair(&task);
        let partial_components = if task.state == TaskState::Partial && task.artifact.is_some() {
            crate::task_ui::task_component_failures(&task)
                .into_iter()
                .filter_map(|(component, _, _)| course2md::execution::Component::parse(&component))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut actions = h_flex().gap(rem(8.)).flex_wrap();
        if let Some(course) = task.artifact.as_ref().and_then(|path| {
            self.courses
                .iter()
                .find(|course| &course.dir == path)
                .cloned()
        }) {
            let label = if self.course_has_reading_position(&course) {
                course2md::tr!("继续阅读")
            } else {
                course2md::tr!("阅读笔记")
            };
            actions = actions.child(
                action("attention-read", ButtonVariant::Tonal)
                    .icon(symbols::menu_book())
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_course(course.clone(), cx);
                    })),
            );
        }
        let task_id = id.clone();
        actions = actions.child(if requires_repair {
            action("repair", ButtonVariant::Text)
                .icon(symbols::settings())
                .child(course2md::tr!("修复 AI 服务并补做"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.repair_task_service(task_id.clone(), window, cx);
                }))
        } else if !uncertain && !partial_components.is_empty() {
            let labels = partial_components
                .iter()
                .map(|component| component.as_str().to_owned())
                .collect::<Vec<_>>();
            action("retry", ButtonVariant::Text)
                .icon(symbols::refresh())
                .child(crate::task_ui::retry_components_label(&labels))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.reprocess_task(
                        task_id.clone(),
                        partial_components.clone(),
                        Vec::new(),
                        cx,
                    );
                }))
        } else if !uncertain && task.state == TaskState::Paused {
            action("resume", ButtonVariant::Text)
                .icon(symbols::play_arrow())
                .child(course2md::tr!("继续生成"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_task_intent(task_id.clone(), Intent::Run, cx);
                }))
        } else {
            action("task", ButtonVariant::Text)
                .icon(symbols::task())
                .child(course2md::tr!("查看任务"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    // 统一走 navigate()：保存 New 草稿、阅读位置、跟随清理
                    this.select_task(&task_id, cx);
                    this.navigate(Page::Task, cx);
                }))
        });
        let scale = TileScale::Regular;
        let summary = crate::task_ui::task_attention_summary(&task);
        tile(outer).child(
            v_flex()
                .w_full()
                .min_w_0()
                .px(ITEM_INSET)
                .py(rem(16.))
                .gap(rem(8.))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap(rem(12.))
                        .items_center()
                        .flex_wrap()
                        .child(
                            scale
                                .label(
                                    SharedString::from(format!("recent-attention-{id}")),
                                    task.plan.title.clone(),
                                )
                                .min_w(rem(160.))
                                .flex_1(),
                        )
                        .child(status_chip(StatusKind::Warning).child(task.state.label()))
                        .child(actions),
                )
                .when(summary != task.state.label(), |card| {
                    card.child(
                        scale.support(SharedString::from(format!("recent-summary-{id}")), summary),
                    )
                }),
        )
    }

    /// Workbench "最近笔记": attention tasks first, then recent readable notes.
    /// First use keeps a compact explanation below the source input.
    /// Notes waiting on the person stay listed even when recent notes are
    /// turned off; only finished notes and the empty state follow the setting.
    pub fn recent_notes_section(&self, cx: &mut Context<Self>) -> Option<Div> {
        use crate::workspace::TaskState;
        let show_notes = self.desktop_settings.home_recent_notes;
        let linked = self.current_input_task(cx).map(|task| task.id.clone());
        let mut attention: Vec<_> = self
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.state.tasks)
            .filter(|task| {
                Some(&task.id) != linked.as_ref()
                    && task.handled_by.is_none()
                    && (matches!(
                        task.state,
                        TaskState::NeedsAttention | TaskState::Uncertain | TaskState::Paused
                    ) || (task.state == TaskState::Partial
                        && task.artifact.is_some()
                        && !crate::task_ui::task_component_failures(task).is_empty()))
            })
            .collect();
        attention.sort_by_key(|task| std::cmp::Reverse(task.updated));
        attention.truncate(2);
        let attention: Vec<_> = attention.into_iter().cloned().collect();
        let notes: Vec<usize> = self
            .courses
            .iter()
            .enumerate()
            .filter(|(_, course)| {
                !attention
                    .iter()
                    .any(|task| task.artifact.as_ref() == Some(&course.dir))
            })
            .take(if show_notes { 4 } else { 0 })
            .map(|(index, _)| index)
            .collect();
        if attention.is_empty() && !show_notes {
            return None;
        }
        if attention.is_empty() && notes.is_empty() {
            return Some(home_section(
                "recent-title",
                course2md::tr!("最近的笔记"),
                None,
                self.recent_empty(cx),
            ));
        }
        let count = attention.len() + notes.len();
        let mut rows = v_flex().w_full().min_w_0().gap(TILE_GAP);
        let mut position = 0;
        for task in attention {
            rows = rows.child(self.attention_row(task, list_corners(position, count), cx));
            position += 1;
        }
        for index in notes {
            rows = rows.child(self.note_row(index, list_corners(position, count), cx));
            position += 1;
        }
        Some(home_section(
            "recent-title",
            if show_notes {
                course2md::tr!("最近的笔记")
            } else {
                course2md::tr!("需要处理")
            },
            Some(
                material_button("recent-all", ButtonVariant::Text)
                    .icon(symbols::chevron_right())
                    .icon_position(ButtonIconPosition::Trailing)
                    .child(course2md::tr!("笔记库"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.folder_filter = FolderFilter::All;
                        this.navigate(Page::Library, cx);
                    }))
                    .into_any_element(),
            ),
            rows,
        ))
    }

    /// The workbench with no notes: still loading, unreachable, or the first use's
    /// short account of what happens.
    fn recent_empty(&self, cx: &mut Context<Self>) -> Div {
        let access = self.cached_library_access();
        let checking = self.loading || access.is_none();
        let unavailable = !self.library_issues.is_empty()
            || access.is_some_and(|access| !access.unavailable.is_empty());
        if checking {
            return empty_state(symbols::menu_book(), course2md::tr!("正在读取笔记库"), None);
        }
        if unavailable {
            return empty_state(
                symbols::menu_book(),
                course2md::tr!("暂时无法读取笔记库"),
                Some(course2md::tr!("检查保存位置后，即可继续查看已有笔记")),
            )
            .child(
                material_button("recent-unavailable-library", ButtonVariant::Text)
                    .child(course2md::tr!("查看笔记库"))
                    .on_click(cx.listener(|this, _, _, cx| this.navigate(Page::Library, cx))),
            );
        }
        let steps = [
            course2md::tr!("粘贴链接或拖入视频"),
            course2md::tr!("读取字幕或识别语音"),
            course2md::tr!("AI 校对并生成笔记"),
        ]
        .join(" · ");
        empty_state(
            symbols::menu_book(),
            course2md::tr!("还没有笔记"),
            Some(course2md::tr!("整理好的笔记会出现在这里")),
        )
        .child(
            ui::status_line("recent-steps", &steps)
                .justify_center()
                .text_size(TEXT_BODY)
                .text_color(color(ColorRole::ON_SURFACE_VARIANT)),
        )
    }
}

/// A place with nothing in it: a tinted icon, what that means and the way forward,
/// which the caller appends.
fn empty_state(
    icon: SymbolIcon,
    title: impl Into<SharedString>,
    description: Option<&'static str>,
) -> Div {
    let role = TypeRole::TitleLarge.scale();
    tile([true; 4])
        .flex()
        .flex_col()
        .items_center()
        .text_center()
        .gap(rem(16.))
        .py(rem(40.))
        .px(rem(24.))
        .child(tinted_icon(icon, Tint::Blue, rem(56.)))
        .child(
            accessible_text("empty-state-title", title)
                .role(Role::Heading)
                .text_size(role.font_size)
                .line_height(role.line_height),
        )
        .children(description.map(|text| TileScale::Regular.support("empty-state-text", text)))
}

/// A workbench section: its title, an optional trailing action and its content.
fn home_section(
    id: &'static str,
    title: impl Into<SharedString>,
    trailing: Option<AnyElement>,
    content: impl IntoElement,
) -> Div {
    let role = TypeRole::TitleLarge.scale();
    v_flex()
        .w_full()
        .min_w_0()
        .gap(rem(12.))
        .child(
            h_flex()
                .w_full()
                .min_h(rem(48.))
                .pl(rem(4.))
                .gap(rem(12.))
                .items_center()
                .child(
                    accessible_text(id, title)
                        .role(Role::Heading)
                        .flex_1()
                        .text_size(role.font_size)
                        .line_height(role.line_height),
                )
                .children(trailing),
        )
        .child(content)
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
    use super::LibraryItem;
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
        let row = |courses: &[usize], columns| {
            LibraryItem::notes(&all, courses, Some(columns))
                .remove(0)
                .key()
        };
        let mut items = LibraryItem::notes(&all, &[0, 1], Some(3));
        items.extend(LibraryItem::notes(&all, &[0, 1], None));
        items.push(LibraryItem::Coverage("m".into()));
        items.push(LibraryItem::GroupHeader {
            key: "lib:0".into(),
            name: "未分类".into(),
            count: 2,
            collapsed: false,
        });
        items.push(LibraryItem::GroupBody {
            key: "lib:0".into(),
            collapsed: false,
            courses: vec![0, 1],
        });
        let keys: Vec<String> = items.iter().map(LibraryItem::key).collect();
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len(), "library rows need distinct keys");
        // Regrouping for a new column count replaces the affected rows.
        assert_ne!(row(&[0, 1], 3), row(&[0, 1], 2));
        // Content edits keep the row identity keyed by its first course; the
        // row re-measures when it is visible.
        assert_eq!(row(&[0, 2], 3), row(&[0, 1], 3));
    }
}
