use super::*;
use crate::{symbols, ui};
use desktop_store::records::{FolderContext, FolderPlace, folders_unavailable};
use gpui_kit_material::{
    color::ColorRole,
    icons::Icon,
    prelude::{
        ButtonVariant, ExposedDropdown, ListDensity, ListItem, ListLeading, MaterialDialog,
        TextField, TextFieldShape, TextFieldVariant, TileScale, material_button, rem,
        segmented_list,
    },
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// What the name dialog names.
#[derive(Clone)]
pub(crate) enum NameTarget {
    /// A new folder inside `parent`, chosen for `draft` when it was asked for from one,
    /// and receiving `then_move` when the move dialog had nowhere else to offer.
    NewFolder {
        root: PathBuf,
        parent: Option<u64>,
        draft: Option<String>,
        then_move: Option<Moving>,
    },
    Folder {
        root: PathBuf,
        id: u64,
    },
    Note {
        root: PathBuf,
        storage: PathBuf,
    },
}

/// The library page's own view state: its dialogs, folder tree and virtualized list.
pub(crate) struct LibraryUi {
    pub name_dialog: Option<NameDialog>,
    pub move_dialog: Option<MoveDialog>,
    pub folder_picker_open: bool,
    /// The folder navigation's sheet on narrow windows.
    pub places_open: bool,
    /// Tree nodes the person toggled: opened folders, or closed library tops (id 0).
    pub folder_tree_open: HashSet<(PathBuf, u64)>,
    pub folder_tree_scroll: ScrollHandle,
    /// The virtualized notes: the list, and the identity and focus of what it describes.
    pub list: ListState,
    pub keys: Vec<String>,
    pub focus: Vec<FocusHandle>,
    pub rem: f32,
}

impl LibraryUi {
    pub fn new() -> Self {
        Self {
            name_dialog: None,
            move_dialog: None,
            folder_picker_open: false,
            places_open: false,
            folder_tree_open: HashSet::new(),
            folder_tree_scroll: ScrollHandle::new(),
            list: ListState::new(0, ListAlignment::Top, px(1000.)),
            keys: Vec::new(),
            focus: Vec::new(),
            rem: f32::NAN,
        }
    }
}

/// The dialog naming a new folder, a folder or a note. It outlives its answer, so it
/// can play its exit; `open` says whether the question still stands.
pub(crate) struct NameDialog {
    target: NameTarget,
    error: Option<String>,
    saving: bool,
    open: bool,
    /// Its field takes focus once the dialog has taken it, a frame after opening.
    focus_field: bool,
}

/// What the move dialog moves.
#[derive(Clone)]
pub(crate) enum Moving {
    Note(PathBuf),
    Folder(u64),
}

/// The dialog choosing where to move a note or folder, inside library `root`.
pub(crate) struct MoveDialog {
    root: PathBuf,
    moving: Moving,
    open: bool,
}

impl Desktop {
    pub fn begin_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.import_video_from_action(window, cx);
        if self.online(cx) && self.value(Field::Source, cx).is_empty() {
            self.inputs[&Field::Source].update(cx, |state, cx| state.focus(window, cx));
        }
    }

    pub fn invalidate_source(&mut self) {
        self.import.invalidate_source();
    }

    pub fn inspect_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut input = self.value(Field::Source, cx);
        if !self.prepare_next_import(&input, window, cx) {
            return;
        }
        if self.import.preview_cancel.is_some()
            && self.draft(cx).is_some_and(|draft| draft.input == input)
        {
            return;
        }
        let previous_source = self.import.source_preview.clone();
        self.invalidate_source();
        if input.is_empty() {
            self.import.source_validation = Some(
                if self.online(cx) {
                    course2md::tr!("先粘贴视频链接")
                } else {
                    course2md::tr!("先选择一个视频")
                }
                .into(),
            );
            cx.notify();
            return;
        }
        if self.online(cx)
            && let Some(path) = source::pasted_local_path(&input)
        {
            self.drop_source_files(&[path], window, cx);
            return;
        }
        if self.online(cx) {
            let links = source::video_links(&input);
            if links.len() > 1 {
                self.import.source_candidates = links
                    .into_iter()
                    .map(|input| {
                        let mut candidate = source::SourceCandidate {
                            title: input.clone(),
                            input,
                            identity: None,
                            duration: None,
                            thumbnail: None,
                        };
                        candidate.identity = course2md::fetch::candidate_identity(&candidate);
                        candidate
                    })
                    .collect();
                self.import.source_collection_title =
                    Some(course2md::tr!("分享内容中有多个链接，请选择一个视频").into());
                self.save_current_draft(cx);
                cx.notify();
                return;
            }
            if let Some(link) = links.into_iter().next()
                && input != link
            {
                self.inputs[&Field::Source].update(cx, |state, cx| {
                    state.set_value(link.clone(), window, cx);
                });
                input = link;
            }
            if let Err(error) = source::validate_url(&input) {
                // Record the input first: otherwise the field's own notification after
                // Enter reads as a new edit and clears the message before it is drawn (V4).
                self.save_current_draft(cx);
                self.import.source_validation = Some(error.to_string());
                cx.notify();
                return;
            }
        }
        if !self.save_current_draft(cx) {
            return;
        }
        let token = self
            .workspace(cx)
            .and_then(|workspace| workspace.state.draft())
            .map(|draft| (draft.id.clone(), draft.revision));
        let generation = self.import.preview_generation;
        let request_input = input.clone();
        self.import.preview_workers += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.import.preview_cancel = Some(cancel.clone());
        let online = self.online(cx);
        let handle = window.window_handle();
        // probe is blocking network/subprocess work; see crate::spawn_blocking_io.
        let task = crate::spawn_blocking_result(move || source::probe(input, online, cancel));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            // Always return the worker count, window or not (request_close waits for zero).
            let _ = this.update(cx, |this, _| {
                this.import.preview_workers = this.import.preview_workers.saturating_sub(1);
            });
            {
                let _ = cx.update_window(handle, |_, window, cx| this.update(cx, |this, cx| {
                    if this.import.preview_generation != generation || this.value(Field::Source, cx) != request_input { return; }
                    if let Some((id, revision)) = &token
                        && !this.workspace(cx).is_some_and(|workspace| workspace.state.matches_input(id, *revision))
                    {
                        return;
                    }
                    this.import.preview_cancel = None;
                    match result {
                        Ok(source::SourceProbe::Single(source)) => {
                            let mut source = *source;
                            if let course2md::subtitle::SubtitleEvidence::Found { tracks, .. } = &mut source.subtitles {
                                course2md::subtitle::sort_tracks(tracks, &this.preferences.generation().preferred_subtitle_languages, course2md::i18n::lang().code(), source.original_language.as_deref());
                            }
                            let title = this.workspace(cx).and_then(|workspace| workspace.state.draft())
                                .filter(|draft| draft.custom_title).map(|draft| draft.title.clone()).unwrap_or_else(|| source.title.clone());
                            this.inputs[&Field::Title].update(cx, |state, cx| state.set_value(title, window, cx));
                            let previous_source = previous_source.as_ref().filter(|previous| previous.identity == source.identity);
                            let wanted = previous_source.and_then(|previous| previous.subtitle_request.as_ref().map(|track| track.id.clone()).or_else(|| previous.selected_subtitle.as_ref().map(|subtitle| subtitle.track_id.clone())));
                            let default_track = match &wanted {
                                Some(id) => source.subtitles.tracks().iter().find(|track| &track.id == id).cloned().or_else(|| previous_source.and_then(|previous| previous.subtitles.tracks().iter().find(|track| &track.id == id && matches!(track.origin, course2md::subtitle::SubtitleOrigin::File { .. })).cloned())),
                                None => source.subtitles.tracks().first().cloned(),
                            };
                            if let Some(previous) = previous_source { source.selected_subtitle = previous.selected_subtitle.clone(); }
                            if wanted.is_some() && default_track.is_none() {
                                let message = course2md::tr!("原来选择的字幕无法读取，请重新选择文字来源").to_owned();
                                this.import.subtitle_error = Some(message.clone());
                                source.subtitle_read_error = Some(course2md::subtitle::SubtitleReadError::Failed { message });
                            }
                            this.import.source_preview = Some(source);
                            this.save_current_draft(cx);
                            if this.task_options(cx).source_mode != course2md::config::TranscriptSource::Asr && let Some(track) = default_track {
                                if wanted.is_none() { this.confirm_default_subtitle(track, cx); } else { this.confirm_subtitle(track, false, cx); }
                            }
                        }
                        Ok(source::SourceProbe::Collection { title, candidates, unavailable_entries }) => {
                            this.import.source_collection_name = (!title.is_empty()).then(|| title.clone());
                            this.import.source_collection_title = Some(if title.is_empty() { course2md::tr!("请选择本次处理的视频").into() } else { title });
                            this.start_source_cover_fetch(&candidates, generation, window, cx);
                            this.import.source_candidates = candidates
                                .into_iter()
                                .map(|mut candidate| {
                                    candidate.identity = course2md::fetch::candidate_identity(&candidate);
                                    candidate
                                })
                                .collect();
                            if this.import.source_candidates.is_empty() {
                                this.import.preview_error = Some(course2md::tr!("还无法确定要处理哪个视频。请复制具体视频的链接。").into());
                            } else if unavailable_entries > 0 {
                                this.import.preview_error = Some(course2md::tr!("另有 {} 个条目无法确认", unavailable_entries));
                            }
                        }
                        Ok(source::SourceProbe::Unresolved { message }) => this.import.preview_error = Some(message),
                        Err(error) => this.import.preview_error = Some(format!("{error:#}")),
                    }
                    this.advance_conversion(window, cx);
                    cx.notify();
                }));
            }
        }).detach();
        cx.notify();
    }

    /// Download candidate row previews in the background, each cover URL once (episodes without a first
    /// frame share the video cover), refreshing a row as each arrives. preview_generation drops stale
    /// results, as it does for probe results.
    fn start_source_cover_fetch(
        &mut self,
        candidates: &[source::SourceCandidate],
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut groups: Vec<(String, Arc<Vec<String>>)> = Vec::new();
        for candidate in candidates {
            let Some(url) = candidate.thumbnail.clone() else {
                continue;
            };
            if let Some((_, inputs)) = groups.iter_mut().find(|(known, _)| *known == url) {
                Arc::make_mut(inputs).push(candidate.input.clone());
            } else {
                groups.push((url, Arc::new(vec![candidate.input.clone()])));
            }
        }
        if groups.is_empty() {
            return;
        }
        if let Some(cancel) = self.import.source_covers_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.import.source_covers_cancel = Some(cancel.clone());
        let workers = groups.len().min(4);
        let queue = Arc::new(std::sync::Mutex::new(
            groups
                .into_iter()
                .collect::<std::collections::VecDeque<_>>(),
        ));
        let (tx, rx) = smol::channel::unbounded::<(Arc<Vec<String>>, PathBuf)>();
        for _ in 0..workers {
            let queue = queue.clone();
            let tx = tx.clone();
            let cancel = cancel.clone();
            // Blocking downloads stay off the GPUI executor; see crate::spawn_blocking_io.
            std::thread::spawn(move || {
                loop {
                    let next = queue.lock().unwrap_or_else(|p| p.into_inner()).pop_front();
                    let Some((url, inputs)) = next else { break };
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(path) = source::cache_remote_cover(&url, &cancel) {
                        let _ = tx.send_blocking((inputs, path));
                    }
                }
            });
        }
        drop(tx);
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            // The channel closes once every worker ends, which ends the loop.
            while let Ok((inputs, path)) = rx.recv().await {
                let keep = cx
                    .update_window(handle, |_, _window, cx| {
                        this.update(cx, |this, cx| {
                            if this.import.preview_generation != generation {
                                return false;
                            }
                            for input in inputs.iter() {
                                this.import
                                    .source_candidate_covers
                                    .insert(input.clone(), path.clone());
                            }
                            cx.notify();
                            true
                        })
                        .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    /// The library a new folder goes in, and the draft it is for on the workbench.
    fn current_folder_origin(&self, cx: &App) -> (PathBuf, Option<String>) {
        if self.page == Page::New
            && let Some(workspace) = &self.records.read(cx).workspace
            && let Some(draft) = workspace.state.draft()
            && let Some(library) = workspace.state.library(&draft.library_id)
        {
            return (library.root.clone(), Some(draft.id.clone()));
        }
        (self.library(cx).root.clone(), None)
    }

    /// A new top-level folder in the current library, for the workbench's draft there.
    pub fn begin_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (root, draft) = self.current_folder_origin(cx);
        let parent = None;
        self.begin_naming(
            NameTarget::NewFolder {
                root,
                parent,
                draft,
                then_move: None,
            },
            window,
            cx,
        )
    }

    /// Rename folder `id`, or create a folder inside `parent`, in the library at `root`.
    pub(crate) fn begin_folder_at(
        &mut self,
        root: PathBuf,
        id: Option<u64>,
        parent: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = match id {
            Some(id) => NameTarget::Folder { root, id },
            None => NameTarget::NewFolder {
                root,
                parent,
                draft: None,
                then_move: None,
            },
        };
        self.begin_naming(target, window, cx)
    }

    /// Opens the name dialog, holding the target's current name.
    pub(crate) fn begin_naming(
        &mut self,
        target: NameTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = match &target {
            NameTarget::NewFolder { .. } => String::new(),
            NameTarget::Folder { root, id } => {
                let Some(library) = self.library(cx).indexes.get(root) else {
                    self.message = Some(folders_unavailable().into());
                    cx.notify();
                    return;
                };
                library.folders.get(id).cloned().unwrap_or_default()
            }
            NameTarget::Note { storage, .. } => self
                .library(cx)
                .courses
                .iter()
                .find(|course| &course.storage_dir() == storage)
                .map(|course| course.title.clone())
                .unwrap_or_default(),
        };
        if !matches!(self.page, Page::Library | Page::New) {
            self.navigate(Page::Library, cx);
        }
        self.inputs[&Field::Name].update(cx, |state, cx| state.set_value(name, window, cx));
        self.library_ui.name_dialog = Some(NameDialog {
            target,
            error: None,
            saving: false,
            open: true,
            focus_field: true,
        });
        cx.notify();
    }

    pub(crate) fn close_name_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.library_ui.name_dialog {
            dialog.open = false;
        }
        cx.notify();
    }

    /// The name changed: an earlier failure no longer applies.
    pub(crate) fn name_edited(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.library_ui.name_dialog {
            dialog.error = None;
        }
        cx.notify();
    }

    /// Runs blocking `work` off the UI thread, then hands its outcome to `done`.
    pub(crate) fn in_background<T: Send + 'static>(
        &mut self,
        work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, anyhow::Result<T>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let task = crate::spawn_blocking_result(work);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                done(this, result, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Writes the folder record on a blocking thread, then stores the result in
    /// the folder cache before `done` sees the outcome.
    pub(crate) fn edit_folders<T: Send + 'static>(
        &mut self,
        root: PathBuf,
        change: impl FnOnce(&mut organize::Library) -> anyhow::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, anyhow::Result<T>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let worker_root = root.clone();
        self.in_background(
            move || {
                let mut value = None;
                let library = organize::Library::edit(&worker_root, |library| {
                    value = Some(change(library)?);
                    Ok(())
                })?;
                let value = anyhow::Context::context(value, course2md::tr!("文件夹记录未更新"))?;
                Ok((library, value))
            },
            move |this, result, cx| {
                let result = result.map(|(library, value)| {
                    this.store_folders(&root, library, cx);
                    value
                });
                done(this, result, cx);
            },
            cx,
        );
    }

    pub(crate) fn store_folders(&mut self, root: &Path, library: organize::Library, cx: &mut App) {
        self.records.change(cx, |records, _| {
            let library_state = &mut records.library;
            if library_state.root == root {
                library_state.current = library.clone();
                library_state.error = None;
            }
            library_state.indexes.insert(root.to_owned(), library);
        });
    }

    /// Saves the dialog's name. Pressing Return in its field lands here too.
    pub fn save_name(&mut self, cx: &mut Context<Self>) {
        let name = self.value(Field::Name, cx);
        let Some(dialog) = self
            .library_ui
            .name_dialog
            .as_mut()
            .filter(|dialog| dialog.open && !dialog.saving && !name.is_empty())
        else {
            return;
        };
        dialog.saving = true;
        let target = dialog.target.clone();
        match target.clone() {
            NameTarget::Note { root, storage } => self.in_background(
                move || organize::rename_course(&root, &storage, &name),
                move |this, result, cx| {
                    let done = result.is_ok();
                    this.name_saved(result.map(|()| 0), &target, cx);
                    if done {
                        this.refresh_library(cx);
                    }
                },
                cx,
            ),
            NameTarget::Folder { root, id } => self.edit_folders(
                root,
                move |library| library.rename(Some(id), &name),
                move |this, result, cx| this.name_saved(result, &target, cx),
                cx,
            ),
            NameTarget::NewFolder { root, parent, .. } => self.edit_folders(
                root,
                move |library| library.create(parent, &name),
                move |this, result, cx| this.name_saved(result, &target, cx),
                cx,
            ),
        }
        cx.notify();
    }

    /// A save finished. `saved` is the folder it wrote. A closed dialog keeps the
    /// saved name but no longer navigates.
    fn name_saved(
        &mut self,
        result: anyhow::Result<u64>,
        target: &NameTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self
            .library_ui
            .name_dialog
            .as_mut()
            .filter(|dialog| dialog.open)
        else {
            return;
        };
        dialog.saving = false;
        let saved = match result {
            Ok(saved) => saved,
            Err(error) => {
                dialog.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        match target {
            NameTarget::Note { .. } => {}
            // Asked for from the move dialog: what was being moved goes into it.
            NameTarget::NewFolder {
                root,
                then_move: Some(moving),
                ..
            } => {
                let (root, moving) = (root.clone(), moving.clone());
                self.close_name_dialog(cx);
                match moving {
                    Moving::Note(storage) => self.file_note(root, storage, Some(saved), cx),
                    Moving::Folder(id) => self.move_folder_into(root, id, Some(saved), cx),
                }
                return;
            }
            NameTarget::NewFolder {
                root,
                draft: Some(draft_id),
                ..
            } => {
                if let Some(Err(error)) = self.with_workspace(cx, |workspace| {
                    workspace.transaction(|state| {
                        if let Some(draft) =
                            state.drafts.iter_mut().find(|draft| &draft.id == draft_id)
                        {
                            draft.folder = Some(saved);
                        }
                        Ok(())
                    })
                }) && let Some(dialog) = &mut self.library_ui.name_dialog
                {
                    dialog.error = Some(course2md::tr!(
                        "文件夹已创建，保存位置尚未更新：{}",
                        format!("{error:#}")
                    ));
                    dialog.target = NameTarget::Folder {
                        root: root.clone(),
                        id: saved,
                    };
                    cx.notify();
                    return;
                }
            }
            NameTarget::NewFolder { root, .. } | NameTarget::Folder { root, .. } => {
                self.select_library_scope(root.clone(), FolderFilter::Folder(saved), cx);
                if self.page != Page::Library {
                    self.navigate(Page::Library, cx);
                }
            }
        }
        self.close_name_dialog(cx);
    }

    /// The dialog naming a folder or note, drawn by the shell.
    pub(crate) fn name_dialog_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let input = self.inputs[&Field::Name].clone();
        let dialog = self.library_ui.name_dialog.as_mut()?;
        if std::mem::take(&mut dialog.focus_field) {
            let input = input.clone();
            window.on_next_frame(move |window, cx| {
                input.update(cx, |input, cx| input.focus(window, cx))
            });
        }
        let (headline, label, action, icon) = match &dialog.target {
            NameTarget::NewFolder {
                parent, then_move, ..
            } => (
                if parent.is_some() {
                    course2md::tr!("新建子文件夹")
                } else {
                    course2md::tr!("新建文件夹")
                },
                course2md::tr!("文件夹名称"),
                if then_move.is_some() {
                    course2md::tr!("创建并移入")
                } else {
                    course2md::tr!("创建文件夹")
                },
                symbols::create_new_folder(),
            ),
            NameTarget::Folder { .. } => (
                course2md::tr!("重命名文件夹"),
                course2md::tr!("文件夹名称"),
                course2md::tr!("保存名称"),
                symbols::check(),
            ),
            NameTarget::Note { .. } => (
                course2md::tr!("重命名笔记"),
                course2md::tr!("笔记名称"),
                course2md::tr!("保存名称"),
                symbols::check(),
            ),
        };
        let error = dialog.error.clone();
        let (open, saving) = (dialog.open, dialog.saving);
        let empty = input.read(cx).value().trim().is_empty();
        Some(
            MaterialDialog::new("name-dialog")
                .width(crate::ui::DIALOG_WIDTH_ROOMY)
                .container(ColorRole::SURFACE)
                .headline(headline)
                .content(
                    v_flex()
                        .px(rem(24.))
                        .gap(rem(8.))
                        .child(TileScale::Regular.support("name-field-label", label))
                        .child(
                            TextField::new("name-field", &input)
                                .placeholder(label)
                                .outlined()
                                .shape(TextFieldShape::Rounded)
                                .w_full()
                                .error(error.is_some())
                                .when_some(error, |field, error| field.supporting_text(error)),
                        ),
                )
                .action(
                    material_button("name-cancel", ButtonVariant::Text)
                        .child(course2md::tr!("取消"))
                        .on_click(cx.listener(|this, _, _, cx| this.close_name_dialog(cx))),
                )
                .action(
                    material_button("name-save", ButtonVariant::Filled)
                        .icon(icon)
                        .child(action)
                        .disabled(empty)
                        .loading(saving, action)
                        .on_click(cx.listener(|this, _, _, cx| this.save_name(cx))),
                )
                .modal(open)
                .dismiss_on_scrim(false)
                .on_dismiss({
                    let desktop = cx.weak_entity();
                    move |_, cx| {
                        let _ = desktop.update(cx, |this, cx| this.close_name_dialog(cx));
                    }
                })
                .into_any_element(),
        )
    }

    pub(crate) fn begin_move(&mut self, root: PathBuf, moving: Moving, cx: &mut Context<Self>) {
        self.library_ui.move_dialog = Some(MoveDialog {
            root,
            moving,
            open: true,
        });
        cx.notify();
    }

    fn close_move_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.library_ui.move_dialog {
            dialog.open = false;
        }
        cx.notify();
    }

    /// The dialog choosing a destination, drawn by the shell. A list of folders can
    /// outgrow a menu, and a folder cannot move inside itself.
    pub(crate) fn move_dialog_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.library_ui.move_dialog.as_ref()?;
        let library = self.library(cx).indexes.get(&dialog.root)?;
        let (top, top_icon, current, inside) = match &dialog.moving {
            Moving::Note(storage) => (
                course2md::tr!("未分类").to_owned(),
                symbols::inbox(),
                self.library(cx)
                    .view_cache
                    .locations
                    .get(storage)
                    .and_then(|location| library.folder_key(&location.relative)),
                Vec::new(),
            ),
            Moving::Folder(id) => (
                self.library_name(&dialog.root, cx),
                symbols::database(),
                library.parent(*id),
                library.subtree(*id),
            ),
        };
        let folders: Vec<_> = library
            .tree()
            .into_iter()
            .filter(|(id, _)| !inside.contains(id))
            .map(|(id, _)| (Some(id), library.path(id).join(" / ")))
            .collect();
        // Nowhere to go but where it already is: say so, and offer the folder to make.
        if folders.is_empty() && current.is_none() {
            return Some(self.move_nowhere_view(dialog, cx));
        }
        let rows = std::iter::once((None, top)).chain(folders).enumerate();
        let rows = rows.map(|(index, (destination, label))| {
            let (root, moving) = (dialog.root.clone(), dialog.moving.clone());
            ListItem::new(("move-destination", index), label)
                .density(ListDensity::Compact)
                .container(ColorRole::SURFACE_CONTAINER_HIGH)
                .leading(ListLeading::Icon(if destination.is_some() {
                    Icon::from(symbols::folder())
                } else {
                    Icon::from(top_icon)
                }))
                .selected(destination == current)
                .disabled(destination == current)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_move_dialog(cx);
                    match &moving {
                        Moving::Note(storage) => {
                            this.file_note(root.clone(), storage.clone(), destination, cx)
                        }
                        Moving::Folder(id) => {
                            this.move_folder_into(root.clone(), *id, destination, cx)
                        }
                    }
                }))
        });
        Some(
            MaterialDialog::new("move-dialog")
                .width(crate::ui::DIALOG_WIDTH_ROOMY)
                .container(ColorRole::SURFACE)
                .headline(course2md::tr!("移动到"))
                .content(
                    div()
                        .id("move-destinations")
                        .max_h(rem(320.))
                        .overflow_y_scroll()
                        // On the title's and the actions' margins.
                        .child(
                            div()
                                .px(rem(24.))
                                .child(segmented_list(rows.collect::<Vec<_>>())),
                        ),
                )
                .action(
                    material_button("move-cancel", ButtonVariant::Text)
                        .child(course2md::tr!("取消"))
                        .on_click(cx.listener(|this, _, _, cx| this.close_move_dialog(cx))),
                )
                .modal(dialog.open)
                .on_dismiss({
                    let desktop = cx.weak_entity();
                    move |_, cx| {
                        let _ = desktop.update(cx, |this, cx| this.close_move_dialog(cx));
                    }
                })
                .into_any_element(),
        )
    }

    /// The move dialog when the library has no other folder: what that means, and a
    /// new folder that what is being moved goes into.
    fn move_nowhere_view(&self, dialog: &MoveDialog, cx: &mut Context<Self>) -> AnyElement {
        let text = match dialog.moving {
            Moving::Note(_) => {
                course2md::tr!("这个笔记库还没有文件夹。新建一个，这份笔记会移入其中。")
            }
            Moving::Folder(_) => {
                course2md::tr!("这个笔记库没有可以移入的文件夹。新建一个，这个文件夹会移入其中。")
            }
        };
        let (root, moving) = (dialog.root.clone(), dialog.moving.clone());
        MaterialDialog::new("move-dialog")
            .width(crate::ui::DIALOG_WIDTH_ROOMY)
            .container(ColorRole::SURFACE)
            .headline(course2md::tr!("移动到"))
            .supporting_text(text)
            .action(
                material_button("move-cancel", ButtonVariant::Text)
                    .child(course2md::tr!("取消"))
                    .on_click(cx.listener(|this, _, _, cx| this.close_move_dialog(cx))),
            )
            .action(
                material_button("move-new-folder", ButtonVariant::Filled)
                    .icon(symbols::create_new_folder())
                    .child(course2md::tr!("新建文件夹"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_move_dialog(cx);
                        this.begin_naming(
                            NameTarget::NewFolder {
                                root: root.clone(),
                                parent: None,
                                draft: None,
                                then_move: Some(moving.clone()),
                            },
                            window,
                            cx,
                        );
                    })),
            )
            .modal(dialog.open)
            .on_dismiss({
                let desktop = cx.weak_entity();
                move |_, cx| {
                    let _ = desktop.update(cx, |this, cx| this.close_move_dialog(cx));
                }
            })
            .into_any_element()
    }

    /// Deleting a folder removes the folders inside it; notes are kept and
    /// return to their library's top level.
    pub(crate) fn begin_delete_folder_at(
        &mut self,
        root: PathBuf,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let library = self.library(cx).indexes.get(&root);
        let name = [library, Some(&self.library(cx).current)]
            .into_iter()
            .flatten()
            .find_map(|library| library.folders.get(&id))
            .cloned()
            .unwrap_or_else(|| course2md::tr!("已删除的文件夹").into());
        let nested = library.map_or(0, |library| library.subtree(id).len() - 1);
        let detail = if nested > 0 {
            course2md::tr!(
                "其中的 {} 个子文件夹也会删除；笔记会保留，移到未分类",
                nested
            )
        } else {
            course2md::tr!("其中的笔记会移到未分类").to_owned()
        };
        ui::Confirm::new(
            course2md::tr!("删除「{}」文件夹？", name),
            course2md::tr!("保留文件夹"),
            course2md::tr!("删除文件夹"),
        )
        .danger()
        .text(detail)
        .ask(
            cx.listener(move |this, _, _, cx| {
                let root = root.clone();
                let removed_root = root.clone();
                this.edit_folders(
                    root,
                    move |library| {
                        library.remove(id);
                        Ok(())
                    },
                    move |this, result, cx| match result {
                        // Drafts keep the deleted folder so their next submission
                        // asks for a deliberate destination.
                        Ok(()) => this.records.change(cx, |records, _| {
                            let library = &mut records.library;
                            let removed = library.folder_filter.folder_id().is_some_and(|folder| {
                                library
                                    .indexes
                                    .get(&removed_root)
                                    .is_none_or(|index| !index.folders.contains_key(&folder))
                            });
                            if library.root == removed_root && removed {
                                library.folder_filter = FolderFilter::Unfiled;
                            }
                        }),
                        Err(error) => {
                            this.message =
                                Some(course2md::tr!("无法删除文件夹：{}", format!("{error:#}")))
                        }
                    },
                    cx,
                );
            }),
            window,
            cx,
        );
    }

    pub(crate) fn folder_context(&self, course: Option<PathBuf>, cx: &App) -> FolderContext {
        let place = match &course {
            Some(path) => FolderPlace::Note(path),
            None => {
                let (root, draft_id) = self.current_folder_origin(cx);
                FolderPlace::Draft {
                    root,
                    draft_id,
                    folder: self.target_folder(cx),
                }
            }
        };
        self.records.read(cx).folder_context(place)
    }

    /// Where the workbench's next note is saved: one of many folders, by name.
    pub fn folder_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let context = self.folder_context(None, cx);
        let mut destinations = vec![None];
        destinations.extend(context.folders.iter().map(|(id, _)| Some(*id)));
        let selected = destinations.iter().position(|id| *id == context.folder);
        let draft_id = context.draft_id;
        ExposedDropdown::new("folder-picker", course2md::tr!("保存到"))
            .variant(TextFieldVariant::Outlined)
            // A form's field rounds with its surfaces (G8).
            .shape(TextFieldShape::Rounded)
            .leading_icon(Icon::from(symbols::folder()))
            .options(
                std::iter::once(course2md::tr!("未分类").to_owned())
                    .chain(context.folders.into_iter().map(|(_, path)| path)),
            )
            .selected(selected)
            .full_width()
            .open(self.library_ui.folder_picker_open)
            .disabled(self.library(cx).loading || context.load_error.is_some())
            .error(context.load_error.is_some())
            .when_some(context.load_error, |picker, error| {
                picker.supporting_text(error)
            })
            .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                this.library_ui.folder_picker_open = *open;
                cx.notify();
            }))
            .on_select(cx.listener(move |this, index: &usize, _, cx| {
                this.library_ui.folder_picker_open = false;
                let current = this
                    .workspace(cx)
                    .and_then(|workspace| workspace.state.draft())
                    .is_some_and(|draft| Some(&draft.id) == draft_id.as_ref());
                if current {
                    let id = destinations[*index];
                    this.update_draft(|draft| draft.folder = id, cx);
                }
                cx.notify();
            }))
    }
}
