// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![feature(allocator_api, linked_list_cursors, string_from_utf8_lossy_owned)]

mod documents;
mod draw_editor;
mod draw_filepicker;
mod draw_menubar;
mod draw_statusbar;
mod config;
mod commands;
mod find_in_files;
mod localization;
mod state;

use std::borrow::Cow;
#[cfg(feature = "debug-latency")]
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, process};

use draw_editor::*;
use draw_filepicker::*;
use draw_menubar::*;
use draw_statusbar::*;
use edit::framebuffer::{self, IndexedColor};
use edit::helpers::{CoordType, KIBI, MEBI, MetricFormatter, Rect, Size};
use edit::input::{self, vk};
use edit::oklab::StraightRgba;
use edit::tui::*;
use edit::vt::{self, Token};
use edit::{apperr, base64, path, sys, unicode};
use localization::*;
use state::*;
use stdext::arena::{self, Arena, ArenaString, scratch_arena};
use stdext::arena_format;

#[cfg(target_pointer_width = "32")]
const SCRATCH_ARENA_CAPACITY: usize = 128 * MEBI;
#[cfg(target_pointer_width = "64")]
const SCRATCH_ARENA_CAPACITY: usize = 512 * MEBI;

fn main() -> process::ExitCode {
    if cfg!(debug_assertions) {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            drop(RestoreModes);
            drop(sys::Deinit);
            hook(info);
        }));
    }

    match run() {
        Ok(()) => process::ExitCode::SUCCESS,
        Err(err) => {
            sys::write_stdout(&format!("{}\n", FormatApperr::from(err)));
            process::ExitCode::FAILURE
        }
    }
}

fn run() -> apperr::Result<()> {
    // Init `sys` first, as everything else may depend on its functionality (IO, function pointers, etc.).
    let _sys_deinit = sys::init();
    // Next init `arena`, so that `scratch_arena` works. `loc` depends on it.
    arena::init(SCRATCH_ARENA_CAPACITY)?;
    // Init the `loc` module, so that error messages are localized.
    localization::init();

    let mut state = State::new()?;
    if handle_args(&mut state)? {
        return Ok(());
    }

    // This will reopen stdin if it's redirected (which may fail) and switch
    // the terminal to raw mode which prevents the user from pressing Ctrl+C.
    // `handle_args` may want to print a help message (must not fail),
    // and reads files (may hang; should be cancelable with Ctrl+C).
    // As such, we call this after `handle_args`.
    sys::switch_modes()?;

    let mut vt_parser = vt::Parser::new();
    let mut input_parser = input::Parser::new();
    let mut tui = Tui::new()?;

    let _restore = setup_terminal(&mut tui, &mut state, &mut vt_parser);

    apply_theme(&mut tui, &mut state);
    tui.setup_modifier_translations(ModifierTranslations {
        ctrl: loc(LocId::Ctrl),
        alt: loc(LocId::Alt),
        shift: loc(LocId::Shift),
    });

    sys::inject_window_size_into_stdin();

    #[cfg(feature = "debug-latency")]
    let mut last_latency_width = 0;

    loop {
        if state.needs_theme_refresh {
            apply_theme(&mut tui, &mut state);
        }
        #[cfg(feature = "debug-latency")]
        let time_beg;
        #[cfg(feature = "debug-latency")]
        let mut passes;

        // Process a batch of input.
        {
            let scratch = scratch_arena(None);
            let read_timeout = vt_parser.read_timeout().min(tui.read_timeout());
            let Some(input) = sys::read_stdin(&scratch, read_timeout) else {
                break;
            };

            #[cfg(feature = "debug-latency")]
            {
                time_beg = std::time::Instant::now();
                passes = 0usize;
            }

            let vt_iter = vt_parser.parse(&input);
            let mut input_iter = input_parser.parse(vt_iter);

            while {
                let input = input_iter.next();
                let more = input.is_some();
                let mut apply_theme_now = false;
                {
                    let mut ctx = tui.create_context(input);

                    draw(&mut ctx, &mut state);
                    if state.needs_theme_refresh {
                        ctx.needs_rerender();
                        apply_theme_now = true;
                    }

                    #[cfg(feature = "debug-latency")]
                    {
                        passes += 1;
                    }
                }
                if apply_theme_now {
                    apply_theme(&mut tui, &mut state);
                }

                more
            } {}
        }

        // Continue rendering until the layout has settled.
        // This can take >1 frame, if the input focus is tossed between different controls.
        while tui.needs_settling() {
            let mut apply_theme_now = false;
            {
                let mut ctx = tui.create_context(None);

                draw(&mut ctx, &mut state);
                if state.needs_theme_refresh {
                    ctx.needs_rerender();
                    apply_theme_now = true;
                }

                #[cfg(feature = "debug-latency")]
                {
                    passes += 1;
                }
            }
            if apply_theme_now {
                apply_theme(&mut tui, &mut state);
            }
        }

        if state.exit {
            break;
        }

        // Render the UI and write it to the terminal.
        {
            let scratch = scratch_arena(None);
            let mut output = tui.render(&scratch);

            write_terminal_title(&mut output, &mut state);

            if state.osc_clipboard_sync {
                write_osc_clipboard(&mut tui, &mut state, &mut output);
            }

            #[cfg(feature = "debug-latency")]
            {
                // Print the number of passes and latency in the top right corner.
                let time_end = std::time::Instant::now();
                let status = time_end - time_beg;

                let scratch_alt = scratch_arena(Some(&scratch));
                let status = arena_format!(
                    &scratch_alt,
                    "{}P {}B {:.3}μs",
                    passes,
                    output.len(),
                    status.as_nanos() as f64 / 1000.0
                );

                // "μs" is 3 bytes and 2 columns.
                let cols = status.len() as edit::helpers::CoordType - 3 + 2;

                // Since the status may shrink and grow, we may have to overwrite the previous one with whitespace.
                let padding = (last_latency_width - cols).max(0);

                // If the `output` is already very large,
                // Rust may double the size during the write below.
                // Let's avoid that by reserving the needed size in advance.
                output.reserve_exact(128);

                // To avoid moving the cursor, push and pop it onto the VT cursor stack.
                _ = write!(
                    output,
                    "\x1b7\x1b[0;41;97m\x1b[1;{0}H{1:2$}{3}\x1b8",
                    tui.size().width - cols - padding + 1,
                    "",
                    padding as usize,
                    status
                );

                last_latency_width = cols;
            }

            sys::write_stdout(&output);
        }
    }

    Ok(())
}

