// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::path::PathBuf;

use edit::framebuffer::{Attributes, IndexedColor};
use edit::fuzzy::score_fuzzy;
use edit::helpers::*;
use edit::icu;
use edit::input::vk;
use edit::tui::*;
use stdext::arena::scratch_arena;
use stdext::arena_format;

use crate::commands::{self, CommandGroup, CommandId};
use crate::config::{self, ThemeId};
use crate::localization::*;
use crate::state::*;
use crate::{SearchAction, find_in_files, search_execute};

pub fn draw_statusbar(ctx: &mut Context, state: &mut State) {
    ctx.table_begin("statusbar");
    ctx.attr_focus_well();
    ctx.attr_background_rgba(state.menubar_color_bg);
    ctx.attr_foreground_rgba(state.menubar_color_fg);
    ctx.table_set_cell_gap(Size { width: 2, height: 0 });
    ctx.attr_intrinsic_size(Size { width: COORD_TYPE_SAFE_MAX, height: 1 });
    ctx.attr_padding(Rect::two(0, 1));

    if let Some(doc) = state.documents.active() {
        let mut tb = doc.buffer.borrow_mut();

        ctx.table_next_row();

        let is_crlf = tb.is_crlf();
        if ctx.button("newline", if is_crlf { "CRLF" } else { "LF" }, ButtonStyle::default()) {
            tb.normalize_newlines(!is_crlf);
            ctx.needs_rerender();
        }
        if state.wants_statusbar_focus {
            state.wants_statusbar_focus = false;
            ctx.steal_focus();
        }

        state.wants_encoding_picker |=
            ctx.button("encoding", tb.encoding(), ButtonStyle::default());
        if state.wants_encoding_picker {
            if doc.path.is_some() {
                ctx.block_begin("frame");
                ctx.attr_float(FloatSpec {
                    anchor: Anchor::Last,
                    gravity_x: 0.0,
                    gravity_y: 1.0,
                    offset_x: 0.0,
                    offset_y: 0.0,
                });
                ctx.attr_padding(Rect::two(0, 1));
                ctx.attr_border();
                {
                    if ctx.button("reopen", loc(LocId::EncodingReopen), ButtonStyle::default()) {
                        state.wants_encoding_change = StateEncodingChange::Reopen;
                    }
                    ctx.focus_on_first_present();
                    if ctx.button("convert", loc(LocId::EncodingConvert), ButtonStyle::default()) {
                        state.wants_encoding_change = StateEncodingChange::Convert;
                    }
                }
                ctx.block_end();
            } else {
                // Can't reopen a file that doesn't exist.
                state.wants_encoding_change = StateEncodingChange::Convert;
            }

            if !ctx.contains_focus() {
                state.wants_encoding_picker = false;
                ctx.needs_rerender();
            }
        }

        state.wants_indentation_picker |= ctx.button(
            "indentation",
            &arena_format!(
                ctx.arena(),
                "{}:{}",
                loc(if tb.indent_with_tabs() {
                    LocId::IndentationTabs
                } else {
                    LocId::IndentationSpaces
                }),
                tb.tab_size(),
            ),
            ButtonStyle::default(),
        );
        if state.wants_indentation_picker {
            ctx.table_begin("indentation-picker");
            ctx.attr_float(FloatSpec {
                anchor: Anchor::Last,
                gravity_x: 0.0,
                gravity_y: 1.0,
                offset_x: 0.0,
                offset_y: 0.0,
            });
            ctx.attr_border();
            ctx.attr_padding(Rect::two(0, 1));
            ctx.table_set_cell_gap(Size { width: 1, height: 0 });
            {
                if ctx.contains_focus() && ctx.consume_shortcut(vk::RETURN) {
                    ctx.toss_focus_up();
                }

                ctx.table_next_row();

                ctx.list_begin("type");
                ctx.focus_on_first_present();
                ctx.attr_padding(Rect::two(0, 1));
                {
                    if ctx.list_item(tb.indent_with_tabs(), loc(LocId::IndentationTabs))
                        != ListSelection::Unchanged
                    {
                        tb.set_indent_with_tabs(true);
                        ctx.needs_rerender();
                    }
                    if ctx.list_item(!tb.indent_with_tabs(), loc(LocId::IndentationSpaces))
                        != ListSelection::Unchanged
                    {
                        tb.set_indent_with_tabs(false);
                        ctx.needs_rerender();
                    }
                }
                ctx.list_end();

                ctx.list_begin("width");
                ctx.attr_padding(Rect::two(0, 2));
                {
                    for width in 1u8..=8 {
                        let ch = [b'0' + width];
                        let label = unsafe { std::str::from_utf8_unchecked(&ch) };

                        if ctx.list_item(tb.tab_size() == width as CoordType, label)
                            != ListSelection::Unchanged
                        {
                            tb.set_tab_size(width as CoordType);
                            ctx.needs_rerender();
                        }
                    }
                }
                ctx.list_end();
            }
            ctx.table_end();

            if !ctx.contains_focus() {
                state.wants_indentation_picker = false;
                ctx.needs_rerender();
            }
        }

        ctx.label(
            "theme",
            &arena_format!(ctx.arena(), "Theme: {}", state.settings.theme.display_name()),
        );

        ctx.label(
            "location",
            &arena_format!(
                ctx.arena(),
                "{}:{}",
                tb.cursor_logical_pos().y + 1,
                tb.cursor_logical_pos().x + 1
            ),
        );

        if let Some(sel_len) = tb.selection_length() {
            let sel_words = tb.selection_word_count().unwrap_or(0);
            ctx.label(
                "selection-stats",
                &arena_format!(ctx.arena(), "Sel: {} words, {} chars", sel_words, sel_len),
            );
        } else {
            ctx.label(
                "doc-stats",
                &arena_format!(
                    ctx.arena(),
                    "{} words, {} chars",
                    tb.word_count(),
                    tb.text_length()
                ),
            );
        }

        #[cfg(feature = "debug-latency")]
        ctx.label(
            "stats",
            &arena_format!(ctx.arena(), "{}/{}", tb.logical_line_count(), tb.visual_line_count(),),
        );

        if tb.is_overtype() && ctx.button("overtype", "OVR", ButtonStyle::default()) {
            tb.set_overtype(false);
            ctx.needs_rerender();
        }

        if tb.is_dirty() {
            ctx.label("dirty", "*");
        }

        ctx.block_begin("filename-container");
        ctx.attr_intrinsic_size(Size { width: COORD_TYPE_SAFE_MAX, height: 1 });
        {
            let total = state.documents.len();
            let mut filename = doc.filename.as_str();
            let filename_buf;

            if total > 1 {
                filename_buf = arena_format!(ctx.arena(), "{} + {}", filename, total - 1);
                filename = &filename_buf;
            }

            state.wants_go_to_file |= ctx.button("filename", filename, ButtonStyle::default());
            ctx.inherit_focus();
            ctx.attr_overflow(Overflow::TruncateMiddle);
            ctx.attr_position(Position::Right);
        }
        ctx.block_end();
    } else {
        state.wants_statusbar_focus = false;
        state.wants_encoding_picker = false;
        state.wants_indentation_picker = false;
    }

    ctx.table_end();
}

