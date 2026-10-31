//! The note library's folder navigation. Each library is a root whose unfiled notes
//! sit at its top level; folders nest inside it, and notes and folders move by
//! dragging onto a folder or through the same commands on right-click.
use super::*;
use crate::library_ui::Moving;
use crate::symbols;
use gpui_kit::component::{
    menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
    scroll::Scrollbar,
};
use gpui_kit_material::{
    color::ColorRole,
    icons::{Icon, SymbolIcon},
    prelude::{
        AssistChip, ButtonSize, ButtonVariant, IconButton, ListDensity, ListItem, ListLeading,
        ListTrailing, MaterialMenu, MaterialMenuItem, MaterialTooltip, color, material_button, rem,
    },
    tokens::shape::RADIUS_CARD,
};
use std::{collections::HashMap, path::Path};

/// Width of the folder navigation beside the notes.
pub(crate) const FOLDER_SIDEBAR: f32 = 256.;
/// Space between the folder navigation and the notes.
pub(crate) const FOLDER_SIDEBAR_GAP: f32 = 32.;
/// Below this content width (navigation, gap and a note column) the navigation
/// moves into a sheet opened from the toolbar.
const FOLDER_SIDEBAR_MIN_CONTENT: f32 = 832.;
/// Nested folders step in by this much per level.
const TREE_INDENT: f32 = 12.;

/// Wide enough for the longest command in every language.
const COMMAND_MENU_WIDTH: f32 = 320.;

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
        AssistChip::new("note-drag", self.title.clone())
            .icon(symbols::article())
            .elevated(true)
    }
}

impl Render for FolderDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        AssistChip::new("folder-drag", self.name.clone())
            .icon(symbols::folder())
            .elevated(true)
    }
}

type CommandRun = Rc<dyn Fn(&mut Window, &mut App)>;

/// One command of a note's or folder's menu. The list draws both behind the ⋮ button
/// (the kit's menu) and on right-click (Kit's, which `context_menu` requires).
#[derive(Clone)]
pub(crate) struct Command {
    label: SharedString,
    icon: SymbolIcon,
    danger: bool,
    ends_group: bool,
    run: CommandRun,
}

impl Command {
    /// A command that runs on the desktop.
    pub(crate) fn new(
        label: impl Into<SharedString>,
        icon: SymbolIcon,
        desktop: &WeakEntity<Desktop>,
        run: impl Fn(&mut Desktop, &mut Window, &mut Context<Desktop>) + 'static,
    ) -> Self {
        let desktop = desktop.clone();
        Self::global(label, icon, move |window, cx| {
            let _ = desktop.update(cx, |this, cx| run(this, window, cx));
        })
    }

    /// A command that needs nothing from the desktop.
    pub(crate) fn global(
        label: impl Into<SharedString>,
        icon: SymbolIcon,
        run: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            icon,
            danger: false,
            ends_group: false,
            run: Rc::new(run),
        }
    }

    pub(crate) fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub(crate) fn ends_group(mut self) -> Self {
        self.ends_group = true;
        self
    }
}

/// The commands as the kit's menu.
pub(crate) fn command_menu(id: impl Into<ElementId>, commands: Vec<Command>) -> MaterialMenu {
    MaterialMenu::new(id)
        .width(COMMAND_MENU_WIDTH)
        .items(commands.into_iter().map(|command| {
            let icon = Icon::from(command.icon);
            MaterialMenuItem::new(command.label)
                .leading(if command.danger {
                    icon.color(color(ColorRole::ERROR))
                } else {
                    icon
                })
                .divider(command.ends_group)
                .on_click(move |_, window, cx| (command.run)(window, cx))
        }))
}