// Returns true if the application should exit early.
fn handle_args(state: &mut State) -> apperr::Result<bool> {
    let scratch = scratch_arena(None);
    let mut paths: Vec<PathBuf, &Arena> = Vec::new_in(&*scratch);
    let cwd = env::current_dir()?;
    let mut dir = None;
    let mut parse_args = true;

    // The best CLI argument parser in the world.
    for arg in env::args_os().skip(1) {
        if parse_args {
            if arg == "--" {
                parse_args = false;
                continue;
            }
            if arg == "-" {
                paths.clear();
                break;
            }
            if arg == "-h" || arg == "--help" || (cfg!(windows) && arg == "/?") {
                print_help();
                return Ok(true);
            }
            if arg == "-v" || arg == "--version" {
                print_version();
                return Ok(true);
            }
        }

        let p = cwd.join(Path::new(&arg));
        let p = path::normalize(&p);
        if p.is_dir() {
            state.wants_file_picker = StateFilePicker::Open;
            dir = Some(p);
        } else {
            paths.push(p);
        }
    }

    for p in &paths {
        match state.documents.add_file_path(p, &state.settings)? {
            documents::OpenOutcome::Opened => {
                state::push_recent_file(state, p.to_path_buf());
            }
            documents::OpenOutcome::BinaryDetected { path, goto } => {
                state.wants_binary_prompt = true;
                state.binary_prompt_path = Some(path);
                state.binary_prompt_goto = goto;
            }
        }
    }

    if let Some(mut file) = sys::open_stdin_if_redirected() {
        let doc = state.documents.add_untitled(&state.settings)?;
        let mut tb = doc.buffer.borrow_mut();
        tb.read_file(&mut file, None)?;
        tb.mark_as_dirty();
    } else if paths.is_empty() {
        // No files were passed, and stdin is not redirected.
        state.documents.add_untitled(&state.settings)?;
    }

    if dir.is_none()
        && let Some(parent) = paths.last().and_then(|p| p.parent())
    {
        dir = Some(parent.to_path_buf());
    }

    state.file_picker_pending_dir = DisplayablePathBuf::from_path(dir.unwrap_or(cwd));
    Ok(false)
}

fn print_help() {
    sys::write_stdout(concat!(
        "Usage: edit32 [OPTIONS] [FILE[:LINE[:COLUMN]]]\n",
        "Options:\n",
        "    -h, --help       Print this help message\n",
        "    -v, --version    Print the version number\n",
        "\n",
        "Arguments:\n",
        "    FILE[:LINE[:COLUMN]]    The file to open, optionally with line and column (e.g., foo.txt:123:45)\n",
    ));
}

fn print_version() {
    sys::write_stdout(concat!("edit32 version ", env!("CARGO_PKG_VERSION"), "\n"));
}

