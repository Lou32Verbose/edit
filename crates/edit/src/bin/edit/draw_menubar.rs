// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use edit::helpers::*;
use edit::input::vk;
use edit::tui::*;
use stdext::arena_format;

use crate::commands;
use crate::localization::*;
use crate::state::*;

pub fn draw_menubar(ctx: &mut Context, state: &mut State) {
    ctx.menubar_begin();
    ctx.attr_background_rgba(state.menubar_color_bg);
    ctx.attr_foreground_rgba(state.menubar_color_fg);
    {
        let contains_focus = ctx.contains_focus();

        if ctx.menubar_menu_begin(loc(LocId::File), 'F') {
            draw_menu_file(ctx, state);
        }
        if !contains_focus && ctx.consume_shortcut(vk::F10) {
            ctx.steal_focus();
        }
        if state.documents.active().is_some() {
            if ctx.menubar_menu_begin(loc(LocId::Edit), 'E') {
                draw_menu_edit(ctx, state);
            }
            if ctx.menubar_menu_begin(loc(LocId::View), 'V') {
                draw_menu_view(ctx, state);
            }
        }
        if ctx.menubar_menu_begin(loc(LocId::Help), 'H') {
            draw_menu_help(ctx, state);
        }
    }
    ctx.menubar_end();
}

fn draw_menu_file(ctx: &mut Context, state: &mut State) {
    if ctx.menubar_menu_button(
        loc(LocId::FileNew),
        'N',
        state.keybindings.shortcut(commands::CommandId::FileNew),
    ) {
        draw_add_untitled_document(ctx, state);
    }
    if ctx.menubar_menu_button(
        loc(LocId::FileOpen),
        'O',
        state.keybindings.shortcut(commands::CommandId::FileOpen),
    ) {
        state.wants_file_picker = StateFilePicker::Open;
    }
    if ctx.menubar_menu_button(
        "Open Folder",
        'F',
        state.keybindings.shortcut(commands::CommandId::FileOpenFolder),
    ) {
        state.wants_file_picker = StateFilePicker::OpenFolder;
    }
    if ctx.menubar_menu_button(
        "Open Recent",
        'R',
        state.keybindings.shortcut(commands::CommandId::FileOpenRecent),
    ) {
        state.wants_recent_files = true;
        state.recent_files_selected = 0;
    }
    if ctx.menubar_menu_button(
        "Open Recent Folder",
        'L',
        state.keybindings.shortcut(commands::CommandId::FileOpenRecentFolder),
    ) {
        state.wants_quick_switcher = true;
        state.quick_switcher_query = "folder:".to_string();
        state.quick_switcher_selected = 0;
    }
    if state.documents.active().is_some() {
        if ctx.menubar_menu_button(
            loc(LocId::FileSave),
            'S',
            state.keybindings.shortcut(commands::CommandId::FileSave),
        ) {
            state.wants_save = true;
        }
        if ctx.menubar_menu_button(
            loc(LocId::FileSaveAs),
            'A',
            state.keybindings.shortcut(commands::CommandId::FileSaveAs),
        ) {
            state.wants_file_picker = StateFilePicker::SaveAs;
        }
        if ctx.menubar_menu_button(
            loc(LocId::FileClose),
            'C',
            state.keybindings.shortcut(commands::CommandId::FileClose),
        ) {
            state.wants_close = true;
        }
    }
    if ctx.menubar_menu_button(
        loc(LocId::FileExit),
        'X',
        state.keybindings.shortcut(commands::CommandId::FileExit),
    ) {
        state.wants_exit = true;
    }
    ctx.menubar_menu_end();
}

pub fn draw_recent_files(ctx: &mut Context, state: &mut State) {
    let mut activated_path: Option<std::path::PathBuf> = None;
    let mut done = false;

    ctx.modal_begin("recent_files", "Open Recent");
    ctx.attr_intrinsic_size(Size { width: 60, height: 16 });
    {
        if ctx.contains_focus() && ctx.consume_shortcut(vk::ESCAPE) {
            done = true;
        }

        if state.recent_files.is_empty() {
            ctx.block_begin("empty");
            ctx.attr_padding(Rect::two(2, 2));
            {
                ctx.label("msg", "(No recent files)");
                ctx.attr_position(Position::Center);
            }
            ctx.block_end();
        } else {
            ctx.scrollarea_begin("files", Size { width: 0, height: 14 });
            ctx.attr_padding(Rect::two(1, 2));
            {
                ctx.list_begin("items");
                ctx.focus_on_first_present();
                ctx.inherit_focus();

                for (i, path) in state.recent_files.iter().enumerate() {
                    // Format: "filename - full/path"
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                    let dir = path.parent().and_then(|p| p.to_str()).unwrap_or("");
                    let display = arena_format!(ctx.arena(), "{} - {}", name, dir);

                    let selected = i == state.recent_files_selected;
                    let selection = ctx.list_item(selected, &display);
                    if selection == ListSelection::Selected {
                        state.recent_files_selected = i;
                    } else if selection == ListSelection::Activated {
                        activated_path = Some(path.clone());
                        done = true;
                    }
                }

                ctx.list_end();
            }
            ctx.scrollarea_end();
        }
    }
    if ctx.modal_end() {
        done = true;
    }

    // Open the activated file
    if let Some(path) = activated_path {
        match state.documents.add_file_path(&path, &state.settings) {
            Ok(crate::documents::OpenOutcome::Opened) => {
                // File opened successfully
            }
            Ok(crate::documents::OpenOutcome::BinaryDetected { path, goto }) => {
                state.wants_binary_prompt = true;
                state.binary_prompt_path = Some(path);
                state.binary_prompt_goto = goto;
            }
            Err(err) => error_log_add(ctx, state, err),
        }
    }

    if done {
        state.wants_recent_files = false;
        state.recent_files_selected = 0;
        ctx.needs_rerender();
    }
}

