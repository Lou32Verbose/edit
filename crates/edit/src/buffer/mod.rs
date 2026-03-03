// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! A text buffer for a text editor.
//!
//! Implements a Unicode-aware, layout-aware text buffer for terminals.
//! It's based on a gap buffer. It has no line cache and instead relies
//! on the performance of the ucd module for fast text navigation.
//!
//! ---
//!
//! If the project ever outgrows a basic gap buffer (e.g. to add time travel)
//! an ideal, alternative architecture would be a piece table with immutable trees.
//! The tree nodes can be allocated on the same arena allocator as the added chunks,
//! making lifetime management fairly easy. The algorithm is described here:
//! * <https://cdacamar.github.io/data%20structures/algorithms/benchmarking/text%20editors/c++/editor-data-structures/>
//! * <https://github.com/cdacamar/fredbuf>
//!
//! The downside is that text navigation & search takes a performance hit due to small chunks.
//! The solution to the former is to keep line caches, which further complicates the architecture.
//! There's no solution for the latter. However, there's a chance that the performance will still be sufficient.

mod edit_ops;
mod gap_buffer;
mod highlight;
mod history;
mod io;
mod navigation;
mod search;
mod transform;

use std::cell::UnsafeCell;
use std::collections::LinkedList;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::mem::{self, MaybeUninit};
use std::ops::Range;
use std::rc::Rc;
use std::str;

pub use gap_buffer::GapBuffer;
use stdext::arena::{Arena, ArenaString, scratch_arena};

use crate::cell::SemiRefCell;
use crate::clipboard::Clipboard;
use crate::document::{ReadableDocument, WriteableDocument};
use crate::framebuffer::{Framebuffer, IndexedColor};
use crate::helpers::*;
use crate::oklab::StraightRgba;
use crate::simd::memchr2;
use crate::unicode::{self, Cursor, MeasurementConfig, Utf8Chars};
use crate::{apperr, icu, simd};

/// The margin template is used for line numbers.
/// The max. line number we should ever expect is probably 64-bit,
/// and so this template fits 19 digits, followed by " │ ".
const MARGIN_TEMPLATE: &str = "                    │ ";
/// Just a bunch of whitespace you can use for turning tabs into spaces.
/// Happens to reuse MARGIN_TEMPLATE, because it has sufficient whitespace.
const TAB_WHITESPACE: &str = MARGIN_TEMPLATE;
const VISUAL_SPACE: &str = "･";
const VISUAL_SPACE_PREFIX_ADD: usize = '･'.len_utf8() - 1;
const VISUAL_TAB: &str = "￫       ";
const VISUAL_TAB_PREFIX_ADD: usize = '￫'.len_utf8() - 1;

/// Stores statistics about the whole document.
#[derive(Copy, Clone)]
pub struct TextBufferStatistics {
    logical_lines: CoordType,
    visual_lines: CoordType,
}

/// Stores the active text selection anchors.
///
/// The two points are not sorted. Instead, `beg` refers to where the selection
/// started being made and `end` refers to the currently being updated position.
#[derive(Copy, Clone)]
struct TextBufferSelection {
    beg: Point,
    end: Point,
}

/// A secondary cursor with its optional selection.
/// Primary cursor is stored separately for backwards compatibility.
#[derive(Copy, Clone)]
struct SecondaryCursor {
    cursor: Cursor,
    selection: Option<TextBufferSelection>,
}

/// Block (rectangular/column) selection state.
/// When active, represents a rectangular region of text.
#[derive(Copy, Clone)]
struct BlockSelection {
    /// The anchor point where the block selection started (logical position).
    anchor: Point,
    /// The starting column (visual) of the block selection.
    start_column: CoordType,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Language {
    PlainText,
    Rust,
    Toml,
    Json,
    Markdown,
    Python,
    JavaScript,
    TypeScript,
    Html,
    Css,
    Yaml,
    Shell,
    C,
    Cpp,
    CSharp,
    Go,
    Java,
    Kotlin,
    Ruby,
    Php,
    Sql,
    Xml,
    Ini,
    Lua,
    Makefile,
    PowerShell,
    R,
    Swift,
    ObjectiveC,
    Dart,
    Scala,
    Haskell,
    Elixir,
    Erlang,
    Clojure,
    FSharp,
    VbNet,
    Perl,
    Groovy,
    Terraform,
    Nix,
    Assembly,
    Latex,
    Mdx,
    Graphql,
    Csv,
}

#[derive(Clone, Copy)]
enum HighlightKind {
    Comment,
    String,
    Number,
    Keyword,
    CsvColumn(u8),
    Section,
    Key,
}

struct HighlightSpan {
    start: usize,
    end: usize,
    kind: HighlightKind,
}

struct SearchHighlight {
    needle: String,
    options: SearchOptions,
}

/// In order to group actions into a single undo step,
/// we need to know the type of action that was performed.
/// This stores the action type.
#[derive(Copy, Clone, Eq, PartialEq)]
enum HistoryType {
    Other,
    Write,
    Delete,
}

/// An undo/redo entry.
struct HistoryEntry {
    /// [`TextBuffer::cursor`] position before the change was made.
    cursor_before: Point,
    /// [`TextBuffer::selection`] before the change was made.
    selection_before: Option<TextBufferSelection>,
    /// [`TextBuffer::stats`] before the change was made.
    stats_before: TextBufferStatistics,
    /// [`GapBuffer::generation`] before the change was made.
    ///
    /// **NOTE:** Entries with the same generation are grouped together.
    generation_before: u32,
    /// Logical cursor position where the change took place.
    /// The position is at the start of the changed range.
    cursor: Point,
    /// Text that was deleted from the buffer.
    deleted: Vec<u8>,
    /// Text that was added to the buffer.
    added: Vec<u8>,
}

/// Caches an ICU search operation.
struct ActiveSearch {
    /// The search pattern.
    pattern: String,
    /// The search options.
    options: SearchOptions,
    /// The ICU `UText` object.
    text: icu::Text,
    /// The ICU `URegularExpression` object.
    regex: icu::Regex,
    /// [`GapBuffer::generation`] when the search was created.
    /// This is used to detect if we need to refresh the
    /// [`ActiveSearch::regex`] object.
    buffer_generation: u32,
    /// [`TextBuffer::selection_generation`] when the search was
    /// created. When the user manually selects text, we need to
    /// refresh the [`ActiveSearch::pattern`] with it.
    selection_generation: u32,
    /// Stores the text buffer offset in between searches.
    next_search_offset: usize,
    /// If we know there were no hits, we can skip searching.
    no_matches: bool,
}

/// Options for a search operation.
#[derive(Default, Clone, Copy, Eq, PartialEq)]
pub struct SearchOptions {
    /// If true, the search is case-sensitive.
    pub match_case: bool,
    /// If true, the search matches whole words.
    pub whole_word: bool,
    /// If true, the search uses regex.
    pub use_regex: bool,
}

enum RegexReplacement<'a> {
    Group(i32),
    Text(Vec<u8, &'a Arena>),
}

/// Caches the start and length of the active edit line for a single edit.
/// This helps us avoid having to remeasure the buffer after an edit.
struct ActiveEditLineInfo {
    /// Points to the start of the currently being edited line.
    safe_start: Cursor,
    /// Number of visual rows of the line that starts
    /// at [`ActiveEditLineInfo::safe_start`].
    line_height_in_rows: CoordType,
    /// Byte distance from the start of the line at
    /// [`ActiveEditLineInfo::safe_start`] to the next line.
    distance_next_line_start: usize,
}

/// Undo/redo grouping works by recording a set of "overrides",
/// which are then applied in [`TextBuffer::edit_begin()`].
/// This allows us to create a group of edits that all share a
/// common `generation_before` and can be undone/redone together.
/// This struct stores those overrides.
struct ActiveEditGroupInfo {
    /// [`TextBuffer::cursor`] position before the change was made.
    cursor_before: Point,
    /// [`TextBuffer::selection`] before the change was made.
    selection_before: Option<TextBufferSelection>,
    /// [`TextBuffer::stats`] before the change was made.
    stats_before: TextBufferStatistics,
    /// [`GapBuffer::generation`] before the change was made.
    ///
    /// **NOTE:** Entries with the same generation are grouped together.
    generation_before: u32,
}

/// Char- or word-wise navigation? Your choice.
#[derive(Clone, Copy)]
pub enum CursorMovement {
    Grapheme,
    Word,
}

/// See [`TextBuffer::move_selected_lines`].
pub enum MoveLineDirection {
    Up,
    Down,
}

/// The result of a call to [`TextBuffer::render()`].
pub struct RenderResult {
    /// The maximum visual X position we encountered during rendering.
    pub visual_pos_x_max: CoordType,
}

/// A [`TextBuffer`] with inner mutability.
pub type TextBufferCell = SemiRefCell<TextBuffer>;

/// A [`TextBuffer`] inside an [`Rc`].
///
/// We need this because the TUI system needs to borrow
/// the given text buffer(s) until after the layout process.
pub type RcTextBuffer = Rc<TextBufferCell>;

/// A text buffer for a text editor.
pub struct TextBuffer {
    buffer: GapBuffer,

    undo_stack: LinkedList<SemiRefCell<HistoryEntry>>,
    redo_stack: LinkedList<SemiRefCell<HistoryEntry>>,
    last_history_type: HistoryType,
    last_save_generation: u32,

    active_edit_group: Option<ActiveEditGroupInfo>,
    active_edit_line_info: Option<ActiveEditLineInfo>,
    active_edit_depth: i32,
    active_edit_off: usize,

    stats: TextBufferStatistics,
    cursor: Cursor,
    // When scrolling significant amounts of text away from the cursor,
    // rendering will naturally slow down proportionally to the distance.
    // To avoid this, we cache the cursor position for rendering.
    // Must be cleared on every edit or reflow.
    cursor_for_rendering: Option<Cursor>,
    selection: Option<TextBufferSelection>,
    selection_generation: u32,
    // Additional cursors for multi-cursor editing.
    // The primary cursor is `cursor` above; these are secondary cursors.
    secondary_cursors: Vec<SecondaryCursor>,
    // Block (rectangular/column) selection state.
    // When Some, we're in block selection mode.
    block_selection: Option<BlockSelection>,
    search: Option<UnsafeCell<ActiveSearch>>,

    width: CoordType,
    margin_width: CoordType,
    margin_enabled: bool,
    word_wrap_column: CoordType,
    word_wrap_enabled: bool,
    tab_size: CoordType,
    indent_with_tabs: bool,
    line_highlight_enabled: bool,
    ruler: CoordType,
    show_whitespace: bool,
    encoding: &'static str,
    language: Language,
    search_highlight: Option<SearchHighlight>,
    newlines_are_crlf: bool,
    insert_final_newline: bool,
    overtype: bool,

    wants_cursor_visibility: bool,
}

impl TextBuffer {
    /// Creates a new text buffer inside an [`Rc`].
    /// See [`TextBuffer::new()`].
    pub fn new_rc(small: bool) -> apperr::Result<RcTextBuffer> {
        let buffer = Self::new(small)?;
        Ok(Rc::new(SemiRefCell::new(buffer)))
    }