fn draw(ctx: &mut Context, state: &mut State) {
    // Draw menubar first so it can handle ESC to close menus
    draw_menubar(ctx, state);

    // Handle ESC to exit at top level, but only if menubar didn't consume it
    // (keyboard_input() returns None if input was consumed)
    if state.settings.escape_to_exit
        && ctx.keyboard_input() == Some(vk::ESCAPE)
        && is_at_top_level(state)
    {
        state.wants_exit = true;
        ctx.set_input_consumed();
        ctx.needs_rerender();
        return;
    }
    draw_editor(ctx, state);
    draw_statusbar(ctx, state);

    if state.wants_close {
        draw_handle_wants_close(ctx, state);
    }
    if state.wants_exit {
        draw_handle_wants_exit(ctx, state);
    }
    if state.wants_goto {
        draw_goto_menu(ctx, state);
    }
    if state.wants_file_picker != StateFilePicker::None {
        draw_file_picker(ctx, state);
    }
    if state.wants_save {
        draw_handle_save(ctx, state);
    }
    if state.wants_encoding_change != StateEncodingChange::None {
        draw_dialog_encoding_change(ctx, state);
    }
    if state.wants_go_to_file {
        draw_go_to_file(ctx, state);
    }
    if state.wants_find_in_files {
        draw_find_in_files(ctx, state);
    }
    if state.wants_command_palette {
        draw_command_palette(ctx, state);
    }
    if state.wants_quick_switcher {
        draw_quick_switcher(ctx, state);
    }
    if state.wants_theme_picker {
        draw_theme_picker(ctx, state);
    }
    if state.wants_recent_files {
        draw_menubar::draw_recent_files(ctx, state);
    }
    if state.wants_keybinding_editor {
        draw_keybinding_editor(ctx, state);
    }
    if state.wants_about {
        draw_dialog_about(ctx, state);
    }
    if state.wants_context_help {
        draw_context_help(ctx, state);
    }
    if state.wants_quick_start {
        draw_quick_start(ctx, state);
    }
    if state.wants_binary_prompt {
        draw_binary_prompt(ctx, state);
    }
    if state.wants_replace_preview {
        draw_replace_preview(ctx, state);
    }
    if ctx.clipboard_ref().wants_host_sync() {
        draw_handle_clipboard_change(ctx, state);
    }
    if state.error_log_count != 0 {
        draw_error_log(ctx, state);
    }

    if let Some(key) = ctx.keyboard_input() {
        // Shortcuts that are not handled as part of the textarea, etc.

        if key == state.keybindings.shortcut(commands::CommandId::FileNew) {
            commands::run_command(ctx, state, commands::CommandId::FileNew);
        } else if key == state.keybindings.shortcut(commands::CommandId::FileOpen) {
            commands::run_command(ctx, state, commands::CommandId::FileOpen);
        } else if key == state.keybindings.shortcut(commands::CommandId::FileSave) {
            commands::run_command(ctx, state, commands::CommandId::FileSave);
        } else if key == state.keybindings.shortcut(commands::CommandId::FileSaveAs) {
            commands::run_command(ctx, state, commands::CommandId::FileSaveAs);
        } else if key == state.keybindings.shortcut(commands::CommandId::FileClose) {
            commands::run_command(ctx, state, commands::CommandId::FileClose);
        } else if key == state.keybindings.shortcut(commands::CommandId::ViewGoToFile) {
            commands::run_command(ctx, state, commands::CommandId::ViewGoToFile);
        } else if key == state.keybindings.shortcut(commands::CommandId::FileExit) {
            commands::run_command(ctx, state, commands::CommandId::FileExit);
        } else if key == state.keybindings.shortcut(commands::CommandId::FileGoto) {
            commands::run_command(ctx, state, commands::CommandId::FileGoto);
        } else if key == state.keybindings.shortcut(commands::CommandId::EditFind)
            && state.wants_search.kind != StateSearchKind::Disabled
        {
            commands::run_command(ctx, state, commands::CommandId::EditFind);
        } else if key == state.keybindings.shortcut(commands::CommandId::EditReplace)
            && state.wants_search.kind != StateSearchKind::Disabled
        {
            commands::run_command(ctx, state, commands::CommandId::EditReplace);
        } else if key == state.keybindings.shortcut(commands::CommandId::FindInFiles) {
            commands::run_command(ctx, state, commands::CommandId::FindInFiles);
        } else if key == state.keybindings.shortcut(commands::CommandId::EditDuplicateLine) {
            commands::run_command(ctx, state, commands::CommandId::EditDuplicateLine);
        } else if key == state.keybindings.shortcut(commands::CommandId::EditDeleteLine) {
            commands::run_command(ctx, state, commands::CommandId::EditDeleteLine);
        } else if key == state.keybindings.shortcut(commands::CommandId::EditJoinLines) {
            commands::run_command(ctx, state, commands::CommandId::EditJoinLines);
        } else if key == state.keybindings.shortcut(commands::CommandId::EditGotoMatchingBracket) {
            commands::run_command(ctx, state, commands::CommandId::EditGotoMatchingBracket);
        } else if key == state.keybindings.shortcut(commands::CommandId::CommandPalette) {
            commands::run_command(ctx, state, commands::CommandId::CommandPalette);
        } else if key == state.keybindings.shortcut(commands::CommandId::ThemePicker) {
            commands::run_command(ctx, state, commands::CommandId::ThemePicker);
        } else if key == state.keybindings.shortcut(commands::CommandId::QuickSwitcher) {
            commands::run_command(ctx, state, commands::CommandId::QuickSwitcher);
        } else if key == state.keybindings.shortcut(commands::CommandId::SettingsOpenConfig) {
            commands::run_command(ctx, state, commands::CommandId::SettingsOpenConfig);
        } else if key == state.keybindings.shortcut(commands::CommandId::SettingsReload) {
            commands::run_command(ctx, state, commands::CommandId::SettingsReload);
        } else if key == vk::F3 {
            search_execute(ctx, state, SearchAction::Search);
        } else {
            return;
        }

        // All of the above shortcuts happen to require a rerender.
        ctx.needs_rerender();
        ctx.set_input_consumed();
    }
}

/// Returns true if no dialogs, modals, or overlays are open.
fn is_at_top_level(state: &State) -> bool {
    !state.wants_close
        && !state.wants_exit
        && !state.wants_goto
        && state.wants_file_picker == StateFilePicker::None
        && !state.wants_save
        && state.wants_encoding_change == StateEncodingChange::None
        && !state.wants_go_to_file
        && !state.wants_find_in_files
        && !state.wants_command_palette
        && !state.wants_quick_switcher
        && !state.wants_theme_picker
        && !state.wants_recent_files
        && !state.wants_keybinding_editor
        && !state.wants_about
        && !state.wants_context_help
        && !state.wants_quick_start
        && !state.wants_binary_prompt
        && !state.wants_replace_preview
        && state.error_log_count == 0
        && state.wants_search.kind == StateSearchKind::Hidden
}

fn draw_handle_wants_exit(_ctx: &mut Context, state: &mut State) {
    while let Some(doc) = state.documents.active() {
        if doc.buffer.borrow().is_dirty() {
            state.wants_close = true;
            return;
        }
        state.documents.remove_active();
    }

    if state.documents.len() == 0 {
        state.exit = true;
    }
}

fn write_terminal_title(output: &mut ArenaString, state: &mut State) {
    let (filename, dirty) = state
        .documents
        .active()
        .map_or(("", false), |d| (&d.filename, d.buffer.borrow().is_dirty()));

    if filename == state.osc_title_file_status.filename
        && dirty == state.osc_title_file_status.dirty
    {
        return;
    }

    output.push_str("\x1b]0;");
    if !filename.is_empty() {
        if dirty {
            output.push_str("● ");
        }
        output.push_str(&sanitize_control_chars(filename));
        output.push_str(" - ");
    }
    output.push_str("Edit32\x1b\\");

    state.osc_title_file_status.filename = filename.to_string();
    state.osc_title_file_status.dirty = dirty;
}

const LARGE_CLIPBOARD_THRESHOLD: usize = 128 * KIBI;

