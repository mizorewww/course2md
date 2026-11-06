//! The note library and the workbench's lists of notes and tasks.
use super::*;
use crate::folder_tree::{
    COMMAND_MENU_WIDTH, Command, NoteDrag, command_context_menu, command_menu, note_count,
};
use crate::library_ui::{Moving, NameTarget};
use crate::{symbols, ui};
use desktop_store::library::*;
use desktop_store::order::{self, NoteFacts, Place, Sort, SortRule};
use desktop_tasks::task_view::TaskAction;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit_material::components::MaterialContextMenu;
use gpui_kit_material::{
    color::ColorRole,
    icons::{Icon, SymbolIcon},
    motion,
    prelude::{
        ButtonIconPosition, ButtonShape, ButtonSize, ButtonTone, ButtonVariant, CardVariant,
        ConnectedButtonGroup, ITEM_INSET, IconButton, IconButtonVariant, ListDensity, ListItem,
        ListLeading, MaterialCard, MaterialMenu, MaterialMenuItem, MaterialSideSheet,
        MaterialTooltip, NoticeKind, RADIUS_LARGE, RADIUS_SMALL, SearchField, SearchFieldStyle,
        StatusKind, TEXT_AUX, TEXT_BODY, TILE_GAP, TILE_RADIUS, TileScale, Tint, TypeRole,
        accessible_text, color, disclosure_chevron, list_corners, material_button, media_label,
        notice_banner, progress, rem, spinner, status_chip, tile, tile_chevron, tinted_icon,
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
pub(crate) fn row_cover(image: Option<PathBuf>, icon: SymbolIcon) -> Div {
    cover(image, icon)
        .w(rem(ROW_COVER))
        .max_w(relative(ROW_COVER_SHARE))
        .flex_shrink_0()
        .rounded(RADIUS_SMALL)
}

/// One row of a note list: rows share one surface with the page's large corners, and
/// a hairline inset past the cover column parts them.
fn list_surface(outer: OuterCorners) -> Div {
    v_flex()
        .w_full()
        .min_w_0()
        .bg(color(ColorRole::SURFACE))
        .when(outer[0], |row| row.rounded_t(TILE_RADIUS))
        .when(outer[2], |row| row.rounded_b(TILE_RADIUS))
        .when(!outer[0], |row| {
            row.child(
                div()
                    .h(px(1.))
                    .ml(rem(ROW_COVER + 32.))
                    .mr(rem(16.))
                    .bg(color(ColorRole::OUTLINE_VARIANT)),
            )
        })
}

/// A note picked up to file into a folder of the tree.
fn note_drag(course: &Course) -> NoteDrag {
    NoteDrag {
        storage: course.dir.clone(),
        title: course.title.clone().into(),
    }
}

/// One row of the virtualized library page. Folder groups stay one item per
/// disclosure, so a group arrives as one unit.
enum LibraryItem {
    /// Work in progress, with its message.
    Status(&'static str),
    Recovery(crate::storage::LibraryAccess),
    /// What the startup migration removed, until the person closes it.
    Migrated(String),
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
            LibraryItem::Migrated(_) => "migrated".to_owned(),
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

/// A thing's commands behind a ⋮ button, as one element so it can reach the window.
#[derive(gpui::IntoElement)]
pub(crate) struct CommandsButton {
    pub(crate) id: ElementId,
    /// The tooltip, and what the button is called; `subject` says whose commands.
    pub(crate) label: SharedString,
    pub(crate) subject: Option<SharedString>,
    pub(crate) commands: Vec<Command>,
    /// The menu's width; `None` is the kit's default.
    pub(crate) width: Option<f32>,
    pub(crate) tonal: bool,
}

impl RenderOnce for CommandsButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let button = ElementId::NamedChild(Arc::new(self.id.clone()), "button".into());
        let tip = ElementId::NamedChild(Arc::new(self.id.clone()), "tip".into());
        let items = ElementId::NamedChild(Arc::new(self.id.clone()), "items".into());
        let key_toggle = ui::menu_key_toggle(&self.id, window, cx);
        ui::menu(
            self.id,
            MaterialTooltip::plain(tip, self.label.clone()).trigger(
                IconButton::new(
                    button,
                    symbols::more_vert(),
                    self.subject.unwrap_or(self.label),
                )
                .when(self.tonal, |button| {
                    button
                        .variant(IconButtonVariant::Tonal)
                        .tone(ButtonTone::Neutral)
                })
                .on_click(key_toggle),
            ),
            command_menu(items, self.width, self.commands),
            window,
            cx,
        )
        .anchor(Anchor::TopRight)
    }
}

impl Desktop {
    /// Notes already made from the video with this identity.
    pub(crate) fn notes_for_source<'a>(
        &self,
        identity: &'a str,
        cx: &'a App,
    ) -> impl Iterator<Item = &'a Course> {
        self.library(cx).notes_for_source(identity)
    }

    /// Apply the names already read by the library worker. The UI does no I/O.
    pub fn apply_course_title_aliases(&mut self, cx: &mut App) {
        self.records
            .change(cx, |records, _| records.apply_course_title_aliases());
        let title = self
            .opened(cx)
            .preview
            .as_ref()
            .map(|preview| self.library(cx).course_display_title(&preview.course));
        if let Some(title) = title {
            self.reader
                .change(cx, |reader, cx| reader.retitle(title, cx));
        }
    }

    pub fn begin_course_rename(
        &mut self,
        course: Course,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self
            .course_location(&course, cx)
            .map(|library| library.root.clone())
        else {
            self.message = Some(
                course2md::tr!("这份笔记的保存位置暂时无法确认，请在笔记库菜单中重新扫描。").into(),
            );
            cx.notify();
            return;
        };
        let storage = course.dir.clone();
        self.begin_naming(NameTarget::Note { root, storage }, window, cx);
    }

    /// Asks before moving a note to the system trash. A note that an unfinished
    /// task will replace cannot be deleted yet.
    pub fn confirm_course_delete(
        &mut self,
        course: Course,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((root, library_id)) = self
            .course_location(&course, cx)
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
        let busy = self.workspace(cx).is_some_and(|workspace| {
            note_has_unfinished_task(&workspace.state.tasks, &library_id, source)
        });
        if busy {
            self.message =
                Some(course2md::tr!("这份笔记还有未完成的任务，请在任务结束或取消后再删除").into());
            cx.notify();
            return;
        }
        let title = self.library(cx).course_display_title(&course);
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
        let title = self.library(cx).course_display_title(&course);
        let dir = course.dir.clone();
        let worker_dir = dir.clone();
        self.in_background(
            move || organize::delete_course(&root, &worker_dir),
            move |this, result, cx| match result {
                Ok(()) => {
                    let reading = this
                        .opened(cx)
                        .preview
                        .as_ref()
                        .is_some_and(|preview| preview.course.dir == dir);
                    if reading && this.page == Page::Result {
                        this.navigate(Page::Library, cx);
                    } else {
                        this.refresh_library(cx);
                    }
                    if reading {
                        this.reader
                            .change(cx, |reader, _| reader.opened.preview = None);
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
        let path = course.dir.clone();
        let link = self
            .library(cx)
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
                move |this, window, cx| {
                    let course = course.clone();
                    this.exports
                        .update(cx, |exports, cx| exports.export(course, format, window, cx))
                },
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
        ]);
        commands.extend(self.note_move_commands(course, cx));
        commands.extend([
            on_course(
                course2md::tr!("移动到…"),
                symbols::drive_file_move(),
                |this, course, _, cx| {
                    let root = this.folder_context(Some(course.dir.clone()), cx).root;
                    this.begin_move(root, Moving::Note(course.dir.clone()), cx)
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
            width: Some(COMMAND_MENU_WIDTH),
            tonal,
        }
    }

    fn recover_library_folders(&mut self, root: PathBuf, rebuild: bool, cx: &mut Context<Self>) {
        let worker_root = root.clone();
        self.in_background(
            move || organize::recover(&worker_root, rebuild),
            move |this, result, cx| match result {
                Ok((library, _)) => {
                    this.store_folders(&root, library, cx);
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

    fn library_needs_recovery(&self, access: &crate::storage::LibraryAccess, cx: &App) -> bool {
        self.workspace(cx).is_some_and(|workspace| {
            workspace.state.libraries.iter().any(|location| {
                access.unavailable.contains(&location.root)
                    || self
                        .library(cx)
                        .view_cache
                        .recovery
                        .contains_key(&location.root)
                    || self
                        .library(cx)
                        .view_cache
                        .title_recovery
                        .contains_key(&location.root)
            })
        }) || !self.library(cx).note_attention.is_empty()
    }

    fn library_recovery_view(
        &self,
        access: &crate::storage::LibraryAccess,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let workspace = self.workspace(cx)?;
        let details = |prefix: &str, location: &crate::workspace::LibraryLocation, kind| {
            let messages = self.library(cx).issue_messages(&location.root, kind);
            (!messages.is_empty())
                .then(|| IssueDetails::new(format!("{prefix}:{}", location.id), messages))
        };
        let mut notices = Vec::new();
        for (index, location) in workspace.state.libraries.iter().enumerate() {
            let root = &location.root;
            if access.unavailable.contains(root) {
                let message = course2md::tr!("{} 暂时无法访问", location.label());
                notices.push(
                    notice_banner(("library-unavailable", index), NoticeKind::Info, message).child(
                        material_button(("retry-library-location", index), ButtonVariant::Tonal)
                            .icon(symbols::refresh())
                            .child(course2md::tr!("重新检查保存位置"))
                            .loading(self.library(cx).loading, course2md::tr!("重新检查保存位置"))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx))),
                    ),
                );
                continue;
            }
            if let Some(recovery) = self.library(cx).view_cache.recovery.get(root) {
                let (restore, rebuild, folder) = (root.clone(), root.clone(), root.clone());
                let message = course2md::tr!("{} 的文件夹信息已损坏", location.label());
                notices.push(
                    notice_banner(
                        ("classification-recovery", index),
                        NoticeKind::Danger,
                        message,
                    )
                    .when(recovery.has_backup, |notice| {
                        notice.child(
                            material_button(
                                ("restore-classification", index),
                                ButtonVariant::Tonal,
                            )
                            .icon(symbols::history())
                            .child(course2md::tr!("恢复最近备份"))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.recover_library_folders(restore.clone(), false, cx)
                                },
                            )),
                        )
                    })
                    .child(
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
                        })),
                    )
                    .child(
                        material_button(("show-classification-files", index), ButtonVariant::Text)
                            .icon(symbols::folder_open())
                            .child(course2md::tr!("打开保存位置"))
                            .on_click(move |_, _, cx| cx.open_with_system(&folder)),
                    )
                    .children(details(
                        "folders",
                        location,
                        LibraryIssueKind::Folders,
                    )),
                );
            }
            if let Some(recovery) = self.library(cx).view_cache.title_recovery.get(root) {
                let actions = [false, true]
                    .into_iter()
                    .filter(|reset| *reset || recovery.has_backup)
                    .map(|reset| {
                        let root = root.clone();
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
                    });
                let message = course2md::tr!("{} 的笔记名称记录无法读取", location.label());
                notices.push(
                    notice_banner(("title-recovery", index), NoticeKind::Danger, message)
                        .children(actions)
                        .children(details("names", location, LibraryIssueKind::Titles)),
                );
            }
        }
        // A note whose replacement stopped is not listed until it is sorted out;
        // copies kept beside a note are named so they do not pile up unseen.
        use course2md::artifact::NoteAttention;
        for (index, attention) in self
            .library(cx)
            .note_attention
            .clone()
            .into_iter()
            .enumerate()
        {
            let (kind, message, folder) = match attention {
                NoteAttention::Blocked { swap, .. } => (
                    NoticeKind::Danger,
                    course2md::outcome::recovery_blocked_message(&swap, course2md::i18n::lang()),
                    swap,
                ),
                // At the limit, new tasks for the note stop: say so and what frees it.
                NoteAttention::Kept { swap, copies, .. }
                    if copies.len() >= course2md::artifact::MAX_KEPT_COPIES =>
                {
                    (
                        NoticeKind::Danger,
                        course2md::outcome::copies_kept_message(&swap, course2md::i18n::lang()),
                        swap,
                    )
                }
                NoteAttention::Kept { note, swap, copies } => (
                    NoticeKind::Info,
                    course2md::tr!(
                        "{} 的文件夹里保留了 {} 份副本，无法确认是否还需要，请查看后删除不需要的",
                        note.display(),
                        copies.len()
                    )
                    .to_string(),
                    swap,
                ),
                NoteAttention::Failed { .. } => continue,
            };
            // The message holds long paths: its actions take a row of their own below
            // it instead of narrowing it.
            notices.push(
                notice_banner(("note-attention", index), kind, message).child(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .gap(rem(8.))
                        .child(
                            material_button(("open-note-attention", index), ButtonVariant::Filled)
                                .icon(symbols::folder_open())
                                .child(course2md::tr!("打开需要处理的文件夹"))
                                .on_click(move |_, _, cx| cx.open_with_system(&folder)),
                        )
                        .child(
                            material_button(("recheck-note-attention", index), ButtonVariant::Text)
                                .icon(symbols::refresh())
                                .child(course2md::tr!("重新检查保存位置"))
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx))),
                        ),
                ),
            );
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
        // The notes as shown, run by run, for moving one by the keyboard.
        let mut runs: Vec<Vec<usize>> = Vec::new();
        let mut flat = Vec::new();
        for item in &items {
            match item {
                LibraryItem::GroupBody { courses, .. } => runs.push(courses.clone()),
                LibraryItem::Cards { courses, .. } => flat.extend(courses),
                LibraryItem::Note { course, .. } => flat.push(*course),
                _ => {}
            }
        }
        if !flat.is_empty() {
            runs.push(flat);
        }
        self.library_ui.note_runs = runs;
        self.library_ui.note_columns = columns;
        if let Some(moved) = self.library_ui.reveal.take()
            && let Some(row) = items.iter().position(|item| match item {
                LibraryItem::Note { course, .. } => *course == moved,
                LibraryItem::Cards { courses, .. } | LibraryItem::GroupBody { courses, .. } => {
                    courses.contains(&moved)
                }
                _ => false,
            })
        {
            self.library_ui.list.scroll_to_reveal_item(row);
        }
        let count = count.filter(|_| controls);
        let notes = self.library_notes(items, columns, rem_px, cx);
        let pane = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .gap(rem(20.))
            .child(self.library_header(count, cx))
            .children(controls.then(|| self.library_toolbar(sidebar, cx)))
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
        let Some(all_access) = self.cached_library_access(cx) else {
            items.push(LibraryItem::Status(course2md::tr!("正在检查保存位置…")));
            return (items, None);
        };
        let in_scope = |root: &&PathBuf| {
            self.library(cx).folder_filter.is_all() || **root == self.library(cx).root
        };
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
        let courses = self.place_courses(&query, &scope.available, cx);
        let courses = self.arrange_courses(courses, cx);
        if self.library_needs_recovery(&all_access, cx) {
            items.push(LibraryItem::Recovery(all_access.clone()));
        }
        if let Some(notice) = &self.migration_notice {
            items.push(LibraryItem::Migrated(notice.clone()));
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
            .library(cx)
            .issues
            .iter()
            .filter(|issue| {
                let claimed = match issue.kind {
                    LibraryIssueKind::Scan => false,
                    LibraryIssueKind::Folders => self
                        .library(cx)
                        .view_cache
                        .recovery
                        .contains_key(&issue.root),
                    LibraryIssueKind::Titles => self
                        .library(cx)
                        .view_cache
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
        if self.library(cx).loading {
            items.push(LibraryItem::Status(course2md::tr!("正在读取笔记…")));
        }
        if coverage == LibraryCoverage::Unavailable
            || (self.library(cx).loading && courses.is_empty())
        {
            return (items, None);
        }
        // Inside a folder, its folders come before its notes.
        let folders = self
            .library(cx)
            .folder_filter
            .folder_id()
            .map(Some)
            .filter(|parent| {
                query.is_empty()
                    && self
                        .library(cx)
                        .indexes
                        .get(&self.library(cx).root)
                        .is_some_and(|library| !library.children(*parent).is_empty())
            });
        // The place's notes, its subfolders' included as in the navigation;
        // a search reports its matches on the same line, so nothing moves.
        let count = Some(if !query.is_empty() {
            found_notes(courses.len())
        } else if let FolderFilter::Folder(id) = self.library(cx).folder_filter {
            note_count(self.folder_note_count(&self.library(cx).root, id, cx))
        } else {
            note_count(courses.len())
        });
        if let Some(parent) = folders {
            items.push(LibraryItem::Folders(self.library(cx).root.clone(), parent));
        }
        if courses.is_empty() {
            let obstructed = query.is_empty()
                && (coverage == LibraryCoverage::Partial || !self.library(cx).issues.is_empty());
            if folders.is_none() && !obstructed {
                items.push(LibraryItem::Empty);
            }
        } else if self.desktop_settings.library_group_folders
            && self.library(cx).folder_filter.is_all()
            && self.library(cx).error.is_none()
        {
            self.push_groups(&mut items, courses, query.is_empty(), cx);
        } else {
            items.extend(LibraryItem::notes(
                &self.library(cx).courses,
                &courses,
                columns,
            ));
        }
        (items, count)
    }

    /// The notes of the place in view that are in reach and match `query`, in the
    /// library's own order.
    fn place_courses(&self, query: &str, available: &[PathBuf], cx: &App) -> Vec<usize> {
        let searched: Option<Vec<u64>> = match self.library(cx).folder_filter {
            FolderFilter::Folder(id) if !query.is_empty() => self
                .library(cx)
                .indexes
                .get(&self.library(cx).root)
                .map(|library| library.subtree(id)),
            _ => None,
        };
        self.library(cx)
            .courses
            .iter()
            .enumerate()
            .filter_map(|(index, course)| {
                let location = self.course_location(course, cx);
                let in_scope = location
                    .as_ref()
                    .map(|location| available.contains(&location.root))
                    .unwrap_or_else(|| {
                        self.records.read(cx).workspace.is_none()
                            && self
                                .library(cx)
                                .view_cache
                                .locations
                                .get(&course.dir)
                                .is_some_and(|location| available.contains(&location.root))
                    });
                if !in_scope || !course.title.to_lowercase().contains(query) {
                    return None;
                }
                let matches_folder = match self.library(cx).folder_filter {
                    FolderFilter::All => true,
                    filter => {
                        let folder = self.course_folder(course, cx);
                        location
                            .as_ref()
                            .is_some_and(|location| location.root == self.library(cx).root)
                            && (!self
                                .library(cx)
                                .indexes
                                .contains_key(&self.library(cx).root)
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
            .collect()
    }

    /// The notes grouped by folder, each group its header and its body.
    fn push_groups(
        &self,
        items: &mut Vec<LibraryItem>,
        courses: Vec<usize>,
        collapsible: bool,
        cx: &App,
    ) {
        let mut groups: BTreeMap<(PathBuf, u64), Vec<usize>> = BTreeMap::new();
        for index in courses {
            let course = &self.library(cx).courses[index];
            let root = self
                .course_location(course, cx)
                .map(|lib| lib.root.clone())
                .unwrap_or_else(|| self.library(cx).root.clone());
            groups
                .entry((root, self.course_folder(course, cx).unwrap_or(0)))
                .or_default()
                .push(index);
        }
        let state = self.workspace(cx).map(|workspace| &workspace.state);
        for ((root, id), entries) in groups {
            let location = state.and_then(|s| s.libraries.iter().find(|lib| lib.root == root));
            let key = format!(
                "{}:{id}",
                location
                    .map(|lib| lib.id.clone())
                    .unwrap_or_else(|| root.display().to_string())
            );
            let collapsed = collapsible && state.is_some_and(|s| s.collapsed.contains(&key));
            let folder_name = match self.library(cx).indexes.get(&root) {
                Some(library) if library.folders.contains_key(&id) => library.path(id).join(" / "),
                Some(_) => course2md::tr!("未分类").into(),
                None => course2md::tr!("文件夹暂不可用").into(),
            };
            let name = if state.is_some_and(|s| s.libraries.len() > 1) {
                let library = location
                    .map(|lib| lib.label())
                    .unwrap_or_else(|| course2md::tr!("笔记库").to_owned());
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
        if let Some(Err(error)) = self.with_workspace(cx, |workspace| {
            workspace.transaction(|state| {
                if !state.collapsed.remove(key) {
                    state.collapsed.insert(key.to_owned());
                }
                Ok(())
            })
        }) {
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
            .open(self.library_ui.places_open)
            .headline(course2md::tr!("文件夹"))
            .close_button(course2md::tr!("关闭"))
            .on_dismiss(move |_, cx| {
                let _ = desktop.update(cx, |this, cx| {
                    this.library_ui.places_open = false;
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
        let (place, library) = self.library_place(cx);
        let supporting = match (library, count) {
            (Some(library), Some(count)) => Some(format!("{library} · {count}")),
            (Some(library), None) => Some(library),
            (None, count) => count,
        };
        let folder_commands = self
            .library(cx)
            .folder_filter
            .folder_id()
            .map(|id| CommandsButton {
                id: "manage-folder".into(),
                label: course2md::tr!("文件夹操作").into(),
                subject: None,
                commands: self.folder_commands(&self.library(cx).root.clone(), id, cx),
                width: Some(COMMAND_MENU_WIDTH),
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
                    .child(ui::page_title("page-title", place))
                    .children(
                        supporting
                            .map(|text| TileScale::Regular.support("library-place-count", text)),
                    ),
            )
            .children(folder_commands)
            .when(self.library_controls_visible(cx), |row| {
                row.child(
                    material_button("library-new-note", ButtonVariant::Tonal)
                        .button_size(ButtonSize::Medium)
                        .shape(ButtonShape::Square)
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
        let first_items = self.library_ui.keys.is_empty();
        Self::reconcile_list_items(
            &self.library_ui.list,
            &mut self.library_ui.keys,
            &mut self.library_ui.focus,
            keys,
            cx,
        );
        // A scroll offset set while the list was empty would land past these items.
        if first_items {
            self.library_ui.list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
        }
        if self.library_ui.rem != rem_px {
            self.library_ui.list.remeasure();
            self.library_ui.rem = rem_px;
        }
        let focus: Rc<Vec<FocusHandle>> = Rc::new(self.library_ui.focus.clone());
        let state = self.library_ui.list.clone();
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
            LibraryItem::Migrated(message) => {
                notice_banner("library-migrated", NoticeKind::Info, message.clone())
                    .child(
                        material_button("dismiss-library-migrated", ButtonVariant::Text)
                            .icon(symbols::close())
                            .child(course2md::tr!("关闭"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.migration_notice = None;
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
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
                    .loading(self.library(cx).loading, course2md::tr!("重新检查"))
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
                ListItem::new(("library-group", index), format!("{name}  {count}"))
                    .density(ListDensity::Compact)
                    .container(ColorRole::SURFACE_CONTAINER)
                    .segment([true; 4])
                    .leading(ListLeading::Icon(disclosure_chevron(!collapsed)))
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
            LibraryItem::Note { .. } => wrapper,
            LibraryItem::Cards { .. } => wrapper.pb(rem(GRID_GAP)),
            _ => wrapper.pb(rem(24.)),
        };
        // A note's id follows the note, not its row, so the control holding focus
        // keeps it when the note moves; card rows share one id for the same reason.
        let id: ElementId = match item {
            LibraryItem::Note { course, .. } => ("library-note", *course).into(),
            LibraryItem::Cards { .. } => "library-cards".into(),
            _ => ("library-item", index).into(),
        };
        wrapper
            .child(content)
            .id(id)
            .tab_stop(false)
            .when_some(focus, |wrapper, focus| wrapper.track_focus(&focus))
            .into_any_element()
    }

    /// What an empty place says, and the way forward from it.
    fn library_empty(&self, cx: &mut Context<Self>) -> Div {
        let query = self.value(Field::Search, cx);
        let (icon, heading) = match self.library(cx).folder_filter {
            _ if !query.is_empty() => (symbols::search(), course2md::tr!("没有匹配的笔记")),
            FolderFilter::All => (symbols::book_2(), course2md::tr!("还没有笔记")),
            FolderFilter::Unfiled => (symbols::inbox(), course2md::tr!("没有未分类的笔记")),
            FolderFilter::Folder(_) => (symbols::folder(), course2md::tr!("这个文件夹还没有笔记")),
        };
        let action = if !query.is_empty() {
            material_button("clear-search", ButtonVariant::Outlined)
                .tone(ButtonTone::Accent)
                .icon(symbols::close())
                .child(course2md::tr!("清除搜索"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.inputs[&Field::Search]
                        .update(cx, |state, cx| state.set_value("", window, cx));
                    cx.notify();
                }))
        } else if self.library(cx).folder_filter.is_all() {
            material_button("empty-library-add", ButtonVariant::Filled)
                .icon(symbols::add())
                .child(course2md::tr!("导入视频"))
                .on_click(cx.listener(|this, _, window, cx| this.begin_add(window, cx)))
        } else {
            material_button("empty-folder-all-notes", ButtonVariant::Outlined)
                .tone(ButtonTone::Accent)
                .icon(symbols::book_2())
                .child(course2md::tr!("浏览全部笔记"))
                .on_click(cx.listener(|this, _, _, cx| {
                    let root = this.library(cx).root.clone();
                    this.select_library_scope(root, FolderFilter::All, cx);
                }))
        };
        empty_state(icon, heading, None).child(action)
    }

    pub(super) fn library_controls_visible(&self, cx: &App) -> bool {
        !(self.library(cx).courses.is_empty()
            && self.library(cx).folder_filter.is_all()
            && self.value(Field::Search, cx).is_empty()
            && self.library(cx).error.is_none()
            && self.library(cx).issues.is_empty()
            && !self.library(cx).loading)
    }

    /// Search, the folders on narrow windows, grouping, the card/list choice
    /// and a manual rescan, above the notes.
    fn library_toolbar(&self, sidebar: bool, cx: &mut Context<Self>) -> Div {
        let group_on = self.desktop_settings.library_group_folders;
        let can_group = self.library(cx).folder_filter.is_all() && self.library(cx).error.is_none();
        let desktop = cx.weak_entity();
        let places = self.library_place(cx).0;
        let view = ConnectedButtonGroup::new("library-view", course2md::tr!("显示"))
            .tone(ButtonTone::Neutral)
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
            .on_change(move |value, _, cx| {
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
                        .field_style(SearchFieldStyle::Tonal)
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
                                    this.library_ui.places_open = true;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(self.sort_menu(!sidebar, cx))
                    .child(
                        material_button(
                            "library-group-folders",
                            if group_on && can_group {
                                ButtonVariant::Tonal
                            } else {
                                ButtonVariant::Outlined
                            },
                        )
                        // On, it is tonal in the secondary container; off, an outlined
                        // action with a primary label (L6).
                        .tone(if group_on && can_group {
                            ButtonTone::Standard
                        } else {
                            ButtonTone::Accent
                        })
                        .icon(if group_on && can_group {
                            symbols::check()
                        } else {
                            symbols::folder()
                        })
                        .child(course2md::tr!("按文件夹分组"))
                        .disabled(!can_group)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.desktop_settings.library_group_folders = !group_on;
                            this.save_library_presentation(cx);
                        })),
                    )
                    .child(view)
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
                            .disabled(self.library(cx).loading)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx))),
                        ),
                    ),
            )
    }

    /// Where a note lives and came from, on one line; a card's shorter line leaves out
    /// where it came from. Its folder is named only where the place does not already
    /// say it; source identity helps recognize a note.
    fn note_line(&self, course: &Course, card: bool, cx: &App) -> String {
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
        let names_folder = home
            || (self.library(cx).folder_filter.is_all()
                && !self.desktop_settings.library_group_folders);
        let mut parts = Vec::new();
        if names_folder {
            let location = self.course_location(course, cx);
            let library =
                location.and_then(|location| self.library(cx).indexes.get(&location.root));
            match (
                self.course_folder(course, cx).filter(|id| *id != 0),
                library,
            ) {
                (Some(id), Some(library)) => parts.extend(library.folders.get(&id).cloned()),
                (None, Some(_)) if !home => parts.push(course2md::tr!("未分类").to_owned()),
                _ => {}
            }
        }
        if let Some(source) = self.library(cx).note_source(course).filter(|_| !card) {
            if !source.author.trim().is_empty() {
                parts.push(source.author.clone());
            }
            parts.extend(source.duration.map(course2md::render::fmt_ts));
            if source.local {
                parts.push(course2md::tr!("本地视频").into());
            }
        }
        // Sorted by when videos were published, the library shows that date; otherwise
        // when the note was added.
        let published = (!home && self.place_sort(cx).2.rule == SortRule::Published)
            .then(|| self.library(cx).note_source(course)?.published.clone())
            .flatten();
        let date = if home || published.is_some() {
            date
        } else {
            i64::try_from(self.note_facts(course, None, cx).added)
                .ok()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|instant| instant.with_timezone(&chrono::Local).date_naive())
                .or(date)
        };
        match published {
            Some(published) => parts.push(course2md::tr!("发布于 {}", published)),
            None => parts.extend(date.map(|date| {
                if home {
                    crate::i18n::month_day(date.month(), date.day())
                } else {
                    date.format("%Y-%m-%d").to_string()
                }
            })),
        }
        parts.join(" · ")
    }

    pub fn save_library_presentation(&mut self, cx: &mut Context<Self>) {
        let mut preferences = self.application_edit_base();
        preferences.desktop.library_cards = self.desktop_settings.library_cards;
        preferences.desktop.library_group_folders = self.desktop_settings.library_group_folders;
        self.commit_application(preferences, cx);
        cx.notify();
    }

    fn course_has_reading_position(&self, course: &Course, cx: &App) -> bool {
        course.manifest.as_ref().is_some_and(|manifest| {
            let key = desktop_reader_core::reader::manifest_reading_key(&manifest.course_id, 0);
            self.workspace(cx)
                .and_then(|workspace| workspace.state.positions.get(&key))
                .is_some_and(desktop_reader_core::reader::has_reading_position)
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
            .opened(cx)
            .reader_course_error
            .as_ref()
            .filter(|(path, _)| path == &course.dir)?;
        let retry = course.clone();
        let path = course.dir.clone();
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
                rows.children(courses.iter().enumerate().map(|(position, course)| {
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
                let course = self.library(cx).courses.get(*index)?.clone();
                Some(self.note_card(&course, *index, cx))
            }))
    }

    /// A note's real state worth flagging on its cover or row: opening now, a task
    /// still replacing it, or content that still needs completing.
    fn note_status(&self, course: &Course, cx: &App) -> Option<Div> {
        let task = self
            .course_location(course, cx)
            .zip(self.workspace(cx))
            .and_then(|(location, workspace)| {
                note_unfinished_task(
                    &workspace.state.tasks,
                    &location.id,
                    course
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.source_id.as_str()),
                )
            })
            .map(|task| task.state);
        if self.opened(cx).opening_course.as_ref() == Some(&course.dir) {
            Some(status_chip(StatusKind::Progress).child(course2md::tr!("正在打开笔记…")))
        } else if let Some(state) = task {
            let (kind, label) = task_status(state);
            Some(status_chip(kind).child(label))
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
        let cover = cover(course.thumbnail.clone(), symbols::description())
            .rounded(RADIUS_LARGE)
            .children(
                self.note_status(course, cx)
                    .map(|status| div().absolute().left(rem(8.)).top(rem(8.)).child(status)),
            );
        let card = MaterialCard::new(("note-card", index), CardVariant::Surface)
            .rounded(TILE_RADIUS)
            .accessibility_label(course2md::tr!("阅读 {}", course.title))
            .on_click(cx.listener(move |this, _, _, cx| this.open_course(open.clone(), cx)))
            .relative()
            .child(command_context_menu(
                ("note-card-context", index),
                Some(COMMAND_MENU_WIDTH),
                commands,
            ))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .p(rem(10.))
                    .pb(rem(14.))
                    .gap(rem(12.))
                    .child(cover)
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .gap(rem(2.))
                            // Room beside the text for the commands.
                            .pl(rem(4.))
                            .pr(rem(44.))
                            .child(
                                scale
                                    .label(("note-card-title", index), course.title.clone())
                                    // Room for two lines, so a row's cards end together.
                                    .whitespace_normal()
                                    .line_clamp(2)
                                    .text_ellipsis(),
                            )
                            .child(
                                scale
                                    .support(
                                        ("note-card-meta", index),
                                        self.note_line(course, true, cx),
                                    )
                                    .truncate(),
                            ),
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
                    .bottom(rem(12.))
                    .right(rem(8.))
                    .child(self.note_commands_button(course, index, false, cx)),
            )
            .children(self.course_read_error(course, index, cx))
            .children(
                self.exports
                    .update(cx, |exports, cx| exports.feedback(course, cx)),
            )
            .on_drag(note_drag(course), |drag, _, _, cx| cx.new(|_| drag.clone()))
            .map(|card| self.reorderable(card, index, [true; 4], cx))
            .into_any_element()
    }

    /// One note as a row: its cover, title, line and status, the row as one action
    /// and its commands at the end. Library lists and the workbench's recent notes
    /// show the same row.
    fn note_row(&self, index: usize, outer: OuterCorners, cx: &mut Context<Self>) -> AnyElement {
        let Some(course) = self.library(cx).courses.get(index).cloned() else {
            return div().into_any_element();
        };
        let course = &course;
        let scale = TileScale::Regular;
        let home = self.page != Page::Library;
        let commands = self.note_commands(course, cx);
        let details: Vec<Div> = self
            .course_read_error(course, index, cx)
            .into_iter()
            .chain(
                self.exports
                    .update(cx, |exports, cx| exports.feedback(course, cx))
                    .map(|feedback| feedback.px(rem(16.)).pb(rem(12.))),
            )
            .collect();
        let open = course.clone();
        let duration = self
            .library(cx)
            .note_source(course)
            .and_then(|source| source.duration)
            .filter(|_| home)
            .map(course2md::render::fmt_ts);
        let row =
            scale
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
                        .child(
                            row_cover(course.thumbnail.clone(), symbols::description()).children(
                                duration.map(|duration| {
                                    media_label(("note-duration", index), duration)
                                        .absolute()
                                        .right(rem(4.))
                                        .bottom(rem(4.))
                                }),
                            ),
                        )
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
                                .child(scale.support(
                                    ("note-meta", index),
                                    self.note_line(course, false, cx),
                                )),
                        )
                        .children(self.note_status(course, cx))
                        .map(|row| {
                            if home {
                                row.child(tile_chevron())
                            } else {
                                row.child(self.note_commands_button(course, index, false, cx))
                            }
                        }),
                );
        list_surface(outer)
            .id(("note", index))
            .relative()
            .child(command_context_menu(
                ("note-context", index),
                Some(COMMAND_MENU_WIDTH),
                commands,
            ))
            .child(row)
            .children(details)
            .on_drag(note_drag(course), |drag, _, _, cx| cx.new(|_| drag.clone()))
            .when(!home, |row| self.reorderable(row, index, outer, cx))
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
            .workspace(cx)?
            .state
            .tasks
            .iter()
            .filter(|task| {
                Some(&task.id) != linked.as_ref()
                    && task.handled_by.is_none()
                    && (matches!(
                        task.state,
                        TaskState::Queued | TaskState::Running | TaskState::Pausing
                    ) || self.is_running_task(&task.id, cx))
            })
            .cloned()
            .collect();
        if tasks.is_empty() {
            return None;
        }
        let scale = TileScale::Regular;
        let count = tasks.len();
        let mut list = v_flex().w_full().min_w_0().gap(TILE_GAP);
        for (index, task) in tasks.iter().enumerate() {
            let id = task.id.clone();
            let queued = task.state == TaskState::Queued && !self.is_running_task(&id, cx);
            let cancel = id.clone();
            let menu = self.tasks.read(cx).task_menu(task, cx);
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
                            .tone(ButtonTone::Neutral)
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
                    .relative()
                    .w_full()
                    .child(MaterialContextMenu::new(
                        SharedString::from(format!("workbench-task-context-{cancel}")),
                        menu,
                    ))
                    .child(row),
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
        let queued = task.state == TaskState::Queued && !self.is_running_task(id, cx);
        let cancelling = task.intent == Intent::Cancel;
        let progress_now = self.tasks.read(cx).compact_task_progress(task, cx);
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
                    if self.is_running_task(id, cx) {
                        self.tasks
                            .read(cx)
                            .with_job_clock(course2md::tr!("正在准备…"), cx)
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
        use crate::workspace::TaskState;
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
        let requires_repair =
            !uncertain && desktop_tasks::task_view::task_requires_service_repair(&task);
        let partial_components = if task.state == TaskState::Partial && task.artifact.is_some() {
            desktop_tasks::task_view::task_component_failures(&task)
                .into_iter()
                .filter_map(|(component, _, _)| course2md::execution::Component::parse(&component))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut actions = h_flex().gap(rem(8.)).flex_wrap();
        if let Some(course) = task.artifact.as_ref().and_then(|path| {
            self.library(cx)
                .courses
                .iter()
                .find(|course| &course.dir == path)
                .cloned()
        }) {
            let label = if self.course_has_reading_position(&course, cx) {
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
        let next = if !requires_repair && !uncertain && !partial_components.is_empty() {
            let labels = partial_components
                .iter()
                .map(|component| component.as_str().to_owned())
                .collect::<Vec<_>>();
            let task_id = id.clone();
            action("retry", ButtonVariant::Text)
                .icon(symbols::refresh())
                .child(desktop_tasks::task_view::retry_components_label(&labels))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let parts = partial_components.clone();
                    this.reprocess_task(task_id.clone(), parts, Vec::new(), cx);
                }))
        } else {
            let next = if requires_repair {
                TaskAction::Repair
            } else if !uncertain && task.state == TaskState::Paused {
                TaskAction::Resume
            } else {
                TaskAction::ViewTask
            };
            let (label, icon, _) = self.tasks.read(cx).present(&next, &task, cx);
            action(label, ButtonVariant::Text)
                .icon(icon)
                .child(label)
                .on_click(crate::task_ui::action_click(
                    self.tasks.downgrade(),
                    id.clone(),
                    next,
                ))
        };
        actions = actions.child(next.tone(ButtonTone::Neutral));
        let scale = TileScale::Regular;
        let summary = desktop_tasks::task_view::task_attention_summary(&task);
        list_surface(outer).child(
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
                        // The same colour as on Tasks: a failure is red, a pause amber.
                        .child(
                            status_chip(crate::task_ui::task_status_kind(task.state))
                                .child(task.state.label()),
                        )
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
            .records
            .read(cx)
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
                        && !desktop_tasks::task_view::task_component_failures(task).is_empty()))
            })
            .collect();
        attention.sort_by_key(|task| std::cmp::Reverse(task.updated));
        attention.truncate(2);
        let attention: Vec<_> = attention.into_iter().cloned().collect();
        let notes: Vec<usize> = self
            .library(cx)
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
        let mut rows = v_flex().w_full().min_w_0();
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
                        this.records.change(cx, |records, _| {
                            records.library.folder_filter = FolderFilter::All
                        });
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
        let access = self.cached_library_access(cx);
        let checking = self.library(cx).loading || access.is_none();
        let unavailable = !self.library(cx).issues.is_empty()
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

/// How the place in view lists its notes, behind a button that names the rule; the
/// button's arrow says which way the rule runs.
#[derive(gpui::IntoElement)]
struct SortMenu {
    sort: Sort,
    enabled: bool,
    /// An icon button that its tooltip names, where the toolbar is short of room.
    compact: bool,
    desktop: WeakEntity<Desktop>,
}

/// A rule's name in the sort menu and on its button.
fn rule_label(rule: SortRule) -> &'static str {
    match rule {
        SortRule::Manual => course2md::tr!("手动排序"),
        SortRule::Added => course2md::tr!("添加时间"),
        SortRule::Published => course2md::tr!("发布时间"),
        SortRule::Title => course2md::tr!("标题"),
        SortRule::Duration => course2md::tr!("时长"),
        SortRule::LastRead => course2md::tr!("最近阅读"),
    }
}

/// What each direction of a rule puts first: (ascending, descending).
fn direction_labels(rule: SortRule) -> (&'static str, &'static str) {
    match rule {
        SortRule::Added | SortRule::Published => {
            (course2md::tr!("最早的在前"), course2md::tr!("最新的在前"))
        }
        SortRule::Title => (course2md::tr!("A 到 Z"), course2md::tr!("Z 到 A")),
        SortRule::Duration => (course2md::tr!("最短的在前"), course2md::tr!("最长的在前")),
        SortRule::LastRead => (
            course2md::tr!("最久未读的在前"),
            course2md::tr!("最近读过的在前"),
        ),
        SortRule::Manual => ("", ""),
    }
}

impl RenderOnce for SortMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id: ElementId = "library-sort".into();
        let key_toggle = ui::menu_key_toggle(&id, window, cx);
        let Self {
            sort,
            enabled,
            compact,
            desktop,
        } = self;
        let pick = move |sort: Sort| {
            let desktop = desktop.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = desktop.update(cx, |this, cx| this.set_place_sort(sort, cx));
            }
        };
        let mut items: Vec<MaterialMenuItem> = SortRule::ALL
            .into_iter()
            .map(|rule| {
                MaterialMenuItem::new(rule_label(rule))
                    .selected(rule == sort.rule)
                    .on_click(pick(Sort {
                        rule,
                        // A rule keeps the way it ran; one newly picked starts from its
                        // usual way: newest, longest and most recently read first, A to Z.
                        descending: if rule == sort.rule {
                            sort.descending
                        } else {
                            rule != SortRule::Title
                        },
                    }))
            })
            .collect();
        let direction = if sort.rule.has_direction() {
            let (ascending, descending) = direction_labels(sort.rule);
            if let Some(last) = items.pop() {
                items.push(last.divider(true));
            }
            items.extend(
                [(false, ascending), (true, descending)].map(|(way, label)| {
                    MaterialMenuItem::new(label)
                        .selected(way == sort.descending)
                        .on_click(pick(Sort {
                            rule: sort.rule,
                            descending: way,
                        }))
                }),
            );
            let (ascending, descending) = direction_labels(sort.rule);
            Some(if sort.descending {
                descending
            } else {
                ascending
            })
        } else {
            None
        };
        let label = rule_label(sort.rule);
        let spoken = match direction {
            Some(direction) => course2md::tr!("排序方式：{}，{}", label, direction),
            None => course2md::tr!("排序方式：{}", label),
        };
        let icon = match (sort.rule.has_direction(), sort.descending) {
            (false, _) => symbols::drag_indicator(),
            (true, true) => symbols::arrow_downward(),
            (true, false) => symbols::arrow_upward(),
        };
        let trigger = if compact {
            MaterialTooltip::plain("library-sort-tip", spoken.clone())
                .trigger(
                    IconButton::new("library-sort-button", icon, spoken)
                        .variant(IconButtonVariant::Tonal)
                        .tone(ButtonTone::Neutral)
                        .disabled(!enabled)
                        .on_click(key_toggle),
                )
                .into_any_element()
        } else {
            material_button("library-sort-button", ButtonVariant::Tonal)
                .tone(ButtonTone::Neutral)
                .icon(icon)
                .child(label)
                .accessibility_label(spoken)
                .disabled(!enabled)
                .on_click(key_toggle)
                .into_any_element()
        };
        ui::menu(
            id,
            trigger,
            MaterialMenu::new("library-sort-items").items(items),
            window,
            cx,
        )
    }
}

/// How a note's unfinished task shows on it: by its own state, so one stopped for
/// the person, such as at the limit of copies kept beside the note, does not read
/// as still processing.
fn task_status(state: crate::workspace::TaskState) -> (StatusKind, &'static str) {
    use crate::workspace::TaskState;
    match state {
        TaskState::Paused => (StatusKind::Neutral, state.label()),
        TaskState::NeedsAttention | TaskState::Uncertain => (StatusKind::Warning, state.label()),
        _ => (StatusKind::Progress, course2md::tr!("正在处理")),
    }
}

/// Milliseconds since the epoch, now.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The key of the menu items and shortcuts that move a note: Option on macOS.
fn move_shortcut(arrow: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌥{arrow}")
    } else {
        format!("Alt+{arrow}")
    }
}

impl Desktop {
    /// Where the place in view keeps its sort: the library in view's record, or, for
    /// every note (`None`), the app's own record.
    fn order_home(&self, cx: &App) -> Option<PathBuf> {
        let library = self.library(cx);
        (!library.folder_filter.is_all()).then(|| library.root.clone())
    }

    /// The place in view, where it keeps its sort, its sort and whether that record
    /// has been read.
    fn place_sort(&self, cx: &App) -> (Option<PathBuf>, Place, Sort, bool) {
        let home = self.order_home(cx);
        let place = self.library(cx).folder_filter.place();
        let (sort, read) = match &home {
            Some(root) => {
                let order = self.library(cx).view_cache.orders.get(root);
                (
                    order.map(|order| order.sort(place)).unwrap_or_default(),
                    order.is_some(),
                )
            }
            None => match &self.library_ui.all_notes {
                Some(all) => (all.sort(), true),
                None => (Sort::default(), false),
            },
        };
        (home, place, sort, read)
    }

    /// The hand-made order of the place in view.
    fn place_manual<'a>(&'a self, cx: &'a App) -> &'a [String] {
        match self.order_home(cx) {
            Some(root) => self
                .library(cx)
                .view_cache
                .orders
                .get(&root)
                .map(|order| order.manual(self.library(cx).folder_filter.place()))
                .unwrap_or_default(),
            None => self
                .library_ui
                .all_notes
                .as_ref()
                .map(|all| all.manual())
                .unwrap_or_default(),
        }
    }

    /// A note's key in its own library's record: its course id, or a copy's own key.
    fn own_note_key(&self, course: &Course, cx: &App) -> String {
        self.library(cx)
            .note_key(course)
            .map(str::to_owned)
            .or_else(|| {
                course
                    .manifest
                    .as_ref()
                    .map(|manifest| manifest.course_id.clone())
            })
            .unwrap_or_else(|| {
                course
                    .dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
    }

    /// A note's key in the record of `home`: its own key in its library's record, and
    /// in the app's record (`None`), qualified by the library's id.
    fn note_key(&self, course: &Course, home: Option<&std::path::Path>, cx: &App) -> String {
        let key = self.own_note_key(course, cx);
        match (home, self.course_location(course, cx)) {
            (None, Some(location)) => format!("{}/{key}", location.id),
            _ => key,
        }
    }

    /// What the sort rules read of a note: when it was added and last read come from
    /// its own library's record.
    fn note_facts(&self, course: &Course, home: Option<&std::path::Path>, cx: &App) -> NoteFacts {
        let library = self.library(cx);
        let created = course
            .manifest
            .as_ref()
            .map(|manifest| manifest.created_at_ms)
            .unwrap_or_else(|| {
                course
                    .modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_millis() as u64)
                    .unwrap_or(0)
            });
        let own = self
            .course_location(course, cx)
            .and_then(|location| library.view_cache.orders.get(&location.root));
        let own_key = self.own_note_key(course, cx);
        let source = library.note_source(course);
        NoteFacts {
            key: self.note_key(course, home, cx),
            title: course.title.clone(),
            added: own
                .and_then(|order| order.added(&own_key))
                .unwrap_or(created),
            published: source.and_then(|source| source.published.clone()),
            duration: source.and_then(|source| source.duration),
            read: own.and_then(|order| order.last_read(&own_key)),
        }
    }

    /// `courses` in the place's order. The last result is kept until the notes, what
    /// is known of them, the place or its sort change.
    fn arrange_courses(&self, courses: Vec<usize>, cx: &App) -> Vec<usize> {
        use std::hash::{Hash, Hasher};
        let (home, place, sort, _) = self.place_sort(cx);
        let manual = self.place_manual(cx);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        manual.hash(&mut hasher);
        let key = crate::library_ui::ArrangeKey {
            revision: self.library(cx).revision,
            home: home.clone(),
            place,
            sort,
            manual: hasher.finish(),
            courses,
        };
        if let Some((cached, arranged)) = &*self.library_ui.arranged.borrow()
            && *cached == key
        {
            return arranged.clone();
        }
        let library = self.library(cx);
        let facts: Vec<NoteFacts> = key
            .courses
            .iter()
            .map(|index| self.note_facts(&library.courses[*index], home.as_deref(), cx))
            .collect();
        let arranged: Vec<usize> = order::arrange(&facts, sort, manual)
            .into_iter()
            .map(|position| key.courses[position])
            .collect();
        *self.library_ui.arranged.borrow_mut() = Some((key, arranged.clone()));
        arranged
    }

    /// Whether notes can be moved by hand here: the place's record has been read and
    /// the whole place is shown, not a search of it.
    fn can_reorder(&self, cx: &App) -> bool {
        self.place_sort(cx).3
            && self.value(Field::Search, cx).is_empty()
            && self.library(cx).error.is_none()
            && self
                .library_ui
                .note_runs
                .iter()
                .map(Vec::len)
                .sum::<usize>()
                > 1
    }

    /// The sort menu of the place in view.
    fn sort_menu(&self, compact: bool, cx: &mut Context<Self>) -> SortMenu {
        let (_, _, sort, read) = self.place_sort(cx);
        SortMenu {
            sort,
            enabled: read && self.library(cx).error.is_none(),
            compact,
            desktop: cx.weak_entity(),
        }
    }

    /// Changes the place's sort. Picking the hand-made order the first time starts it
    /// from the order the place's notes are in now, whatever a search shows.
    fn set_place_sort(&mut self, sort: Sort, cx: &mut Context<Self>) {
        let (home, place, current, read) = self.place_sort(cx);
        if !read || sort == current {
            return;
        }
        let manual = (sort.rule == SortRule::Manual && self.place_manual(cx).is_empty())
            .then(|| self.place_keys(home.as_deref(), cx));
        self.change_place_order(home, place, sort, manual, cx);
    }

    /// Every note of the place in view in reach, unfiltered by a search, in its
    /// current order, as keys of `home`'s record.
    fn place_keys(&self, home: Option<&std::path::Path>, cx: &App) -> Vec<String> {
        let available: Vec<PathBuf> = self
            .cached_library_access(cx)
            .map(|access| {
                access
                    .available
                    .into_iter()
                    .filter(|root| {
                        self.library(cx).folder_filter.is_all() || *root == self.library(cx).root
                    })
                    .collect()
            })
            .unwrap_or_default();
        let courses = self.place_courses("", &available, cx);
        let library = self.library(cx);
        self.arrange_courses(courses, cx)
            .into_iter()
            .map(|index| self.note_key(&library.courses[index], home, cx))
            .collect()
    }

    /// Sets a place's sort and, when it changed, its hand-made order, in view at
    /// once; then saves it.
    fn change_place_order(
        &mut self,
        home: Option<PathBuf>,
        place: Place,
        sort: Sort,
        manual: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = home else {
            let manual = manual.map(|keys| self.keep_known_keys(keys, cx));
            if let Some(all) = &mut self.library_ui.all_notes {
                all.set(sort, manual);
                self.library_ui.all_notes_unsaved = true;
                self.save_all_notes_order(cx);
            }
            cx.notify();
            return;
        };
        let pending = self
            .library_ui
            .order_unsaved
            .entry((root.clone(), place))
            .or_insert((sort, None));
        pending.0 = sort;
        if manual.is_some() {
            pending.1 = manual;
        }
        self.reapply_pending_orders(cx);
        self.save_place_orders(cx);
        cx.notify();
    }

    /// Writes how every note sorts to the app's own file, off the UI thread and one
    /// write at a time, each with what is in view when its turn comes.
    fn save_all_notes_order(&mut self, cx: &mut Context<Self>) {
        if self.library_ui.all_notes_saving || !self.library_ui.all_notes_unsaved {
            return;
        }
        let Some(all) = self.library_ui.all_notes.clone() else {
            return;
        };
        self.library_ui.all_notes_unsaved = false;
        self.library_ui.all_notes_saving = true;
        self.in_background(
            move || all.save(&order::all_notes_path()),
            |this, result, cx| {
                this.library_ui.all_notes_saving = false;
                match result {
                    Ok(()) => this.save_all_notes_order(cx),
                    // Still unsaved: the next change or scan tries again.
                    Err(error) => {
                        this.library_ui.all_notes_unsaved = true;
                        this.message =
                            Some(course2md::tr!("排序方式尚未保存：{}", format!("{error:#}")));
                    }
                }
            },
            cx,
        );
    }

    /// Reads how every note sorts, once, off the UI thread; until then its sort menu
    /// waits.
    pub(crate) fn load_all_notes_order(&mut self, cx: &mut Context<Self>) {
        if self.library_ui.all_notes.is_some() || self.library_ui.all_notes_loading {
            return;
        }
        self.library_ui.all_notes_loading = true;
        self.in_background(
            || Ok(order::PlaceOrder::load(&order::all_notes_path())),
            |this, result: anyhow::Result<order::PlaceOrder>, cx| {
                this.library_ui.all_notes_loading = false;
                this.library_ui.all_notes = Some(result.unwrap_or_default());
                cx.notify();
            },
            cx,
        );
    }

    /// Of every note's hand-made order, the keys still worth keeping: notes of a
    /// library out of reach keep their turn, and notes their library's record no
    /// longer knows, or of a library no longer registered, are dropped.
    fn keep_known_keys(&self, keys: Vec<String>, cx: &App) -> Vec<String> {
        let Some(workspace) = self.workspace(cx) else {
            return keys;
        };
        let orders = &self.library(cx).view_cache.orders;
        keys.into_iter()
            .filter(|key| {
                let Some((id, own)) = key.split_once('/') else {
                    return false;
                };
                workspace
                    .state
                    .libraries
                    .iter()
                    .find(|location| location.id == id)
                    .is_some_and(|location| {
                        orders
                            .get(&location.root)
                            .is_none_or(|order| order.knows(own))
                    })
            })
            .collect()
    }

    /// Makes the changes to library records again over what a scan read: those
    /// being written, those written since that scan began, notes read but not yet
    /// written down, then sorts not yet saved. A change written before the scan
    /// began is in what it read, and is let go. Called after every scan.
    pub(crate) fn reapply_pending_orders(&mut self, cx: &mut Context<Self>) {
        // A save that failed tries again now.
        self.save_all_notes_order(cx);
        if std::mem::take(&mut self.library_ui.order_save_paused) {
            self.save_place_orders(cx);
        }
        let generation = self.library(cx).generation;
        self.library_ui
            .order_edits
            .retain(|edit| order::edit_still_needed(edit.settled, generation));
        let edits = self.library_ui.order_edits.clone();
        let reads = self.library_ui.reads_unwritten.clone();
        let pending = self.library_ui.order_unsaved.clone();
        if edits.is_empty() && reads.is_empty() && pending.is_empty() {
            return;
        }
        self.records.change(cx, |records, _| {
            let orders = &mut records.library.view_cache.orders;
            for edit in edits {
                edit.edit.apply(orders.entry(edit.root).or_default());
            }
            for ((root, key), at) in reads {
                orders.entry(root).or_default().mark_read(&key, at);
            }
            for ((root, place), (sort, manual)) in pending {
                orders
                    .entry(root)
                    .or_default()
                    .set_place(place, sort, manual);
            }
            records.library.touch();
        });
    }

    /// Writes `edit` to the library's record off the UI thread. Until a scan that
    /// began after the write has read it back, it is made again over every scan.
    fn write_order_edit(&mut self, root: PathBuf, edit: order::OrderEdit, cx: &mut Context<Self>) {
        let id = self.library_ui.next_order_edit;
        self.library_ui.next_order_edit += 1;
        self.library_ui
            .order_edits
            .push(crate::library_ui::OrderEditEntry {
                id,
                root: root.clone(),
                edit: edit.clone(),
                settled: None,
            });
        self.in_background(
            move || order::Order::edit(&root, |order| edit.apply(order)),
            move |this, result, cx| {
                // Written or not, it stays in view until a later scan reads the record;
                // a failure is let go then, as only sorting depends on it.
                let _ = result;
                this.settle_order_edit(id, cx);
            },
            cx,
        );
    }

    /// Marks an edit as written as of the scan in view.
    fn settle_order_edit(&mut self, id: u64, cx: &App) {
        let generation = self.library(cx).generation;
        if let Some(edit) = self
            .library_ui
            .order_edits
            .iter_mut()
            .find(|edit| edit.id == id)
        {
            edit.settled = Some(generation);
        }
    }

    /// Saves the library places whose sort changed, one at a time, each as it is
    /// when its turn comes; the hand-made order is written only when it changed.
    fn save_place_orders(&mut self, cx: &mut Context<Self>) {
        if self.library_ui.order_saving.is_some() || self.library_ui.order_save_paused {
            return;
        }
        let Some((key, (sort, manual))) = self
            .library_ui
            .order_unsaved
            .iter()
            .next()
            .map(|(key, value)| (key.clone(), value.clone()))
        else {
            return;
        };
        self.library_ui.order_saving = Some(key.clone());
        let (root, place) = key.clone();
        let sent = (sort, manual.clone());
        let edit = order::OrderEdit::Place {
            place,
            sort,
            manual,
        };
        let written = edit.clone();
        self.in_background(
            move || order::Order::edit(&root, |order| written.apply(order)),
            move |this, result, cx| {
                this.library_ui.order_saving = None;
                match result {
                    Ok(_) => {
                        // A change made while this one was saving waits for its own turn.
                        if this.library_ui.order_unsaved.get(&key) == Some(&sent) {
                            this.library_ui.order_unsaved.remove(&key);
                        }
                        // Kept until a scan that began after it reads it back.
                        let id = this.library_ui.next_order_edit;
                        this.library_ui.next_order_edit += 1;
                        this.library_ui
                            .order_edits
                            .push(crate::library_ui::OrderEditEntry {
                                id,
                                root: key.0.clone(),
                                edit,
                                settled: Some(this.library(cx).generation),
                            });
                        // A folder deleted while its sort was saving is forgotten again,
                        // so this save does not bring it back.
                        if let Place::Folder(folder) = key.1
                            && this
                                .library(cx)
                                .indexes
                                .get(&key.0)
                                .is_some_and(|index| !index.folders.contains_key(&folder))
                        {
                            this.forget_folder_orders(key.0.clone(), vec![folder], cx);
                        }
                    }
                    Err(error) => {
                        // Kept in view and not claimed saved: the change stays waiting,
                        // and saving pauses until the next change or scan tries again.
                        this.library_ui.order_save_paused = true;
                        this.message =
                            Some(course2md::tr!("排序方式尚未保存：{}", format!("{error:#}")));
                    }
                }
                let paused = this.library_ui.order_save_paused;
                this.reapply_pending_orders(cx);
                this.library_ui.order_save_paused = paused;
                this.save_place_orders(cx);
            },
            cx,
        );
    }

    /// Forgets the sorts and orders of folders that were deleted, including any
    /// change to them not yet saved.
    pub(crate) fn forget_folder_orders(
        &mut self,
        root: PathBuf,
        folders: Vec<u64>,
        cx: &mut Context<Self>,
    ) {
        self.library_ui.order_unsaved.retain(|(pending, place), _| {
            !(*pending == root && matches!(place, Place::Folder(id) if folders.contains(id)))
        });
        self.library_ui.order_edits.retain(|edit| {
            !(edit.root == root
                && matches!(&edit.edit, order::OrderEdit::Place { place: Place::Folder(id), .. } if folders.contains(id)))
        });
        let edit = order::OrderEdit::ForgetFolders(folders);
        self.records.change(cx, |records, _| {
            if let Some(order) = records.library.view_cache.orders.get_mut(&root) {
                edit.apply(order);
            }
            records.library.touch();
        });
        self.write_order_edit(root, edit, cx);
    }

    /// Takes the note in `storage` to where `target` is, as a drag does. Under a
    /// sort rule the place switches to its hand-made order, starting from the order
    /// shown.
    fn drop_note_on(&mut self, storage: &std::path::Path, target: usize, cx: &mut Context<Self>) {
        let library = self.library(cx);
        let Some(moving) = library
            .courses
            .iter()
            .position(|course| course.dir == storage)
        else {
            return;
        };
        if moving == target
            || !self.can_reorder(cx)
            || !self
                .library_ui
                .note_runs
                .iter()
                .flatten()
                .any(|i| *i == moving)
        {
            return;
        }
        self.move_note_onto(moving, target, false, cx);
    }

    /// Moves a note one place along its run, or a card row up or down; down from a
    /// card with no card below it, to the end of its run.
    fn step_note(&mut self, index: usize, step: isize, cx: &mut Context<Self>) {
        if !self.can_reorder(cx) {
            return;
        }
        let neighbor = self
            .library_ui
            .note_runs
            .iter()
            .find_map(|run| neighbor_in_run(run, index, step));
        if let Some(neighbor) = neighbor {
            self.library_ui.reveal = Some(index);
            self.move_note_onto(index, neighbor, true, cx);
        }
    }

    fn move_note_onto(
        &mut self,
        moving: usize,
        target: usize,
        announce: bool,
        cx: &mut Context<Self>,
    ) {
        let (home, place, sort, _) = self.place_sort(cx);
        let library = self.library(cx);
        let (Some(moving_course), Some(target_course)) =
            (library.courses.get(moving), library.courses.get(target))
        else {
            return;
        };
        let title = moving_course.title.clone();
        let moving_key = self.note_key(moving_course, home.as_deref(), cx);
        let target_key = self.note_key(target_course, home.as_deref(), cx);
        let shown = self.place_keys(home.as_deref(), cx);
        let Some(mut moved) = order::move_onto(&shown, &moving_key, &target_key) else {
            return;
        };
        // Notes out of reach keep their turn after the ones in reach.
        let shown: std::collections::HashSet<&String> = shown.iter().collect();
        moved.extend(
            self.place_manual(cx)
                .iter()
                .filter(|key| !shown.contains(key))
                .cloned(),
        );
        let switched = sort.rule != SortRule::Manual;
        let manual = Sort {
            rule: SortRule::Manual,
            descending: sort.descending,
        };
        self.change_place_order(home, place, manual, Some(moved), cx);
        // Where it landed among the notes it is shown with.
        let run = self
            .library_ui
            .note_runs
            .iter()
            .find(|run| run.contains(&moving))
            .cloned()
            .unwrap_or_default();
        let total = run.len();
        let position = self
            .arrange_courses(run, cx)
            .iter()
            .position(|index| *index == moving)
            .map_or(0, |position| position + 1);
        if switched {
            self.message = Some(course2md::tr!(
                "已改为手动排序，《{}》在第 {} 位，共 {} 篇",
                title,
                position,
                total
            ));
        } else if announce {
            self.message = Some(course2md::tr!(
                "《{}》在第 {} 位，共 {} 篇",
                title,
                position,
                total
            ));
        }
        cx.notify();
    }

    /// The note menu's commands that move it earlier and later, when it can move.
    fn note_move_commands(&self, course: &Course, cx: &mut Context<Self>) -> Vec<Command> {
        if !self.can_reorder(cx) {
            return Vec::new();
        }
        let Some(index) = self
            .library(cx)
            .courses
            .iter()
            .position(|item| item.dir == course.dir)
        else {
            return Vec::new();
        };
        let Some((position, length)) = self
            .library_ui
            .note_runs
            .iter()
            .find_map(|run| Some((run.iter().position(|i| *i == index)?, run.len())))
        else {
            return Vec::new();
        };
        let cards = self.library_ui.note_columns.is_some();
        let desktop = cx.weak_entity();
        let (earlier, later) = if cards {
            ("←", "→")
        } else {
            ("↑", "↓")
        };
        vec![
            Command::new(
                course2md::tr!("前移一位"),
                symbols::arrow_upward(),
                &desktop,
                move |this, _, cx| this.step_note(index, -1, cx),
            )
            .shortcut(move_shortcut(earlier))
            .disabled(position == 0),
            Command::new(
                course2md::tr!("后移一位"),
                symbols::arrow_downward(),
                &desktop,
                move |this, _, cx| this.step_note(index, 1, cx),
            )
            .shortcut(move_shortcut(later))
            .disabled(position + 1 >= length)
            .ends_group(),
        ]
    }

    /// Option with an arrow moves the focused note: along a list, or in a grid of
    /// cards sideways one place and up or down one row.
    fn note_move_keys(
        &self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
        let columns = self.library_ui.note_columns;
        cx.listener(move |this, event: &KeyDownEvent, _, cx| {
            let modifiers = event.keystroke.modifiers;
            if !modifiers.alt || modifiers.control || modifiers.platform || modifiers.shift {
                return;
            }
            let row = columns.unwrap_or(1) as isize;
            let step = match (event.keystroke.key.as_str(), columns.is_some()) {
                ("up", _) => -row,
                ("down", _) => row,
                ("left", true) => -1,
                ("right", true) => 1,
                _ => return,
            };
            cx.stop_propagation();
            this.step_note(index, step, cx);
        })
    }

    /// Lets notes dropped on a library note take its place, and Option with an arrow
    /// move it. While a note is dragged over it, it shows the kit's drag state layer
    /// in the primary role, as the kit's drop targets do, inside its own `corners`.
    fn reorderable(
        &self,
        note: Stateful<Div>,
        index: usize,
        corners: OuterCorners,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        if self.page != Page::Library || !self.can_reorder(cx) {
            return note;
        }
        let layer = gpui_kit_material::tokens::state::layer(
            gpui::Rgba {
                r: 0.,
                g: 0.,
                b: 0.,
                a: 0.,
            },
            color(ColorRole::PRIMARY),
            gpui_kit_material::tokens::state::DRAG,
        );
        note.on_key_down(self.note_move_keys(index, cx))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .when(corners[0], |overlay| overlay.rounded_t(TILE_RADIUS))
                    .when(corners[2], |overlay| overlay.rounded_b(TILE_RADIUS))
                    .drag_over::<NoteDrag>(move |style, _, _, _| style.bg(layer)),
            )
            .on_drop(cx.listener(move |this, drag: &NoteDrag, _, cx| {
                this.drop_note_on(&drag.storage.clone(), index, cx)
            }))
    }

    /// Notes when a note was opened, for sorting by what was read last: in view at
    /// once, and written down unless it was written within the last few minutes.
    pub(crate) fn note_opened(&mut self, course: &Course, cx: &mut Context<Self>) {
        let Some(root) = self
            .course_location(course, cx)
            .map(|location| location.root.clone())
        else {
            return;
        };
        // An opened note's manifest stays with its reading; the library's copy names it.
        let listed = self
            .library(cx)
            .courses
            .iter()
            .find(|item| item.dir == course.dir)
            .cloned();
        let Some(listed) = listed.filter(|listed| listed.manifest.is_some()) else {
            return;
        };
        let key = self.own_note_key(&listed, cx);
        let at = now_ms();
        let note = (root.clone(), key.clone());
        let written = self
            .library_ui
            .read_written
            .get(&note)
            .copied()
            .or_else(|| {
                self.library(cx)
                    .view_cache
                    .orders
                    .get(&root)
                    .and_then(|order| order.last_read(&key))
            });
        self.records.change(cx, |records, _| {
            records
                .library
                .view_cache
                .orders
                .entry(root.clone())
                .or_default()
                .mark_read(&key, at);
            records.library.touch();
        });
        if order::read_worth_writing(written, at) {
            self.library_ui.reads_unwritten.remove(&note);
            self.library_ui.read_written.insert(note, at);
            self.write_order_edit(root, order::OrderEdit::Read { key, at }, cx);
        } else {
            // Kept in view over later scans, and written with the next read that is.
            self.library_ui.reads_unwritten.insert(note, at);
            if let Some(written) = written {
                self.library_ui
                    .read_written
                    .entry((root, key))
                    .or_insert(written);
            }
        }
        cx.notify();
    }
}

/// The note `step` places from `index` in `run`, if `index` is in it. A step down
/// a row from a card with no card below it reaches the end of the run.
fn neighbor_in_run(run: &[usize], index: usize, step: isize) -> Option<usize> {
    let position = run.iter().position(|i| *i == index)?;
    let target = position.checked_add_signed(step)?;
    match run.get(target) {
        Some(neighbor) => Some(*neighbor),
        None if step > 1 && position + 1 < run.len() => run.last().copied(),
        None => None,
    }
}

/// A place with nothing in it: a tinted icon, what that means and the way forward,
/// which the caller appends.
pub(crate) fn empty_state(
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
    fn option_down_from_a_card_with_none_below_goes_to_the_end() {
        use super::neighbor_in_run;
        // Three columns: 0 1 2 / 3 4 5 / 6 7.
        let run: Vec<usize> = (10..18).collect();
        assert_eq!(neighbor_in_run(&run, 11, 3), Some(14));
        // Below 15 (last row has two cards) there is none: it goes to the end.
        assert_eq!(neighbor_in_run(&run, 15, 3), Some(17));
        // On the last row there is nowhere further down, and up still steps a row.
        assert_eq!(neighbor_in_run(&run, 16, 3), Some(17));
        assert_eq!(neighbor_in_run(&run, 17, 3), None);
        assert_eq!(neighbor_in_run(&run, 16, -3), Some(13));
        assert_eq!(neighbor_in_run(&run, 11, -3), None);
        // One place along, and a note not in the run.
        assert_eq!(neighbor_in_run(&run, 17, 1), None);
        assert_eq!(neighbor_in_run(&run, 10, -1), None);
        assert_eq!(neighbor_in_run(&run, 99, 1), None);
    }

    #[test]
    fn a_note_whose_task_stopped_for_the_person_does_not_read_as_processing() {
        use crate::workspace::TaskState;
        use gpui_kit_material::prelude::StatusKind;
        let shown = |state| super::task_status(state).0;
        assert!(matches!(
            shown(TaskState::NeedsAttention),
            StatusKind::Warning
        ));
        assert_eq!(
            super::task_status(TaskState::NeedsAttention).1,
            TaskState::NeedsAttention.label()
        );
        assert!(matches!(shown(TaskState::Uncertain), StatusKind::Warning));
        assert!(matches!(shown(TaskState::Paused), StatusKind::Neutral));
        for state in [TaskState::Queued, TaskState::Running, TaskState::Pausing] {
            assert!(matches!(shown(state), StatusKind::Progress));
        }
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