    /// Creates a new text buffer. With `small` you can control
    /// if the buffer is optimized for <1MiB contents.
    pub fn new(small: bool) -> apperr::Result<Self> {
        Ok(Self {
            buffer: GapBuffer::new(small)?,

            undo_stack: LinkedList::new(),
            redo_stack: LinkedList::new(),
            last_history_type: HistoryType::Other,
            last_save_generation: 0,

            active_edit_group: None,
            active_edit_line_info: None,
            active_edit_depth: 0,
            active_edit_off: 0,

            stats: TextBufferStatistics { logical_lines: 1, visual_lines: 1 },
            cursor: Default::default(),
            cursor_for_rendering: None,
            selection: None,
            selection_generation: 0,
            secondary_cursors: Vec::new(),
            block_selection: None,
            search: None,

            width: 0,
            margin_width: 0,
            margin_enabled: false,
            word_wrap_column: 0,
            word_wrap_enabled: false,
            tab_size: 4,
            indent_with_tabs: false,
            line_highlight_enabled: false,
            ruler: 0,
            show_whitespace: false,
            encoding: "UTF-8",
            language: Language::PlainText,
            search_highlight: None,
            newlines_are_crlf: cfg!(windows), // Windows users want CRLF
            insert_final_newline: false,
            overtype: false,

            wants_cursor_visibility: false,
        })
    }

    /// Length of the document in bytes.
    pub fn text_length(&self) -> usize {
        self.buffer.len()
    }

    /// Counts the number of words in the document.
    /// A word is defined as a sequence of non-whitespace characters.
    pub fn word_count(&self) -> usize {
        let text = self.buffer.read_forward(0);
        count_words(text)
    }

    /// Counts the number of words in the current selection.
    /// Returns None if there's no selection.
    pub fn selection_word_count(&self) -> Option<usize> {
        let (beg, end) = self.selection_range()?;
        let mut content = Vec::new();
        self.buffer.extract_raw(beg.offset..end.offset, &mut content, 0);
        Some(count_words(&content))
    }

    /// Returns the byte length of the current selection.
    /// Returns None if there's no selection.
    pub fn selection_length(&self) -> Option<usize> {
        let (beg, end) = self.selection_range()?;
        Some(end.offset - beg.offset)
    }

    /// Number of logical lines in the document,
    /// that is, lines separated by newlines.
    pub fn logical_line_count(&self) -> CoordType {
        self.stats.logical_lines
    }

    /// Number of visual lines in the document,
    /// that is, the number of lines after layout.
    pub fn visual_line_count(&self) -> CoordType {
        self.stats.visual_lines
    }

    /// Does the buffer need to be saved?
    pub fn is_dirty(&self) -> bool {
        self.last_save_generation != self.buffer.generation()
    }

    /// The buffer generation changes on every edit.
    /// With this you can check if it has changed since
    /// the last time you called this function.
    pub fn generation(&self) -> u32 {
        self.buffer.generation()
    }

    /// Force the buffer to be dirty.
    pub fn mark_as_dirty(&mut self) {
        self.last_save_generation = self.buffer.generation().wrapping_sub(1);
    }

    pub fn mark_as_clean(&mut self) {
        self.last_save_generation = self.buffer.generation();
    }

    /// The encoding used during reading/writing. "UTF-8" is the default.
    pub fn encoding(&self) -> &'static str {
        self.encoding
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn set_language(&mut self, language: Language) {
        self.language = language;
    }

    pub fn set_search_highlight(&mut self, needle: &str, options: SearchOptions) {
        if needle.trim_ascii().is_empty() {
            self.search_highlight = None;
            return;
        }

        if let Some(current) = &mut self.search_highlight {
            if current.needle == needle && current.options == options {
                return;
            }
            current.needle.clear();
            current.needle.push_str(needle);
            current.options = options;
            return;
        }

        self.search_highlight = Some(SearchHighlight { needle: needle.to_string(), options });
    }

    pub fn clear_search_highlight(&mut self) {
        self.search_highlight = None;
    }

    /// Set the encoding used during reading/writing.
    pub fn set_encoding(&mut self, encoding: &'static str) {
        if self.encoding != encoding {
            self.encoding = encoding;
            self.mark_as_dirty();
        }
    }

    /// The newline type used in the document. LF or CRLF.
    pub fn is_crlf(&self) -> bool {
        self.newlines_are_crlf
    }

    /// Changes the newline type without normalizing the document.
    pub fn set_crlf(&mut self, crlf: bool) {
        self.newlines_are_crlf = crlf;
    }

    /// Changes the newline type used in the document.
    ///
    /// NOTE: Cannot be undone.
    pub fn normalize_newlines(&mut self, crlf: bool) {
        let newline: &[u8] = if crlf { b"\r\n" } else { b"\n" };
        let mut off = 0;

        let mut cursor_offset = self.cursor.offset;
        let mut cursor_for_rendering_offset =
            self.cursor_for_rendering.map_or(cursor_offset, |c| c.offset);

        #[cfg(debug_assertions)]
        let mut cursor_newlines = 0;

        'outer: loop {
            // Seek to the offset of the next line start.
            loop {
                let chunk = self.read_forward(off);
                if chunk.is_empty() {
                    break 'outer;
                }

                let (delta, line) = simd::lines_fwd(chunk, 0, 0, 1);
                off += delta;
                if line == 1 {
                    break;
                }
            }

            #[cfg(debug_assertions)]
            {
                if off <= cursor_offset {
                    cursor_newlines += 1;
                }
            }

            // Get the preceding newline.
            let chunk = self.read_backward(off);
            let chunk_newline_len = if chunk.ends_with(b"\r\n") { 2 } else { 1 };
            let chunk_newline = &chunk[chunk.len() - chunk_newline_len..];

            if chunk_newline != newline {
                // If this newline is still before our cursor position, then it still has an effect on its offset.
                // Any newline adjustments past that cursor position are irrelevant.
                let delta = newline.len() as isize - chunk_newline_len as isize;
                if off <= cursor_offset {
                    cursor_offset = cursor_offset.saturating_add_signed(delta);
                }
                if off <= cursor_for_rendering_offset {
                    cursor_for_rendering_offset =
                        cursor_for_rendering_offset.saturating_add_signed(delta);
                }

                // Replace the newline.
                off -= chunk_newline_len;
                self.buffer.replace(off..off + chunk_newline_len, newline);
                off += newline.len();
            }
        }

        // If this fails, the cursor offset calculation above is wrong.
        #[cfg(debug_assertions)]
        debug_assert_eq!(cursor_newlines, self.cursor.logical_pos.y);

        self.cursor.offset = cursor_offset;
        if let Some(cursor) = &mut self.cursor_for_rendering {
            cursor.offset = cursor_for_rendering_offset;
        }

        self.newlines_are_crlf = crlf;
    }

    /// If enabled, automatically insert a final newline
    /// when typing at the end of the file.
    pub fn set_insert_final_newline(&mut self, enabled: bool) {
        self.insert_final_newline = enabled;
    }

    /// Whether to insert or overtype text when writing.
    pub fn is_overtype(&self) -> bool {
        self.overtype
    }

    /// Set the overtype mode.
    pub fn set_overtype(&mut self, overtype: bool) {
        self.overtype = overtype;
    }

    /// Gets the logical cursor position, that is,
    /// the position in lines and graphemes per line.
    pub fn cursor_logical_pos(&self) -> Point {
        self.cursor.logical_pos
    }

    /// Gets the visual cursor position, that is,
    /// the position in laid out rows and columns.
    pub fn cursor_visual_pos(&self) -> Point {
        self.cursor.visual_pos
    }

    /// Gets the width of the left margin.
    pub fn margin_width(&self) -> CoordType {
        self.margin_width
    }

    /// Is the left margin enabled?
    pub fn set_margin_enabled(&mut self, enabled: bool) -> bool {
        if self.margin_enabled == enabled {
            false
        } else {
            self.margin_enabled = enabled;
            self.reflow();
            true
        }
    }

    /// Gets the width of the text contents for layout.
    pub fn text_width(&self) -> CoordType {
        self.width - self.margin_width
    }

    /// Ask the TUI system to scroll the buffer and make the cursor visible.
    ///
    /// TODO: This function shows that [`TextBuffer`] is poorly abstracted
    /// away from the TUI system. The only reason this exists is so that
    /// if someone outside the TUI code enables word-wrap, the TUI code
    /// recognizes this and scrolls the cursor into view. But outside of this
    /// scrolling, views, etc., are all UI concerns = this should not be here.
    pub fn make_cursor_visible(&mut self) {
        self.wants_cursor_visibility = true;
    }

    /// For the TUI code to retrieve a prior [`TextBuffer::make_cursor_visible()`] request.
    pub fn take_cursor_visibility_request(&mut self) -> bool {
        mem::take(&mut self.wants_cursor_visibility)
    }

    /// Is word-wrap enabled?
    ///
    /// Technically, this is a misnomer, because it's line-wrapping.
    pub fn is_word_wrap_enabled(&self) -> bool {
        self.word_wrap_enabled
    }

    /// Enable or disable word-wrap.
    ///
    /// NOTE: It's expected that the tui code calls `set_width()` sometime after this.
    /// This will then trigger the actual recalculation of the cursor position.
    pub fn set_word_wrap(&mut self, enabled: bool) {
        if self.word_wrap_enabled != enabled {
            self.word_wrap_enabled = enabled;
            self.width = 0; // Force a reflow.
            self.make_cursor_visible();
        }
    }

    /// Returns whether whitespace visualization is enabled.
    pub fn is_show_whitespace_enabled(&self) -> bool {
        self.show_whitespace
    }

    /// Enables or disables whitespace visualization.
    pub fn set_show_whitespace(&mut self, enabled: bool) {
        self.show_whitespace = enabled;
    }

    /// Set the width available for layout.
    ///
    /// Ideally this would be a pure UI concern, but the text buffer needs this
    /// so that it can abstract away  visual cursor movement such as "go a line up".
    /// What would that even mean if it didn't know how wide a line is?
    pub fn set_width(&mut self, width: CoordType) -> bool {
        if width <= 0 || width == self.width {
            false
        } else {
            self.width = width;
            self.reflow();
            true
        }
    }

    /// Set the tab width. Could be anything, but is expected to be 1-8.
    pub fn tab_size(&self) -> CoordType {
        self.tab_size
    }

    /// Set the tab size. Clamped to 1-8.
    pub fn set_tab_size(&mut self, width: CoordType) -> bool {
        let width = width.clamp(1, 8);
        if width == self.tab_size {
            false
        } else {
            self.tab_size = width;
            self.reflow();
            true
        }
    }

    /// Calculates the amount of spaces a tab key press would insert at the given column.
    /// This also equals the visual width of an actual tab character.
    ///
    /// This exists because Rust doesn't have range constraints yet, and without
    /// them assembly blows up in size by 7x. It's a recurring issue with Rust.
    #[inline]
    fn tab_size_eval(&self, column: CoordType) -> CoordType {
        // SAFETY: `set_tab_size` clamps `self.tab_size` to 1-8.
        unsafe { std::hint::assert_unchecked(self.tab_size >= 1 && self.tab_size <= 8) };
        self.tab_size - (column % self.tab_size)
    }

    /// If the cursor is at an indentation of `column`, this returns
    /// the column to which a backspace key press would delete to.
    #[inline]
    fn tab_size_prev_column(&self, column: CoordType) -> CoordType {
        // SAFETY: `set_tab_size` clamps `self.tab_size` to 1-8.
        unsafe { std::hint::assert_unchecked(self.tab_size >= 1 && self.tab_size <= 8) };
        (column - 1).max(0) / self.tab_size * self.tab_size
    }

    /// Returns whether tabs are used for indentation.
    pub fn indent_with_tabs(&self) -> bool {
        self.indent_with_tabs
    }

    /// Sets whether tabs or spaces are used for indentation.
    pub fn set_indent_with_tabs(&mut self, indent_with_tabs: bool) {
        self.indent_with_tabs = indent_with_tabs;
    }

    /// Sets whether the line the cursor is on should be highlighted.
    pub fn set_line_highlight_enabled(&mut self, enabled: bool) {
        self.line_highlight_enabled = enabled;
    }

    /// Sets a ruler column, e.g. 80.
    pub fn set_ruler(&mut self, column: CoordType) {
        self.ruler = column;
    }