fn apply_theme(tui: &mut Tui, state: &mut State) {
    if state.settings.theme == config::ThemeId::Terminal {
        tui.setup_indexed_colors(state.terminal_palette);
    } else if state.settings.theme == config::ThemeId::Custom {
        if let Some(palette) = state.settings.custom_theme {
            tui.setup_indexed_colors(palette);
        } else {
            tui.setup_indexed_colors(state.terminal_palette);
        }
    } else if let Some(palette) = theme_palette(state.settings.theme) {
        tui.setup_indexed_colors(palette);
    }

    if state.settings.high_contrast {
        state.menubar_color_bg = tui.indexed(IndexedColor::Black);
        state.menubar_color_fg = tui.indexed(IndexedColor::BrightWhite);
        let floater_bg = tui.indexed(IndexedColor::Black);
        let floater_fg = tui.indexed(IndexedColor::BrightWhite);
        tui.set_floater_default_bg(floater_bg);
        tui.set_floater_default_fg(floater_fg);
        tui.set_modal_default_bg(floater_bg);
        tui.set_modal_default_fg(floater_fg);

        state.editor_color_bg = tui.indexed(IndexedColor::Black);
        state.editor_color_fg = tui.indexed(IndexedColor::BrightWhite);
    } else {
        state.menubar_color_bg =
            tui.indexed(IndexedColor::Background).oklab_blend(tui.indexed_alpha(
                IndexedColor::BrightBlue,
                1,
                2,
            ));
        state.menubar_color_fg = tui.contrasted(state.menubar_color_bg);
        let floater_bg = tui
            .indexed_alpha(IndexedColor::Background, 2, 3)
            .oklab_blend(tui.indexed_alpha(IndexedColor::Foreground, 1, 3));
        let floater_fg = tui.contrasted(floater_bg);
        tui.set_floater_default_bg(floater_bg);
        tui.set_floater_default_fg(floater_fg);
        tui.set_modal_default_bg(floater_bg);
        tui.set_modal_default_fg(floater_fg);

        if state.settings.theme == config::ThemeId::Terminal {
            state.editor_color_bg = tui.indexed(IndexedColor::White);
            state.editor_color_fg = tui.indexed(IndexedColor::Black);
        } else {
            state.editor_color_bg = tui.indexed(IndexedColor::Background);
            state.editor_color_fg = tui.indexed(IndexedColor::Foreground);
        }
    }
    state.needs_theme_refresh = false;
}

