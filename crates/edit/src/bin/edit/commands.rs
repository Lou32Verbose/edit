// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use edit::input::{kbmod, vk, InputKey};
use edit::tui::Context;

use crate::config;
use crate::localization::{loc, LocId};
use crate::state::{State, StateFilePicker, StateSearchKind};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandGroup {
    File,
    Edit,
    View,
    Help,
    Settings,
    Themes,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    FileNew,
    FileOpen,
    FileOpenFolder,
    FileOpenRecentFolder,
    FileSave,
    FileSaveAs,
    FileClose,
    FileExit,
    EditUndo,
    EditRedo,
    EditCut,
    EditCopy,
    EditPaste,
    EditFind,
    EditReplace,
    EditSelectAll,
    FindInFiles,
    ViewWordWrap,
    ViewGoToFile,
    FileGoto,
    HelpAbout,
    HelpContext,
    HelpQuickStart,
    CommandPalette,
    ThemePicker,
    SettingsOpenConfig,
    SettingsReload,
    SettingsToggleHighContrast,
    SettingsEditKeybindings,
    SettingsThemeTerminal,
    SettingsThemeNord,
    SettingsThemeOneDark,
    SettingsThemeGruvbox,
    SettingsThemeMonokai,
    SettingsThemeSolarizedDark,
    SettingsThemeSolarizedLight,
    SettingsThemeDracula,
    SettingsThemeTokyoNight,
    SettingsThemeMidnight,
    SettingsThemePaperwhite,
    SettingsThemeCustom,
    SettingsThemeCycle,
    SettingsThemePrevious,
    QuickSwitcher,
}

#[derive(Clone, Copy)]
pub struct Command {
    pub id: CommandId,
    pub label: &'static str,
    pub requires_document: bool,
    pub show_in_palette: bool,
}

pub fn command_list() -> Vec<Command> {
    use CommandId::*;
    vec![
        Command { id: FileNew, label: loc(LocId::FileNew), requires_document: false, show_in_palette: true },
        Command { id: FileOpen, label: loc(LocId::FileOpen), requires_document: false, show_in_palette: true },
        Command { id: FileOpenFolder, label: "Open Folder", requires_document: false, show_in_palette: true },
        Command { id: FileOpenRecentFolder, label: "Open Recent Folder", requires_document: false, show_in_palette: true },
        Command { id: FileSave, label: loc(LocId::FileSave), requires_document: true, show_in_palette: true },
        Command { id: FileSaveAs, label: loc(LocId::FileSaveAs), requires_document: true, show_in_palette: true },
        Command { id: FileClose, label: loc(LocId::FileClose), requires_document: true, show_in_palette: true },
        Command { id: FileExit, label: loc(LocId::FileExit), requires_document: false, show_in_palette: true },
        Command { id: EditUndo, label: loc(LocId::EditUndo), requires_document: true, show_in_palette: true },
        Command { id: EditRedo, label: loc(LocId::EditRedo), requires_document: true, show_in_palette: true },
        Command { id: EditCut, label: loc(LocId::EditCut), requires_document: true, show_in_palette: true },
        Command { id: EditCopy, label: loc(LocId::EditCopy), requires_document: true, show_in_palette: true },
        Command { id: EditPaste, label: loc(LocId::EditPaste), requires_document: true, show_in_palette: true },
        Command { id: EditFind, label: loc(LocId::EditFind), requires_document: true, show_in_palette: true },
        Command { id: EditReplace, label: loc(LocId::EditReplace), requires_document: true, show_in_palette: true },
        Command { id: EditSelectAll, label: loc(LocId::EditSelectAll), requires_document: true, show_in_palette: true },
        Command { id: FindInFiles, label: "Find in Files", requires_document: false, show_in_palette: true },
        Command { id: ViewWordWrap, label: loc(LocId::ViewWordWrap), requires_document: true, show_in_palette: true },
        Command { id: ViewGoToFile, label: loc(LocId::ViewGoToFile), requires_document: true, show_in_palette: true },
        Command { id: FileGoto, label: loc(LocId::FileGoto), requires_document: true, show_in_palette: true },
        Command { id: HelpAbout, label: loc(LocId::HelpAbout), requires_document: false, show_in_palette: true },
        Command { id: HelpContext, label: "Help: Context", requires_document: false, show_in_palette: true },
        Command { id: HelpQuickStart, label: "Help: Quick Start", requires_document: false, show_in_palette: true },
        Command { id: QuickSwitcher, label: "Quick Switcher", requires_document: false, show_in_palette: true },
        Command { id: ThemePicker, label: "Themes: Theme Picker", requires_document: false, show_in_palette: true },
        Command { id: SettingsOpenConfig, label: "Settings: Open Config", requires_document: false, show_in_palette: true },
        Command { id: SettingsReload, label: "Settings: Reload Config", requires_document: false, show_in_palette: true },
        Command { id: SettingsToggleHighContrast, label: "Settings: Toggle High Contrast", requires_document: false, show_in_palette: true },
        Command { id: SettingsEditKeybindings, label: "Settings: Edit Keybindings", requires_document: false, show_in_palette: true },
        Command { id: SettingsThemeTerminal, label: "Theme: Terminal", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeNord, label: "Theme: Nord", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeOneDark, label: "Theme: One Dark", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeGruvbox, label: "Theme: Gruvbox", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeMonokai, label: "Theme: Monokai", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeSolarizedDark, label: "Theme: Solarized Dark", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeSolarizedLight, label: "Theme: Solarized Light", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeDracula, label: "Theme: Dracula", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeTokyoNight, label: "Theme: Tokyo Night", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeMidnight, label: "Theme: Midnight", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemePaperwhite, label: "Theme: Paperwhite", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeCustom, label: "Theme: Custom", requires_document: false, show_in_palette: false },
        Command { id: SettingsThemeCycle, label: "Theme: Cycle", requires_document: false, show_in_palette: true },
        Command { id: SettingsThemePrevious, label: "Theme: Previous", requires_document: false, show_in_palette: true },
    ]
}