pub fn draw_quick_switcher(ctx: &mut Context, state: &mut State) {
    let mut activate_path = None;
    let mut activate_folder = None;
    let mut done = false;

    ctx.modal_begin("quick-switcher", "Quick Switcher");
    ctx.attr_intrinsic_size(Size { width: 60, height: 16 });
    {
        if ctx.contains_focus() && ctx.consume_shortcut(vk::ESCAPE) {
            done = true;
        }

        ctx.table_begin("query");
        ctx.attr_padding(Rect::three(1, 2, 0));
        {
            ctx.table_next_row();
            if ctx.editline("input", &mut state.quick_switcher_query) {
                state.quick_switcher_selected = 0;
            }
            ctx.focus_on_first_present();
        }
        ctx.table_end();

        let mut query = state.quick_switcher_query.trim_ascii();
        let mut folder_only = false;
        if let Some(rest) = query.strip_prefix("folder:") {
            folder_only = true;
            query = rest.trim_start();
        }

        let mut candidates: Vec<(PathBuf, i64)> = Vec::new();
        if folder_only {
            for path in &state.recent_folders {
                let score = if query.is_empty() {
                    0
                } else {
                    score_fuzzy(ctx.arena(), &path.to_string_lossy(), query, true).0
                };
                if query.is_empty() || score > 0 {
                    candidates.push((path.clone(), score as i64));
                }
            }
        } else {
            for doc in state.documents.iter() {
                if let Some(path) = &doc.path {
                    let score = if query.is_empty() {
                        0
                    } else {
                        score_fuzzy(ctx.arena(), &path.to_string_lossy(), query, true).0
                    };
                    if query.is_empty() || score > 0 {
                        candidates.push((path.clone(), score as i64));
                    }
                }
            }
            for path in &state.recent_files {
                if candidates.iter().any(|(p, _)| p == path) {
                    continue;
                }
                let score = if query.is_empty() {
                    0
                } else {
                    score_fuzzy(ctx.arena(), &path.to_string_lossy(), query, true).0
                };
                if query.is_empty() || score > 0 {
                    candidates.push((path.clone(), score as i64));
                }
            }
        }

        candidates.sort_by(|a, b| b.1.cmp(&a.1));
        if state.quick_switcher_selected >= candidates.len() {
            state.quick_switcher_selected = candidates.len().saturating_sub(1);
        }

        ctx.scrollarea_begin("results", Size { width: 0, height: 10 });
        ctx.attr_padding(Rect::two(1, 2));
        {
            ctx.list_begin("items");
            ctx.inherit_focus();
            for (idx, (path, _)) in candidates.iter().enumerate() {
                let selected = idx == state.quick_switcher_selected;
                let result = ctx.list_item(selected, &path.to_string_lossy());
                if result == ListSelection::Selected {
                    state.quick_switcher_selected = idx;
                } else if result == ListSelection::Activated {
                    if folder_only {
                        activate_folder = Some(path.clone());
                    } else {
                        activate_path = Some(path.clone());
                    }
                    done = true;
                }
            }
            ctx.list_end();
        }
        ctx.scrollarea_end();
    }
    if ctx.modal_end() {
        done = true;
    }

    if let Some(path) = activate_path {
        match state.documents.add_file_path(&path, &state.settings) {
            Ok(crate::documents::OpenOutcome::Opened) => {
                push_recent_file(state, path);
            }
            Ok(crate::documents::OpenOutcome::BinaryDetected { path, goto }) => {
                state.wants_binary_prompt = true;
                state.binary_prompt_path = Some(path);
                state.binary_prompt_goto = goto;
            }
            Err(err) => error_log_add(ctx, state, err),
        }
    }
    if let Some(path) = activate_folder {
        state.open_folder = Some(path.clone());
        push_recent_folder(state, path);
    }
    if done {
        state.wants_quick_switcher = false;
        state.quick_switcher_query.clear();
        state.quick_switcher_selected = 0;
        ctx.needs_rerender();
    }
}

