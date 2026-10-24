//! The note library's folder tree. Each library is a root whose unfiled notes
//! sit at its top level; folders nest inside it, and notes and folders move
//! by dragging onto a folder or through the same commands on right-click.
use super::*;
use crate::theme::*;
use gpui_component::button::ButtonVariants;
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use std::{collections::HashMap, path::Path};

/// Width of the folder navigation beside the notes.
pub(crate) const FOLDER_SIDEBAR: Rems = rems(240. / 14.);
/// Space between the folder navigation and the notes.
pub(crate) const FOLDER_SIDEBAR_GAP: Rems = rems(32. / 14.);
/// Below this content width (navigation, gap and a 560px note column) the
/// navigation folds into a menu above the notes.
const FOLDER_SIDEBAR_MIN_CONTENT: f32 = 832.;
/// A navigation destination: a 56px pill with a 22px icon.
const NAV_ROW: Rems = rems(56. / 14.);
const NAV_ICON: Rems = rems(22. / 14.);
const NAV_LABEL: Rems = rems(15. / 14.);
const NAV_COUNT: Rems = rems(13. / 14.);
const NAV_LEADING: Rems = rems(16. / 14.);
const NAV_TRAILING: Rems = rems(20. / 14.);
const NAV_GAP: Rems = rems(12. / 14.);
/// Nested folders step in by this much per level.
const TREE_INDENT: Rems = rems(16. / 14.);
const TREE_DISCLOSURE: Rems = rems(28. / 14.);

/// A note being dragged onto a folder.
#[derive(Clone)]
pub(crate) struct NoteDrag {
    pub(crate) storage: PathBuf,
    pub(crate) title: SharedString,
}

/// A folder being dragged into another folder or its library's top level.
#[derive(Clone)]
pub(crate) struct FolderDrag {
    root: PathBuf,
    id: u64,
    name: SharedString,
}

impl Render for NoteDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_chip(icons::article(), self.title.clone())
    }
}

impl Render for FolderDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_chip(icons::folder(), self.name.clone())
    }
}

/// A drop target under a dragged note or folder, distinct from selection.
fn drop_highlight(style: StyleRefinement) -> StyleRefinement {
    style.bg(blend(color(SECONDARY_CONTAINER), color(ACCENT), 0.3))
}

/// A truncated name shows in full on hover.
fn name_tooltip(name: SharedString) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |window, cx| gpui_component::tooltip::Tooltip::new(name.clone()).build(window, cx)
}

/// Note counts shown beside the navigation's places.
#[derive(Default)]
struct NavCounts {
    all: usize,
    unfiled: HashMap<PathBuf, usize>,
    folders: HashMap<(PathBuf, u64), usize>,
}

/// A place in the tree: every note, a library's top level, or a folder.
#[derive(Clone, PartialEq)]
enum TreeTarget {
    All,
    Root(PathBuf),
    Folder(PathBuf, u64),
}