pub fn shortcut(id: CommandId) -> InputKey {
    use CommandId::*;
    match id {
        FileNew => kbmod::CTRL | vk::N,
        FileOpen => kbmod::CTRL | vk::O,
        FileOpenFolder => kbmod::CTRL_SHIFT | vk::O,
        FileOpenRecentFolder => vk::NULL,
        FileSave => kbmod::CTRL | vk::S,
        FileSaveAs => kbmod::CTRL_SHIFT | vk::S,
        FileClose => kbmod::CTRL | vk::W,
        FileExit => kbmod::CTRL | vk::Q,
        EditUndo => kbmod::CTRL | vk::Z,
        EditRedo => kbmod::CTRL | vk::Y,
        EditCut => kbmod::CTRL | vk::X,
        EditCopy => kbmod::CTRL | vk::C,
        EditPaste => kbmod::CTRL | vk::V,
        EditFind => kbmod::CTRL | vk::F,
        EditReplace => kbmod::CTRL | vk::R,
        EditSelectAll => kbmod::CTRL | vk::A,
        FindInFiles => vk::F4,
        ViewWordWrap => kbmod::ALT | vk::Z,
        ViewGoToFile => kbmod::CTRL | vk::P,
        FileGoto => kbmod::CTRL | vk::G,
        HelpAbout => vk::NULL,
        HelpContext => vk::NULL,
        HelpQuickStart => vk::NULL,
        CommandPalette => vk::F1,
        ThemePicker => kbmod::CTRL_SHIFT | vk::T,
        QuickSwitcher => kbmod::CTRL | vk::E,
        SettingsOpenConfig => vk::NULL,
        SettingsReload => vk::NULL,
        SettingsToggleHighContrast => vk::NULL,
        SettingsEditKeybindings => kbmod::CTRL | vk::K,
        SettingsThemeTerminal => kbmod::CTRL_ALT | vk::N1,
        SettingsThemeNord => kbmod::CTRL_ALT | vk::N2,
        SettingsThemeOneDark => vk::NULL,
        SettingsThemeGruvbox => kbmod::CTRL_ALT | vk::N3,
        SettingsThemeMonokai => vk::NULL,
        SettingsThemeSolarizedDark => vk::NULL,
        SettingsThemeSolarizedLight => kbmod::CTRL_ALT | vk::N4,
        SettingsThemeDracula => kbmod::CTRL_ALT | vk::N5,
        SettingsThemeTokyoNight => kbmod::CTRL_ALT | vk::N6,
        SettingsThemeMidnight => vk::NULL,
        SettingsThemePaperwhite => vk::NULL,
        SettingsThemeCustom => vk::NULL,
        SettingsThemeCycle => kbmod::CTRL_ALT | vk::N0,
        SettingsThemePrevious => kbmod::CTRL_ALT | vk::N9,
    }
}