/// The commands as a right-click menu.
pub(crate) fn command_popup(mut menu: PopupMenu, commands: &[Command]) -> PopupMenu {
    for command in commands {
        let icon = command.icon.component();
        let run = command.run.clone();
        menu = menu.item(
            PopupMenuItem::new(command.label.clone())
                .icon(if command.danger {
                    icon.text_color(color(ColorRole::ERROR))
                } else {
                    icon
                })
                .on_click(move |_, window, cx| run(window, cx)),
        );
        if command.ends_group {
            menu = menu.separator();
        }
    }
    menu
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

/// What a place's row shows.
struct PlaceRow {
    target: TreeTarget,
    depth: usize,
    icon: SymbolIcon,
    label: SharedString,
    count: Option<usize>,
    /// Whether it holds folders, and whether they show.
    disclosure: Option<bool>,
    commands: Vec<Command>,
}

impl Desktop {
    /// Every library with its folder record, when that record has loaded.
    fn tree_libraries(&self) -> Vec<(PathBuf, String, Option<organize::Library>)> {
        let libraries: Vec<_> = self
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.state.libraries)
            .map(|location| {
                (
                    location.root.clone(),
                    location.name.clone(),
                    self.library_indexes.get(&location.root).cloned(),
                )
            })
            .collect();
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

    /// Whether the navigation fits beside the notes, or moves into a sheet.
    pub(crate) fn folder_sidebar_fits(&self, window: &Window) -> bool {
        let scale = f32::from(window.rem_size()) / 14.;
        crate::views::shell_content_width(Page::Library, window)
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
            // A selected folder also shows the folders it holds.
            if !organization.children(Some(id)).is_empty() {
                self.folder_tree_open.insert((root.clone(), id));
            }
            self.folder_tree_open.remove(&(root.clone(), 0));
        }
        self.library_root = root;
        self.folder_filter = filter;
        self.places_open = false;
        self.library_list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    fn select_target(&mut self, target: TreeTarget, cx: &mut Context<Self>) {
        let root = self.library_root.clone();
        match target {
            TreeTarget::All => self.select_library_scope(root, FolderFilter::All, cx),
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

    /// The commands for a folder, shared by the tree, folder rows and the page header.
    pub(crate) fn folder_commands(
        &self,
        root: &Path,
        id: u64,
        cx: &mut Context<Self>,
    ) -> Vec<Command> {
        let desktop = cx.weak_entity();
        let command =
            |label: &'static str,
             icon,
             run: fn(&mut Desktop, PathBuf, u64, &mut Window, &mut Context<Desktop>)| {
                let root = root.to_path_buf();
                Command::new(label, icon, &desktop, move |this, window, cx| {
                    run(this, root.clone(), id, window, cx)
                })
            };
        vec![
            command(
                course2md::tr!("打开"),
                symbols::folder_open(),
                |this, root, id, _, cx| {
                    this.select_library_scope(root, FolderFilter::Folder(id), cx)
                },
            ),
            command(
                course2md::tr!("新建子文件夹…"),
                symbols::create_new_folder(),
                |this, root, id, window, cx| this.begin_folder_at(root, None, Some(id), window, cx),
            )
            .ends_group(),
            command(
                course2md::tr!("重命名…"),
                symbols::edit(),
                |this, root, id, window, cx| this.begin_folder_at(root, Some(id), None, window, cx),
            ),
            command(
                course2md::tr!("移动到…"),
                symbols::drive_file_move(),
                |this, root, id, _, cx| this.begin_move(root, Moving::Folder(id), cx),
            )
            .ends_group(),
            command(
                course2md::tr!("删除文件夹…"),
                symbols::delete(),
                |this, root, id, window, cx| this.begin_delete_folder_at(root, id, window, cx),
            )
            .danger(),
        ]
    }

    fn root_commands(&self, root: &Path, cx: &mut Context<Self>) -> Vec<Command> {
        let (create, reveal) = (root.to_path_buf(), root.to_path_buf());
        vec![
            Command::new(
                course2md::tr!("新建文件夹…"),
                symbols::create_new_folder(),
                &cx.weak_entity(),
                move |this, window, cx| {
                    this.begin_folder_at(create.clone(), None, None, window, cx)
                },
            ),
            Command::global(
                course2md::tr!("打开保存位置"),
                symbols::folder_open(),
                move |_, cx| cx.reveal_path(&reveal),
            ),
        ]
    }

    /// Notes per place for the navigation: every note, each library's
    /// unfiled notes and each folder's notes including its subfolders', so
    /// 全部笔记 is 未分类 plus the top folders and the folder page agrees.
    fn nav_counts(&self) -> NavCounts {
        let mut counts = NavCounts {
            all: self.courses.len(),
            ..NavCounts::default()
        };
        for course in &self.courses {
            let Some(location) = self.course_location(course) else {
                continue;
            };
            let Some(library) = self.library_indexes.get(&location.root) else {
                continue;
            };
            match self.course_folder(course) {
                Some(folder) => {
                    let mut current = Some(folder);
                    while let Some(id) = current {
                        *counts
                            .folders
                            .entry((location.root.clone(), id))
                            .or_default() += 1;
                        current = library.parent(id);
                    }
                }
                None => *counts.unfiled.entry(location.root.clone()).or_default() += 1,
            }
        }
        counts
    }

    /// A folder's notes, its subfolders' included, as its navigation row counts them.
    pub(crate) fn folder_note_count(&self, root: &Path, id: u64) -> usize {
        self.nav_counts()
            .folders
            .get(&(root.to_path_buf(), id))
            .copied()
            .unwrap_or(0)
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

    /// A library's name, standing for its top level.
    pub(crate) fn library_name(&self, root: &Path) -> String {
        self.tree_libraries()
            .into_iter()
            .find(|(candidate, _, _)| candidate == root)
            .map(|(_, name, _)| name)
            .unwrap_or_else(|| course2md::tr!("笔记库").into())
    }

    /// Lets notes and folders drop on `row`, which files them under `folder` of `root`.
    fn drop_target(
        &self,
        row: Stateful<Div>,
        root: &Path,
        folder: Option<u64>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (note_root, folder_root) = (root.to_path_buf(), root.to_path_buf());
        row.border_2()
            .border_color(gpui::transparent_black())
            .rounded(RADIUS_CARD)
            .drag_over::<NoteDrag>(|style, _, _, _| style.border_color(color(ColorRole::PRIMARY)))
            .drag_over::<FolderDrag>(|style, _, _, _| style.border_color(color(ColorRole::PRIMARY)))
            .on_drop(cx.listener(move |this, drag: &NoteDrag, _, cx| {
                this.file_note(note_root.clone(), drag.storage.clone(), folder, cx)
            }))
            .on_drop(cx.listener(move |this, drag: &FolderDrag, _, cx| {
                if drag.root != folder_root {
                    this.message = Some(course2md::tr!("文件夹只能在同一个保存位置里移动").into());
                    cx.notify();
                } else if Some(drag.id) != folder {
                    this.move_folder_into(folder_root.clone(), drag.id, folder, cx)
                }
            }))
    }

    /// One place of the navigation. Rows take keyboard focus like a file manager's
    /// sidebar: arrows move and fold, Return or Space opens, ⌘⌫ deletes a folder.
    fn place_row(
        &self,
        place: PlaceRow,
        nested: bool,
        ground: ColorRole,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let PlaceRow {
            target,
            depth,
            icon,
            label,
            count,
            disclosure,
            commands,
        } = place;
        let key = match &target {
            TreeTarget::All => "folder-tree-all".to_owned(),
            TreeTarget::Root(root) => format!("folder-tree-root-{}", root.display()),
            TreeTarget::Folder(root, id) => format!("folder-tree-{}-{id}", root.display()),
        };
        let (root, id) = match &target {
            TreeTarget::All => (None, 0),
            TreeTarget::Root(root) => (Some(root.clone()), 0),
            TreeTarget::Folder(root, id) => (Some(root.clone()), *id),
        };
        let item = ListItem::new(SharedString::from(key.clone()), label.clone())
            .leading(ListLeading::Icon(icon.into()))
            .density(ListDensity::Compact)
            .container(ground)
            .segment([true; 4])
            .selected(self.selected_target() == target)
            .when_some(count, |item, count| item.trailing_text(count.to_string()))
            .on_click(cx.listener({
                let target = target.clone();
                move |this, _, _, cx| this.select_target(target.clone(), cx)
            }));
        let keys = {
            let (root, target) = (root.clone(), target.clone());
            cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" => window.focus_prev(cx),
                    "down" => window.focus_next(cx),
                    "left" | "right" => {
                        if let (Some(root), Some(open)) = (&root, disclosure)
                            && open != (event.keystroke.key == "right")
                        {
                            this.toggle_tree_node(root, id, cx);
                        }
                    }
                    "backspace" if event.keystroke.modifiers.platform => {
                        if let TreeTarget::Folder(root, id) = &target {
                            this.begin_delete_folder_at(root.clone(), *id, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            })
        };
        let mut row = div()
            .id(SharedString::from(format!("{key}-row")))
            .flex_1()
            .min_w_0()
            .on_key_down(keys)
            .child(
                // A long name is cut by its row; the tooltip shows the whole.
                MaterialTooltip::plain(SharedString::from(format!("{key}-tip")), label.clone())
                    .show_on_focus(false)
                    .trigger(item),
            );
        row = match &root {
            Some(root) => self.drop_target(row, root, folder_of(&target), cx),
            // Keeps its width with the rows that can show a drop.
            None => row.border_2().border_color(gpui::transparent_black()),
        };
        if let TreeTarget::Folder(root, id) = &target {
            let drag = FolderDrag {
                root: root.clone(),
                id: *id,
                name: label.clone(),
            };
            row = row.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()));
        }
        let row = row.context_menu(move |menu, _, _| command_popup(menu, &commands));
        h_flex()
            .w_full()
            .min_w_0()
            .pl(rem(TREE_INDENT * depth as f32))
            .items_center()
            .when(nested, |line| {
                line.child(div().w(rem(32.)).flex_shrink_0().when_some(
                    disclosure.zip(root),
                    |slot, (open, root)| {
                        slot.child(
                            IconButton::new(
                                SharedString::from(format!("{key}-disclosure")),
                                if open {
                                    symbols::keyboard_arrow_down()
                                } else {
                                    symbols::chevron_right()
                                },
                                if open {
                                    course2md::tr!("收起")
                                } else {
                                    course2md::tr!("展开")
                                },
                            )
                            .button_size(ButtonSize::ExtraSmall)
                            .tab_stop(false)
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.toggle_tree_node(&root, id, cx),
                            )),
                        )
                    },
                ))
            })
            .child(row)
            .into_any_element()
    }

    /// A library's name when several are listed: it folds the library's places.
    fn library_row(
        &self,
        root: &Path,
        name: String,
        open: bool,
        ground: ColorRole,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let commands = self.root_commands(root, cx);
        let toggle = root.to_path_buf();
        div()
            .id(SharedString::from(format!(
                "folder-tree-library-{}",
                root.display()
            )))
            .w_full()
            .mt(rem(8.))
            .child(
                ListItem::new("library-header", name)
                    .density(ListDensity::Compact)
                    .container(ground)
                    .segment([true; 4])
                    .leading(ListLeading::Icon(symbols::storage().into()))
                    .trailing(ListTrailing::Icon(
                        if open {
                            symbols::keyboard_arrow_down()
                        } else {
                            symbols::chevron_right()
                        }
                        .into(),
                    ))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_tree_node(&toggle, 0, cx)),
                    ),
            )
            .context_menu(move |menu, _, _| command_popup(menu, &commands))
    }

    /// The folder navigation: every note, each library's unfiled notes and its
    /// nested folders, then a new-folder action. `ground` is the surface it sits on.
    pub(crate) fn folder_tree(&self, ground: ColorRole, cx: &mut Context<Self>) -> Div {
        let counts = self.nav_counts();
        let libraries = self.tree_libraries();
        let several = libraries.len() > 1;
        let nested = libraries.iter().any(|(_, _, library)| {
            library.as_ref().is_some_and(|library| {
                library
                    .folders
                    .keys()
                    .any(|id| !library.children(Some(*id)).is_empty())
            })
        });
        let mut tree = v_flex()
            .id("folder-tree")
            .role(Role::Tree)
            .aria_label(course2md::tr!("文件夹"))
            .w_full()
            .child(self.place_row(
                PlaceRow {
                    target: TreeTarget::All,
                    depth: 0,
                    icon: symbols::book(),
                    label: course2md::tr!("全部笔记").into(),
                    count: Some(counts.all),
                    disclosure: None,
                    commands: Vec::new(),
                },
                nested,
                ground,
                cx,
            ));
        for (root, name, library) in libraries {
            // One library opens straight onto its places; several each get
            // a row that folds them.
            let open = !several || self.tree_node_open(&root, 0);
            if several {
                tree = tree.child(self.library_row(&root, name, open, ground, cx));
            }
            if !open {
                continue;
            }
            tree = tree.child(
                self.place_row(
                    PlaceRow {
                        target: TreeTarget::Root(root.clone()),
                        depth: 0,
                        icon: symbols::inbox(),
                        label: course2md::tr!("未分类").into(),
                        count: library
                            .as_ref()
                            .map(|_| counts.unfiled.get(&root).copied().unwrap_or(0)),
                        disclosure: None,
                        commands: self.root_commands(&root, cx),
                    },
                    nested,
                    ground,
                    cx,
                ),
            );
            let Some(library) = library else {
                continue;
            };
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
                tree = tree.child(
                    self.place_row(
                        PlaceRow {
                            target: TreeTarget::Folder(root.clone(), id),
                            depth,
                            icon: if open && has_children {
                                symbols::folder_open()
                            } else {
                                symbols::folder()
                            },
                            label: library.folders[&id].clone().into(),
                            count: Some(
                                counts
                                    .folders
                                    .get(&(root.clone(), id))
                                    .copied()
                                    .unwrap_or(0),
                            ),
                            disclosure: has_children.then_some(open),
                            commands: self.folder_commands(&root, id, cx),
                        },
                        nested,
                        ground,
                        cx,
                    ),
                );
            }
        }
        let (create_root, create_parent) =
            (self.library_root.clone(), self.folder_filter.folder_id());
        v_flex().w_full().min_w_0().gap(rem(8.)).child(tree).child(
            material_button("folder-tree-new", ButtonVariant::Text)
                .self_start()
                .icon(symbols::add())
                .child(course2md::tr!("新建文件夹"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.begin_folder_at(create_root.clone(), None, create_parent, window, cx)
                })),
        )
    }

    /// The navigation beside the notes, scrolling on its own.
    pub(crate) fn folder_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .relative()
            .w(rem(FOLDER_SIDEBAR))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .child(
                div()
                    .id("folder-tree-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.folder_tree_scroll)
                    .pb(rem(24.))
                    .child(self.folder_tree(ColorRole::SURFACE_CONTAINER, cx)),
            )
            .child(Scrollbar::vertical(&self.folder_tree_scroll))
            .into_any_element()
    }

    /// The folders inside the selected place, before its notes, in the grid's columns.
    pub(crate) fn folder_rows(
        &self,
        root: &Path,
        parent: Option<u64>,
        columns: usize,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let library = self.library_indexes.get(root)?;
        let children = library.children(parent);
        if children.is_empty() {
            return None;
        }
        let counts = self.nav_counts().folders;
        let rows: Vec<_> = children
            .into_iter()
            .map(|id| {
                // The same count as the folder's row in the navigation.
                let notes = counts.get(&(root.to_path_buf(), id)).copied().unwrap_or(0);
                let name: SharedString = library.folders[&id].clone().into();
                let commands = self.folder_commands(root, id, cx);
                let open_root = root.to_path_buf();
                let drag = FolderDrag {
                    root: root.to_path_buf(),
                    id,
                    name: name.clone(),
                };
                self.drop_target(
                    div()
                        .id(SharedString::from(format!("folder-row-{id}")))
                        .min_w_0(),
                    root,
                    Some(id),
                    cx,
                )
                .child(
                    ListItem::new(("folder-item", id as usize), name)
                        .leading(ListLeading::Icon(symbols::folder().into()))
                        .supporting(note_count(notes))
                        .segment([true; 4])
                        .trailing(ListTrailing::Icon(symbols::chevron_right().into()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_library_scope(
                                open_root.clone(),
                                FolderFilter::Folder(id),
                                cx,
                            )
                        })),
                )
                .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                .context_menu(move |menu, _, _| command_popup(menu, &commands))
            })
            .collect();
        Some(
            div()
                .grid()
                .grid_cols(columns as u16)
                .w_full()
                .gap(rem(8.))
                .children(rows),
        )
    }
}

fn folder_of(target: &TreeTarget) -> Option<u64> {
    match target {
        TreeTarget::Folder(_, id) => Some(*id),
        _ => None,
    }
}

/// "1 篇笔记" / "{} 篇笔记", with the singular English form.
pub(crate) fn note_count(count: usize) -> String {
    if count == 1 {
        course2md::tr!("1 篇笔记").to_owned()
    } else {
        course2md::tr!("{} 篇笔记", count)
    }
}