pub fn draw_theme_picker(ctx: &mut Context, state: &mut State) {
    let mut activated = None;
    let mut done = false;

    ctx.modal_begin("theme-picker", "Theme Picker");
    ctx.attr_intrinsic_size(Size { width: 40, height: 14 });
    {
        if ctx.contains_focus() && ctx.consume_shortcut(vk::ESCAPE) {
            done = true;
        }

        ctx.scrollarea_begin("themes", Size { width: 0, height: 12 });
        ctx.attr_padding(Rect::two(1, 2));
        {
            ctx.list_begin("items");
            ctx.focus_on_first_present();
            ctx.inherit_focus();

            let themes = [
                ThemeId::Terminal,
                ThemeId::Nord,
                ThemeId::OneDark,
                ThemeId::Gruvbox,
                ThemeId::Monokai,
                ThemeId::SolarizedDark,
                ThemeId::SolarizedLight,
                ThemeId::Dracula,
                ThemeId::TokyoNight,
                ThemeId::Midnight,
                ThemeId::Paperwhite,
                ThemeId::Custom,
            ];
            for theme in themes {
                let selected = state.settings.theme == theme;
                let selection = ctx.list_item(selected, theme.display_name());
                if selection == ListSelection::Activated {
                    activated = Some(theme);
                }
            }

            ctx.list_end();
        }
        ctx.scrollarea_end();
    }
    if ctx.modal_end() {
        done = true;
    }

    if let Some(theme) = activated {
        if let Some(command) = theme_command_id(theme) {
            commands::run_command(ctx, state, command);
        }
        state.wants_theme_picker = false;
        ctx.needs_rerender();
    } else if done {
        state.wants_theme_picker = false;
        ctx.needs_rerender();
    }
}

