// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::num::ParseIntError;
use std::path::PathBuf;

use edit::framebuffer::IndexedColor;
use edit::helpers::*;
use edit::input::{kbmod, vk};
use edit::tui::*;
use edit::{buffer, icu};

use crate::localization::*;
use crate::state::*;

pub fn draw_editor(ctx: &mut Context, state: &mut State) {
    if !matches!(state.wants_search.kind, StateSearchKind::Hidden | StateSearchKind::Disabled) {
        draw_search(ctx, state);
    }

    let size = ctx.size();
    // TODO: The layout code should be able to just figure out the height on its own.
    let height_reduction = match state.wants_search.kind {
        StateSearchKind::Search => 4,
        StateSearchKind::Replace => 5,
        _ => 2,
    };

    if let Some(doc) = state.documents.active() {
        {
            let mut tb = doc.buffer.borrow_mut();
            if !matches!(
                state.wants_search.kind,
                StateSearchKind::Hidden | StateSearchKind::Disabled
            ) && !state.search_needle.trim_ascii().is_empty()
            {
                tb.set_search_highlight(&state.search_needle, state.search_options);
            } else {
                tb.clear_search_highlight();
            }
        }
        ctx.textarea("textarea", doc.buffer.clone());
        ctx.inherit_focus();
    } else {
        ctx.block_begin("empty");
        ctx.block_end();
    }

    ctx.attr_intrinsic_size(Size { width: 0, height: size.height - height_reduction });
}

fn draw_search(ctx: &mut Context, state: &mut State) {
    if let Err(err) = icu::init() {
        error_log_add(ctx, state, err);
        state.wants_search.kind = StateSearchKind::Disabled;
        return;
    }

    let Some(doc) = state.documents.active() else {
        state.wants_search.kind = StateSearchKind::Hidden;
        return;
    };

    let mut action = None;
    let mut focus = StateSearchKind::Hidden;

    if state.wants_search.focus {
        state.wants_search.focus = false;
        focus = StateSearchKind::Search;

        // If the selection is empty, focus the search input field.
        // Otherwise, focus the replace input field, if it exists.
        if let Some(selection) = doc.buffer.borrow_mut().extract_user_selection(false) {
            state.search_needle = String::from_utf8_lossy_owned(selection);
            focus = state.wants_search.kind;
        }
    }

    ctx.block_begin("search");
    ctx.attr_focus_well();
    ctx.attr_background_rgba(state.editor_color_bg);
    ctx.attr_foreground_rgba(state.editor_color_fg);
    {
        if ctx.contains_focus() && ctx.consume_shortcut(vk::ESCAPE) {
            state.wants_search.kind = StateSearchKind::Hidden;
        }

        ctx.table_begin("needle");
        ctx.table_set_cell_gap(Size { width: 1, height: 0 });
        {
            {
                ctx.table_next_row();
                ctx.label("label", loc(LocId::SearchNeedleLabel));

                if ctx.editline("needle", &mut state.search_needle) {
                    action = Some(SearchAction::Search);
                }
                if !state.search_success {
                    ctx.attr_background_rgba(ctx.indexed(IndexedColor::Red));
                    ctx.attr_foreground_rgba(ctx.indexed(IndexedColor::BrightWhite));
                }
                ctx.attr_intrinsic_size(Size { width: COORD_TYPE_SAFE_MAX, height: 1 });
                if focus == StateSearchKind::Search {
                    ctx.steal_focus();
                }
                if ctx.is_focused() && ctx.consume_shortcut(vk::RETURN) {
                    action = Some(SearchAction::Search);
                }
            }

            if state.wants_search.kind == StateSearchKind::Replace {
                ctx.table_next_row();
                ctx.label("label", loc(LocId::SearchReplacementLabel));

                ctx.editline("replacement", &mut state.search_replacement);
                ctx.attr_intrinsic_size(Size { width: COORD_TYPE_SAFE_MAX, height: 1 });
                if focus == StateSearchKind::Replace {
                    ctx.steal_focus();
                }
                if ctx.is_focused() {
                    if ctx.consume_shortcut(vk::RETURN) {
                        action = Some(SearchAction::Replace);
                    } else if ctx.consume_shortcut(kbmod::CTRL_ALT | vk::RETURN) {
                        action = Some(SearchAction::ReplaceAll);
                    }
                }
            }
        }
        ctx.table_end();

        ctx.table_begin("options");
        ctx.table_set_cell_gap(Size { width: 2, height: 0 });
        {
            let mut change = false;
            let mut change_action = Some(SearchAction::Search);

            ctx.table_next_row();

            change |= ctx.checkbox(
                "match-case",
                loc(LocId::SearchMatchCase),
                &mut state.search_options.match_case,
            );
            change |= ctx.checkbox(
                "whole-word",
                loc(LocId::SearchWholeWord),
                &mut state.search_options.whole_word,
            );
            change |= ctx.checkbox(
                "use-regex",
                loc(LocId::SearchUseRegex),
                &mut state.search_options.use_regex,
            );
            if state.wants_search.kind == StateSearchKind::Replace {
                if ctx.button("replace-all", loc(LocId::SearchReplaceAll), ButtonStyle::default()) {
                    change = true;
                    change_action = Some(SearchAction::ReplaceAll);
                }
                if ctx.button("preview", "Preview", ButtonStyle::default()) {
                    if let Some(doc) = state.documents.active() {
                        let (results, status) = build_replace_preview(doc, state);
                        state.replace_preview_results = results;
                        state.replace_preview_status = status;
                        state.replace_preview_in_files = false;
                        state.wants_replace_preview = true;
                    }
                }
            }
            if ctx.button("close", loc(LocId::SearchClose), ButtonStyle::default()) {
                state.wants_search.kind = StateSearchKind::Hidden;
            }

            if change {
                action = change_action;
                state.wants_search.focus = true;
                ctx.needs_rerender();
            }
        }
        ctx.table_end();
    }
    ctx.block_end();

    if let Some(action) = action {
        search_execute(ctx, state, action);
    }
}