pub fn run_command(ctx: &mut Context, state: &mut State, id: CommandId) {
    use CommandId::*;

    match id {
        FileNew => crate::state::draw_add_untitled_document(ctx, state),
        FileOpen => state.wants_file_picker = StateFilePicker::Open,
        FileOpenFolder => state.wants_file_picker = StateFilePicker::OpenFolder,
        FileOpenRecentFolder => {
            state.wants_quick_switcher = true;
            state.quick_switcher_query = "folder:".to_string();
            state.quick_switcher_selected = 0;
        }
        FileSave => state.wants_save = true,
        FileSaveAs => state.wants_file_picker = StateFilePicker::SaveAs,
        FileClose => state.wants_close = true,
        FileExit => state.wants_exit = true,
        EditUndo => {
            if let Some(doc) = state.documents.active() {
                doc.buffer.borrow_mut().undo();
                ctx.needs_rerender();
            }
        }
        EditRedo => {
            if let Some(doc) = state.documents.active() {
                doc.buffer.borrow_mut().redo();
                ctx.needs_rerender();
            }
        }
        EditCut => {
            if let Some(doc) = state.documents.active() {
                doc.buffer.borrow_mut().cut(ctx.clipboard_mut());
                ctx.needs_rerender();
            }
        }
        EditCopy => {
            if let Some(doc) = state.documents.active() {
                doc.buffer.borrow_mut().copy(ctx.clipboard_mut());
                ctx.needs_rerender();
            }
        }
        EditPaste => {
            if let Some(doc) = state.documents.active() {
                doc.buffer.borrow_mut().paste(ctx.clipboard_ref());
                ctx.needs_rerender();
            }
        }
        EditFind => {
            if state.wants_search.kind != StateSearchKind::Disabled {
                state.wants_search.kind = StateSearchKind::Search;
                state.wants_search.focus = true;
            }
        }
        EditReplace => {
            if state.wants_search.kind != StateSearchKind::Disabled {
                state.wants_search.kind = StateSearchKind::Replace;
                state.wants_search.focus = true;
            }
        }
        EditSelectAll => {
            if let Some(doc) = state.documents.active() {
                doc.buffer.borrow_mut().select_all();
                ctx.needs_rerender();
            }
        }
        FindInFiles => {
            state.wants_find_in_files = true;
            state.ensure_find_in_files_root();
        }
        ViewWordWrap => {
            if let Some(doc) = state.documents.active() {
                let mut tb = doc.buffer.borrow_mut();
                let word_wrap = tb.is_word_wrap_enabled();
                tb.set_word_wrap(!word_wrap);
                ctx.needs_rerender();
            }
        }
        ViewGoToFile => state.wants_go_to_file = true,
        FileGoto => state.wants_goto = true,
        HelpAbout => state.wants_about = true,
        HelpContext => state.wants_context_help = true,
        HelpQuickStart => state.wants_quick_start = true,
        CommandPalette => {
            state.wants_command_palette = true;
            state.command_palette_focus_list = true;
        }
        ThemePicker => {
            state.wants_theme_picker = true;
        }
        QuickSwitcher => {
            state.wants_quick_switcher = true;
            state.quick_switcher_selected = 0;
        }
        SettingsOpenConfig => {
            if let Some(path) = config::ensure_config_file() {
                match state.documents.add_file_path(&path, &state.settings) {
                    Ok(crate::documents::OpenOutcome::Opened) => {}
                    Ok(crate::documents::OpenOutcome::BinaryDetected { .. }) => {}
                    Err(err) => crate::state::error_log_add(ctx, state, err),
                }
            }
        }
        SettingsReload => {
            state.reload_config();
            ctx.needs_rerender();
        }
        SettingsToggleHighContrast => {
            state.settings.high_contrast = !state.settings.high_contrast;
            state.needs_theme_refresh = true;
            ctx.needs_rerender();
        }
        SettingsEditKeybindings => {
            state.wants_keybinding_editor = true;
        }
        SettingsThemeTerminal => {
            apply_theme_change(ctx, state, config::ThemeId::Terminal);
        }
        SettingsThemeNord => {
            apply_theme_change(ctx, state, config::ThemeId::Nord);
        }
        SettingsThemeOneDark => {
            apply_theme_change(ctx, state, config::ThemeId::OneDark);
        }
        SettingsThemeGruvbox => {
            apply_theme_change(ctx, state, config::ThemeId::Gruvbox);
        }
        SettingsThemeMonokai => {
            apply_theme_change(ctx, state, config::ThemeId::Monokai);
        }
        SettingsThemeSolarizedDark => {
            apply_theme_change(ctx, state, config::ThemeId::SolarizedDark);
        }
        SettingsThemeSolarizedLight => {
            apply_theme_change(ctx, state, config::ThemeId::SolarizedLight);
        }
        SettingsThemeDracula => {
            apply_theme_change(ctx, state, config::ThemeId::Dracula);
        }
        SettingsThemeTokyoNight => {
            apply_theme_change(ctx, state, config::ThemeId::TokyoNight);
        }
        SettingsThemeMidnight => {
            apply_theme_change(ctx, state, config::ThemeId::Midnight);
        }
        SettingsThemePaperwhite => {
            apply_theme_change(ctx, state, config::ThemeId::Paperwhite);
        }
        SettingsThemeCustom => {
            apply_theme_change(ctx, state, config::ThemeId::Custom);
        }
        SettingsThemeCycle => {
            let theme = next_theme(state.settings.theme);
            apply_theme_change(ctx, state, theme);
        }
        SettingsThemePrevious => {
            let theme = previous_theme(state.settings.theme);
            apply_theme_change(ctx, state, theme);
        }
    }

    if id != CommandPalette {
        state.record_recent_command(id);
    }
}