impl Desktop {
    /// Every library with its folder record, when that record has loaded.
    fn tree_libraries(&self) -> Vec<(PathBuf, String, Option<organize::Library>)> {
        let libraries: Vec<_> = self
            .workspace
            .as_ref()
            .map(|workspace| {
                workspace
                    .state
                    .libraries
                    .iter()
                    .map(|location| {
                        (
                            location.root.clone(),
                            location.name.clone(),
                            self.library_indexes.get(&location.root).cloned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if libraries.is_empty() {
            vec![(
                self.library_root.clone(),
                course2md::tr!("笔记库").to_owned(),
                Some(self.library.clone()),
            )]
        } else {
            libraries
        }
    }

    pub(crate) fn folder_sidebar_visible(&self, window: &Window, cx: &App) -> bool {
        let scale = f32::from(window.rem_size()) / 14.;
        self.library_controls_visible(cx)
            && crate::views::shell_content_width(Page::Library, window)
                >= FOLDER_SIDEBAR_MIN_CONTENT * scale
    }

    fn selected_target(&self) -> TreeTarget {
        match self.folder_filter {
            FolderFilter::All => TreeTarget::All,
            FolderFilter::Unfiled => TreeTarget::Root(self.library_root.clone()),
            FolderFilter::Folder(id) => TreeTarget::Folder(self.library_root.clone(), id),
        }
    }

    pub(crate) fn select_library_scope(
        &mut self,
        root: PathBuf,
        filter: FolderFilter,
        cx: &mut Context<Self>,
    ) {
        if let Some(organization) = self.library_indexes.get(&root) {
            self.library = organization.clone();
        }
        // The tree keeps the selected folder visible: its library top and
        // every folder above it open.
        if let (FolderFilter::Folder(id), Some(organization)) =
            (filter, self.library_indexes.get(&root))
        {
            let mut parent = organization.parent(id);
            while let Some(folder) = parent {
                self.folder_tree_open.insert((root.clone(), folder));
                parent = organization.parent(folder);
            }
            self.folder_tree_open.remove(&(root.clone(), 0));
        }
        self.library_root = root;
        self.folder_filter = filter;
        self.library_list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    fn select_target(&mut self, target: TreeTarget, cx: &mut Context<Self>) {
        match target {
            TreeTarget::All => {
                let root = self.library_root.clone();
                self.select_library_scope(root, FolderFilter::All, cx)
            }
            TreeTarget::Root(root) => self.select_library_scope(root, FolderFilter::Unfiled, cx),
            TreeTarget::Folder(root, id) => {
                self.select_library_scope(root, FolderFilter::Folder(id), cx)
            }
        }
    }

    fn toggle_tree_node(&mut self, root: &Path, id: u64, cx: &mut Context<Self>) {
        let key = (root.to_path_buf(), id);
        if !self.folder_tree_open.remove(&key) {
            self.folder_tree_open.insert(key);
        }
        cx.notify();
    }

    /// Library tops start open; folders open when the person opens them.
    fn tree_node_open(&self, root: &Path, id: u64) -> bool {
        let open = self.folder_tree_open.contains(&(root.to_path_buf(), id));
        if id == 0 { !open } else { open }
    }

    /// File a note into a folder of its own library, or unfile it.
    pub(crate) fn file_note(
        &mut self,
        root: PathBuf,
        storage: PathBuf,
        folder: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let own_root = self
            .library_view_cache
            .locations
            .get(&storage)
            .map(|location| location.root.clone());
        if own_root.as_ref().is_some_and(|own| *own != root) {
            self.message = Some(course2md::tr!("笔记只能移到它所在保存位置里的文件夹").into());
            cx.notify();
            return;
        }
        let target = root.clone();
        self.edit_folders(
            root,
            move |library| library.assign(&target, &storage, folder),
            |this, result, _| {
                if let Err(error) = result {
                    this.message = Some(course2md::tr!("无法移动笔记：{}", format!("{error:#}")))
                }
            },
            cx,
        );
    }

    pub(crate) fn move_folder_into(
        &mut self,
        root: PathBuf,
        id: u64,
        parent: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let moved = root.clone();
        self.edit_folders(
            root,
            move |library| library.move_folder(id, parent),
            move |this, result, cx| match result {
                Ok(()) => {
                    if let Some(parent) = parent {
                        this.folder_tree_open.insert((moved, parent));
                    }
                }
                Err(error) => {
                    this.message = Some(course2md::tr!("无法移动文件夹：{}", format!("{error:#}")));
                    cx.notify();
                }
            },
            cx,
        );
    }

    /// The commands for a folder, shared by the tree, folder tiles and the path bar.
    pub(crate) fn folder_node_menu(
        &self,
        root: PathBuf,
        id: u64,
        cx: &mut Context<Self>,
    ) -> MenuBuilder {
        let entity = cx.weak_entity();
        Rc::new(move |menu, window, cx| {
            let Some(desktop) = entity.upgrade() else {
                return menu;
            };
            let Some(library) = desktop.read(cx).library_indexes.get(&root).cloned() else {
                return menu;
            };
            // The library's own name stands for its top level, as in the tree.
            let library_name = desktop
                .read(cx)
                .tree_libraries()
                .into_iter()
                .find(|(candidate, _, _)| *candidate == root)
                .map(|(_, name, _)| name)
                .unwrap_or_else(|| course2md::tr!("笔记库").into());
            let act =
                |action: fn(&mut Desktop, PathBuf, u64, &mut Window, &mut Context<Desktop>)| {
                    let entity = entity.clone();
                    let root = root.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        let _ = entity
                            .update(cx, |this, cx| action(this, root.clone(), id, window, cx));
                    }
                };
            let destinations = {
                let entity = entity.clone();
                let root = root.clone();
                let library = library.clone();
                move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                    let inside = library.subtree(id);
                    let current = library.parent(id);
                    let top = (None, library_name.clone());
                    let folders = library
                        .tree()
                        .into_iter()
                        .filter(|(folder, _)| !inside.contains(folder))
                        .map(|(folder, _)| (Some(folder), library.path(folder).join(" / ")));
                    std::iter::once(top)
                        .chain(folders)
                        .fold(menu, |menu, (parent, label)| {
                            let entity = entity.clone();
                            let root = root.clone();
                            menu.item(
                                PopupMenuItem::new(label)
                                    .icon(if parent.is_some() {
                                        icons::folder()
                                    } else {
                                        icons::storage()
                                    })
                                    .checked(parent == current)
                                    .disabled(parent == current)
                                    .on_click(move |_, _, cx| {
                                        let _ = entity.update(cx, |this, cx| {
                                            this.move_folder_into(root.clone(), id, parent, cx)
                                        });
                                    }),
                            )
                        })
                }
            };
            menu.item(
                PopupMenuItem::new(course2md::tr!("打开"))
                    .icon(icons::folder_open())
                    .on_click(act(|this, root, id, _, cx| {
                        this.select_library_scope(root, FolderFilter::Folder(id), cx)
                    })),
            )
            .item(
                PopupMenuItem::new(course2md::tr!("新建子文件夹…"))
                    .icon(icons::folder_plus())
                    .on_click(act(|this, root, id, window, cx| {
                        this.begin_folder_at(root, None, Some(id), window, cx)
                    })),
            )
            .separator()
            .item(
                PopupMenuItem::new(course2md::tr!("重命名…"))
                    .icon(icons::edit())
                    .on_click(act(|this, root, id, window, cx| {
                        this.begin_folder_at(root, Some(id), None, window, cx)
                    })),
            )
            .submenu_with_icon(
                Some(icons::folder_input()),
                course2md::tr!("移动到"),
                window,
                cx,
                destinations,
            )
            .separator()
            .item(
                PopupMenuItem::new(course2md::tr!("删除文件夹…"))
                    .icon(icons::delete())
                    .danger(true)
                    .on_click(act(|this, root, id, window, cx| {
                        this.begin_delete_folder_at(root, id, window, cx)
                    })),
            )
        })
    }

    fn root_node_menu(&self, root: PathBuf, cx: &mut Context<Self>) -> MenuBuilder {
        let entity = cx.weak_entity();
        Rc::new(move |menu, _, _| {
            let create = entity.clone();
            let root_for_create = root.clone();
            let reveal = root.clone();
            menu.item(
                PopupMenuItem::new(course2md::tr!("新建文件夹…"))
                    .icon(icons::folder_plus())
                    .on_click(move |_, window, cx| {
                        let _ = create.update(cx, |this, cx| {
                            this.begin_folder_at(root_for_create.clone(), None, None, window, cx)
                        });
                    }),
            )
            .item(
                PopupMenuItem::new(course2md::tr!("打开保存位置"))
                    .icon(icons::folder_open())
                    .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
            )
        })
    }

    /// Notes per place for the navigation: every note, each library's
    /// unfiled notes and each folder's own notes.
    fn nav_counts(&self) -> NavCounts {
        let mut counts = NavCounts {
            all: self.courses.len(),
            ..NavCounts::default()
        };
        for course in &self.courses {
            let Some(location) = self.course_location(course) else {
                continue;
            };
            if !self.library_indexes.contains_key(&location.root) {
                continue;
            }
            match self.course_folder(course) {
                Some(folder) => {
                    *counts
                        .folders
                        .entry((location.root.clone(), folder))
                        .or_default() += 1
                }
                None => *counts.unfiled.entry(location.root.clone()).or_default() += 1,
            }
        }
        counts
    }

    /// The selected place's name for the page title, and the library it is
    /// in when several libraries are listed.
    pub(crate) fn library_place(&self) -> (String, Option<String>) {
        let libraries = self.tree_libraries();
        let library_name = |root: &Path| {
            (libraries.len() > 1).then(|| {
                libraries
                    .iter()
                    .find(|(candidate, _, _)| candidate == root)
                    .map(|(_, name, _)| name.clone())
                    .unwrap_or_else(|| course2md::tr!("笔记库").into())
            })
        };
        match self.selected_target() {
            TreeTarget::All => (course2md::tr!("全部笔记").into(), None),
            TreeTarget::Root(root) => (course2md::tr!("未分类").into(), library_name(&root)),
            TreeTarget::Folder(root, id) => (
                self.library_indexes
                    .get(&root)
                    .and_then(|library| library.folders.get(&id).cloned())
                    .unwrap_or_else(|| course2md::tr!("文件夹").into()),
                library_name(&root),
            ),
        }
    }

    /// One destination of the folder navigation: icon, name and note count
    /// in a 56px pill, indented by depth, with a trailing disclosure for a
    /// folder that holds folders.
    #[allow(clippy::too_many_arguments)]
    fn tree_row(
        &mut self,
        target: TreeTarget,
        depth: usize,
        icon: gpui_component::Icon,
        label: SharedString,
        count: Option<usize>,
        disclosure: Option<bool>,
        menu: Option<MenuBuilder>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_target() == target;
        let key = match &target {
            TreeTarget::All => "folder-tree-all".to_owned(),
            TreeTarget::Root(root) => format!("folder-tree-root-{}", root.display()),
            TreeTarget::Folder(root, id) => format!("folder-tree-{}-{id}", root.display()),
        };
        let (fg, muted) = if selected {
            (color(ON_SECONDARY_CONTAINER), color(ON_SECONDARY_CONTAINER))
        } else {
            (color(INK), color(GRAY))
        };
        let expander = match (disclosure, &target) {
            (Some(open), TreeTarget::Folder(root, id)) => {
                let (root, id) = (root.clone(), *id);
                let hover = if selected {
                    blend(color(SECONDARY_CONTAINER), fg, 0.12)
                } else {
                    blend(color(CANVAS), color(INK), 0.12)
                };
                Some(
                    div()
                        .id(SharedString::from(format!("{key}-disclosure")))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(TREE_DISCLOSURE)
                        .mr(rems(-6. / 14.))
                        .flex_shrink_0()
                        .rounded_full()
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .child(
                            if open {
                                icons::chevron_down()
                            } else {
                                icons::chevron_right()
                            }
                            .size(rems(16. / 14.))
                            .text_color(muted),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_tree_node(&root, id, cx);
                        })),
                )
            }
            _ => None,
        };
        // Rows take keyboard focus like a file manager's sidebar: arrows move
        // and fold, Return or Space opens, ⌘⌫ deletes a folder.
        let focus = self
            .tree_focus
            .entry(key.clone())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        let keys = {
            let target = target.clone();
            cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                let (root, id) = match &target {
                    TreeTarget::Root(root) => (Some(root.clone()), 0),
                    TreeTarget::Folder(root, id) => (Some(root.clone()), *id),
                    TreeTarget::All => (None, 0),
                };
                match keystroke.key.as_str() {
                    "enter" | "space" => this.select_target(target.clone(), cx),
                    "up" => window.focus_prev(cx),
                    "down" => window.focus_next(cx),
                    "left" | "right" => {
                        let opening = keystroke.key == "right";
                        if let (Some(root), Some(open)) = (root, disclosure)
                            && open != opening
                        {
                            this.toggle_tree_node(&root, id, cx);
                        }
                    }
                    "backspace" if keystroke.modifiers.platform => {
                        if let TreeTarget::Folder(root, id) = &target {
                            this.begin_delete_folder_at(root.clone(), *id, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            })
        };
        let mut row = h_flex()
            .id(SharedString::from(key))
            .track_focus(&focus)
            .on_key_down(keys)
            .border_2()
            .border_color(gpui::transparent_black())
            .focus_visible(|style| style.border_color(color(ACCENT)))
            .role(Role::TreeItem)
            .aria_label(label.clone())
            .when_some(disclosure, |row, open| row.aria_expanded(open))
            .aria_selected(selected)
            .w_full()
            .min_w_0()
            .h(NAV_ROW)
            .flex_shrink_0()
            // The 2px focus border sits inside the pill's 16px/20px insets.
            .pl(rems(
                (NAV_LEADING.0 * 14. - 2.) / 14. + TREE_INDENT.0 * depth as f32,
            ))
            .pr(rems((NAV_TRAILING.0 * 14. - 2.) / 14.))
            .gap(NAV_GAP)
            .items_center()
            .rounded_full()
            .cursor_pointer()
            .text_color(fg)
            .when(selected, |row| row.bg(color(SECONDARY_CONTAINER)))
            .hover(move |style| {
                style.bg(if selected {
                    blend(
                        color(SECONDARY_CONTAINER),
                        color(ON_SECONDARY_CONTAINER),
                        0.08,
                    )
                } else {
                    blend(color(CANVAS), color(INK), 0.08)
                })
            })
            .tooltip(name_tooltip(label.clone()))
            .child(icon.size(NAV_ICON).flex_shrink_0().text_color(muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(NAV_LABEL)
                    .font_weight(if selected {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .child(label.clone()),
            )
            .when_some(count, |row, count| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_size(NAV_COUNT)
                        .text_color(muted)
                        .when(selected, |count| count.font_weight(FontWeight::MEDIUM))
                        .child(count.to_string()),
                )
            })
            .children(expander)
            .on_click(cx.listener({
                let target = target.clone();
                move |this, _, _, cx| this.select_target(target.clone(), cx)
            }));
        // Libraries and folders take notes and folders dropped on them.
        if let TreeTarget::Root(root) | TreeTarget::Folder(root, _) = &target {
            let folder = match &target {
                TreeTarget::Folder(_, id) => Some(*id),
                _ => None,
            };
            let note_root = root.clone();
            let folder_root = root.clone();
            row = row
                .drag_over::<NoteDrag>(|style, _, _, _| drop_highlight(style))
                .drag_over::<FolderDrag>(|style, _, _, _| drop_highlight(style))
                .on_drop(cx.listener(move |this, drag: &NoteDrag, _, cx| {
                    this.file_note(note_root.clone(), drag.storage.clone(), folder, cx)
                }))
                .on_drop(cx.listener(move |this, drag: &FolderDrag, _, cx| {
                    if drag.root != folder_root {
                        this.message =
                            Some(course2md::tr!("文件夹只能在同一个保存位置里移动").into());
                        cx.notify();
                    } else if Some(drag.id) != folder {
                        this.move_folder_into(folder_root.clone(), drag.id, folder, cx)
                    }
                }));
        }
        if let TreeTarget::Folder(root, id) = &target {
            let drag = FolderDrag {
                root: root.clone(),
                id: *id,
                name: label.clone(),
            };
            row = row.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()));
        }
        match menu {
            Some(menu) => row
                .context_menu(move |popup, window, cx| menu(popup, window, cx))
                .into_any_element(),
            None => row.into_any_element(),
        }
    }

    /// A 13px group label in the navigation: 文件夹, or a library's name
    /// when several libraries are listed (then it also folds the library).
    fn nav_label(
        &self,
        root: &Path,
        label: SharedString,
        library: Option<(bool, MenuBuilder)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_size(NAV_COUNT)
            .font_weight(FontWeight::MEDIUM)
            .text_color(color(GRAY))
            .child(label.clone());
        let Some((open, menu)) = library else {
            return h_flex()
                .w_full()
                .pt(rems(16. / 14.))
                .pb(rems(6. / 14.))
                .px(NAV_LEADING)
                .child(text)
                .into_any_element();
        };
        let toggle = root.to_path_buf();
        h_flex()
            .id(SharedString::from(format!(
                "folder-tree-library-{}",
                root.display()
            )))
            .role(Role::Button)
            .aria_label(label.clone())
            .aria_expanded(open)
            .w_full()
            .mt(rems(10. / 14.))
            .h(rems(32. / 14.))
            .px(NAV_LEADING)
            .gap_2()
            .items_center()
            .rounded_full()
            .cursor_pointer()
            .hover(|style| style.bg(blend(color(CANVAS), color(INK), 0.08)))
            .tooltip(name_tooltip(label))
            .child(text)
            .child(
                if open {
                    icons::chevron_down()
                } else {
                    icons::chevron_right()
                }
                .size(rems(1.))
                .text_color(color(GRAY)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_tree_node(&toggle, 0, cx)))
            .context_menu(move |popup, window, cx| menu(popup, window, cx))
            .into_any_element()
    }

    /// The folder navigation beside the notes: every note, each library's
    /// unfiled notes and its nested folders, then a new-folder action.
    pub(crate) fn folder_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let counts = self.nav_counts();
        let libraries = self.tree_libraries();
        let several = libraries.len() > 1;
        let mut tree = v_flex()
            .id("folder-tree")
            .role(Role::Tree)
            .aria_label(course2md::tr!("文件夹"))
            .w_full()
            .gap(px(4.))
            .child(self.tree_row(
                TreeTarget::All,
                0,
                icons::notebook(),
                course2md::tr!("全部笔记").into(),
                Some(counts.all),
                None,
                None,
                cx,
            ));
        for (root, name, library) in libraries {
            // One library opens straight onto its places; several each get
            // a label that folds them.
            let open = !several || self.tree_node_open(&root, 0);
            if several {
                let menu = self.root_node_menu(root.clone(), cx);
                tree = tree.child(self.nav_label(&root, name.into(), Some((open, menu)), cx));
            }
            if !open {
                continue;
            }
            let menu = self.root_node_menu(root.clone(), cx);
            tree = tree.child(
                self.tree_row(
                    TreeTarget::Root(root.clone()),
                    0,
                    icons::inbox(),
                    course2md::tr!("未分类").into(),
                    library
                        .as_ref()
                        .map(|_| counts.unfiled.get(&root).copied().unwrap_or(0)),
                    None,
                    Some(menu),
                    cx,
                ),
            );
            let Some(library) = library else {
                continue;
            };
            if !several && !library.folders.is_empty() {
                tree = tree.child(self.nav_label(&root, course2md::tr!("文件夹").into(), None, cx));
            }
            // A closed folder hides everything deeper than itself.
            let mut hidden_below: Option<usize> = None;
            for (id, depth) in library.tree() {
                if hidden_below.is_some_and(|limit| depth > limit) {
                    continue;
                }
                hidden_below = None;
                let has_children = !library.children(Some(id)).is_empty();
                let open = self.tree_node_open(&root, id);
                if has_children && !open {
                    hidden_below = Some(depth);
                }
                let menu = self.folder_node_menu(root.clone(), id, cx);
                tree = tree.child(
                    self.tree_row(
                        TreeTarget::Folder(root.clone(), id),
                        depth,
                        if open && has_children {
                            icons::folder_open()
                        } else {
                            icons::folder()
                        },
                        library.folders[&id].clone().into(),
                        Some(
                            counts
                                .folders
                                .get(&(root.clone(), id))
                                .copied()
                                .unwrap_or(0),
                        ),
                        has_children.then_some(open),
                        Some(menu),
                        cx,
                    ),
                );
            }
        }
        let create_root = self.library_root.clone();
        let create_parent = self.folder_filter.folder_id();
        let create = control("folder-tree-new")
            .ghost()
            .w_full()
            .h(NAV_ROW)
            .min_h(NAV_ROW)
            .pl(NAV_LEADING)
            .pr(NAV_TRAILING)
            .justify_start()
            .text_color(color(ACCENT))
            .accessibility_label(course2md::tr!("新建文件夹"))
            .child(
                h_flex()
                    .w_full()
                    .gap(NAV_GAP)
                    .items_center()
                    .child(icons::plus().size(NAV_ICON).text_color(color(ACCENT)))
                    .child(
                        div()
                            .text_size(NAV_LABEL)
                            .font_weight(FontWeight::MEDIUM)
                            .child(course2md::tr!("新建文件夹")),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.begin_folder_at(create_root.clone(), None, create_parent, window, cx)
            }));
        v_flex()
            .id("folder-tree-scroll")
            .w(FOLDER_SIDEBAR)
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .overflow_y_scroll()
            .pb_6()
            .gap(px(4.))
            .child(tree)
            .child(create)
            .into_any_element()
    }

    /// The places menu that stands in for the folder navigation when the
    /// window is too narrow for it: the selected place, opening every place.
    pub(crate) fn library_places_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.weak_entity();
        let libraries = self.tree_libraries();
        let several = libraries.len() > 1;
        let (place, _) = self.library_place();
        let create_root = self.library_root.clone();
        let create_parent = self.folder_filter.folder_id();
        filter_chip("library-places", false, cx)
            .h(rems(40. / 14.))
            .min_h(rems(40. / 14.))
            .icon(icons::folder())
            .label(place)
            .child(icons::chevron_down().size(rems(16. / 14.)))
            .tooltip(course2md::tr!("文件夹"))
            .dropdown_menu(move |menu, _, _| {
                let pick = |target: TreeTarget| {
                    let entity = entity.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        let _ =
                            entity.update(cx, |this, cx| this.select_target(target.clone(), cx));
                    }
                };
                let mut menu = menu.item(
                    PopupMenuItem::new(course2md::tr!("全部笔记"))
                        .icon(icons::notebook())
                        .on_click(pick(TreeTarget::All)),
                );
                for (root, name, library) in &libraries {
                    menu = menu.separator().item(
                        PopupMenuItem::new(if several {
                            format!("{name} · {}", course2md::tr!("未分类"))
                        } else {
                            course2md::tr!("未分类").to_owned()
                        })
                        .icon(icons::inbox())
                        .on_click(pick(TreeTarget::Root(root.clone()))),
                    );
                    for (id, _) in library.iter().flat_map(organize::Library::tree) {
                        let label = library
                            .as_ref()
                            .map(|library| library.path(id).join(" / "))
                            .unwrap_or_default();
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .icon(icons::folder())
                                .on_click(pick(TreeTarget::Folder(root.clone(), id))),
                        );
                    }
                }
                let create = entity.clone();
                let create_root = create_root.clone();
                menu.separator().item(
                    PopupMenuItem::new(course2md::tr!("新建文件夹…"))
                        .icon(icons::folder_plus())
                        .on_click(move |_, window, cx| {
                            let _ = create.update(cx, |this, cx| {
                                this.begin_folder_at(
                                    create_root.clone(),
                                    None,
                                    create_parent,
                                    window,
                                    cx,
                                )
                            });
                        }),
                )
            })
    }

    /// The folders inside the selected place, before its notes.
    pub(crate) fn folder_tiles(
        &self,
        root: &Path,
        parent: Option<u64>,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let library = self.library_indexes.get(root)?;
        let children = library.children(parent);
        if children.is_empty() {
            return None;
        }
        let mut counts: HashMap<u64, usize> = HashMap::new();
        for course in &self.courses {
            if self
                .course_location(course)
                .is_some_and(|location| location.root == root)
                && let Some(folder) = self.course_folder(course)
            {
                *counts.entry(folder).or_default() += 1;
            }
        }
        let tiles = children.into_iter().map(|id| {
            // What opening the folder shows: its folders and its own notes.
            let items = library.children(Some(id)).len() + counts.get(&id).copied().unwrap_or(0);
            let name: SharedString = library.folders[&id].clone().into();
            let menu = self.folder_node_menu(root.to_path_buf(), id, cx);
            let open_root = root.to_path_buf();
            let note_root = root.to_path_buf();
            let folder_root = root.to_path_buf();
            let drag = FolderDrag {
                root: root.to_path_buf(),
                id,
                name: name.clone(),
            };
            h_flex()
                .id(SharedString::from(format!("folder-tile-{id}")))
                .role(Role::Button)
                .aria_label(name.clone())
                .w(rems(240. / 14.))
                .min_w_0()
                .h(rems(64. / 14.))
                .gap(NAV_GAP)
                .pl(NAV_LEADING)
                .pr(NAV_TRAILING)
                .items_center()
                .rounded(TILE_RADIUS)
                .bg(color(SURFACE))
                .cursor_pointer()
                .hover(|style| style.bg(blend(color(SURFACE), color(INK), 0.08)))
                .tooltip(name_tooltip(name.clone()))
                .child(icons::folder().size(NAV_ICON).text_color(color(GRAY)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(NAV_LABEL)
                                .font_weight(FontWeight::MEDIUM)
                                .child(name.clone()),
                        )
                        .child(
                            div()
                                .text_size(NAV_COUNT)
                                .text_color(color(GRAY))
                                .child(course2md::tr!("{} 项", items)),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_library_scope(open_root.clone(), FolderFilter::Folder(id), cx)
                }))
                .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                .drag_over::<NoteDrag>(|style, _, _, _| drop_highlight(style))
                .drag_over::<FolderDrag>(|style, _, _, _| drop_highlight(style))
                .on_drop(cx.listener(move |this, drag: &NoteDrag, _, cx| {
                    this.file_note(note_root.clone(), drag.storage.clone(), Some(id), cx)
                }))
                .on_drop(cx.listener(move |this, drag: &FolderDrag, _, cx| {
                    if drag.root != folder_root {
                        this.message =
                            Some(course2md::tr!("文件夹只能在同一个保存位置里移动").into());
                        cx.notify();
                    } else if drag.id != id {
                        this.move_folder_into(folder_root.clone(), drag.id, Some(id), cx)
                    }
                }))
                .context_menu(move |popup, window, cx| menu(popup, window, cx))
        });
        Some(
            h_flex()
                .w_full()
                .gap(rems(12. / 14.))
                .flex_wrap()
                .children(tiles),
        )
    }
}