fn theme_palette(
    theme: config::ThemeId,
) -> Option<[StraightRgba; framebuffer::INDEXED_COLORS_COUNT]> {
    use config::ThemeId;

    match theme {
        ThemeId::Terminal | ThemeId::Custom => None,
        ThemeId::Nord => Some([
            StraightRgba::from_be(0x3b4252ff), // Black
            StraightRgba::from_be(0xbf616aff), // Red
            StraightRgba::from_be(0xa3be8cff), // Green
            StraightRgba::from_be(0xebcb8bff), // Yellow
            StraightRgba::from_be(0x81a1c1ff), // Blue
            StraightRgba::from_be(0xb48eadff), // Magenta
            StraightRgba::from_be(0x88c0d0ff), // Cyan
            StraightRgba::from_be(0xe5e9f0ff), // White
            StraightRgba::from_be(0x4c566aff), // BrightBlack
            StraightRgba::from_be(0xd57780ff), // BrightRed
            StraightRgba::from_be(0xb7d39aff), // BrightGreen
            StraightRgba::from_be(0xf0d399ff), // BrightYellow
            StraightRgba::from_be(0x8fafd1ff), // BrightBlue
            StraightRgba::from_be(0xc49bbdff), // BrightMagenta
            StraightRgba::from_be(0x97d0e0ff), // BrightCyan
            StraightRgba::from_be(0xeceff4ff), // BrightWhite
            StraightRgba::from_be(0x2e3440ff), // Background
            StraightRgba::from_be(0xd8dee9ff), // Foreground
        ]),
        ThemeId::OneDark => Some([
            StraightRgba::from_be(0x282c34ff), // Black
            StraightRgba::from_be(0xe06c75ff), // Red
            StraightRgba::from_be(0x98c379ff), // Green
            StraightRgba::from_be(0xe5c07bff), // Yellow
            StraightRgba::from_be(0x61afefff), // Blue
            StraightRgba::from_be(0xc678ddff), // Magenta
            StraightRgba::from_be(0x56b6c2ff), // Cyan
            StraightRgba::from_be(0xabb2bfff), // White
            StraightRgba::from_be(0x5c6370ff), // BrightBlack
            StraightRgba::from_be(0xff7b86ff), // BrightRed
            StraightRgba::from_be(0xb6f28aff), // BrightGreen
            StraightRgba::from_be(0xffd68aff), // BrightYellow
            StraightRgba::from_be(0x78bdfbff), // BrightBlue
            StraightRgba::from_be(0xd7a1f0ff), // BrightMagenta
            StraightRgba::from_be(0x70d9e3ff), // BrightCyan
            StraightRgba::from_be(0xe6ebf2ff), // BrightWhite
            StraightRgba::from_be(0x282c34ff), // Background
            StraightRgba::from_be(0xabb2bfff), // Foreground
        ]),
        ThemeId::Gruvbox => Some([
            StraightRgba::from_be(0x282828ff), // Black
            StraightRgba::from_be(0xcc241dff), // Red
            StraightRgba::from_be(0x98971aff), // Green
            StraightRgba::from_be(0xd79921ff), // Yellow
            StraightRgba::from_be(0x458588ff), // Blue
            StraightRgba::from_be(0xb16286ff), // Magenta
            StraightRgba::from_be(0x689d6aff), // Cyan
            StraightRgba::from_be(0xa89984ff), // White
            StraightRgba::from_be(0x928374ff), // BrightBlack
            StraightRgba::from_be(0xfb4934ff), // BrightRed
            StraightRgba::from_be(0xb8bb26ff), // BrightGreen
            StraightRgba::from_be(0xfabd2fff), // BrightYellow
            StraightRgba::from_be(0x83a598ff), // BrightBlue
            StraightRgba::from_be(0xd3869bff), // BrightMagenta
            StraightRgba::from_be(0x8ec07cff), // BrightCyan
            StraightRgba::from_be(0xebdbb2ff), // BrightWhite
            StraightRgba::from_be(0x282828ff), // Background
            StraightRgba::from_be(0xebdbb2ff), // Foreground
        ]),
        ThemeId::Monokai => Some([
            StraightRgba::from_be(0x272822ff), // Black
            StraightRgba::from_be(0xf92672ff), // Red
            StraightRgba::from_be(0xa6e22eff), // Green
            StraightRgba::from_be(0xe6db74ff), // Yellow
            StraightRgba::from_be(0x66d9efff), // Blue
            StraightRgba::from_be(0xae81ffff), // Magenta
            StraightRgba::from_be(0xa1efe4ff), // Cyan
            StraightRgba::from_be(0xf8f8f2ff), // White
            StraightRgba::from_be(0x75715eff), // BrightBlack
            StraightRgba::from_be(0xff6188ff), // BrightRed
            StraightRgba::from_be(0xb9f27cff), // BrightGreen
            StraightRgba::from_be(0xfff4a3ff), // BrightYellow
            StraightRgba::from_be(0x78e6ffff), // BrightBlue
            StraightRgba::from_be(0xc7a4ffff), // BrightMagenta
            StraightRgba::from_be(0x8fe3d5ff), // BrightCyan
            StraightRgba::from_be(0xffffffff), // BrightWhite
            StraightRgba::from_be(0x272822ff), // Background
            StraightRgba::from_be(0xf8f8f2ff), // Foreground
        ]),
        ThemeId::SolarizedDark => Some([
            StraightRgba::from_be(0x073642ff), // Black
            StraightRgba::from_be(0xdc322fff), // Red
            StraightRgba::from_be(0x859900ff), // Green
            StraightRgba::from_be(0xb58900ff), // Yellow
            StraightRgba::from_be(0x268bd2ff), // Blue
            StraightRgba::from_be(0xd33682ff), // Magenta
            StraightRgba::from_be(0x2aa198ff), // Cyan
            StraightRgba::from_be(0xeee8d5ff), // White
            StraightRgba::from_be(0x002b36ff), // BrightBlack
            StraightRgba::from_be(0xcb4b16ff), // BrightRed
            StraightRgba::from_be(0x586e75ff), // BrightGreen
            StraightRgba::from_be(0x657b83ff), // BrightYellow
            StraightRgba::from_be(0x839496ff), // BrightBlue
            StraightRgba::from_be(0x6c71c4ff), // BrightMagenta
            StraightRgba::from_be(0x93a1a1ff), // BrightCyan
            StraightRgba::from_be(0xfdf6e3ff), // BrightWhite
            StraightRgba::from_be(0x002b36ff), // Background
            StraightRgba::from_be(0x839496ff), // Foreground
        ]),
        ThemeId::SolarizedLight => Some([
            StraightRgba::from_be(0x073642ff), // Black
            StraightRgba::from_be(0xdc322fff), // Red
            StraightRgba::from_be(0x859900ff), // Green
            StraightRgba::from_be(0xb58900ff), // Yellow
            StraightRgba::from_be(0x268bd2ff), // Blue
            StraightRgba::from_be(0xd33682ff), // Magenta
            StraightRgba::from_be(0x2aa198ff), // Cyan
            StraightRgba::from_be(0xeee8d5ff), // White
            StraightRgba::from_be(0x002b36ff), // BrightBlack
            StraightRgba::from_be(0xcb4b16ff), // BrightRed
            StraightRgba::from_be(0x586e75ff), // BrightGreen
            StraightRgba::from_be(0x657b83ff), // BrightYellow
            StraightRgba::from_be(0x839496ff), // BrightBlue
            StraightRgba::from_be(0x6c71c4ff), // BrightMagenta
            StraightRgba::from_be(0x93a1a1ff), // BrightCyan
            StraightRgba::from_be(0xfdf6e3ff), // BrightWhite
            StraightRgba::from_be(0xfdf6e3ff), // Background
            StraightRgba::from_be(0x657b83ff), // Foreground
        ]),
        ThemeId::Dracula => Some([
            StraightRgba::from_be(0x21222cff), // Black
            StraightRgba::from_be(0xff5555ff), // Red
            StraightRgba::from_be(0x50fa7bff), // Green
            StraightRgba::from_be(0xf1fa8cff), // Yellow
            StraightRgba::from_be(0xbd93f9ff), // Blue
            StraightRgba::from_be(0xff79c6ff), // Magenta
            StraightRgba::from_be(0x8be9fdff), // Cyan
            StraightRgba::from_be(0xf8f8f2ff), // White
            StraightRgba::from_be(0x6272a4ff), // BrightBlack
            StraightRgba::from_be(0xff6e6eff), // BrightRed
            StraightRgba::from_be(0x69ff94ff), // BrightGreen
            StraightRgba::from_be(0xffffa5ff), // BrightYellow
            StraightRgba::from_be(0xd6acffff), // BrightBlue
            StraightRgba::from_be(0xff92dfff), // BrightMagenta
            StraightRgba::from_be(0xa4ffffff), // BrightCyan
            StraightRgba::from_be(0xffffffff), // BrightWhite
            StraightRgba::from_be(0x282a36ff), // Background
            StraightRgba::from_be(0xf8f8f2ff), // Foreground
        ]),
        ThemeId::TokyoNight => Some([
            StraightRgba::from_be(0x15161eff), // Black
            StraightRgba::from_be(0xf7768eff), // Red
            StraightRgba::from_be(0x9ece6aff), // Green
            StraightRgba::from_be(0xe0af68ff), // Yellow
            StraightRgba::from_be(0x7aa2f7ff), // Blue
            StraightRgba::from_be(0xbb9af7ff), // Magenta
            StraightRgba::from_be(0x7dcfffff), // Cyan
            StraightRgba::from_be(0xa9b1d6ff), // White
            StraightRgba::from_be(0x414868ff), // BrightBlack
            StraightRgba::from_be(0xff7a93ff), // BrightRed
            StraightRgba::from_be(0xb9f27cff), // BrightGreen
            StraightRgba::from_be(0xff9e64ff), // BrightYellow
            StraightRgba::from_be(0x7da6ffff), // BrightBlue
            StraightRgba::from_be(0xc8a2ffff), // BrightMagenta
            StraightRgba::from_be(0x9aa5ceff), // BrightCyan
            StraightRgba::from_be(0xc0caf5ff), // BrightWhite
            StraightRgba::from_be(0x1a1b26ff), // Background
            StraightRgba::from_be(0xc0caf5ff), // Foreground
        ]),
        ThemeId::Midnight => Some([
            StraightRgba::from_be(0x000000ff), // Black
            StraightRgba::from_be(0xff2b2bff), // Red
            StraightRgba::from_be(0x22ff88ff), // Green
            StraightRgba::from_be(0xffd400ff), // Yellow
            StraightRgba::from_be(0x2f8bffff), // Blue
            StraightRgba::from_be(0xc08bffff), // Magenta
            StraightRgba::from_be(0x00f0ffff), // Cyan
            StraightRgba::from_be(0xf0f2f6ff), // White
            StraightRgba::from_be(0x20232bff), // BrightBlack
            StraightRgba::from_be(0xff5b5bff), // BrightRed
            StraightRgba::from_be(0x4dffa3ff), // BrightGreen
            StraightRgba::from_be(0xffe866ff), // BrightYellow
            StraightRgba::from_be(0x62b1ffff), // BrightBlue
            StraightRgba::from_be(0xd1adffff), // BrightMagenta
            StraightRgba::from_be(0x5af6ffff), // BrightCyan
            StraightRgba::from_be(0xffffffff), // BrightWhite
            StraightRgba::from_be(0x020203ff), // Background
            StraightRgba::from_be(0xf8f9fbff), // Foreground
        ]),
        ThemeId::Paperwhite => Some([
            StraightRgba::from_be(0x22201bff), // Black
            StraightRgba::from_be(0x9a1f2bff), // Red
            StraightRgba::from_be(0x1a7b3bff), // Green
            StraightRgba::from_be(0xa86a10ff), // Yellow
            StraightRgba::from_be(0x1c5fa8ff), // Blue
            StraightRgba::from_be(0x7a2a9bff), // Magenta
            StraightRgba::from_be(0x1e7c8cff), // Cyan
            StraightRgba::from_be(0xeae6dcff), // White
            StraightRgba::from_be(0x5a5a5aff), // BrightBlack
            StraightRgba::from_be(0xc23b4bff), // BrightRed
            StraightRgba::from_be(0x3f9a5aff), // BrightGreen
            StraightRgba::from_be(0xd59a3aff), // BrightYellow
            StraightRgba::from_be(0x3a7fc4ff), // BrightBlue
            StraightRgba::from_be(0xa066d6ff), // BrightMagenta
            StraightRgba::from_be(0x3bb0bbff), // BrightCyan
            StraightRgba::from_be(0xfaf8f2ff), // BrightWhite
            StraightRgba::from_be(0xf3f1eaff), // Background
            StraightRgba::from_be(0x22201bff), // Foreground
        ]),
    }
}
fn draw_handle_clipboard_change(ctx: &mut Context, state: &mut State) {
    let data_len = ctx.clipboard_ref().read().len();

    if state.osc_clipboard_always_send || data_len < LARGE_CLIPBOARD_THRESHOLD {
        ctx.clipboard_mut().mark_as_synchronized();
        state.osc_clipboard_sync = true;
        return;
    }

    let over_limit = data_len >= SCRATCH_ARENA_CAPACITY / 4;
    let mut done = None;

    ctx.modal_begin("warning", loc(LocId::WarningDialogTitle));
    {
        ctx.block_begin("description");
        ctx.attr_padding(Rect::three(1, 2, 1));

        if over_limit {
            ctx.label("line1", loc(LocId::LargeClipboardWarningLine1));
            ctx.attr_position(Position::Center);
            ctx.label("line2", loc(LocId::SuperLargeClipboardWarning));
            ctx.attr_position(Position::Center);
        } else {
            let label2 = {
                let template = loc(LocId::LargeClipboardWarningLine2);
                let size = arena_format!(ctx.arena(), "{}", MetricFormatter(data_len));

                let mut label =
                    ArenaString::with_capacity_in(template.len() + size.len(), ctx.arena());
                label.push_str(template);
                label.replace_once_in_place("{size}", &size);
                label
            };

            ctx.label("line1", loc(LocId::LargeClipboardWarningLine1));
            ctx.attr_position(Position::Center);
            ctx.label("line2", &label2);
            ctx.attr_position(Position::Center);
            ctx.label("line3", loc(LocId::LargeClipboardWarningLine3));
            ctx.attr_position(Position::Center);
        }
        ctx.block_end();

        ctx.table_begin("choices");
        ctx.inherit_focus();
        ctx.attr_padding(Rect::three(0, 2, 1));
        ctx.attr_position(Position::Center);
        ctx.table_set_cell_gap(Size { width: 2, height: 0 });
        {
            ctx.table_next_row();
            ctx.inherit_focus();
            ctx.focus_on_first_present();

            if over_limit {
                if ctx.button("ok", loc(LocId::Ok), ButtonStyle::default()) {
                    done = Some(true);
                }
                ctx.inherit_focus();
            } else {
                if ctx.button("always", loc(LocId::Always), ButtonStyle::default()) {
                    state.osc_clipboard_always_send = true;
                    done = Some(true);
                }

                if ctx.button("yes", loc(LocId::Yes), ButtonStyle::default()) {
                    done = Some(true);
                }
                if data_len < 10 * LARGE_CLIPBOARD_THRESHOLD {
                    ctx.inherit_focus();
                }

                if ctx.button("no", loc(LocId::No), ButtonStyle::default()) {
                    done = Some(false);
                }
                if data_len >= 10 * LARGE_CLIPBOARD_THRESHOLD {
                    ctx.inherit_focus();
                }
            }
        }
        ctx.table_end();
    }
    if ctx.modal_end() {
        done = Some(false);
    }

    if let Some(sync) = done {
        state.osc_clipboard_sync = sync;
        ctx.clipboard_mut().mark_as_synchronized();
        ctx.needs_rerender();
    }
}