fn apply_theme_change(ctx: &mut Context, state: &mut State, theme: config::ThemeId) {
    state.settings.theme = theme;
    state.needs_theme_refresh = true;
    ctx.needs_rerender();

    if let Err(err) = config::persist_theme(theme) {
        crate::state::error_log_add(ctx, state, err);
    }
}

pub fn command_group(id: CommandId) -> CommandGroup {
    use CommandGroup::*;
    match id {
        CommandId::FileNew
        | CommandId::FileOpen
        | CommandId::FileOpenFolder
        | CommandId::FileOpenRecentFolder
        | CommandId::FileSave
        | CommandId::FileSaveAs
        | CommandId::FileClose
        | CommandId::FileExit => File,
        CommandId::EditUndo
        | CommandId::EditRedo
        | CommandId::EditCut
        | CommandId::EditCopy
        | CommandId::EditPaste
        | CommandId::EditFind
        | CommandId::EditReplace
        | CommandId::EditSelectAll
        | CommandId::FindInFiles => Edit,
        CommandId::ViewWordWrap | CommandId::ViewGoToFile | CommandId::FileGoto => View,
        CommandId::HelpAbout | CommandId::HelpContext | CommandId::HelpQuickStart => Help,
        CommandId::ThemePicker
        | CommandId::SettingsThemeCycle
        | CommandId::SettingsThemePrevious => Themes,
        CommandId::SettingsOpenConfig
        | CommandId::SettingsReload
        | CommandId::SettingsToggleHighContrast
        | CommandId::SettingsEditKeybindings
        | CommandId::SettingsThemeTerminal
        | CommandId::SettingsThemeNord
        | CommandId::SettingsThemeOneDark
        | CommandId::SettingsThemeGruvbox
        | CommandId::SettingsThemeMonokai
        | CommandId::SettingsThemeSolarizedDark
        | CommandId::SettingsThemeSolarizedLight
        | CommandId::SettingsThemeDracula
        | CommandId::SettingsThemeTokyoNight
        | CommandId::SettingsThemeMidnight
        | CommandId::SettingsThemePaperwhite
        | CommandId::SettingsThemeCustom
        => Settings,
        CommandId::CommandPalette | CommandId::QuickSwitcher => Other,
    }
}

fn next_theme(current: config::ThemeId) -> config::ThemeId {
    use config::ThemeId;

    match current {
        ThemeId::Terminal => ThemeId::Nord,
        ThemeId::Nord => ThemeId::OneDark,
        ThemeId::OneDark => ThemeId::Gruvbox,
        ThemeId::Gruvbox => ThemeId::Monokai,
        ThemeId::Monokai => ThemeId::SolarizedDark,
        ThemeId::SolarizedDark => ThemeId::Dracula,
        ThemeId::Dracula => ThemeId::TokyoNight,
        ThemeId::TokyoNight => ThemeId::Midnight,
        ThemeId::Midnight => ThemeId::Paperwhite,
        ThemeId::Paperwhite => ThemeId::SolarizedLight,
        ThemeId::SolarizedLight => ThemeId::Custom,
        ThemeId::Custom => ThemeId::Terminal,
    }
}