pub fn draw_keybinding_editor(ctx: &mut Context, state: &mut State) {
    let mut done = false;
    let mut pending_capture = state.keybinding_capture;

    ctx.modal_begin("keybindings", "Keybindings");
    ctx.attr_intrinsic_size(Size { width: 70, height: 18 });
    {
        if ctx.contains_focus() && ctx.consume_shortcut(vk::ESCAPE) {
            if state.keybinding_capture.is_some() {
                state.keybinding_capture = None;
            } else {
                done = true;
            }
        }

        ctx.table_begin("query");
        ctx.attr_padding(Rect::three(1, 2, 0));
        {
            ctx.table_next_row();
            if ctx.editline("input", &mut state.keybinding_query) {
                state.keybinding_selected = 0;
            }
            ctx.focus_on_first_present();
        }
        ctx.table_end();

        let query = state.keybinding_query.trim_ascii();
        let mut commands = commands::command_list()
            .into_iter()
            .map(|cmd| {
                let score = if query.is_empty() {
                    0
                } else {
                    score_fuzzy(ctx.arena(), cmd.label, query, true).0
                };
                (cmd, score)
            })
            .filter(|(_, score)| query.is_empty() || *score > 0)
            .collect::<Vec<_>>();
        commands.sort_by(|a, b| b.1.cmp(&a.1));

        if state.keybinding_selected >= commands.len() {
            state.keybinding_selected = commands.len().saturating_sub(1);
        }

        ctx.scrollarea_begin("results", Size { width: 0, height: 12 });
        ctx.attr_padding(Rect::two(1, 2));
        {
            ctx.list_begin("items");
            ctx.inherit_focus();
            for (idx, (cmd, _)) in commands.iter().enumerate() {
                let shortcut = commands::format_shortcut(state.keybindings.shortcut(cmd.id));
                let label = if let Some(shortcut) = shortcut {
                    arena_format!(ctx.arena(), "{}\t{}", cmd.label, shortcut)
                } else {
                    arena_format!(ctx.arena(), "{}\t{}", cmd.label, "Unassigned")
                };
                let selected = idx == state.keybinding_selected;
                let result = ctx.list_item(selected, &label);
                if result == ListSelection::Selected {
                    state.keybinding_selected = idx;
                } else if result == ListSelection::Activated {
                    pending_capture = Some(cmd.id);
                }
            }
            ctx.list_end();
        }
        ctx.scrollarea_end();

        if let Some(cmd) = pending_capture {
            ctx.block_begin("capture");
            ctx.attr_padding(Rect::three(0, 2, 1));
            ctx.label("prompt", "Press a new keybinding (Esc to cancel)");
            ctx.block_end();

            if let Some(key) = ctx.keyboard_input() {
                if key == vk::ESCAPE {
                    pending_capture = None;
                } else {
                    state.keybindings.set_override(cmd, key);
                    if let Err(err) = config::persist_keybindings(&state.keybindings) {
                        error_log_add(ctx, state, err);
                    }
                    pending_capture = None;
                }
                ctx.needs_rerender();
            }
        }
    }
    if ctx.modal_end() {
        done = true;
    }

    state.keybinding_capture = pending_capture;

    if done {
        state.wants_keybinding_editor = false;
        state.keybinding_query.clear();
        state.keybinding_selected = 0;
        state.keybinding_capture = None;
        ctx.needs_rerender();
    }
}

fn theme_command_id(theme: ThemeId) -> Option<CommandId> {
    Some(match theme {
        ThemeId::Terminal => CommandId::SettingsThemeTerminal,
        ThemeId::Nord => CommandId::SettingsThemeNord,
        ThemeId::OneDark => CommandId::SettingsThemeOneDark,
        ThemeId::Gruvbox => CommandId::SettingsThemeGruvbox,
        ThemeId::Monokai => CommandId::SettingsThemeMonokai,
        ThemeId::SolarizedDark => CommandId::SettingsThemeSolarizedDark,
        ThemeId::SolarizedLight => CommandId::SettingsThemeSolarizedLight,
        ThemeId::Dracula => CommandId::SettingsThemeDracula,
        ThemeId::TokyoNight => CommandId::SettingsThemeTokyoNight,
        ThemeId::Midnight => CommandId::SettingsThemeMidnight,
        ThemeId::Paperwhite => CommandId::SettingsThemePaperwhite,
        ThemeId::Custom => CommandId::SettingsThemeCustom,
    })
}