#[cold]
fn write_osc_clipboard(tui: &mut Tui, state: &mut State, output: &mut ArenaString) {
    let clipboard = tui.clipboard_mut();
    let data = clipboard.read();

    if !data.is_empty() {
        // Rust doubles the size of a string when it needs to grow it.
        // If `data` is *really* large, this may then double
        // the size of the `output` from e.g. 100MB to 200MB. Not good.
        // We can avoid that by reserving the needed size in advance.
        output.reserve_exact(base64::encode_len(data.len()) + 16);
        output.push_str("\x1b]52;c;");
        base64::encode(output, data);
        output.push_str("\x1b\\");
    }

    state.osc_clipboard_sync = false;
}

struct RestoreModes;

impl Drop for RestoreModes {
    fn drop(&mut self) {
        // Same as in the beginning but in the reverse order.
        // It also includes DECSCUSR 0 to reset the cursor style and DECTCEM to show the cursor.
        // We specifically don't reset mode 1036, because most applications expect it to be set nowadays.
        sys::write_stdout("\x1b[0 q\x1b[?25h\x1b]0;\x07\x1b[?1002;1006;2004l\x1b[?1049l");
    }
}

fn setup_terminal(tui: &mut Tui, state: &mut State, vt_parser: &mut vt::Parser) -> RestoreModes {
    sys::write_stdout(concat!(
        // 1049: Alternative Screen Buffer
        //   I put the ASB switch in the beginning, just in case the terminal performs
        //   some additional state tracking beyond the modes we enable/disable.
        // 1002: Cell Motion Mouse Tracking
        // 1006: SGR Mouse Mode
        // 2004: Bracketed Paste Mode
        // 1036: Xterm: "meta sends escape" (Alt keypresses should be encoded with ESC + char)
        "\x1b[?1049h\x1b[?1002;1006;2004h\x1b[?1036h",
        // OSC 4 color table requests for indices 0 through 15 (base colors).
        "\x1b]4;0;?;1;?;2;?;3;?;4;?;5;?;6;?;7;?\x07",
        "\x1b]4;8;?;9;?;10;?;11;?;12;?;13;?;14;?;15;?\x07",
        // OSC 10 and 11 queries for the current foreground and background colors.
        "\x1b]10;?\x07\x1b]11;?\x07",
        // Test whether ambiguous width characters are two columns wide.
        // We use "…", because it's the most common ambiguous width character we use,
        // and the old Windows conhost doesn't actually use wcwidth, it measures the
        // actual display width of the character and assigns it columns accordingly.
        // We detect it by writing the character and asking for the cursor position.
        "\r…\x1b[6n",
        // CSI c reports the terminal capabilities.
        // It also helps us to detect the end of the responses, because not all
        // terminals support the OSC queries, but all of them support CSI c.
        "\x1b[c",
    ));

    let mut done = false;
    let mut osc_buffer = String::new();
    let mut indexed_colors = framebuffer::DEFAULT_THEME;
    let mut color_responses = 0;
    let mut ambiguous_width = 1;

    while !done {
        let scratch = scratch_arena(None);

        // We explicitly set a high read timeout, because we're not
        // waiting for user keyboard input. If we encounter a lone ESC,
        // it's unlikely to be from a ESC keypress, but rather from a VT sequence.
        let Some(input) = sys::read_stdin(&scratch, Duration::from_secs(3)) else {
            break;
        };

        let mut vt_stream = vt_parser.parse(&input);
        while let Some(token) = vt_stream.next() {
            match token {
                Token::Csi(csi) => match csi.final_byte {
                    'c' => done = true,
                    // CPR (Cursor Position Report) response.
                    'R' => ambiguous_width = csi.params[1] as CoordType - 1,
                    _ => {}
                },
                Token::Osc { mut data, partial } => {
                    if partial {
                        osc_buffer.push_str(data);
                        continue;
                    }
                    if !osc_buffer.is_empty() {
                        osc_buffer.push_str(data);
                        data = &osc_buffer;
                    }

                    let mut splits = data.split_terminator(';');

                    let color = match splits.next().unwrap_or("") {
                        // The response is `4;<color>;rgb:<r>/<g>/<b>`.
                        "4" => match splits.next().unwrap_or("").parse::<usize>() {
                            Ok(val) if val < 16 => &mut indexed_colors[val],
                            _ => continue,
                        },
                        // The response is `10;rgb:<r>/<g>/<b>`.
                        "10" => &mut indexed_colors[IndexedColor::Foreground as usize],
                        // The response is `11;rgb:<r>/<g>/<b>`.
                        "11" => &mut indexed_colors[IndexedColor::Background as usize],
                        _ => continue,
                    };

                    let color_param = splits.next().unwrap_or("");
                    if !color_param.starts_with("rgb:") {
                        continue;
                    }

                    let mut iter = color_param[4..].split_terminator('/');
                    let rgb_parts = [(); 3].map(|_| iter.next().unwrap_or("0"));
                    let mut rgb = 0;

                    for part in rgb_parts {
                        if part.len() == 2 || part.len() == 4 {
                            let Ok(mut val) = usize::from_str_radix(part, 16) else {
                                continue;
                            };
                            if part.len() == 4 {
                                // Round from 16 bits to 8 bits.
                                val = (val * 0xff + 0x7fff) / 0xffff;
                            }
                            rgb = (rgb >> 8) | ((val as u32) << 16);
                        }
                    }

                    *color = StraightRgba::from_le(rgb | 0xff000000);
                    color_responses += 1;
                    osc_buffer.clear();
                }
                _ => {}
            }
        }
    }

    if ambiguous_width == 2 {
        unicode::setup_ambiguous_width(2);
        state.documents.reflow_all();
    }

    if color_responses == indexed_colors.len() {
        tui.setup_indexed_colors(indexed_colors);
    }
    state.terminal_palette = indexed_colors;

    RestoreModes
}