fn previous_theme(current: config::ThemeId) -> config::ThemeId {
    use config::ThemeId;

    match current {
        ThemeId::Terminal => ThemeId::Custom,
        ThemeId::Nord => ThemeId::Terminal,
        ThemeId::OneDark => ThemeId::Nord,
        ThemeId::Gruvbox => ThemeId::OneDark,
        ThemeId::Monokai => ThemeId::Gruvbox,
        ThemeId::SolarizedDark => ThemeId::Monokai,
        ThemeId::Dracula => ThemeId::SolarizedDark,
        ThemeId::TokyoNight => ThemeId::Dracula,
        ThemeId::Midnight => ThemeId::TokyoNight,
        ThemeId::Paperwhite => ThemeId::Midnight,
        ThemeId::SolarizedLight => ThemeId::Paperwhite,
        ThemeId::Custom => ThemeId::SolarizedLight,
    }
}

pub fn command_group_label(group: CommandGroup) -> &'static str {
    match group {
        CommandGroup::File => "File",
        CommandGroup::Edit => "Edit",
        CommandGroup::View => "View",
        CommandGroup::Help => "Help",
        CommandGroup::Settings => "Settings",
        CommandGroup::Themes => "Themes",
        CommandGroup::Other => "Other",
    }
}

pub fn format_shortcut(key: InputKey) -> Option<String> {
    if key == vk::NULL {
        return None;
    }

    let modifiers = key.modifiers();
    let base = key.key();
    let mut parts = Vec::new();

    if modifiers.contains(kbmod::CTRL) {
        parts.push(loc(LocId::Ctrl));
    }
    if modifiers.contains(kbmod::ALT) {
        parts.push(loc(LocId::Alt));
    }
    if modifiers.contains(kbmod::SHIFT) {
        parts.push(loc(LocId::Shift));
    }

    if let Some(name) = key_name(base) {
        parts.push(name);
    } else {
        return None;
    }

    Some(parts.join("+"))
}

fn key_name(key: InputKey) -> Option<&'static str> {
    Some(match key {
        vk::A => "A",
        vk::B => "B",
        vk::C => "C",
        vk::D => "D",
        vk::E => "E",
        vk::F => "F",
        vk::G => "G",
        vk::H => "H",
        vk::I => "I",
        vk::J => "J",
        vk::K => "K",
        vk::L => "L",
        vk::M => "M",
        vk::N => "N",
        vk::O => "O",
        vk::P => "P",
        vk::Q => "Q",
        vk::R => "R",
        vk::S => "S",
        vk::T => "T",
        vk::U => "U",
        vk::V => "V",
        vk::W => "W",
        vk::X => "X",
        vk::Y => "Y",
        vk::Z => "Z",
        vk::N0 => "0",
        vk::N1 => "1",
        vk::N2 => "2",
        vk::N3 => "3",
        vk::N4 => "4",
        vk::N5 => "5",
        vk::N6 => "6",
        vk::N7 => "7",
        vk::N8 => "8",
        vk::N9 => "9",
        vk::F1 => "F1",
        vk::F2 => "F2",
        vk::F3 => "F3",
        vk::F4 => "F4",
        vk::F5 => "F5",
        vk::F6 => "F6",
        vk::F7 => "F7",
        vk::F8 => "F8",
        vk::F9 => "F9",
        vk::F10 => "F10",
        vk::F11 => "F11",
        vk::F12 => "F12",
        vk::RETURN => "Enter",
        vk::ESCAPE => "Esc",
        vk::SPACE => "Space",
        vk::TAB => "Tab",
        vk::BACK => "Backspace",
        vk::DELETE => "Delete",
        vk::UP => "Up",
        vk::DOWN => "Down",
        vk::LEFT => "Left",
        vk::RIGHT => "Right",
        vk::HOME => "Home",
        vk::END => "End",
        vk::PRIOR => "PageUp",
        vk::NEXT => "PageDown",
        _ => return None,
    })
}