fn draw_menu_edit(ctx: &mut Context, state: &mut State) {
    let doc = state.documents.active().unwrap();
    let mut tb = doc.buffer.borrow_mut();

    if ctx.menubar_menu_button(
        loc(LocId::EditUndo),
        'U',
        state.keybindings.shortcut(commands::CommandId::EditUndo),
    ) {
        tb.undo();
        ctx.needs_rerender();
    }
    if ctx.menubar_menu_button(
        loc(LocId::EditRedo),
        'R',
        state.keybindings.shortcut(commands::CommandId::EditRedo),
    ) {
        tb.redo();
        ctx.needs_rerender();
    }
    if ctx.menubar_menu_button(
        loc(LocId::EditCut),
        'T',
        state.keybindings.shortcut(commands::CommandId::EditCut),
    ) {
        tb.cut(ctx.clipboard_mut());
        ctx.needs_rerender();
    }
    if ctx.menubar_menu_button(
        loc(LocId::EditCopy),
        'C',
        state.keybindings.shortcut(commands::CommandId::EditCopy),
    ) {
        tb.copy(ctx.clipboard_mut());
        ctx.needs_rerender();
    }
    if ctx.menubar_menu_button(
        loc(LocId::EditPaste),
        'P',
        state.keybindings.shortcut(commands::CommandId::EditPaste),
    ) {
        tb.paste(ctx.clipboard_ref());
        ctx.needs_rerender();
    }
    if state.wants_search.kind != StateSearchKind::Disabled {
        if ctx.menubar_menu_button(
            loc(LocId::EditFind),
            'F',
            state.keybindings.shortcut(commands::CommandId::EditFind),
        ) {
            state.wants_search.kind = StateSearchKind::Search;
            state.wants_search.focus = true;
        }
        if ctx.menubar_menu_button(
            loc(LocId::EditReplace),
            'L',
            state.keybindings.shortcut(commands::CommandId::EditReplace),
        ) {
            state.wants_search.kind = StateSearchKind::Replace;
            state.wants_search.focus = true;
        }
    }
    if ctx.menubar_menu_button(
        "Find in Files",
        'I',
        state.keybindings.shortcut(commands::CommandId::FindInFiles),
    ) {
        state.wants_find_in_files = true;
    }
    if ctx.menubar_menu_button(
        loc(LocId::EditSelectAll),
        'A',
        state.keybindings.shortcut(commands::CommandId::EditSelectAll),
    ) {
        tb.select_all();
        ctx.needs_rerender();
    }
    ctx.menubar_menu_end();
}

fn draw_menu_view(ctx: &mut Context, state: &mut State) {
    if let Some(doc) = state.documents.active() {
        {
            let mut tb = doc.buffer.borrow_mut();
            let word_wrap = tb.is_word_wrap_enabled();

            // All values on the statusbar are currently document specific.
            if ctx.menubar_menu_button(loc(LocId::ViewFocusStatusbar), 'S', vk::NULL) {
                state.wants_statusbar_focus = true;
            }
            if ctx.menubar_menu_button(
                loc(LocId::ViewGoToFile),
                'F',
                state.keybindings.shortcut(commands::CommandId::ViewGoToFile),
            ) {
                state.wants_go_to_file = true;
            }
            if ctx.menubar_menu_button(
                "Quick Switcher",
                'Q',
                state.keybindings.shortcut(commands::CommandId::QuickSwitcher),
            ) {
                state.wants_quick_switcher = true;
                state.quick_switcher_selected = 0;
            }
            if ctx.menubar_menu_button(
                loc(LocId::FileGoto),
                'G',
                state.keybindings.shortcut(commands::CommandId::FileGoto),
            ) {
                state.wants_goto = true;
            }
            if ctx.menubar_menu_checkbox(
                loc(LocId::ViewWordWrap),
                'W',
                state.keybindings.shortcut(commands::CommandId::ViewWordWrap),
                word_wrap,
            ) {
                tb.set_word_wrap(!word_wrap);
                ctx.needs_rerender();
            }
        }
        if ctx.menubar_menu_checkbox(
            "High Contrast",
            'H',
            vk::NULL,
            state.settings.high_contrast,
        ) {
            state.settings.high_contrast = !state.settings.high_contrast;
            state.needs_theme_refresh = true;
            ctx.needs_rerender();
        }
        if ctx.menubar_menu_checkbox(
            "Escape to Exit",
            'X',
            vk::NULL,
            state.settings.escape_to_exit,
        ) {
            state.settings.escape_to_exit = !state.settings.escape_to_exit;
            ctx.needs_rerender();
        }
        if ctx.menubar_menu_button(
            "Edit Keybindings",
            'K',
            state.keybindings.shortcut(commands::CommandId::SettingsEditKeybindings),
        ) {
            state.wants_keybinding_editor = true;
        }
        if ctx.menubar_submenu_begin("Theme", 'T') {
            draw_menu_theme(ctx, state);
        }
        if ctx.menubar_menu_button(
            "Command Palette",
            'P',
            state.keybindings.shortcut(commands::CommandId::CommandPalette),
        ) {
            state.wants_command_palette = true;
        }
    }

    ctx.menubar_menu_end();
}