pub fn draw_dialog_encoding_change(ctx: &mut Context, state: &mut State) {
    let encoding = state.documents.active_mut().map_or("", |doc| doc.buffer.borrow().encoding());
    let reopen = state.wants_encoding_change == StateEncodingChange::Reopen;
    let width = (ctx.size().width - 20).max(10);
    let height = (ctx.size().height - 10).max(10);
    let mut change = None;
    let mut done = encoding.is_empty();

    ctx.modal_begin(
        "encode",
        if reopen { loc(LocId::EncodingReopen) } else { loc(LocId::EncodingConvert) },
    );
    {
        ctx.table_begin("encoding-search");
        ctx.table_set_columns(&[0, COORD_TYPE_SAFE_MAX]);
        ctx.table_set_cell_gap(Size { width: 1, height: 0 });
        ctx.inherit_focus();
        {
            ctx.table_next_row();
            ctx.inherit_focus();

            ctx.label("needle-label", loc(LocId::SearchNeedleLabel));

            if ctx.editline("needle", &mut state.encoding_picker_needle) {
                encoding_picker_update_list(state);
            }
            ctx.inherit_focus();
            ctx.focus_on_first_present();
        }
        ctx.table_end();

        ctx.scrollarea_begin("scrollarea", Size { width, height });
        ctx.attr_background_rgba(ctx.indexed_alpha(IndexedColor::Black, 1, 4));
        {
            ctx.list_begin("encodings");
            ctx.inherit_focus();

            for enc in state
                .encoding_picker_results
                .as_deref()
                .unwrap_or_else(|| icu::get_available_encodings().preferred)
            {
                if ctx.list_item(enc.canonical == encoding, enc.label) == ListSelection::Activated {
                    change = Some(enc.canonical);
                    break;
                }
                ctx.attr_overflow(Overflow::TruncateTail);
            }
            ctx.list_end();
        }
        ctx.scrollarea_end();
    }
    done |= ctx.modal_end();
    done |= change.is_some();

    if let Some(encoding) = change
        && let Some(doc) = state.documents.active_mut()
    {
        if reopen && doc.path.is_some() {
            let mut res = Ok(());
            if doc.buffer.borrow().is_dirty() {
                res = doc.save(None);
            }
            if res.is_ok() {
                res = doc.reread(Some(encoding));
            }
            if let Err(err) = res {
                error_log_add(ctx, state, err);
            }
        } else {
            doc.buffer.borrow_mut().set_encoding(encoding);
        }
    }

    if done {
        state.wants_encoding_change = StateEncodingChange::None;
        state.encoding_picker_needle.clear();
        state.encoding_picker_results = None;
        ctx.needs_rerender();
    }
}

fn encoding_picker_update_list(state: &mut State) {
    state.encoding_picker_results = None;

    let needle = state.encoding_picker_needle.trim_ascii();
    if needle.is_empty() {
        return;
    }

    let encodings = icu::get_available_encodings();
    let scratch = scratch_arena(None);
    let mut matches = Vec::new_in(&*scratch);

    for enc in encodings.all {
        let local_scratch = scratch_arena(Some(&scratch));
        let (score, _) = score_fuzzy(&local_scratch, enc.label, needle, true);

        if score > 0 {
            matches.push((score, *enc));
        }
    }

    matches.sort_by(|a, b| b.0.cmp(&a.0));
    state.encoding_picker_results = Some(Vec::from_iter(matches.iter().map(|(_, enc)| *enc)));
}

pub fn draw_go_to_file(ctx: &mut Context, state: &mut State) {
    ctx.modal_begin("go-to-file", loc(LocId::ViewGoToFile));
    {
        let width = (ctx.size().width - 20).max(10);
        let height = (ctx.size().height - 10).max(10);

        ctx.scrollarea_begin("scrollarea", Size { width, height });
        ctx.attr_background_rgba(ctx.indexed_alpha(IndexedColor::Black, 1, 4));
        ctx.inherit_focus();
        {
            ctx.list_begin("documents");
            ctx.focus_on_first_present();
            ctx.inherit_focus();

            if state.documents.update_active(|doc| {
                let tb = doc.buffer.borrow();

                ctx.styled_list_item_begin();
                ctx.attr_overflow(Overflow::TruncateTail);
                ctx.styled_label_add_text(if tb.is_dirty() { "* " } else { "  " });
                ctx.styled_label_add_text(&doc.filename);

                if let Some(path) = &doc.dir {
                    ctx.styled_label_add_text("   ");
                    ctx.styled_label_set_attributes(Attributes::Italic);
                    ctx.styled_label_add_text(path.as_str());
                }

                ctx.styled_list_item_end(false) == ListSelection::Activated
            }) {
                state.wants_go_to_file = false;
                ctx.needs_rerender();
            }

            ctx.list_end();
        }
        ctx.scrollarea_end();
    }
    if ctx.modal_end() {
        state.wants_go_to_file = false;
    }
}