pub enum SearchAction {
    Search,
    Replace,
    ReplaceAll,
}

pub fn search_execute(ctx: &mut Context, state: &mut State, action: SearchAction) {
    let Some(doc) = state.documents.active_mut() else {
        return;
    };

    state.search_success = match action {
        SearchAction::Search => {
            doc.buffer.borrow_mut().find_and_select(&state.search_needle, state.search_options)
        }
        SearchAction::Replace => doc.buffer.borrow_mut().find_and_replace(
            &state.search_needle,
            state.search_options,
            state.search_replacement.as_bytes(),
        ),
        SearchAction::ReplaceAll => doc.buffer.borrow_mut().find_and_replace_all(
            &state.search_needle,
            state.search_options,
            state.search_replacement.as_bytes(),
        ),
    }
    .is_ok();

    ctx.needs_rerender();
}

fn build_replace_preview(
    doc: &crate::documents::Document,
    state: &State,
) -> (Vec<ReplacePreviewItem>, String) {
    let needle = state.search_needle.trim_ascii();
    if needle.is_empty() {
        return (Vec::new(), "No search text provided.".to_string());
    }
    if state.search_options.use_regex {
        return (Vec::new(), "Preview not available for regex search.".to_string());
    }

    let mut bytes = Vec::new();
    {
        let tb = doc.buffer.borrow();
        let mut off = 0usize;
        loop {
            let chunk = tb.read_forward(off);
            if chunk.is_empty() {
                break;
            }
            bytes.extend_from_slice(chunk);
            off += chunk.len();
        }
    }

    let text = String::from_utf8_lossy(&bytes);
    let path = doc
        .path
        .as_ref()
        .map(|p| DisplayablePathBuf::from_path(p.clone()))
        .unwrap_or_else(|| DisplayablePathBuf::from_path(PathBuf::from(&doc.filename)));

    let mut results = Vec::new();
    const MAX_PREVIEW: usize = 200;

    for (line_idx, line) in text.lines().enumerate() {
        let matches = preview_find_matches(line, needle, state.search_options);
        if matches.is_empty() {
            continue;
        }

        let after = replace_line_with_matches(line, &matches, &state.search_replacement);
        let column = matches.first().map_or(1, |m| m.start + 1);

        results.push(ReplacePreviewItem {
            path: path.clone(),
            line: line_idx + 1,
            column,
            before: line.to_string(),
            after,
        });

        if results.len() >= MAX_PREVIEW {
            break;
        }
    }

    let status = format!("{} preview item(s)", results.len());
    (results, status)
}

fn preview_find_matches(
    line: &str,
    needle: &str,
    options: buffer::SearchOptions,
) -> Vec<std::ops::Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }

    let bytes = line.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.len() > bytes.len() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut i = 0;
    while i + needle_bytes.len() <= bytes.len() {
        if !preview_matches_at(bytes, needle_bytes, i, options.match_case) {
            i += 1;
            continue;
        }

        if options.whole_word && !preview_is_word_boundary(bytes, i, needle_bytes.len()) {
            i += 1;
            continue;
        }

        matches.push(i..(i + needle_bytes.len()));
        i += needle_bytes.len().max(1);
    }

    matches
}

fn preview_matches_at(haystack: &[u8], needle: &[u8], start: usize, match_case: bool) -> bool {
    for (idx, &b) in needle.iter().enumerate() {
        let h = haystack[start + idx];
        if match_case {
            if h != b {
                return false;
            }
        } else if h.to_ascii_lowercase() != b.to_ascii_lowercase() {
            return false;
        }
    }
    true
}

fn preview_is_word_boundary(haystack: &[u8], start: usize, len: usize) -> bool {
    let left = start.checked_sub(1).map(|i| haystack[i]);
    let right = haystack.get(start + len).copied();

    let left_ok = left.map_or(true, |b| !preview_is_word_byte(b));
    let right_ok = right.map_or(true, |b| !preview_is_word_byte(b));

    left_ok && right_ok
}