    pub fn reflow(&mut self) {
        self.reflow_internal(true);
    }

    fn recalc_after_content_changed(&mut self) {
        self.reflow_internal(false);
    }

    fn reflow_internal(&mut self, force: bool) {
        let word_wrap_column_before = self.word_wrap_column;

        {
            // +1 onto logical_lines, because line numbers are 1-based.
            // +1 onto log10, because we want the digit width and not the actual log10.
            // +3 onto log10, because we append " | " to the line numbers to form the margin.
            self.margin_width = if self.margin_enabled {
                self.stats.logical_lines.ilog10() as CoordType + 4
            } else {
                0
            };

            let text_width = self.text_width();
            // 2 columns are required, because otherwise wide glyphs wouldn't ever fit.
            self.word_wrap_column =
                if self.word_wrap_enabled && text_width >= 2 { text_width } else { 0 };
        }

        self.cursor_for_rendering = None;

        if force || self.word_wrap_column != word_wrap_column_before {
            // Recalculate the cursor position.
            self.cursor = self.cursor_move_to_logical_internal(
                if self.word_wrap_column > 0 {
                    Default::default()
                } else {
                    self.goto_line_start(self.cursor, self.cursor.logical_pos.y)
                },
                self.cursor.logical_pos,
            );

            // Recalculate the line statistics.
            if self.word_wrap_column > 0 {
                let end = self.cursor_move_to_logical_internal(self.cursor, Point::MAX);
                self.stats.visual_lines = end.visual_pos.y + 1;
            } else {
                self.stats.visual_lines = self.stats.logical_lines;
            }
        }
    }

    /// Replaces the entire buffer contents with the given `text`.
    /// Assumes that the line count doesn't change.
    pub fn copy_from_str(&mut self, text: &dyn ReadableDocument) {
        if self.buffer.copy_from(text) {
            self.recalc_after_content_swap();
            self.cursor_move_to_logical(Point { x: CoordType::MAX, y: 0 });

            let delete = self.buffer.len() - self.cursor.offset;
            if delete != 0 {
                self.buffer.allocate_gap(self.cursor.offset, 0, delete);
            }
        }
    }

    fn recalc_after_content_swap(&mut self) {
        // If the buffer was changed, nothing we previously saved can be relied upon.
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_history_type = HistoryType::Other;
        self.cursor = Default::default();
        self.set_selection(None);
        self.mark_as_clean();
        self.reflow();
    }

    /// Copies the contents of the buffer into a string.
    pub fn save_as_string(&mut self, dst: &mut dyn WriteableDocument) {
        self.buffer.copy_into(dst);
        self.mark_as_clean();
    }

    /// Returns the current selection.
    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    fn set_selection(&mut self, selection: Option<TextBufferSelection>) -> u32 {
        self.selection = selection.filter(|s| s.beg != s.end);
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.selection_generation
    }

    /// Moves the cursor by `offset` and updates the selection to contain it.
    pub fn selection_update_offset(&mut self, offset: usize) {
        self.set_cursor_for_selection(self.cursor_move_to_offset_internal(self.cursor, offset));
    }

    /// Moves the cursor to `visual_pos` and updates the selection to contain it.
    pub fn selection_update_visual(&mut self, visual_pos: Point) {
        self.set_cursor_for_selection(self.cursor_move_to_visual_internal(self.cursor, visual_pos));
    }

    /// Moves the cursor to `logical_pos` and updates the selection to contain it.
    pub fn selection_update_logical(&mut self, logical_pos: Point) {
        self.set_cursor_for_selection(
            self.cursor_move_to_logical_internal(self.cursor, logical_pos),
        );
    }

    /// Moves the cursor by `delta` and updates the selection to contain it.
    pub fn selection_update_delta(&mut self, granularity: CursorMovement, delta: CoordType) {
        self.set_cursor_for_selection(self.cursor_move_delta_internal(
            self.cursor,
            granularity,
            delta,
        ));
    }

    /// Select the current word.
    pub fn select_word(&mut self) {
        let Range { start, end } = navigation::word_select(&self.buffer, self.cursor.offset);
        let beg = self.cursor_move_to_offset_internal(self.cursor, start);
        let end = self.cursor_move_to_offset_internal(beg, end);
        unsafe { self.set_cursor(end) };
        self.set_selection(Some(TextBufferSelection {
            beg: beg.logical_pos,
            end: end.logical_pos,
        }));
    }

    /// Select the current line.
    pub fn select_line(&mut self) {
        let beg = self.cursor_move_to_logical_internal(
            self.cursor,
            Point { x: 0, y: self.cursor.logical_pos.y },
        );
        let end = self
            .cursor_move_to_logical_internal(beg, Point { x: 0, y: self.cursor.logical_pos.y + 1 });
        unsafe { self.set_cursor(end) };
        self.set_selection(Some(TextBufferSelection {
            beg: beg.logical_pos,
            end: end.logical_pos,
        }));
    }

    /// Select the entire document.
    pub fn select_all(&mut self) {
        let beg = Default::default();
        let end = self.cursor_move_to_logical_internal(beg, Point::MAX);
        unsafe { self.set_cursor(end) };
        self.set_selection(Some(TextBufferSelection {
            beg: beg.logical_pos,
            end: end.logical_pos,
        }));
    }

    /// Starts a new selection, if there's none already.
    pub fn start_selection(&mut self) {
        if self.selection.is_none() {
            self.set_selection(Some(TextBufferSelection {
                beg: self.cursor.logical_pos,
                end: self.cursor.logical_pos,
            }));
        }
    }

    /// Destroy the current selection.
    pub fn clear_selection(&mut self) -> bool {
        let had_selection = self.selection.is_some();
        self.set_selection(None);
        had_selection
    }

    // ==================== Multi-cursor support ====================

    /// Returns whether there are multiple cursors active.
    pub fn has_multiple_cursors(&self) -> bool {
        !self.secondary_cursors.is_empty()
    }

    /// Returns the number of cursors (primary + secondary).
    pub fn cursor_count(&self) -> usize {
        1 + self.secondary_cursors.len()
    }

