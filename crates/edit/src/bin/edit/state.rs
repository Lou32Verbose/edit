// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::mem;
use std::path::{Path, PathBuf};

use edit::framebuffer::{self, IndexedColor};
use edit::helpers::*;
use edit::oklab::StraightRgba;
use edit::tui::*;
use edit::{apperr, buffer, icu, sys};

use crate::config::{self, EditorSettings, Keybindings};
use crate::documents::DocumentManager;
use crate::find_in_files::FindInFilesResult;
use crate::localization::*;

#[repr(transparent)]
pub struct FormatApperr(apperr::Error);

impl From<apperr::Error> for FormatApperr {
    fn from(err: apperr::Error) -> Self {
        Self(err)
    }
}

impl std::fmt::Display for FormatApperr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            apperr::APP_ICU_MISSING => f.write_str(loc(LocId::ErrorIcuMissing)),
            apperr::Error::App(code) => write!(f, "Unknown app error code: {code}"),
            apperr::Error::Icu(code) => icu::apperr_format(f, code),
            apperr::Error::Sys(code) => sys::apperr_format(f, code),
        }
    }
}

pub struct DisplayablePathBuf {
    value: PathBuf,
    str: Cow<'static, str>,
}

impl DisplayablePathBuf {
    #[allow(dead_code, reason = "only used on Windows")]
    pub fn from_string(string: String) -> Self {
        let str = Cow::Borrowed(string.as_str());
        let str = unsafe { mem::transmute::<Cow<'_, str>, Cow<'_, str>>(str) };
        let value = PathBuf::from(string);
        Self { value, str }
    }

    pub fn from_path(value: PathBuf) -> Self {
        let str = value.to_string_lossy();
        let str = unsafe { mem::transmute::<Cow<'_, str>, Cow<'_, str>>(str) };
        Self { value, str }
    }

    pub fn as_path(&self) -> &Path {
        &self.value
    }

    pub fn as_str(&self) -> &str {
        &self.str
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.value.as_os_str().as_encoded_bytes()
    }
}

impl Default for DisplayablePathBuf {
    fn default() -> Self {
        Self { value: Default::default(), str: Cow::Borrowed("") }
    }
}

impl Clone for DisplayablePathBuf {
    fn clone(&self) -> Self {
        Self::from_path(self.value.clone())
    }
}

impl From<OsString> for DisplayablePathBuf {
    fn from(s: OsString) -> Self {
        Self::from_path(PathBuf::from(s))
    }
}

impl<T: ?Sized + AsRef<OsStr>> From<&T> for DisplayablePathBuf {
    fn from(s: &T) -> Self {
        Self::from_path(PathBuf::from(s))
    }
}