pub fn draw_command_palette(ctx: &mut Context, state: &mut State) {
    let mut activated = None;
    let mut done = false;

    ctx.modal_begin("command-palette", "Command Palette");
    {
        let width = (ctx.size().width - 20).max(20);
        let height = (ctx.size().height - 10).max(10);

        if ctx.contains_focus() && ctx.consume_shortcut(vk::ESCAPE) {
            done = true;
        }

        ctx.table_begin("palette");
        ctx.table_set_columns(&[0, COORD_TYPE_SAFE_MAX]);
        ctx.table_set_cell_gap(Size { width: 1, height: 0 });
        ctx.inherit_focus();
        {
            ctx.table_next_row();
            ctx.label("label", loc(LocId::SearchNeedleLabel));

            if ctx.editline("query", &mut state.command_palette_query) {
                state.command_palette_selected = 0;
            }
            ctx.inherit_focus();
            ctx.focus_on_first_present();
        }
        ctx.table_end();

        let mut matches = Vec::new();
        let scratch = scratch_arena(None);
        let needle = state.command_palette_query.trim_ascii();
        let has_doc = state.documents.active().is_some();

        let command_list = commands::command_list();
        for cmd in &command_list {
            if !cmd.show_in_palette {
                continue;
            }
            if cmd.requires_document && !has_doc {
                continue;
            }
            let score = if needle.is_empty() {
                1
            } else {
                score_fuzzy(&scratch, cmd.label, needle, true).0
            };
            if score > 0 {
                let mut score = score;
                if state.recent_commands.iter().any(|id| *id == cmd.id) {
                    score += 50;
                }
                matches.push((score, *cmd));
            }
        }

        let mut grouped: [Vec<(i32, commands::Command)>; 7] =
            [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for (score, cmd) in matches {
            let index = match commands::command_group(cmd.id) {
                CommandGroup::File => 0,
                CommandGroup::Edit => 1,
                CommandGroup::View => 2,
                CommandGroup::Help => 3,
                CommandGroup::Settings => 4,
                CommandGroup::Themes => 5,
                CommandGroup::Other => 6,
            };
            grouped[index].push((score, cmd));
        }

        for group in &mut grouped {
            group.sort_by(|a, b| b.0.cmp(&a.0));
        }

        let mut ordered_commands = Vec::new();
        for group in &grouped {
            for (_, cmd) in group {
                ordered_commands.push(*cmd);
            }
        }

        if state.command_palette_selected >= ordered_commands.len() {
            state.command_palette_selected = ordered_commands.len().saturating_sub(1);
        }

        if ctx.contains_focus() {
            if ctx.consume_shortcut(vk::UP) {
                state.command_palette_selected = state.command_palette_selected.saturating_sub(1);
            } else if ctx.consume_shortcut(vk::DOWN) {
                state.command_palette_selected = (state.command_palette_selected + 1)
                    .min(ordered_commands.len().saturating_sub(1));
            }
        }

        ctx.scrollarea_begin("results", Size { width, height });
        ctx.attr_background_rgba(ctx.indexed_alpha(IndexedColor::Black, 1, 4));
        ctx.inherit_focus();
        {
            ctx.list_begin("commands");
            ctx.inherit_focus();

            let group_labels = [
                CommandGroup::File,
                CommandGroup::Edit,
                CommandGroup::View,
                CommandGroup::Help,
                CommandGroup::Settings,
                CommandGroup::Themes,
                CommandGroup::Other,
            ];

            let mut command_index = 0usize;
            for (group_index, group) in grouped.iter().enumerate() {
                if group.is_empty() {
                    continue;
                }

                let label = commands::command_group_label(group_labels[group_index]);
                let _ = ctx.list_item(false, label);
                ctx.attr_overflow(Overflow::TruncateTail);

                for (_, cmd) in group {
                    let selected = command_index == state.command_palette_selected;
                    let shortcut = commands::format_shortcut(state.keybindings.shortcut(cmd.id));
                    let label = if let Some(shortcut) = shortcut {
                        arena_format!(ctx.arena(), "{}  [{}]", cmd.label, shortcut)
                    } else {
                        arena_format!(ctx.arena(), "{}", cmd.label)
                    };
                    let selection = ctx.list_item(selected, &label);
                    if selected && state.command_palette_focus_list {
                        ctx.list_item_steal_focus();
                        state.command_palette_focus_list = false;
                    }
                    if selection == ListSelection::Activated {
                        activated = Some(cmd.id);
                    }
                    ctx.attr_overflow(Overflow::TruncateTail);
                    command_index += 1;
                }
            }

            ctx.list_end();
        }
        ctx.scrollarea_end();

        if ctx.contains_focus()
            && ctx.consume_shortcut(vk::RETURN)
            && state.command_palette_selected < ordered_commands.len()
        {
            activated = Some(ordered_commands[state.command_palette_selected].id);
        }
    }
    if ctx.modal_end() {
        done = true;
    }

    if let Some(id) = activated {
        if id == CommandId::CommandPalette {
            // Prevent recursion from re-opening itself.
            state.wants_command_palette = false;
        } else {
            commands::run_command(ctx, state, id);
            state.wants_command_palette = false;
        }
        state.command_palette_focus_list = false;
        state.command_palette_query.clear();
        state.command_palette_selected = 0;
        ctx.needs_rerender();
    } else if done {
        state.wants_command_palette = false;
        state.command_palette_focus_list = false;
        state.command_palette_query.clear();
        state.command_palette_selected = 0;
        ctx.needs_rerender();
    }
}

pub fn draw_find_in_files(ctx: &mut Context, state: &mut State) {
    state.ensure_find_in_files_root();

    let mut do_search = false;
    let mut do_replace = false;
    let mut do_preview = false;
    let mut activated = None;
    let mut done = false;

    ctx.modal_begin("find-in-files", "Find in Files");
    {
        let width = (ctx.size().width - 20).max(20);
        let height = (ctx.size().height - 12).max(10);

        if ctx.contains_focus() && ctx.consume_shortcut(vk::ESCAPE) {
            done = true;
        }

        ctx.table_begin("inputs");
        ctx.table_set_columns(&[0, COORD_TYPE_SAFE_MAX]);
        ctx.table_set_cell_gap(Size { width: 1, height: 0 });
        ctx.inherit_focus();
        {
            ctx.table_next_row();
            ctx.label("root-label", "Root");
            ctx.label("root", state.find_in_files_root.as_str());
            ctx.attr_overflow(Overflow::TruncateMiddle);

            ctx.table_next_row();
            ctx.label("query-label", loc(LocId::SearchNeedleLabel));
            if ctx.editline("query", &mut state.find_in_files_query) {
                state.find_in_files_selected = 0;
            }
            ctx.inherit_focus();
            ctx.focus_on_first_present();

            ctx.table_next_row();
            ctx.label("replace-label", loc(LocId::SearchReplacementLabel));
            if ctx.editline("replace", &mut state.find_in_files_replacement) {
                state.find_in_files_selected = 0;
            }
            ctx.inherit_focus();

            if ctx.contains_focus() && ctx.consume_shortcut(vk::RETURN) {
                do_search = true;
            }
        }
        ctx.table_end();

        ctx.table_begin("actions");
        ctx.table_set_cell_gap(Size { width: 2, height: 0 });
        ctx.inherit_focus();
        {
            ctx.table_next_row();
            ctx.focus_on_first_present();
            if ctx.button("search", loc(LocId::EditFind), ButtonStyle::default()) {
                do_search = true;
            }
            if ctx.button("replace-all", loc(LocId::SearchReplaceAll), ButtonStyle::default()) {
                do_replace = true;
            }
            if ctx.button("preview", "Preview", ButtonStyle::default()) {
                do_preview = true;
            }
            if ctx.button("close", loc(LocId::SearchClose), ButtonStyle::default()) {
                done = true;
            }
        }
        ctx.table_end();

        if !state.find_in_files_status.is_empty() {
            ctx.label("status", &state.find_in_files_status);
            ctx.attr_overflow(Overflow::TruncateTail);
        }

        if ctx.contains_focus() {
            if ctx.consume_shortcut(vk::UP) {
                state.find_in_files_selected = state.find_in_files_selected.saturating_sub(1);
            } else if ctx.consume_shortcut(vk::DOWN) {
                state.find_in_files_selected = (state.find_in_files_selected + 1)
                    .min(state.find_in_files_results.len().saturating_sub(1));
            }
        }

        ctx.scrollarea_begin("results", Size { width, height });
        ctx.attr_background_rgba(ctx.indexed_alpha(IndexedColor::Black, 1, 4));
        ctx.inherit_focus();
        {
            ctx.list_begin("results-list");
            ctx.inherit_focus();

            for (idx, result) in state.find_in_files_results.iter().enumerate() {
                let selected = idx == state.find_in_files_selected;
                let label = arena_format!(
                    ctx.arena(),
                    "{}:{}:{}  {}",
                    result.path.as_str(),
                    result.line,
                    result.column,
                    result.preview
                );
                let selection = ctx.list_item(selected, &label);
                if selection == ListSelection::Activated {
                    activated = Some(idx);
                }
                ctx.attr_overflow(Overflow::TruncateTail);
            }

            ctx.list_end();
        }
        ctx.scrollarea_end();

        if ctx.contains_focus()
            && ctx.consume_shortcut(vk::RETURN)
            && state.find_in_files_selected < state.find_in_files_results.len()
        {
            activated = Some(state.find_in_files_selected);
        }
    }
    if ctx.modal_end() {
        done = true;
    }

    if do_search {
        let root = state.find_in_files_root.as_path();
        let results = find_in_files::search(root, &state.find_in_files_query, state.search_options);
        state.find_in_files_results = results;
        state.find_in_files_selected = 0;
        let truncated = state.find_in_files_results.len() >= find_in_files::max_results_limit();
        state.find_in_files_status = if truncated {
            format!(
                "{} result(s) (truncated at limit {})",
                state.find_in_files_results.len(),
                find_in_files::max_results_limit()
            )
        } else {
            format!("{} result(s)", state.find_in_files_results.len())
        };
        ctx.needs_rerender();
    } else if do_preview {
        let root = state.find_in_files_root.as_path();
        let (results, status) = find_in_files::preview_replace(
            root,
            &state.find_in_files_query,
            &state.find_in_files_replacement,
            state.search_options,
        );
        state.replace_preview_results = results;
        state.replace_preview_status = status;
        state.replace_preview_in_files = true;
        state.wants_replace_preview = true;
        state.wants_find_in_files = false;
        ctx.needs_rerender();
    } else if do_replace {
        let root = state.find_in_files_root.as_path();
        match find_in_files::replace_all(
            root,
            &state.find_in_files_query,
            &state.find_in_files_replacement,
            state.search_options,
            &mut state.documents,
        ) {
            Ok(stats) => {
                state.find_in_files_status = format!(
                    "Replaced {} occurrence(s) in {} file(s), skipped {} dirty file(s)",
                    stats.replacements, stats.files_changed, stats.skipped_dirty
                );
                state.find_in_files_results =
                    find_in_files::search(root, &state.find_in_files_query, state.search_options);
                state.find_in_files_selected = 0;
            }
            Err(err) => {
                error_log_add(ctx, state, err);
            }
        }
        ctx.needs_rerender();
    } else if let Some(index) = activated {
        if let Some(result) = state.find_in_files_results.get(index) {
            let path = result.path.as_path();
            let goto = format!("{}:{}:{}", path.to_string_lossy(), result.line, result.column);
            let goto_path = std::path::PathBuf::from(goto);
            match state.documents.add_file_path(&goto_path, &state.settings) {
                Ok(crate::documents::OpenOutcome::Opened) => {
                    push_recent_file(state, path.to_path_buf());
                }
                Ok(crate::documents::OpenOutcome::BinaryDetected { path, goto }) => {
                    state.wants_binary_prompt = true;
                    state.binary_prompt_path = Some(path);
                    state.binary_prompt_goto = goto;
                }
                Err(err) => {
                    error_log_add(ctx, state, err);
                }
            }
        }
        ctx.needs_rerender();
    } else if done {
        state.wants_find_in_files = false;
        ctx.needs_rerender();
    }
}

pub fn draw_replace_preview(ctx: &mut Context, state: &mut State) {
    let mut apply = false;
    let mut done = false;

    ctx.modal_begin("replace-preview", "Replace Preview");
    {
        let width = (ctx.size().width - 20).max(20);
        let height = (ctx.size().height - 12).max(10);

        if ctx.contains_focus() && ctx.consume_shortcut(vk::ESCAPE) {
            done = true;
        }

        if !state.replace_preview_status.is_empty() {
            ctx.label("status", &state.replace_preview_status);
            ctx.attr_overflow(Overflow::TruncateTail);
        }

        ctx.scrollarea_begin("results", Size { width, height });
        ctx.attr_background_rgba(ctx.indexed_alpha(IndexedColor::Black, 1, 4));
        ctx.inherit_focus();
        {
            ctx.list_begin("preview-list");
            ctx.inherit_focus();

            for item in &state.replace_preview_results {
                let label = arena_format!(
                    ctx.arena(),
                    "{}:{}:{}  {} => {}",
                    item.path.as_str(),
                    item.line,
                    item.column,
                    item.before,
                    item.after
                );
                let _ = ctx.list_item(false, &label);
                ctx.attr_overflow(Overflow::TruncateTail);
            }

            ctx.list_end();
        }
        ctx.scrollarea_end();

        ctx.table_begin("actions");
        ctx.table_set_cell_gap(Size { width: 2, height: 0 });
        ctx.inherit_focus();
        {
            ctx.table_next_row();
            ctx.focus_on_first_present();
            if ctx.button("apply", "Apply All", ButtonStyle::default()) {
                apply = true;
            }
            if ctx.button("close", loc(LocId::SearchClose), ButtonStyle::default()) {
                done = true;
            }
        }
        ctx.table_end();
    }
    if ctx.modal_end() {
        done = true;
    }

    if apply {
        if state.replace_preview_in_files {
            let root = state.find_in_files_root.as_path();
            if let Err(err) = find_in_files::replace_all(
                root,
                &state.find_in_files_query,
                &state.find_in_files_replacement,
                state.search_options,
                &mut state.documents,
            ) {
                error_log_add(ctx, state, err);
            }
        } else {
            search_execute(ctx, state, SearchAction::ReplaceAll);
        }
        state.wants_replace_preview = false;
        ctx.needs_rerender();
    } else if done {
        state.wants_replace_preview = false;
        ctx.needs_rerender();
    }
}