    /// Returns an iterator over all cursors (primary first, then secondary).
    pub fn all_cursors(&self) -> impl Iterator<Item = Cursor> + '_ {
        std::iter::once(self.cursor).chain(self.secondary_cursors.iter().map(|sc| sc.cursor))
    }

    /// Returns an iterator over all selection ranges, sorted by position.
    /// Each range is (begin_cursor, end_cursor).
    #[allow(dead_code)]
    pub fn all_selection_ranges(&self) -> Vec<(Cursor, Cursor)> {
        let mut ranges: Vec<(Cursor, Cursor)> = Vec::new();

        // Primary cursor selection
        if let Some(range) = self.selection_range() {
            ranges.push(range);
        }

        // Secondary cursor selections
        for sc in &self.secondary_cursors {
            if let Some(sel) = sc.selection {
                let [beg, end] = minmax(sel.beg, sel.end);
                let beg_cursor = self.cursor_move_to_logical_internal(sc.cursor, beg);
                let end_cursor = self.cursor_move_to_logical_internal(beg_cursor, end);
                if beg_cursor.offset < end_cursor.offset {
                    ranges.push((beg_cursor, end_cursor));
                }
            }
        }

        // Sort by offset
        ranges.sort_by_key(|(beg, _)| beg.offset);
        ranges
    }

    /// Adds a cursor at the given logical position.
    /// If a cursor already exists at that position, nothing happens.
    pub fn add_cursor_at_logical(&mut self, pos: Point) {
        let new_cursor = self.cursor_move_to_logical_internal(self.cursor, pos);

        // Check if cursor already exists at this offset
        if self.cursor.offset == new_cursor.offset {
            return;
        }
        for sc in &self.secondary_cursors {
            if sc.cursor.offset == new_cursor.offset {
                return;
            }
        }

        self.secondary_cursors.push(SecondaryCursor { cursor: new_cursor, selection: None });
        self.normalize_cursors();
    }

    /// Adds a cursor at the given visual position.
    pub fn add_cursor_at_visual(&mut self, pos: Point) {
        let new_cursor = self.cursor_move_to_visual_internal(self.cursor, pos);

        // Check if cursor already exists at this offset
        if self.cursor.offset == new_cursor.offset {
            return;
        }
        for sc in &self.secondary_cursors {
            if sc.cursor.offset == new_cursor.offset {
                return;
            }
        }

        self.secondary_cursors.push(SecondaryCursor { cursor: new_cursor, selection: None });
        self.normalize_cursors();
    }

    /// Adds a cursor above the current cursor (same column, previous line).
    pub fn add_cursor_above(&mut self) {
        let pos = self.cursor.logical_pos;
        if pos.y > 0 {
            self.add_cursor_at_logical(Point { x: pos.x, y: pos.y - 1 });
        }
    }

    /// Adds a cursor below the current cursor (same column, next line).
    pub fn add_cursor_below(&mut self) {
        let pos = self.cursor.logical_pos;
        if pos.y < self.stats.logical_lines - 1 {
            self.add_cursor_at_logical(Point { x: pos.x, y: pos.y + 1 });
        }
    }

    /// Adds a cursor at the next occurrence of the current selection.
    /// If there's no selection, selects the current word first.
    /// This is the Ctrl+D behavior in VS Code.
    pub fn add_cursor_at_next_occurrence(&mut self) {
        // If there's no selection, select the current word first
        if self.selection.is_none() {
            self.select_word();
            return; // First Ctrl+D just selects the word
        }

        // Get the selected text
        let (beg, end) = match self.selection_range() {
            Some(range) => range,
            None => return,
        };

        // Extract the selected text
        let mut selected_text = Vec::new();
        self.buffer.extract_raw(beg.offset..end.offset, &mut selected_text, 0);

        if selected_text.is_empty() {
            return;
        }

        // Search for the next occurrence starting after the current selection end
        let search_start = end.offset;

        // Search forward from end of selection
        if let Some(found_offset) = self.find_next_occurrence(&selected_text, search_start) {
            let found_beg = self.cursor_move_to_offset_internal(self.cursor, found_offset);
            let found_end =
                self.cursor_move_to_offset_internal(found_beg, found_offset + selected_text.len());

            // Add as secondary cursor with selection
            self.secondary_cursors.push(SecondaryCursor {
                cursor: found_end,
                selection: Some(TextBufferSelection {
                    beg: found_beg.logical_pos,
                    end: found_end.logical_pos,
                }),
            });
            self.normalize_cursors();
        } else {
            // Wrap around: search from beginning of document
            if let Some(found_offset) = self.find_next_occurrence(&selected_text, 0) {
                // Only add if it's before our original selection
                if found_offset < beg.offset {
                    let found_beg = self.cursor_move_to_offset_internal(self.cursor, found_offset);
                    let found_end = self.cursor_move_to_offset_internal(
                        found_beg,
                        found_offset + selected_text.len(),
                    );

                    self.secondary_cursors.push(SecondaryCursor {
                        cursor: found_end,
                        selection: Some(TextBufferSelection {
                            beg: found_beg.logical_pos,
                            end: found_end.logical_pos,
                        }),
                    });
                    self.normalize_cursors();
                }
            }
        }
    }

    /// Collapses all cursors to just the primary cursor.
    pub fn collapse_cursors(&mut self) {
        self.secondary_cursors.clear();
    }

    /// Normalizes cursors by sorting them and removing duplicates.
    /// After text edits, cursors may have become invalid or overlapping.
    fn normalize_cursors(&mut self) {
        if self.secondary_cursors.is_empty() {
            return;
        }

        // Sort secondary cursors by offset
        self.secondary_cursors.sort_by_key(|sc| sc.cursor.offset);

        // Remove duplicates (cursors at the same offset)
        self.secondary_cursors.dedup_by_key(|sc| sc.cursor.offset);

        // Remove any secondary cursor at the same offset as primary
        self.secondary_cursors.retain(|sc| sc.cursor.offset != self.cursor.offset);
    }

    /// Recalculates all secondary cursor positions after text modification.
    /// Call this after edits that change the buffer content.
    #[allow(dead_code)]
    fn refresh_secondary_cursors(&mut self) {
        let text_len = self.text_length();
        for i in 0..self.secondary_cursors.len() {
            // Clamp offset to valid range
            let offset = self.secondary_cursors[i].cursor.offset.min(text_len);
            let new_cursor = self.cursor_move_to_offset_internal(Default::default(), offset);
            self.secondary_cursors[i].cursor = new_cursor;

            // Update selection if present
            if let Some(sel) = self.secondary_cursors[i].selection {
                // Re-validate selection points - they use logical positions
                // which should still be valid after the edit
                let cursor = self.secondary_cursors[i].cursor;
                let beg = self.cursor_move_to_logical_internal(cursor, sel.beg);
                let end = self.cursor_move_to_logical_internal(beg, sel.end);
                if beg.offset >= end.offset {
                    self.secondary_cursors[i].selection = None;
                }
            }
        }
        self.normalize_cursors();
    }

    // ==================== Block selection support ====================

    /// Returns whether block selection mode is active.
    pub fn has_block_selection(&self) -> bool {
        self.block_selection.is_some()
    }

    /// Starts block selection at the current cursor position.
    pub fn start_block_selection(&mut self) {
        if self.block_selection.is_none() {
            self.block_selection = Some(BlockSelection {
                anchor: self.cursor.logical_pos,
                start_column: self.cursor.visual_pos.x,
            });
        }
    }

    /// Clears block selection mode.
    pub fn clear_block_selection(&mut self) {
        self.block_selection = None;
    }

    /// Extends block selection in the given direction.
    /// delta_x: -1 for left, 1 for right, 0 for no horizontal movement
    /// delta_y: -1 for up, 1 for down, 0 for no vertical movement
    pub fn block_selection_extend(&mut self, delta_x: CoordType, delta_y: CoordType) {
        // Start block selection if not already active
        if self.block_selection.is_none() {
            self.start_block_selection();
        }

        // Move cursor
        let new_pos = Point {
            x: (self.cursor.visual_pos.x + delta_x).max(0),
            y: (self.cursor.visual_pos.y + delta_y).max(0).min(self.stats.visual_lines - 1),
        };
        self.cursor_move_to_visual(new_pos);
    }

    /// Returns the block selection bounds if active.
    /// Returns (top_line, bottom_line, left_column, right_column) in visual coordinates.
    pub fn block_selection_bounds(&self) -> Option<(CoordType, CoordType, CoordType, CoordType)> {
        let block = self.block_selection?;

        let anchor_visual = self.cursor_move_to_logical_internal(self.cursor, block.anchor);

        let top = anchor_visual.visual_pos.y.min(self.cursor.visual_pos.y);
        let bottom = anchor_visual.visual_pos.y.max(self.cursor.visual_pos.y);
        let left = block.start_column.min(self.cursor.visual_pos.x);
        let right = block.start_column.max(self.cursor.visual_pos.x);

        Some((top, bottom, left, right))
    }

    /// Converts the current block selection to multiple cursors.
    /// Each line in the block selection gets a cursor at the left edge of the selection.
    /// Called before editing to enable multi-cursor editing of the block.
    pub fn block_selection_to_cursors(&mut self) {
        let Some((top, bottom, left, right)) = self.block_selection_bounds() else {
            return;
        };

        // Clear existing secondary cursors
        self.secondary_cursors.clear();

        // Create a cursor for each line in the block
        for visual_y in top..=bottom {
            let cursor =
                self.cursor_move_to_visual_internal(self.cursor, Point { x: left, y: visual_y });

            // Calculate selection end for this line
            let cursor_end =
                self.cursor_move_to_visual_internal(cursor, Point { x: right, y: visual_y });

            if visual_y == self.cursor.visual_pos.y {
                // This is the primary cursor's line
                self.cursor = cursor_end;
                if left != right {
                    self.selection = Some(TextBufferSelection {
                        beg: cursor.logical_pos,
                        end: cursor_end.logical_pos,
                    });
                } else {
                    self.selection = None;
                }
            } else {
                // Add as secondary cursor
                let selection = if left != right {
                    Some(TextBufferSelection {
                        beg: cursor.logical_pos,
                        end: cursor_end.logical_pos,
                    })
                } else {
                    None
                };

                self.secondary_cursors.push(SecondaryCursor { cursor: cursor_end, selection });
            }
        }

        // Clear block selection mode
        self.block_selection = None;
        self.normalize_cursors();
    }

    // ==================== End block selection support ====================

    // ==================== End multi-cursor support ====================

    fn measurement_config(&self) -> MeasurementConfig<'_> {
        MeasurementConfig::new(&self.buffer)
            .with_word_wrap_column(self.word_wrap_column)
            .with_tab_size(self.tab_size)
    }

    fn goto_line_start(&self, cursor: Cursor, y: CoordType) -> Cursor {
        let mut result = cursor;
        let mut seek_to_line_start = true;

        if y > result.logical_pos.y {
            while y > result.logical_pos.y {
                let chunk = self.read_forward(result.offset);
                if chunk.is_empty() {
                    break;
                }

                let (delta, line) = simd::lines_fwd(chunk, 0, result.logical_pos.y, y);
                result.offset += delta;
                result.logical_pos.y = line;
            }

            // If we're at the end of the buffer, we could either be there because the last
            // character in the buffer is genuinely a newline, or because the buffer ends in a
            // line of text without trailing newline. The only way to make sure is to seek
            // backwards to the line start again. But otherwise we can skip that.
            seek_to_line_start =
                result.offset == self.text_length() && result.offset != cursor.offset;
        }

        if seek_to_line_start {
            loop {
                let chunk = self.read_backward(result.offset);
                if chunk.is_empty() {
                    break;
                }

                let (delta, line) = simd::lines_bwd(chunk, chunk.len(), result.logical_pos.y, y);
                result.offset -= chunk.len() - delta;
                result.logical_pos.y = line;
                if delta > 0 {
                    break;
                }
            }
        }

        if result.offset == cursor.offset {
            return result;
        }

        result.logical_pos.x = 0;
        result.visual_pos.x = 0;
        result.visual_pos.y = result.logical_pos.y;
        result.column = 0;
        result.wrap_opp = false;

        if self.word_wrap_column > 0 {
            let upward = result.offset < cursor.offset;
            let (top, bottom) = if upward { (result, cursor) } else { (cursor, result) };

            let mut bottom_remeasured =
                self.measurement_config().with_cursor(top).goto_logical(bottom.logical_pos);

            // The second problem is that visual positions can be ambiguous. A single logical position
            // can map to two visual positions: One at the end of the preceding line in front of
            // a word wrap, and another at the start of the next line after the same word wrap.
            //
            // This, however, only applies if we go upwards, because only then `bottom ≅ cursor`,
            // and thus only then this `bottom` is ambiguous. Otherwise, `bottom ≅ result`
            // and `result` is at a line start which is never ambiguous.
            if upward {
                let a = bottom_remeasured.visual_pos.x;
                let b = bottom.visual_pos.x;
                bottom_remeasured.visual_pos.y = bottom_remeasured.visual_pos.y
                    + (a != 0 && b == 0) as CoordType
                    - (a == 0 && b != 0) as CoordType;
            }

            let mut delta = bottom_remeasured.visual_pos.y - top.visual_pos.y;
            if upward {
                delta = -delta;
            }

            result.visual_pos.y = cursor.visual_pos.y + delta;
        }

        result
    }

    fn cursor_move_to_offset_internal(&self, mut cursor: Cursor, offset: usize) -> Cursor {
        if offset == cursor.offset {
            return cursor;
        }

        // goto_line_start() is fast for seeking across lines _if_ line wrapping is disabled.
        // For backward seeking we have to use it either way, so we're covered there.
        // This implements the forward seeking portion, if it's approx. worth doing so.
        if self.word_wrap_column <= 0 && offset.saturating_sub(cursor.offset) > 1024 {
            // Replacing this with a more optimal, direct memchr() loop appears
            // to improve performance only marginally by another 2% or so.
            // Still, it's kind of "meh" looking at how poorly this is implemented...
            loop {
                let next = self.goto_line_start(cursor, cursor.logical_pos.y + 1);
                // Stop when we either ran past the target offset,
                // or when we hit the end of the buffer and `goto_line_start` backtracked to the line start.
                if next.offset > offset || next.offset <= cursor.offset {
                    break;
                }
                cursor = next;
            }
        }

        while offset < cursor.offset {
            cursor = self.goto_line_start(cursor, cursor.logical_pos.y - 1);
        }

        self.measurement_config().with_cursor(cursor).goto_offset(offset)
    }

    fn cursor_move_to_logical_internal(&self, mut cursor: Cursor, pos: Point) -> Cursor {
        let pos = Point { x: pos.x.max(0), y: pos.y.max(0) };

        if pos == cursor.logical_pos {
            return cursor;
        }

        // goto_line_start() is the fastest way for seeking across lines. As such we always
        // use it if the requested `.y` position is different. We still need to use it if the
        // `.x` position is smaller, but only because `goto_logical()` cannot seek backwards.
        if pos.y != cursor.logical_pos.y || pos.x < cursor.logical_pos.x {
            cursor = self.goto_line_start(cursor, pos.y);
        }

        self.measurement_config().with_cursor(cursor).goto_logical(pos)
    }

    fn cursor_move_to_visual_internal(&self, mut cursor: Cursor, pos: Point) -> Cursor {
        let pos = Point { x: pos.x.max(0), y: pos.y.max(0) };

        if pos == cursor.visual_pos {
            return cursor;
        }

        if self.word_wrap_column <= 0 {
            // Identical to the fast-pass in `cursor_move_to_logical_internal()`.
            if pos.y != cursor.visual_pos.y || pos.x < cursor.visual_pos.x {
                cursor = self.goto_line_start(cursor, pos.y);
            }
        } else {
            // `goto_visual()` can only seek forward, so we need to seek backward here if needed.
            // NOTE that this intentionally doesn't use the `Eq` trait of `Point`, because if
            // `pos.y == cursor.visual_pos.y` we don't need to go to `cursor.logical_pos.y - 1`.
            while pos.y < cursor.visual_pos.y {
                cursor = self.goto_line_start(cursor, cursor.logical_pos.y - 1);
            }
            if pos.y == cursor.visual_pos.y && pos.x < cursor.visual_pos.x {
                cursor = self.goto_line_start(cursor, cursor.logical_pos.y);
            }
        }

        self.measurement_config().with_cursor(cursor).goto_visual(pos)
    }

    fn cursor_move_delta_internal(
        &self,
        mut cursor: Cursor,
        granularity: CursorMovement,
        mut delta: CoordType,
    ) -> Cursor {
        if delta == 0 {
            return cursor;
        }

        let sign = if delta > 0 { 1 } else { -1 };

        match granularity {
            CursorMovement::Grapheme => {
                let start_x = if delta > 0 { 0 } else { CoordType::MAX };

                loop {
                    let target_x = cursor.logical_pos.x + delta;

                    cursor = self.cursor_move_to_logical_internal(
                        cursor,
                        Point { x: target_x, y: cursor.logical_pos.y },
                    );

                    // We can stop if we ran out of remaining delta
                    // (or perhaps ran past the goal; in either case the sign would've changed),
                    // or if we hit the beginning or end of the buffer.
                    delta = target_x - cursor.logical_pos.x;
                    if delta.signum() != sign
                        || (delta < 0 && cursor.offset == 0)
                        || (delta > 0 && cursor.offset >= self.text_length())
                    {
                        break;
                    }

                    cursor = self.cursor_move_to_logical_internal(
                        cursor,
                        Point { x: start_x, y: cursor.logical_pos.y + sign },
                    );

                    // We crossed a newline which counts for 1 grapheme cluster.
                    // So, we also need to run the same check again.
                    delta -= sign;
                    if delta.signum() != sign
                        || cursor.offset == 0
                        || cursor.offset >= self.text_length()
                    {
                        break;
                    }
                }
            }
            CursorMovement::Word => {
                let doc = &self.buffer as &dyn ReadableDocument;
                let mut offset = self.cursor.offset;

                while delta != 0 {
                    if delta < 0 {
                        offset = navigation::word_backward(doc, offset);
                    } else {
                        offset = navigation::word_forward(doc, offset);
                    }
                    delta -= sign;
                }

                cursor = self.cursor_move_to_offset_internal(cursor, offset);
            }
        }

        cursor
    }

    /// Moves the cursor to the given offset.
    pub fn cursor_move_to_offset(&mut self, offset: usize) {
        unsafe { self.set_cursor(self.cursor_move_to_offset_internal(self.cursor, offset)) }
    }

    /// Moves the cursor to the given logical position.
    pub fn cursor_move_to_logical(&mut self, pos: Point) {
        unsafe { self.set_cursor(self.cursor_move_to_logical_internal(self.cursor, pos)) }
    }

    /// Moves the cursor to the given visual position.
    pub fn cursor_move_to_visual(&mut self, pos: Point) {
        unsafe { self.set_cursor(self.cursor_move_to_visual_internal(self.cursor, pos)) }
    }

    /// Moves the cursor by the given delta.
    pub fn cursor_move_delta(&mut self, granularity: CursorMovement, delta: CoordType) {
        unsafe { self.set_cursor(self.cursor_move_delta_internal(self.cursor, granularity, delta)) }
    }

    /// Moves all cursors (primary and secondary) by the given delta.
    /// This is used for arrow key navigation when multiple cursors are active.
    pub fn cursor_move_delta_all(&mut self, granularity: CursorMovement, delta: CoordType) {
        // Move primary cursor
        let new_primary = self.cursor_move_delta_internal(self.cursor, granularity, delta);
        unsafe { self.set_cursor(new_primary) };

        // Move all secondary cursors
        for i in 0..self.secondary_cursors.len() {
            let cursor = self.secondary_cursors[i].cursor;
            self.secondary_cursors[i].cursor =
                self.cursor_move_delta_internal(cursor, granularity, delta);
            self.secondary_cursors[i].selection = None; // Clear selection when moving without shift
        }
        self.normalize_cursors();
    }

    /// Extends selection for all cursors by the given delta.
    /// This is used for Shift+arrow navigation when multiple cursors are active.
    pub fn selection_update_delta_all(&mut self, granularity: CursorMovement, delta: CoordType) {
        // Update primary cursor selection
        self.set_cursor_for_selection(self.cursor_move_delta_internal(
            self.cursor,
            granularity,
            delta,
        ));

        // Update all secondary cursor selections
        for i in 0..self.secondary_cursors.len() {
            let beg = match self.secondary_cursors[i].selection {
                Some(TextBufferSelection { beg, .. }) => beg,
                None => self.secondary_cursors[i].cursor.logical_pos,
            };

            let cursor = self.secondary_cursors[i].cursor;
            self.secondary_cursors[i].cursor =
                self.cursor_move_delta_internal(cursor, granularity, delta);
            let end = self.secondary_cursors[i].cursor.logical_pos;
            self.secondary_cursors[i].selection =
                if beg == end { None } else { Some(TextBufferSelection { beg, end }) };
        }
        self.normalize_cursors();
    }

    /// Sets the cursor to the given position, and clears the selection.
    ///
    /// # Safety
    ///
    /// This function performs no checks that the cursor is valid. "Valid" in this case means
    /// that the TextBuffer has not been modified since you received the cursor from this class.
    pub unsafe fn set_cursor(&mut self, cursor: Cursor) {
        self.set_cursor_internal(cursor);
        self.last_history_type = HistoryType::Other;
        self.set_selection(None);
    }

    fn set_cursor_for_selection(&mut self, cursor: Cursor) {
        let beg = match self.selection {
            Some(TextBufferSelection { beg, .. }) => beg,
            None => self.cursor.logical_pos,
        };

        self.set_cursor_internal(cursor);
        self.last_history_type = HistoryType::Other;

        let end = self.cursor.logical_pos;
        self.set_selection(if beg == end { None } else { Some(TextBufferSelection { beg, end }) });
    }

    fn set_cursor_internal(&mut self, cursor: Cursor) {
        debug_assert!(
            cursor.offset <= self.text_length()
                && cursor.logical_pos.x >= 0
                && cursor.logical_pos.y >= 0
                && cursor.logical_pos.y <= self.stats.logical_lines
                && cursor.visual_pos.x >= 0
                && (self.word_wrap_column <= 0 || cursor.visual_pos.x <= self.word_wrap_column)
                && cursor.visual_pos.y >= 0
                && cursor.visual_pos.y <= self.stats.visual_lines
        );
        self.cursor = cursor;
    }

    /// Extracts a rectangular region of the text buffer and writes it to the framebuffer.
    /// The `destination` rect is framebuffer coordinates. The extracted region within this
    /// text buffer has the given `origin` and the same size as the `destination` rect.
    pub fn render(
        &mut self,
        origin: Point,
        destination: Rect,
        focused: bool,
        fb: &mut Framebuffer,
    ) -> Option<RenderResult> {
        if destination.is_empty() {
            return None;
        }

        let scratch = scratch_arena(None);
        let width = destination.width();
        let height = destination.height();
        let line_number_width = self.margin_width.max(3) as usize - 3;
        let text_width = width - self.margin_width;
        let mut visualizer_buf = [0xE2, 0x90, 0x80]; // U+2400 in UTF8
        let mut line = ArenaString::new_in(&scratch);
        let mut visual_pos_x_max = 0;

        // Pick the cursor closer to the `origin.y`.
        let mut cursor = {
            let a = self.cursor;
            let b = self.cursor_for_rendering.unwrap_or_default();
            let da = (a.visual_pos.y - origin.y).abs();
            let db = (b.visual_pos.y - origin.y).abs();
            if da < db { a } else { b }
        };

        let [selection_beg, selection_end] = match self.selection {
            None => [Point::MIN, Point::MIN],
            Some(TextBufferSelection { beg, end }) => minmax(beg, end),
        };

        // Get block selection bounds if active (top, bottom, left, right in visual coords)
        let block_bounds = self.block_selection_bounds();

        // Find matching bracket for highlighting (if cursor is on a bracket)
        let matching_bracket_offset =
            if focused { self.find_matching_bracket_offset() } else { None };
        // Also highlight the bracket under cursor if there's a match
        let cursor_bracket_offset =
            if matching_bracket_offset.is_some() { Some(self.cursor.offset) } else { None };

        line.reserve(width as usize * 2);

        for y in 0..height {
            line.clear();
            let mut selection_rect = None;

            let visual_line = origin.y + y;
            let mut cursor_beg =
                self.cursor_move_to_visual_internal(cursor, Point { x: origin.x, y: visual_line });
            let cursor_end = self.cursor_move_to_visual_internal(
                cursor_beg,
                Point { x: origin.x + text_width, y: visual_line },
            );

            // Accelerate the next render pass by remembering where we started off.
            if y == 0 {
                self.cursor_for_rendering = Some(cursor_beg);
            }

            if line_number_width != 0 {
                if visual_line >= self.stats.visual_lines {
                    // Past the end of the buffer? Place "    | " in the margin.
                    // Since we know that we won't see line numbers greater than i64::MAX (9223372036854775807)
                    // any time soon, we can use a static string as the template (`MARGIN`) and slice it,
                    // because `line_number_width` can't possibly be larger than 19.
                    let off = 19 - line_number_width;
                    unsafe { std::hint::assert_unchecked(off < MARGIN_TEMPLATE.len()) };
                    line.push_str(&MARGIN_TEMPLATE[off..]);
                } else if self.word_wrap_column <= 0 || cursor_beg.logical_pos.x == 0 {
                    // Regular line? Place "123 | " in the margin.
                    _ = write!(line, "{:1$} │ ", cursor_beg.logical_pos.y + 1, line_number_width);
                } else {
                    // Wrapped line? Place " ... | " in the margin.
                    let number_width = (cursor_beg.logical_pos.y + 1).ilog10() as usize + 1;
                    _ = write!(
                        line,
                        "{0:1$}{0:∙<2$} │ ",
                        "",
                        line_number_width - number_width,
                        number_width
                    );
                    // Blending in the background color will "dim" the indicator dots.
                    let left = destination.left;
                    let top = destination.top + y;
                    fb.blend_fg(
                        Rect {
                            left,
                            top,
                            right: left + line_number_width as CoordType,
                            bottom: top + 1,
                        },
                        fb.indexed_alpha(IndexedColor::Background, 1, 2),
                    );
                }
            }

            // Track the byte offset where content starts (after the margin).
            // This differs from margin_width (visual columns) due to multi-byte UTF-8 chars in the margin.
            let content_byte_start = line.len();

            // Figure out the selection range on this line, if any.
            // Block selection takes priority over regular selection.
            if let Some((block_top, block_bottom, block_left, block_right)) = block_bounds {
                // Check if this line is within the block selection
                if visual_line >= block_top && visual_line <= block_bottom {
                    let left = destination.left + self.margin_width - origin.x;
                    let top = destination.top + y;
                    let rect = Rect {
                        left: left + block_left.max(origin.x),
                        top,
                        right: left + block_right.min(origin.x + text_width),
                        bottom: top + 1,
                    };

                    let mut bg = fb
                        .indexed(IndexedColor::Foreground)
                        .oklab_blend(fb.indexed_alpha(IndexedColor::BrightBlue, 1, 2));
                    if !focused {
                        bg = bg.oklab_blend(fb.indexed_alpha(IndexedColor::Background, 1, 2));
                    };
                    let fg = fb.contrasted(bg);
                    selection_rect = Some((rect, bg, fg));
                }
            } else if cursor_beg.visual_pos.y == visual_line
                && selection_beg <= cursor_end.logical_pos
                && selection_end >= cursor_beg.logical_pos
            {
                let mut cursor = cursor_beg;

                // By default, we assume the entire line is selected.
                let mut selection_pos_beg = 0;
                let mut selection_pos_end = COORD_TYPE_SAFE_MAX;

                // The start of the selection is within this line. We need to update selection_beg.
                if selection_beg <= cursor_end.logical_pos
                    && selection_beg >= cursor_beg.logical_pos
                {
                    cursor = self.cursor_move_to_logical_internal(cursor, selection_beg);
                    selection_pos_beg = cursor.visual_pos.x;
                }

                // The end of the selection is within this line. We need to update selection_end.
                if selection_end <= cursor_end.logical_pos
                    && selection_end >= cursor_beg.logical_pos
                {
                    cursor = self.cursor_move_to_logical_internal(cursor, selection_end);
                    selection_pos_end = cursor.visual_pos.x;
                }

                let left = destination.left + self.margin_width - origin.x;
                let top = destination.top + y;
                let rect = Rect {
                    left: left + selection_pos_beg.max(origin.x),
                    top,
                    right: left + selection_pos_end.min(origin.x + text_width),
                    bottom: top + 1,
                };

                let mut bg = fb.indexed(IndexedColor::Foreground).oklab_blend(fb.indexed_alpha(
                    IndexedColor::BrightBlue,
                    1,
                    2,
                ));
                if !focused {
                    bg = bg.oklab_blend(fb.indexed_alpha(IndexedColor::Background, 1, 2));
                };
                let fg = fb.contrasted(bg);
                selection_rect = Some((rect, bg, fg));
            }

            // Nothing to do if the entire line is empty.
            if cursor_beg.offset != cursor_end.offset {
                // If we couldn't reach the left edge, we may have stopped short due to a wide glyph.
                // In that case we'll try to find the next character and then compute by how many
                // columns it overlaps the left edge (can be anything between 1 and 7).
                if cursor_beg.visual_pos.x < origin.x {
                    let cursor_next = self.cursor_move_to_logical_internal(
                        cursor_beg,
                        Point { x: cursor_beg.logical_pos.x + 1, y: cursor_beg.logical_pos.y },
                    );

                    if cursor_next.visual_pos.x > origin.x {
                        let overlap = cursor_next.visual_pos.x - origin.x;
                        debug_assert!((1..=7).contains(&overlap));
                        line.push_str(&TAB_WHITESPACE[..overlap as usize]);
                        cursor_beg = cursor_next;
                    }
                }

                let mut global_off = cursor_beg.offset;
                let mut cursor_line = cursor_beg;

                while global_off < cursor_end.offset {
                    let chunk = self.read_forward(global_off);
                    let chunk = &chunk[..chunk.len().min(cursor_end.offset - global_off)];
                    let mut it = Utf8Chars::new(chunk, 0);

                    // TODO: Looping char-by-char is bad for performance.
                    // >25% of the total rendering time is spent here.
                    loop {
                        let chunk_off = it.offset();
                        let global_off = global_off + chunk_off;
                        let Some(ch) = it.next() else {
                            break;
                        };

                        if ch == ' ' || ch == '\t' {
                            let is_tab = ch == '\t';
                            let visualize = self.show_whitespace;
                            let mut whitespace = TAB_WHITESPACE;
                            let mut prefix_add = 0;

                            if is_tab || visualize {
                                // We need the character's visual position in order to either compute the tab size,
                                // or set the foreground color of the visualizer, respectively.
                                // TODO: Doing this char-by-char is of course also bad for performance.
                                cursor_line =
                                    self.cursor_move_to_offset_internal(cursor_line, global_off);
                            }

                            let tab_size =
                                if is_tab { self.tab_size_eval(cursor_line.column) } else { 1 };

                            if visualize {
                                // If the whitespace is part of the selection,
                                // we replace " " with "･" and "\t" with "￫".
                                (whitespace, prefix_add) = if is_tab {
                                    (VISUAL_TAB, VISUAL_TAB_PREFIX_ADD)
                                } else {
                                    (VISUAL_SPACE, VISUAL_SPACE_PREFIX_ADD)
                                };

                                // Make the visualized characters slightly gray.
                                let visualizer_rect = {
                                    let left = destination.left
                                        + self.margin_width
                                        + cursor_line.visual_pos.x
                                        - origin.x;
                                    let top = destination.top + cursor_line.visual_pos.y - origin.y;
                                    Rect { left, top, right: left + 1, bottom: top + 1 }
                                };
                                fb.blend_fg(
                                    visualizer_rect,
                                    fb.indexed_alpha(IndexedColor::Foreground, 1, 2),
                                );
                            }

                            line.push_str(&whitespace[..prefix_add + tab_size as usize]);
                        } else if ch <= '\x1f' || ('\u{7f}'..='\u{9f}').contains(&ch) {
                            // Append a Unicode representation of the C0 or C1 control character.
                            visualizer_buf[2] = if ch <= '\x1f' {
                                0x80 | ch as u8 // U+2400..=U+241F
                            } else if ch == '\x7f' {
                                0xA1 // U+2421
                            } else {
                                0xA6 // U+2426, because there are no pictures for C1 control characters.
                            };

                            // Our manually constructed UTF8 is never going to be invalid. Trust.
                            line.push_str(unsafe { str::from_utf8_unchecked(&visualizer_buf) });

                            // Highlight the control character yellow.
                            cursor_line =
                                self.cursor_move_to_offset_internal(cursor_line, global_off);
                            let visualizer_rect = {
                                let left =
                                    destination.left + self.margin_width + cursor_line.visual_pos.x
                                        - origin.x;
                                let top = destination.top + cursor_line.visual_pos.y - origin.y;
                                Rect { left, top, right: left + 1, bottom: top + 1 }
                            };
                            let bg = fb.indexed(IndexedColor::Yellow);
                            let fg = fb.contrasted(bg);
                            fb.blend_bg(visualizer_rect, bg);
                            fb.blend_fg(visualizer_rect, fg);
                        } else {
                            line.push(ch);
                        }
                    }

                    global_off += chunk.len();
                }

                visual_pos_x_max = visual_pos_x_max.max(cursor_end.visual_pos.x);
            }

            fb.replace_text(destination.top + y, destination.left, destination.right, &line);

            if let Some(highlight) = &self.search_highlight {
                if !highlight.options.use_regex {
                    let content = line.get(content_byte_start..).unwrap_or("");
                    let matches = highlight::find_search_matches(
                        content,
                        &highlight.needle,
                        highlight.options,
                    );
                    if !matches.is_empty() {
                        let base_left = destination.left + self.margin_width;
                        let top = destination.top + y;
                        let color = fb.indexed_alpha(IndexedColor::BrightYellow, 1, 3);

                        for range in matches {
                            let left = base_left
                                + highlight::count_columns(content, range.start) as CoordType;
                            let right = base_left
                                + highlight::count_columns(content, range.end) as CoordType;
                            if left < right {
                                fb.blend_bg(Rect { left, top, right, bottom: top + 1 }, color);
                            }
                        }
                    }
                }
            }

            if self.language != Language::PlainText {
                let content = line.get(content_byte_start..).unwrap_or("");
                let spans = Self::highlight_line(self.language, content);
                if !spans.is_empty() {
                    let top = destination.top + y;
                    let base_left = destination.left + self.margin_width;

                    for span in spans {
                        if span.start >= span.end {
                            continue;
                        }

                        let left = base_left + span.start as CoordType;
                        let right = (base_left + span.end as CoordType).min(destination.right);
                        if left >= right {
                            continue;
                        }

                        let color = match span.kind {
                            HighlightKind::Comment => fb.indexed(IndexedColor::BrightGreen),
                            HighlightKind::String => fb.indexed(IndexedColor::BrightBlue),
                            HighlightKind::Number => fb.indexed(IndexedColor::BrightMagenta),
                            HighlightKind::Keyword => fb.indexed(IndexedColor::BrightYellow),
                            HighlightKind::CsvColumn(col) => {
                                const CSV_COLORS: [IndexedColor; 8] = [
                                    IndexedColor::BrightCyan,
                                    IndexedColor::BrightYellow,
                                    IndexedColor::BrightMagenta,
                                    IndexedColor::BrightGreen,
                                    IndexedColor::BrightBlue,
                                    IndexedColor::Cyan,
                                    IndexedColor::Yellow,
                                    IndexedColor::Magenta,
                                ];
                                fb.indexed(CSV_COLORS[(col % 8) as usize])
                            }
                            HighlightKind::Section => fb.indexed(IndexedColor::BrightYellow),
                            HighlightKind::Key => fb.indexed(IndexedColor::BrightCyan),
                        };
                        fb.blend_fg(Rect { left, top, right, bottom: top + 1 }, color);
                    }
                }
            }

            // Highlight matching bracket pair if on this line
            for bracket_off in
                [matching_bracket_offset, cursor_bracket_offset].into_iter().flatten()
            {
                if bracket_off >= cursor_beg.offset && bracket_off < cursor_end.offset {
                    // Calculate visual column for the bracket
                    let content = line.get(content_byte_start..).unwrap_or("");
                    let offset_in_line = bracket_off - cursor_beg.offset;

                    // Count columns from start of content to the bracket position
                    let mut col: CoordType = 0;
                    let mut byte_pos = 0;
                    for ch in content.chars() {
                        if byte_pos >= offset_in_line {
                            break;
                        }
                        byte_pos += ch.len_utf8();
                        col += if ch == '\t' { self.tab_size - (col % self.tab_size) } else { 1 };
                    }

                    let left = destination.left + self.margin_width + col;
                    let top = destination.top + y;
                    if left < destination.right {
                        // Highlight with a visible background color
                        fb.blend_bg(
                            Rect { left, top, right: left + 1, bottom: top + 1 },
                            fb.indexed_alpha(IndexedColor::BrightYellow, 1, 2),
                        );
                    }
                }
            }

            if let Some((rect, bg, fg)) = selection_rect {
                fb.blend_bg(rect, bg);
                fb.blend_fg(rect, fg);
            }

            cursor = cursor_end;
        }

        // Colorize the margin that we wrote above.
        if self.margin_width > 0 {
            let margin = Rect {
                left: destination.left,
                top: destination.top,
                right: destination.left + self.margin_width,
                bottom: destination.bottom,
            };
            fb.blend_fg(margin, StraightRgba::from_le(0x7f7f7f7f));
        }

        if self.ruler > 0 {
            let left = destination.left + self.margin_width + (self.ruler - origin.x).max(0);
            let right = destination.right;
            if left < right {
                fb.blend_bg(
                    Rect { left, top: destination.top, right, bottom: destination.bottom },
                    fb.indexed_alpha(IndexedColor::BrightRed, 1, 4),
                );
            }
        }

        if focused {
            let text = Rect {
                left: destination.left + self.margin_width,
                top: destination.top,
                right: destination.right,
                bottom: destination.bottom,
            };

            // Helper to convert cursor visual position to screen position
            let cursor_to_screen = |cursor_visual: Point| -> Point {
                let mut x = cursor_visual.x;
                let mut y = cursor_visual.y;

                if self.word_wrap_column > 0 && x >= self.word_wrap_column {
                    // The line the cursor is on wraps exactly on the word wrap column which
                    // means the cursor is invisible. We need to move it to the next line.
                    x = 0;
                    y += 1;
                }

                // Move the cursor into screen space.
                x += destination.left - origin.x + self.margin_width;
                y += destination.top - origin.y;

                Point { x, y }
            };

            // Render primary cursor
            let primary_cursor = cursor_to_screen(self.cursor.visual_pos);
            if text.contains(primary_cursor) {
                fb.set_cursor(primary_cursor, self.overtype);

                if self.line_highlight_enabled && selection_beg >= selection_end {
                    fb.blend_bg(
                        Rect {
                            left: destination.left,
                            top: primary_cursor.y,
                            right: destination.right,
                            bottom: primary_cursor.y + 1,
                        },
                        StraightRgba::from_le(0x7f7f7f7f),
                    );
                }
            }

            // Render secondary cursors
            for sc in &self.secondary_cursors {
                let secondary_cursor = cursor_to_screen(sc.cursor.visual_pos);
                if text.contains(secondary_cursor) {
                    // Secondary cursors are rendered as block cursors with a different color
                    fb.set_cursor(secondary_cursor, true); // Always block for secondary cursors
                }
            }
        }

        Some(RenderResult { visual_pos_x_max })
    }

    pub fn cut(&mut self, clipboard: &mut Clipboard) {
        self.cut_copy(clipboard, true);
    }

    pub fn copy(&mut self, clipboard: &mut Clipboard) {
        self.cut_copy(clipboard, false);
    }

    fn cut_copy(&mut self, clipboard: &mut Clipboard, cut: bool) {
        let line_copy = !self.has_selection();
        let selection = self.extract_selection(cut);
        clipboard.write(selection);
        clipboard.write_was_line_copy(line_copy);
    }

    pub fn paste(&mut self, clipboard: &Clipboard) {
        let data = clipboard.read();
        if data.is_empty() {
            return;
        }

        let pos = self.cursor_logical_pos();
        let at = if clipboard.is_line_copy() {
            self.goto_line_start(self.cursor, pos.y)
        } else {
            self.cursor
        };

        self.write(data, at, true);

        if clipboard.is_line_copy() {
            self.cursor_move_to_logical(Point { x: pos.x, y: pos.y + 1 });
        }
    }

    /// Inserts the user input `text` at the current cursor position.
    /// Replaces tabs with whitespace if needed, etc.
    pub fn write_canon(&mut self, text: &[u8]) {
        self.write(text, self.cursor, false);
    }

    /// Inserts `text` as-is at the current cursor position.
    /// The only transformation applied is that newlines are normalized.
    pub fn write_raw(&mut self, text: &[u8]) {
        self.write(text, self.cursor, true);
    }

    /// Inserts text at all cursor positions (primary and secondary).
    /// Processes from end to start to avoid offset shifting issues.
    pub fn write_canon_all(&mut self, text: &[u8]) {
        // Convert block selection to multiple cursors before editing
        if self.has_block_selection() {
            self.block_selection_to_cursors();
        }

        if self.secondary_cursors.is_empty() {
            self.write_canon(text);
            return;
        }

        // Collect all cursors with their selections, sorted by offset descending
        let mut cursors_data: Vec<(Cursor, Option<TextBufferSelection>)> = Vec::new();
        cursors_data.push((self.cursor, self.selection));
        for sc in &self.secondary_cursors {
            cursors_data.push((sc.cursor, sc.selection));
        }
        // Sort by offset descending (process from end to start)
        cursors_data.sort_by(|a, b| b.0.offset.cmp(&a.0.offset));

        self.edit_begin_grouping();

        // Process each cursor from end to start
        for (cursor, selection) in cursors_data {
            // Temporarily set the primary cursor and selection
            self.cursor = cursor;
            self.selection = selection;
            self.write(text, self.cursor, false);
        }

        self.edit_end_grouping();

        // Clear secondary cursors - after multi-cursor edit they'll need to be
        // recalculated based on the new text positions
        self.secondary_cursors.clear();
    }

    /// Deletes at all cursor positions (primary and secondary).
    /// Uses selection if present, otherwise deletes based on granularity and delta.
    pub fn delete_all(&mut self, granularity: CursorMovement, delta: CoordType) {
        // Convert block selection to multiple cursors before editing
        if self.has_block_selection() {
            self.block_selection_to_cursors();
        }

        if self.secondary_cursors.is_empty() {
            self.delete(granularity, delta);
            return;
        }

        if delta == 0 {
            return;
        }

        // Collect all deletion ranges, sorted by offset descending
        let mut deletions: Vec<(Cursor, Cursor)> = Vec::new();

        // Primary cursor
        if let Some((beg, end)) = self.selection_range_internal(false) {
            deletions.push((beg, end));
        } else if !((delta < 0 && self.cursor.offset == 0)
            || (delta > 0 && self.cursor.offset >= self.text_length()))
        {
            let mut beg = self.cursor;
            let mut end = self.cursor_move_delta_internal(beg, granularity, delta);
            if beg.offset > end.offset {
                mem::swap(&mut beg, &mut end);
            }
            if beg.offset != end.offset {
                deletions.push((beg, end));
            }
        }

        // Secondary cursors
        for sc in &self.secondary_cursors {
            if let Some(sel) = sc.selection {
                let [beg_pt, end_pt] = minmax(sel.beg, sel.end);
                let beg = self.cursor_move_to_logical_internal(sc.cursor, beg_pt);
                let end = self.cursor_move_to_logical_internal(beg, end_pt);
                if beg.offset < end.offset {
                    deletions.push((beg, end));
                }
            } else if !((delta < 0 && sc.cursor.offset == 0)
                || (delta > 0 && sc.cursor.offset >= self.text_length()))
            {
                let mut beg = sc.cursor;
                let mut end = self.cursor_move_delta_internal(beg, granularity, delta);
                if beg.offset > end.offset {
                    mem::swap(&mut beg, &mut end);
                }
                if beg.offset != end.offset {
                    deletions.push((beg, end));
                }
            }
        }

        if deletions.is_empty() {
            return;
        }

        // Sort by offset descending (process from end to start)
        deletions.sort_by(|a, b| b.0.offset.cmp(&a.0.offset));

        self.edit_begin_grouping();

        // Process each deletion from end to start
        for (beg, end) in deletions {
            self.edit_begin(HistoryType::Delete, beg);
            self.edit_delete(end);
            self.edit_end();
        }

        self.edit_end_grouping();

        // Clear secondary cursors and selections
        self.secondary_cursors.clear();
        self.set_selection(None);
    }

    fn write(&mut self, text: &[u8], at: Cursor, raw: bool) {
        let history_type = if raw { HistoryType::Other } else { HistoryType::Write };
        let mut edit_begun = false;

        // If we have an active selection, writing an empty `text`
        // will still delete the selection. As such, we check this first.
        if let Some((beg, end)) = self.selection_range_internal(false) {
            self.edit_begin(history_type, beg);
            self.edit_delete(end);
            self.set_selection(None);
            edit_begun = true;
        }

        // If the text is empty the remaining code won't do anything,
        // allowing us to exit early.
        if text.is_empty() {
            // ...we still need to end any active edit session though.
            if edit_begun {
                self.edit_end();
            }
            return;
        }

        if !edit_begun {
            self.edit_begin(history_type, at);
        }

        let mut offset = 0;
        let scratch = scratch_arena(None);
        let mut newline_buffer = ArenaString::new_in(&scratch);

        loop {
            // Can't use `unicode::newlines_forward` because bracketed paste uses CR instead of LF/CRLF.
            let offset_next = memchr2(b'\r', b'\n', text, offset);
            let line = &text[offset..offset_next];
            let column_before = self.cursor.logical_pos.x;

            // Write the contents of the line into the buffer.
            let mut line_off = 0;
            while line_off < line.len() {
                // Split the line into chunks of non-tabs and tabs.
                let mut plain = line;
                if !raw && !self.indent_with_tabs {
                    let end = memchr2(b'\t', b'\t', line, line_off);
                    plain = &line[line_off..end];
                }

                // Non-tabs are written as-is, because the outer loop already handles newline translation.
                self.edit_write(plain);
                line_off += plain.len();

                // Now replace tabs with spaces.
                while line_off < line.len() && line[line_off] == b'\t' {
                    let spaces = self.tab_size_eval(self.cursor.column);
                    let spaces = &TAB_WHITESPACE.as_bytes()[..spaces as usize];
                    self.edit_write(spaces);
                    line_off += 1;
                }
            }

            if !raw && self.overtype {
                let delete = self.cursor.logical_pos.x - column_before;
                let end = self.cursor_move_to_logical_internal(
                    self.cursor,
                    Point { x: self.cursor.logical_pos.x + delete, y: self.cursor.logical_pos.y },
                );
                self.edit_delete(end);
            }

            offset += line.len();
            if offset >= text.len() {
                break;
            }

            // First, write the newline.
            newline_buffer.clear();
            newline_buffer.push_str(if self.newlines_are_crlf { "\r\n" } else { "\n" });

            if !raw {
                // We'll give the next line the same indentation as the previous one.
                // This block figures out how much that is. We can't reuse that value,
                // because "  a\n  a\n" should give the 3rd line a total indentation of 4.
                // Assuming your terminal has bracketed paste, this won't be a concern though.
                // (If it doesn't, use a different terminal.)
                let line_beg = self.goto_line_start(self.cursor, self.cursor.logical_pos.y);
                let limit = self.cursor.offset;
                let mut off = line_beg.offset;
                let mut newline_indentation = 0;

                'outer: while off < limit {
                    let chunk = self.read_forward(off);
                    let chunk = &chunk[..chunk.len().min(limit - off)];

                    for &c in chunk {
                        if c == b' ' {
                            newline_indentation += 1;
                        } else if c == b'\t' {
                            newline_indentation += self.tab_size_eval(newline_indentation);
                        } else {
                            break 'outer;
                        }
                    }

                    off += chunk.len();
                }

                // If tabs are enabled, add as many tabs as we can.
                if self.indent_with_tabs {
                    let tab_count = newline_indentation / self.tab_size;
                    newline_buffer.push_repeat('\t', tab_count as usize);
                    newline_indentation -= tab_count * self.tab_size;
                }

                // If tabs are disabled, or if the indentation wasn't a multiple of the tab size,
                // add spaces to make up the difference.
                newline_buffer.push_repeat(' ', newline_indentation as usize);
            }

            self.edit_write(newline_buffer.as_bytes());

            // Skip one CR/LF/CRLF.
            if offset >= text.len() {
                break;
            }
            if text[offset] == b'\r' {
                offset += 1;
            }
            if offset >= text.len() {
                break;
            }
            if text[offset] == b'\n' {
                offset += 1;
            }
            if offset >= text.len() {
                break;
            }
        }

        // POSIX mandates that all valid lines end in a newline.
        // This isn't all that common on Windows and so we have
        // `self.final_newline` to control this.
        //
        // In order to not annoy people with this, we only add a
        // newline if you just edited the very end of the buffer.
        if self.insert_final_newline
            && self.cursor.offset > 0
            && self.cursor.offset == self.text_length()
            && self.cursor.logical_pos.x > 0
        {
            let cursor = self.cursor;
            self.edit_write(if self.newlines_are_crlf { b"\r\n" } else { b"\n" });
            self.set_cursor_internal(cursor);
        }

        self.edit_end();
    }

    /// Deletes 1 grapheme cluster from the buffer.
    /// `cursor_movements` is expected to be -1 for backspace and 1 for delete.
    /// If there's a current selection, it will be deleted and `cursor_movements` ignored.
    /// The selection is cleared after the call.
    /// Deletes characters from the buffer based on a delta from the cursor.
    pub fn delete(&mut self, granularity: CursorMovement, delta: CoordType) {
        if delta == 0 {
            return;
        }

        let mut beg;
        let mut end;

        if let Some(r) = self.selection_range_internal(false) {
            (beg, end) = r;
        } else {
            if (delta < 0 && self.cursor.offset == 0)
                || (delta > 0 && self.cursor.offset >= self.text_length())
            {
                // Nothing to delete.
                return;
            }

            beg = self.cursor;
            end = self.cursor_move_delta_internal(beg, granularity, delta);
            if beg.offset == end.offset {
                return;
            }
            if beg.offset > end.offset {
                mem::swap(&mut beg, &mut end);
            }
        }

        self.edit_begin(HistoryType::Delete, beg);
        self.edit_delete(end);
        self.edit_end();

        self.set_selection(None);
    }

    /// Returns the logical position of the first character on this line.
    /// Return `.x == 0` if there are no non-whitespace characters.
    pub fn indent_end_logical_pos(&self) -> Point {
        let cursor = self.goto_line_start(self.cursor, self.cursor.logical_pos.y);
        let (chars, _) = self.measure_indent_internal(cursor.offset, CoordType::MAX);
        Point { x: chars, y: cursor.logical_pos.y }
    }

    /// Indents/unindents the current selection or line.
    pub fn indent_change(&mut self, direction: CoordType) {
        let selection = self.selection;
        let mut selection_beg = self.cursor.logical_pos;
        let mut selection_end = selection_beg;

        if let Some(TextBufferSelection { beg, end }) = &selection {
            selection_beg = *beg;
            selection_end = *end;
        }

        if direction >= 0 && self.selection.is_none_or(|sel| sel.beg.y == sel.end.y) {
            self.write_canon(b"\t");
            return;
        }

        self.edit_begin_grouping();

        for y in selection_beg.y.min(selection_end.y)..=selection_beg.y.max(selection_end.y) {
            self.cursor_move_to_logical(Point { x: 0, y });

            let line_start_offset = self.cursor.offset;
            let (curr_chars, curr_columns) =
                self.measure_indent_internal(line_start_offset, CoordType::MAX);

            self.cursor_move_to_logical(Point { x: curr_chars, y: self.cursor.logical_pos.y });

            let delta;

            if direction < 0 {
                // Unindent the line. If there's no indentation, skip.
                if curr_columns <= 0 {
                    continue;
                }

                let (prev_chars, _) = self.measure_indent_internal(
                    line_start_offset,
                    self.tab_size_prev_column(curr_columns),
                );

                delta = prev_chars - curr_chars;
                self.delete(CursorMovement::Grapheme, delta);
            } else {
                // Indent the line. `self.cursor` is already at the level of indentation.
                delta = self.tab_size_eval(curr_columns);
                self.write_canon(b"\t");
            }

            // As the lines get unindented, the selection should shift with them.
            if y == selection_beg.y {
                selection_beg.x += delta;
            }
            if y == selection_end.y {
                selection_end.x += delta;
            }
        }
        self.edit_end_grouping();

        // Move the cursor to the new end of the selection.
        self.set_cursor_internal(self.cursor_move_to_logical_internal(self.cursor, selection_end));

        // NOTE: If the selection was previously `None`,
        // it should continue to be `None` after this.
        self.set_selection(
            selection.map(|_| TextBufferSelection { beg: selection_beg, end: selection_end }),
        );
    }

    fn measure_indent_internal(
        &self,
        mut offset: usize,
        max_columns: CoordType,
    ) -> (CoordType, CoordType) {
        let mut chars = 0;
        let mut columns = 0;

        'outer: loop {
            let chunk = self.read_forward(offset);
            if chunk.is_empty() {
                break;
            }

            for &c in chunk {
                let next = match c {
                    b' ' => columns + 1,
                    b'\t' => columns + self.tab_size_eval(columns),
                    _ => break 'outer,
                };
                if next > max_columns {
                    break 'outer;
                }
                chars += 1;
                columns = next;
            }

            offset += chunk.len();

            // No need to do another round if we
            // already got the exact right amount.
            if columns >= max_columns {
                break;
            }
        }

        (chars, columns)
    }

    /// Displaces the current, cursor or the selection, line(s) in the given direction.
    pub fn move_selected_lines(&mut self, direction: MoveLineDirection) {
        let selection = self.selection;
        let cursor = self.cursor;

        // If there's no selection, we move the line the cursor is on instead.
        let [beg, end] = match self.selection {
            Some(s) => minmax(s.beg.y, s.end.y),
            None => [cursor.logical_pos.y, cursor.logical_pos.y],
        };

        // Check if this would be a no-op.
        if match direction {
            MoveLineDirection::Up => beg <= 0,
            MoveLineDirection::Down => end >= self.stats.logical_lines - 1,
        } {
            return;
        }

        let delta = match direction {
            MoveLineDirection::Up => -1,
            MoveLineDirection::Down => 1,
        };
        let (cut, paste) = match direction {
            MoveLineDirection::Up => (beg - 1, end),
            MoveLineDirection::Down => (end + 1, beg),
        };

        self.edit_begin_grouping();
        {
            // Let's say this is `MoveLineDirection::Up`.
            // In that case, we'll cut (remove) the line above the selection here...
            self.cursor_move_to_logical(Point { x: 0, y: cut });
            let line = self.extract_selection(true);

            // ...and paste it below the selection. This will then
            // appear to the user as if the selection was moved up.
            self.cursor_move_to_logical(Point { x: 0, y: paste });
            self.edit_begin(HistoryType::Write, self.cursor);
            // The `extract_selection` call can return an empty `Vec`),
            // if the `cut` line was at the end of the file. Since we want to
            // paste the line somewhere it needs a trailing newline at the minimum.
            //
            // Similarly, if the `paste` line is at the end of the file
            // and there's no trailing newline, we'll have failed to reach
            // that end in which case `logical_pos.y != past`.
            if line.is_empty() || self.cursor.logical_pos.y != paste {
                self.write_canon(b"\n");
            }
            if !line.is_empty() {
                self.write_raw(&line);
            }
            self.edit_end();
        }
        self.edit_end_grouping();

        // Shift the cursor and selection together with the moved lines.
        self.cursor_move_to_logical(Point {
            x: cursor.logical_pos.x,
            y: cursor.logical_pos.y + delta,
        });
        self.set_selection(selection.map(|mut s| {
            s.beg.y += delta;
            s.end.y += delta;
            s
        }));
    }

    /// Duplicates the current line or selected lines.
    pub fn duplicate_lines(&mut self) {
        let cursor = self.cursor;

        // Get the line range to duplicate
        let [beg_y, end_y] = match self.selection {
            Some(s) => minmax(s.beg.y, s.end.y),
            None => [cursor.logical_pos.y, cursor.logical_pos.y],
        };

        // Move to start of first line and get the content
        let start_cursor =
            self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y: beg_y });
        let end_cursor =
            self.cursor_move_to_logical_internal(start_cursor, Point { x: 0, y: end_y + 1 });

        // Extract the lines
        let mut content = Vec::new();
        self.buffer.extract_raw(start_cursor.offset..end_cursor.offset, &mut content, 0);

        // Ensure content ends with newline
        if !content.ends_with(b"\n") {
            content.extend_from_slice(if self.newlines_are_crlf { b"\r\n" } else { b"\n" });
        }

        // Insert at end of last line
        self.cursor_move_to_logical(Point { x: 0, y: end_y + 1 });
        self.edit_begin(HistoryType::Write, self.cursor);
        self.write_raw(&content);
        self.edit_end();

        // Move cursor down to the duplicated content
        self.cursor_move_to_logical(Point {
            x: cursor.logical_pos.x,
            y: cursor.logical_pos.y + (end_y - beg_y + 1),
        });
    }

    /// Deletes the current line entirely.
    pub fn delete_current_line(&mut self) {
        let y = self.cursor.logical_pos.y;

        // Select the entire line
        let start_cursor = self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y });
        let end_cursor =
            self.cursor_move_to_logical_internal(start_cursor, Point { x: 0, y: y + 1 });

        if start_cursor.offset < end_cursor.offset {
            self.edit_begin(HistoryType::Delete, start_cursor);
            self.edit_delete(end_cursor);
            self.edit_end();
        }

        // Stay at beginning of line
        self.cursor_move_to_logical(Point { x: 0, y });
    }

    /// Joins the current line with the next line (or joins all selected lines).
    pub fn join_lines(&mut self) {
        let cursor = self.cursor;

        // Get the line range
        let [beg_y, end_y] = match self.selection {
            Some(s) => minmax(s.beg.y, s.end.y),
            None => [cursor.logical_pos.y, cursor.logical_pos.y + 1],
        };

        // Can't join if there's only one line or we're at the last line
        if end_y <= beg_y || beg_y >= self.stats.logical_lines - 1 {
            return;
        }

        self.edit_begin_grouping();

        // Work from end to beginning to avoid offset issues
        for y in (beg_y..end_y).rev() {
            // Find the end of line y (position before newline)
            let line_end =
                self.cursor_move_to_logical_internal(self.cursor, Point { x: CoordType::MAX, y });

            // Find start of next line
            let next_line_start =
                self.cursor_move_to_logical_internal(line_end, Point { x: 0, y: y + 1 });

            // Delete newline and leading whitespace, replace with single space
            if line_end.offset < next_line_start.offset {
                // First, find where whitespace ends on the next line
                let mut ws_end = next_line_start;
                let text = self.buffer.read_forward(next_line_start.offset);
                let mut ws_len = 0;
                for &b in text {
                    if b == b' ' || b == b'\t' {
                        ws_len += 1;
                    } else {
                        break;
                    }
                }

                ws_end.offset += ws_len;

                self.cursor_move_to_offset(line_end.offset);
                self.edit_begin(HistoryType::Delete, self.cursor);
                self.edit_delete(ws_end);
                self.edit_end();

                // Insert single space
                self.edit_begin(HistoryType::Write, self.cursor);
                self.write_raw(b" ");
                self.edit_end();
            }
        }

        self.edit_end_grouping();
        self.set_selection(None);
    }

    /// Extracts the contents of the current selection.
    /// May optionally delete it, if requested. This is meant to be used for Ctrl+X.
    fn extract_selection(&mut self, delete: bool) -> Vec<u8> {
        let line_copy = !self.has_selection();
        let Some((beg, end)) = self.selection_range_internal(true) else {
            return Vec::new();
        };

        let mut out = Vec::new();
        self.buffer.extract_raw(beg.offset..end.offset, &mut out, 0);

        if delete && !out.is_empty() {
            self.edit_begin(HistoryType::Delete, beg);
            self.edit_delete(end);
            self.edit_end();
            self.set_selection(None);
        }

        // Line copies (= Ctrl+C when there's no selection) always end with a newline.
        if line_copy && !out.ends_with(b"\n") {
            out.replace_range(out.len().., if self.newlines_are_crlf { b"\r\n" } else { b"\n" });
        }

        out
    }

    /// Extracts the contents of the current selection the user made.
    /// This differs from [`TextBuffer::extract_selection()`] in that
    /// it does nothing if the selection was made by searching.
    pub fn extract_user_selection(&mut self, delete: bool) -> Option<Vec<u8>> {
        if !self.has_selection() {
            return None;
        }

        if let Some(search) = &self.search {
            let search = unsafe { &*search.get() };
            if search.selection_generation == self.selection_generation {
                return None;
            }
        }

        Some(self.extract_selection(delete))
    }

    /// Returns the current selection anchors, or `None` if there
    /// is no selection. The returned logical positions are sorted.
    pub fn selection_range(&self) -> Option<(Cursor, Cursor)> {
        self.selection_range_internal(false)
    }

    /// Returns the current selection anchors.
    ///
    /// If there's no selection and `line_fallback` is `true`,
    /// the start/end of the current line are returned.
    /// This is meant to be used for Ctrl+C / Ctrl+X.
    fn selection_range_internal(&self, line_fallback: bool) -> Option<(Cursor, Cursor)> {
        let [beg, end] = match self.selection {
            None if !line_fallback => return None,
            None => [
                Point { x: 0, y: self.cursor.logical_pos.y },
                Point { x: 0, y: self.cursor.logical_pos.y + 1 },
            ],
            Some(TextBufferSelection { beg, end }) => minmax(beg, end),
        };

        let beg = self.cursor_move_to_logical_internal(self.cursor, beg);
        let end = self.cursor_move_to_logical_internal(beg, end);

        if beg.offset < end.offset { Some((beg, end)) } else { None }
    }

    /// For interfacing with ICU.
    pub(crate) fn read_backward(&self, off: usize) -> &[u8] {
        self.buffer.read_backward(off)
    }

    /// For interfacing with ICU.
    pub fn read_forward(&self, off: usize) -> &[u8] {
        self.buffer.read_forward(off)
    }
}

pub enum Bom {
    None,
    UTF8,
    UTF16LE,
    UTF16BE,
    UTF32LE,
    UTF32BE,
    GB18030,
}

/// Counts words in a byte slice.
/// A word is a sequence of non-whitespace characters.
fn count_words(text: &[u8]) -> usize {
    let mut count = 0;
    let mut in_word = false;

    for &byte in text {
        let is_whitespace = byte.is_ascii_whitespace();
        if in_word && is_whitespace {
            in_word = false;
        } else if !in_word && !is_whitespace {
            in_word = true;
            count += 1;
        }
    }

    count
}

/// Finds the first occurrence of a subsequence in a byte slice.
/// Returns the starting index or None if not found.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }

    haystack.windows(needle.len()).position(|window| window == needle)
}