pub struct StateSearch {
    pub kind: StateSearchKind,
    pub focus: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StateSearchKind {
    Hidden,
    Disabled,
    Search,
    Replace,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StateFilePicker {
    None,
    Open,
    OpenFolder,
    SaveAs,

    SaveAsShown, // Transitioned from SaveAs
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StateEncodingChange {
    None,
    Convert,
    Reopen,
}

#[derive(Default)]
pub struct OscTitleFileStatus {
    pub filename: String,
    pub dirty: bool,
}

pub struct State {
    pub menubar_color_bg: StraightRgba,
    pub menubar_color_fg: StraightRgba,
    pub editor_color_bg: StraightRgba,
    pub editor_color_fg: StraightRgba,
    pub terminal_palette: [StraightRgba; framebuffer::INDEXED_COLORS_COUNT],

    pub documents: DocumentManager,
    pub settings: EditorSettings,
    pub keybindings: Keybindings,

    // A ring buffer of the last 10 errors.
    pub error_log: [String; 10],
    pub error_log_index: usize,
    pub error_log_count: usize,

    pub wants_file_picker: StateFilePicker,
    pub file_picker_pending_dir: DisplayablePathBuf,
    pub file_picker_pending_dir_revision: u64, // Bumped every time `file_picker_pending_dir` changes.
    pub file_picker_pending_name: PathBuf,
    pub file_picker_entries: Option<[Vec<DisplayablePathBuf>; 3]>, // ["..", directories, files]
    pub file_picker_overwrite_warning: Option<PathBuf>,            // The path the warning is about.
    pub file_picker_autocomplete: Vec<DisplayablePathBuf>,

    pub wants_search: StateSearch,
    pub search_needle: String,
    pub search_replacement: String,
    pub search_options: buffer::SearchOptions,
    pub search_success: bool,

    pub wants_encoding_picker: bool,
    pub wants_encoding_change: StateEncodingChange,
    pub encoding_picker_needle: String,
    pub encoding_picker_results: Option<Vec<icu::Encoding>>,

    pub wants_save: bool,
    pub wants_statusbar_focus: bool,
    pub wants_indentation_picker: bool,
    pub wants_go_to_file: bool,
    pub wants_about: bool,
    pub wants_close: bool,
    pub wants_exit: bool,
    pub wants_goto: bool,
    pub goto_target: String,
    pub goto_invalid: bool,
    pub wants_theme_picker: bool,
    pub wants_recent_files: bool,
    pub recent_files_selected: usize,
    pub wants_command_palette: bool,
    pub command_palette_query: String,
    pub command_palette_selected: usize,
    pub command_palette_focus_list: bool,
    pub recent_commands: Vec<crate::commands::CommandId>,
    pub wants_quick_switcher: bool,
    pub quick_switcher_query: String,
    pub quick_switcher_selected: usize,
    pub wants_keybinding_editor: bool,
    pub keybinding_query: String,
    pub keybinding_selected: usize,
    pub keybinding_capture: Option<crate::commands::CommandId>,
    pub wants_context_help: bool,
    pub wants_quick_start: bool,
    pub wants_binary_prompt: bool,
    pub binary_prompt_path: Option<PathBuf>,
    pub binary_prompt_goto: Option<Point>,
    pub recent_files: Vec<PathBuf>,
    pub recent_folders: Vec<PathBuf>,
    pub open_folder: Option<PathBuf>,
    pub wants_find_in_files: bool,
    pub find_in_files_query: String,
    pub find_in_files_replacement: String,
    pub find_in_files_results: Vec<FindInFilesResult>,
    pub find_in_files_selected: usize,
    pub find_in_files_status: String,
    pub find_in_files_root: DisplayablePathBuf,
    pub wants_replace_preview: bool,
    pub replace_preview_results: Vec<ReplacePreviewItem>,
    pub replace_preview_status: String,
    pub replace_preview_in_files: bool,
    pub needs_theme_refresh: bool,

    pub osc_title_file_status: OscTitleFileStatus,
    pub osc_clipboard_sync: bool,
    pub osc_clipboard_always_send: bool,
    pub exit: bool,
}

impl State {
    pub fn new() -> apperr::Result<Self> {
        let cfg = config::load_config();
        Ok(Self {
            menubar_color_bg: StraightRgba::zero(),
            menubar_color_fg: StraightRgba::zero(),
            editor_color_bg: StraightRgba::zero(),
            editor_color_fg: StraightRgba::zero(),
            terminal_palette: framebuffer::DEFAULT_THEME,

            documents: Default::default(),
            settings: cfg.editor,
            keybindings: cfg.keybindings,

            error_log: [const { String::new() }; 10],
            error_log_index: 0,
            error_log_count: 0,

            wants_file_picker: StateFilePicker::None,
            file_picker_pending_dir: Default::default(),
            file_picker_pending_dir_revision: 0,
            file_picker_pending_name: Default::default(),
            file_picker_entries: None,
            file_picker_overwrite_warning: None,
            file_picker_autocomplete: Vec::new(),

            wants_search: StateSearch { kind: StateSearchKind::Hidden, focus: false },
            search_needle: Default::default(),
            search_replacement: Default::default(),
            search_options: Default::default(),
            search_success: true,

            wants_encoding_picker: false,
            encoding_picker_needle: Default::default(),
            encoding_picker_results: Default::default(),

            wants_save: false,
            wants_statusbar_focus: false,
            wants_encoding_change: StateEncodingChange::None,
            wants_indentation_picker: false,
            wants_go_to_file: false,
            wants_about: false,
            wants_close: false,
            wants_exit: false,
            wants_goto: false,
            goto_target: Default::default(),
            goto_invalid: false,
            wants_theme_picker: false,
            wants_recent_files: false,
            recent_files_selected: 0,
            wants_command_palette: false,
            command_palette_query: Default::default(),
            command_palette_selected: 0,
            command_palette_focus_list: false,
            recent_commands: Vec::new(),
            wants_quick_switcher: false,
            quick_switcher_query: Default::default(),
            quick_switcher_selected: 0,
            wants_keybinding_editor: false,
            keybinding_query: Default::default(),
            keybinding_selected: 0,
            keybinding_capture: None,
            wants_context_help: false,
            wants_quick_start: false,
            wants_binary_prompt: false,
            binary_prompt_path: None,
            binary_prompt_goto: None,
            recent_files: cfg.recent_files,
            recent_folders: cfg.recent_folders,
            open_folder: None,
            wants_find_in_files: false,
            find_in_files_query: Default::default(),
            find_in_files_replacement: Default::default(),
            find_in_files_results: Vec::new(),
            find_in_files_selected: 0,
            find_in_files_status: Default::default(),
            find_in_files_root: Default::default(),
            wants_replace_preview: false,
            replace_preview_results: Vec::new(),
            replace_preview_status: Default::default(),
            replace_preview_in_files: false,
            needs_theme_refresh: true,

            osc_title_file_status: Default::default(),
            osc_clipboard_sync: false,
            osc_clipboard_always_send: false,
            exit: false,
        })
    }
}

pub struct ReplacePreviewItem {
    pub path: DisplayablePathBuf,
    pub line: usize,
    pub column: usize,
    pub before: String,
    pub after: String,
}

impl State {
    pub fn reload_config(&mut self) {
        let cfg = config::load_config();
        self.settings = cfg.editor;
        self.keybindings = cfg.keybindings;
        config::apply_settings_to_all(&self.settings, &self.documents);
        self.needs_theme_refresh = true;
    }

    pub fn ensure_find_in_files_root(&mut self) {
        if !self.find_in_files_root.as_path().as_os_str().is_empty() {
            return;
        }

        if let Some(doc) = self.documents.active() {
            if let Some(dir) = &doc.dir {
                self.find_in_files_root = dir.clone();
                return;
            }
        }

        if let Some(folder) = &self.open_folder {
            self.find_in_files_root = DisplayablePathBuf::from_path(folder.clone());
            return;
        }

        self.find_in_files_root = self.file_picker_pending_dir.clone();
    }

    pub fn record_recent_command(&mut self, id: crate::commands::CommandId) {
        if let Some(pos) = self.recent_commands.iter().position(|existing| *existing == id) {
            self.recent_commands.remove(pos);
        }
        self.recent_commands.insert(0, id);
        if self.recent_commands.len() > 10 {
            self.recent_commands.truncate(10);
        }
    }
}

pub fn draw_add_untitled_document(ctx: &mut Context, state: &mut State) {
    if let Err(err) = state.documents.add_untitled(&state.settings) {
        error_log_add(ctx, state, err);
    }
}

pub fn push_recent_file(state: &mut State, path: PathBuf) {
    if let Some(pos) = state.recent_files.iter().position(|p| p == &path) {
        state.recent_files.remove(pos);
    }
    state.recent_files.insert(0, path);
    state.recent_files.truncate(20);
    let _ = config::persist_recents(&state.recent_files, &state.recent_folders);
}

pub fn push_recent_folder(state: &mut State, path: PathBuf) {
    if let Some(pos) = state.recent_folders.iter().position(|p| p == &path) {
        state.recent_folders.remove(pos);
    }
    state.recent_folders.insert(0, path);
    state.recent_folders.truncate(10);
    let _ = config::persist_recents(&state.recent_files, &state.recent_folders);
}

pub fn error_log_add(ctx: &mut Context, state: &mut State, err: apperr::Error) {
    let msg = format!("{}", FormatApperr::from(err));
    if !msg.is_empty() {
        state.error_log[state.error_log_index] = msg;
        state.error_log_index = (state.error_log_index + 1) % state.error_log.len();
        state.error_log_count = state.error_log.len().min(state.error_log_count + 1);
        ctx.needs_rerender();
    }
}

pub fn draw_error_log(ctx: &mut Context, state: &mut State) {
    ctx.modal_begin("error", loc(LocId::ErrorDialogTitle));
    ctx.attr_background_rgba(ctx.indexed(IndexedColor::Red));
    ctx.attr_foreground_rgba(ctx.indexed(IndexedColor::BrightWhite));
    {
        ctx.block_begin("content");
        ctx.attr_padding(Rect::three(0, 2, 1));
        {
            let off = state.error_log_index + state.error_log.len() - state.error_log_count;

            for i in 0..state.error_log_count {
                let idx = (off + i) % state.error_log.len();
                let msg = &state.error_log[idx][..];

                if !msg.is_empty() {
                    ctx.next_block_id_mixin(i as u64);
                    ctx.label("error", msg);
                    ctx.attr_overflow(Overflow::TruncateTail);
                }
            }
        }
        ctx.block_end();

        ctx.focus_on_first_present();
        if ctx.button("ok", loc(LocId::Ok), ButtonStyle::default()) {
            state.error_log_count = 0;
        }
        ctx.attr_position(Position::Center);
        ctx.inherit_focus();
    }
    if ctx.modal_end() {
        state.error_log_count = 0;
    }
}