fn preview_is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn replace_line_with_matches(
    line: &str,
    matches: &[std::ops::Range<usize>],
    replacement: &str,
) -> String {
    if matches.is_empty() {
        return line.to_string();
    }

    let mut out = String::new();
    let mut last = 0;
    for m in matches {
        out.push_str(&line[last..m.start]);
        out.push_str(replacement);
        last = m.end;
    }
    out.push_str(&line[last..]);
    out
}

pub fn draw_handle_save(ctx: &mut Context, state: &mut State) {
    if let Some(doc) = state.documents.active_mut() {
        if doc.path.is_some() {
            if let Err(err) = doc.save(None) {
                error_log_add(ctx, state, err);
            }
        } else {
            // No path? Show the file picker.
            state.wants_file_picker = StateFilePicker::SaveAs;
            state.wants_save = false;
            ctx.needs_rerender();
        }
    }

    state.wants_save = false;
}

pub fn draw_handle_wants_close(ctx: &mut Context, state: &mut State) {
    let Some(doc) = state.documents.active() else {
        state.wants_close = false;
        return;
    };

    if !doc.buffer.borrow().is_dirty() {
        state.documents.remove_active();
        state.wants_close = false;
        ctx.needs_rerender();
        return;
    }

    enum Action {
        None,
        Save,
        Discard,
        Cancel,
    }
    let mut action = Action::None;

    ctx.modal_begin("unsaved-changes", loc(LocId::UnsavedChangesDialogTitle));
    ctx.attr_background_rgba(ctx.indexed(IndexedColor::Red));
    ctx.attr_foreground_rgba(ctx.indexed(IndexedColor::BrightWhite));
    {
        let contains_focus = ctx.contains_focus();

        ctx.label("description", loc(LocId::UnsavedChangesDialogDescription));
        ctx.attr_padding(Rect::three(1, 2, 1));

        ctx.table_begin("choices");
        ctx.inherit_focus();
        ctx.attr_padding(Rect::three(0, 2, 1));
        ctx.attr_position(Position::Center);
        ctx.table_set_cell_gap(Size { width: 2, height: 0 });
        {
            ctx.table_next_row();
            ctx.inherit_focus();
            ctx.focus_on_first_present();

            if ctx.button(
                "yes",
                loc(LocId::UnsavedChangesDialogYes),
                ButtonStyle::default().accelerator('S'),
            ) {
                action = Action::Save;
            }
            ctx.inherit_focus();
            if ctx.button(
                "no",
                loc(LocId::UnsavedChangesDialogNo),
                ButtonStyle::default().accelerator('N'),
            ) {
                action = Action::Discard;
            }
            if ctx.button("cancel", loc(LocId::Cancel), ButtonStyle::default()) {
                action = Action::Cancel;
            }

            // Handle accelerator shortcuts
            if contains_focus {
                if ctx.consume_shortcut(vk::S) {
                    action = Action::Save;
                } else if ctx.consume_shortcut(vk::N) {
                    action = Action::Discard;
                }
            }
        }
        ctx.table_end();
    }
    if ctx.modal_end() {
        action = Action::Cancel;
    }

    match action {
        Action::None => return,
        Action::Save => {
            state.wants_save = true;
        }
        Action::Discard => {
            state.documents.remove_active();
            state.wants_close = false;
        }
        Action::Cancel => {
            state.wants_exit = false;
            state.wants_close = false;
        }
    }

    ctx.needs_rerender();
}

pub fn draw_goto_menu(ctx: &mut Context, state: &mut State) {
    let mut done = false;

    if let Some(doc) = state.documents.active_mut() {
        ctx.modal_begin("goto", loc(LocId::FileGoto));
        {
            if ctx.editline("goto-line", &mut state.goto_target) {
                state.goto_invalid = false;
            }
            if state.goto_invalid {
                ctx.attr_background_rgba(ctx.indexed(IndexedColor::Red));
                ctx.attr_foreground_rgba(ctx.indexed(IndexedColor::BrightWhite));
            }

            ctx.attr_intrinsic_size(Size { width: 24, height: 1 });
            ctx.steal_focus();

            if ctx.consume_shortcut(vk::RETURN) {
                match validate_goto_point(&state.goto_target) {
                    Ok(point) => {
                        let mut buf = doc.buffer.borrow_mut();
                        buf.cursor_move_to_logical(point);
                        buf.make_cursor_visible();
                        done = true;
                    }
                    Err(_) => state.goto_invalid = true,
                }
                ctx.needs_rerender();
            }
        }
        done |= ctx.modal_end();
    } else {
        done = true;
    }

    if done {
        state.wants_goto = false;
        state.goto_target.clear();
        state.goto_invalid = false;
        ctx.needs_rerender();
    }
}

fn validate_goto_point(line: &str) -> Result<Point, ParseIntError> {
    let mut coords = [0; 2];
    let (y, x) = line.split_once(':').unwrap_or((line, "0"));
    // Using a loop here avoids 2 copies of the str->int code.
    // This makes the binary more compact.
    for (i, s) in [x, y].iter().enumerate() {
        coords[i] = s.parse::<CoordType>()?.saturating_sub(1);
    }
    Ok(Point { x: coords[0], y: coords[1] })
}