fn draw_binary_prompt(ctx: &mut Context, state: &mut State) {
    let mut open_hex = false;
    let mut done = false;

    ctx.modal_begin("binary", "Binary File Detected");
    {
        ctx.block_begin("content");
        ctx.attr_padding(Rect::three(1, 2, 1));
        ctx.label("line1", "This file appears to be binary.");
        ctx.attr_position(Position::Center);
        ctx.label("line2", "Open it in hex view?");
        ctx.attr_position(Position::Center);
        ctx.block_end();

        ctx.table_begin("choices");
        ctx.inherit_focus();
        ctx.attr_padding(Rect::three(0, 2, 1));
        ctx.attr_position(Position::Center);
        ctx.table_set_cell_gap(Size { width: 2, height: 0 });
        {
            ctx.table_next_row();
            ctx.inherit_focus();
            ctx.focus_on_first_present();

            if ctx.button("hex", "Open Hex", ButtonStyle::default()) {
                open_hex = true;
                done = true;
            }
            ctx.inherit_focus();
            if ctx.button("cancel", "Cancel", ButtonStyle::default()) {
                done = true;
            }
        }
        ctx.table_end();
    }
    if ctx.modal_end() {
        done = true;
    }

    if done {
        if open_hex {
            if let Some(path) = state.binary_prompt_path.take() {
                if let Err(err) = state.documents.add_file_path_hex(&path, &state.settings) {
                    error_log_add(ctx, state, err);
                } else {
                    state::push_recent_file(state, path);
                }
            }
        } else {
            state.binary_prompt_path = None;
        }
        state.binary_prompt_goto = None;
        state.wants_binary_prompt = false;
        ctx.needs_rerender();
    }
}