fn draw_menu_help(ctx: &mut Context, state: &mut State) {
    if ctx.menubar_menu_button(loc(LocId::HelpAbout), 'A', vk::NULL) {
        state.wants_about = true;
    }
    if ctx.menubar_menu_button(
        "Context Help",
        'C',
        state.keybindings.shortcut(commands::CommandId::HelpContext),
    ) {
        state.wants_context_help = true;
    }
    if ctx.menubar_menu_button(
        "Quick Start",
        'Q',
        state.keybindings.shortcut(commands::CommandId::HelpQuickStart),
    ) {
        state.wants_quick_start = true;
    }
    ctx.menubar_menu_end();
}

fn draw_menu_theme(ctx: &mut Context, state: &mut State) {
    if ctx.menubar_menu_button(
        "Theme Picker",
        'P',
        state.keybindings.shortcut(commands::CommandId::ThemePicker),
    ) {
        state.wants_theme_picker = true;
    }
    if ctx.menubar_menu_button(
        "Cycle Theme",
        'C',
        state.keybindings.shortcut(commands::CommandId::SettingsThemeCycle),
    ) {
        commands::run_command(ctx, state, commands::CommandId::SettingsThemeCycle);
    }
    if ctx.menubar_menu_button(
        "Previous Theme",
        'P',
        state.keybindings.shortcut(commands::CommandId::SettingsThemePrevious),
    ) {
        commands::run_command(ctx, state, commands::CommandId::SettingsThemePrevious);
    }
    ctx.menubar_menu_end();
}

pub fn draw_dialog_about(ctx: &mut Context, state: &mut State) {
    ctx.modal_begin("about", loc(LocId::AboutDialogTitle));
    {
        ctx.block_begin("content");
        ctx.inherit_focus();
        ctx.attr_padding(Rect::three(1, 2, 1));
        {
            ctx.label("title", "Edit32 - Quick Help");
            ctx.attr_overflow(Overflow::TruncateTail);
            ctx.attr_position(Position::Center);

            ctx.label(
                "version",
                &arena_format!(
                    ctx.arena(),
                    "{}{}",
                    loc(LocId::AboutDialogVersion),
                    env!("CARGO_PKG_VERSION")
                ),
            );
            ctx.attr_overflow(Overflow::TruncateHead);
            ctx.attr_position(Position::Center);

            ctx.label("blurb", "A simple editor for simple needs.");
            ctx.attr_overflow(Overflow::TruncateTail);
            ctx.attr_position(Position::Center);

            ctx.block_begin("shortcuts");
            ctx.attr_padding(Rect::three(1, 0, 0));
            {
                ctx.label("shortcuts-title", "Shortcuts");
                ctx.attr_overflow(Overflow::TruncateTail);

                ctx.label("shortcut-1", "F1: Command Palette");
                ctx.label("shortcut-2", "Ctrl+N: New");
                ctx.label("shortcut-3", "Ctrl+O: Open");
                ctx.label("shortcut-4", "Ctrl+S: Save");
                ctx.label("shortcut-5", "Ctrl+F: Find");
                ctx.label("shortcut-6", "Ctrl+R: Replace");
                ctx.label("shortcut-7", "F4: Find in Files");
                ctx.label("shortcut-8", "Ctrl+G: Go to Line");
                ctx.label("shortcut-9", "Ctrl+P: Go to File");
                ctx.label("shortcut-10", "Alt+Z: Word Wrap");
                ctx.label("shortcut-11", "Ctrl+W: Close");
                ctx.label("shortcut-12", "Ctrl+Q: Exit");
            }
            ctx.block_end();

            ctx.block_begin("choices");
            ctx.inherit_focus();
            ctx.attr_padding(Rect::three(1, 2, 0));
            ctx.attr_position(Position::Center);
            {
                ctx.focus_on_first_present();
                if ctx.button("ok", loc(LocId::Ok), ButtonStyle::default()) {
                    state.wants_about = false;
                }
                ctx.inherit_focus();
            }
            ctx.block_end();
        }
        ctx.block_end();
    }
    if ctx.modal_end() {
        state.wants_about = false;
    }
}