fn draw_context_help(ctx: &mut Context, state: &mut State) {
    let mut done = false;

    ctx.modal_begin("context-help", "Context Help");
    {
        ctx.block_begin("content");
        ctx.attr_padding(Rect::three(1, 2, 1));
        for (idx, line) in context_help_lines(state).iter().enumerate() {
            ctx.next_block_id_mixin(idx as u64);
            ctx.label("line", line);
            ctx.attr_overflow(Overflow::TruncateTail);
        }
        ctx.block_end();

        if ctx.button("ok", loc(LocId::Ok), ButtonStyle::default()) {
            done = true;
        }
        ctx.attr_position(Position::Center);
        ctx.focus_on_first_present();
    }
    if ctx.modal_end() {
        done = true;
    }

    if done {
        state.wants_context_help = false;
        ctx.needs_rerender();
    }
}

fn context_help_lines(state: &State) -> &'static [&'static str] {
    if state.wants_file_picker != StateFilePicker::None {
        return &[
            "File picker:",
            "- Enter to open, Esc to cancel",
            "- Type to filter or use arrows to navigate",
            "- Alt+Up to go to parent folder",
        ];
    }
    if state.wants_search.kind != StateSearchKind::Hidden {
        return &[
            "Search:",
            "- Enter to search, Esc to close",
            "- Ctrl+R to switch to Replace",
            "- F3 to find next",
        ];
    }
    if state.wants_quick_switcher {
        return &[
            "Quick Switcher:",
            "- Type to filter, Enter to open",
            "- Use folder: prefix for recent folders",
        ];
    }
    if state.wants_keybinding_editor {
        return &[
            "Keybindings:",
            "- Enter to rebind selected command",
            "- Press Esc to cancel capture",
        ];
    }
    &[
        "General:",
        "- F1: Command Palette",
        "- Ctrl+O: Open file",
        "- Ctrl+Shift+O: Open folder",
    ]
}

fn draw_quick_start(ctx: &mut Context, state: &mut State) {
    let mut done = false;

    ctx.modal_begin("quick-start", "Quick Start");
    {
        ctx.block_begin("content");
        ctx.attr_padding(Rect::three(1, 2, 1));
        for (idx, line) in quick_start_lines().iter().enumerate() {
            ctx.next_block_id_mixin(idx as u64);
            ctx.label("line", line);
            ctx.attr_overflow(Overflow::TruncateTail);
        }
        ctx.block_end();

        if ctx.button("ok", loc(LocId::Ok), ButtonStyle::default()) {
            done = true;
        }
        ctx.attr_position(Position::Center);
        ctx.focus_on_first_present();
    }
    if ctx.modal_end() {
        done = true;
    }

    if done {
        state.wants_quick_start = false;
        ctx.needs_rerender();
    }
}

fn quick_start_lines() -> &'static [&'static str] {
    &[
        "Quick Start:",
        "- Ctrl+O: Open file",
        "- Ctrl+Shift+O: Open folder",
        "- Ctrl+S: Save",
        "- Ctrl+F: Find, Ctrl+R: Replace",
        "- F1: Command Palette",
        "- Ctrl+E: Quick Switcher",
        "- View > Theme: Theme Picker",
    ]
}

/// Strips all C0 control characters from the string and replaces them with "_".
///
/// Jury is still out on whether this should also strip C1 control characters.
/// That requires parsing UTF8 codepoints, which is annoying.
fn sanitize_control_chars(text: &str) -> Cow<'_, str> {
    if let Some(off) = text.bytes().position(|b| (..0x20).contains(&b)) {
        let mut sanitized = text.to_string();
        // SAFETY: We only search for ASCII and replace it with ASCII.
        let vec = unsafe { sanitized.as_bytes_mut() };

        for i in &mut vec[off..] {
            *i = if (..0x20).contains(i) { b'_' } else { *i }
        }

        Cow::Owned(sanitized)
    } else {
        Cow::Borrowed(text)
    }
}
