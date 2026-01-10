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

mod gap_buffer;
mod navigation;

use std::borrow::Cow;
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

        self.search_highlight = Some(SearchHighlight {
            needle: needle.to_string(),
            options,
        });
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

    /// Reads a file from disk into the text buffer, detecting encoding and BOM.
    pub fn read_file(
        &mut self,
        file: &mut File,
        encoding: Option<&'static str>,
    ) -> apperr::Result<()> {
        let scratch = scratch_arena(None);
        let mut buf = scratch.alloc_uninit().transpose();
        let mut first_chunk_len = 0;
        let mut read = 0;

        // Read enough bytes to detect the BOM.
        while first_chunk_len < BOM_MAX_LEN {
            read = file_read_uninit(file, &mut buf[first_chunk_len..])?;
            if read == 0 {
                break;
            }
            first_chunk_len += read;
        }

        if let Some(encoding) = encoding {
            self.encoding = encoding;
        } else {
            let bom = detect_bom(unsafe { buf[..first_chunk_len].assume_init_ref() });
            self.encoding = bom.unwrap_or("UTF-8");
        }

        // TODO: Since reading the file can fail, we should ensure that we also reset the cursor here.
        // I don't do it, so that `recalc_after_content_swap()` works.
        self.buffer.clear();

        let done = read == 0;
        if self.encoding == "UTF-8" {
            self.read_file_as_utf8(file, &mut buf, first_chunk_len, done)?;
        } else {
            self.read_file_with_icu(file, &mut buf, first_chunk_len, done)?;
        }

        // Figure out
        // * the logical line count
        // * the newline type (LF or CRLF)
        // * the indentation type (tabs or spaces)
        // * whether there's a final newline
        {
            let chunk = self.read_forward(0);
            let mut offset = 0;
            let mut lines = 0;
            // Number of lines ending in CRLF.
            let mut crlf_count = 0;
            // Number of lines starting with a tab.
            let mut tab_indentations = 0;
            // Number of lines starting with a space.
            let mut space_indentations = 0;
            // Histogram of the indentation depth of lines starting with between 2 and 8 spaces.
            // In other words, `space_indentation_sizes[0]` is the number of lines starting with 2 spaces.
            let mut space_indentation_sizes = [0; 7];

            loop {
                // Check if the line starts with a tab.
                if offset < chunk.len() && chunk[offset] == b'\t' {
                    tab_indentations += 1;
                } else {
                    // Otherwise, check how many spaces the line starts with. Searching for >8 spaces
                    // allows us to reject lines that have more than 1 level of indentation.
                    let space_indentation =
                        chunk[offset..].iter().take(9).take_while(|&&c| c == b' ').count();

                    // We'll also reject lines starting with 1 space, because that's too fickle as a heuristic.
                    if (2..=8).contains(&space_indentation) {
                        space_indentations += 1;

                        // If we encounter an indentation depth of 6, it may either be a 6-space indentation,
                        // two 3-space indentation or 3 2-space indentations. To make this work, we increment
                        // all 3 possible histogram slots.
                        //   2 -> 2
                        //   3 -> 3
                        //   4 -> 4 2
                        //   5 -> 5
                        //   6 -> 6 3 2
                        //   7 -> 7
                        //   8 -> 8 4 2
                        space_indentation_sizes[space_indentation - 2] += 1;
                        if space_indentation & 4 != 0 {
                            space_indentation_sizes[0] += 1;
                        }
                        if space_indentation == 6 || space_indentation == 8 {
                            space_indentation_sizes[space_indentation / 2 - 2] += 1;
                        }
                    }
                }

                (offset, lines) = simd::lines_fwd(chunk, offset, lines, lines + 1);

                // Check if the preceding line ended in CRLF.
                if offset >= 2 && &chunk[offset - 2..offset] == b"\r\n" {
                    crlf_count += 1;
                }

                // We'll limit our heuristics to the first 1000 lines.
                // That should hopefully be enough in practice.
                if offset >= chunk.len() || lines >= 1000 {
                    break;
                }
            }

            // We'll assume CRLF if more than half of the lines end in CRLF.
            let newlines_are_crlf = crlf_count >= lines / 2;

            // We'll assume tabs if there are more lines starting with tabs than with spaces.
            let indent_with_tabs = tab_indentations > space_indentations;
            let tab_size = if indent_with_tabs {
                // Tabs will get a visual size of 4 spaces by default.
                4
            } else {
                // Otherwise, we'll assume the most common indentation depth.
                // If there are conflicting indentation depths, we'll prefer the maximum, because in the loop
                // above we incremented the histogram slot for 2-spaces when encountering 4-spaces and so on.
                let mut max = 1;
                let mut tab_size = 4;
                for (i, &count) in space_indentation_sizes.iter().enumerate() {
                    if count >= max {
                        max = count;
                        tab_size = i as CoordType + 2;
                    }
                }
                tab_size
            };

            // If the file has more than 1000 lines, figure out how many are remaining.
            if offset < chunk.len() {
                (_, lines) = simd::lines_fwd(chunk, offset, lines, CoordType::MAX);
            }

            let final_newline = chunk.ends_with(b"\n");

            // Add 1, because the last line doesn't end in a newline (it ends in the literal end).
            self.stats.logical_lines = lines + 1;
            self.stats.visual_lines = self.stats.logical_lines;
            self.newlines_are_crlf = newlines_are_crlf;
            self.insert_final_newline = final_newline;
            self.indent_with_tabs = indent_with_tabs;
            self.tab_size = tab_size;
        }

        self.recalc_after_content_swap();
        Ok(())
    }

    fn read_file_as_utf8(
        &mut self,
        file: &mut File,
        buf: &mut [MaybeUninit<u8>; 4 * KIBI],
        first_chunk_len: usize,
        done: bool,
    ) -> apperr::Result<()> {
        {
            let mut first_chunk = unsafe { buf[..first_chunk_len].assume_init_ref() };
            if first_chunk.starts_with(b"\xEF\xBB\xBF") {
                first_chunk = &first_chunk[3..];
                self.encoding = "UTF-8 BOM";
            }

            self.buffer.replace(0..0, first_chunk);
        }

        if done {
            return Ok(());
        }

        // If we don't have file metadata, the input may be a pipe or a socket.
        // Every read will have the same size until we hit the end.
        let mut chunk_size = 128 * KIBI;
        let mut extra_chunk_size = 128 * KIBI;

        if let Ok(m) = file.metadata() {
            // Usually the next read of size `chunk_size` will read the entire file,
            // but if the size has changed for some reason, then `extra_chunk_size`
            // should be large enough to read the rest of the file.
            // 4KiB is not too large and not too slow.
            let len = m.len() as usize;
            chunk_size = len.saturating_sub(first_chunk_len);
            extra_chunk_size = 4 * KIBI;
        }

        loop {
            let gap = self.buffer.allocate_gap(self.text_length(), chunk_size, 0);
            if gap.is_empty() {
                break;
            }

            let read = file.read(gap)?;
            if read == 0 {
                break;
            }

            self.buffer.commit_gap(read);
            chunk_size = extra_chunk_size;
        }

        Ok(())
    }

    fn read_file_with_icu(
        &mut self,
        file: &mut File,
        buf: &mut [MaybeUninit<u8>; 4 * KIBI],
        first_chunk_len: usize,
        mut done: bool,
    ) -> apperr::Result<()> {
        let scratch = scratch_arena(None);
        let pivot_buffer = scratch.alloc_uninit_slice(4 * KIBI);
        let mut c = icu::Converter::new(pivot_buffer, self.encoding, "UTF-8")?;
        let mut first_chunk = unsafe { buf[..first_chunk_len].assume_init_ref() };

        while !first_chunk.is_empty() {
            let off = self.text_length();
            let gap = self.buffer.allocate_gap(off, 8 * KIBI, 0);
            let (input_advance, mut output_advance) =
                c.convert(first_chunk, slice_as_uninit_mut(gap))?;

            // Remove the BOM from the file, if this is the first chunk.
            // Our caller ensures to only call us once the BOM has been identified,
            // which means that if there's a BOM it must be wholly contained in this chunk.
            if off == 0 {
                let written = &mut gap[..output_advance];
                if written.starts_with(b"\xEF\xBB\xBF") {
                    written.copy_within(3.., 0);
                    output_advance -= 3;
                }
            }

            self.buffer.commit_gap(output_advance);
            first_chunk = &first_chunk[input_advance..];
        }

        let mut buf_len = 0;

        loop {
            if !done {
                let read = file_read_uninit(file, &mut buf[buf_len..])?;
                buf_len += read;
                done = read == 0;
            }

            let gap = self.buffer.allocate_gap(self.text_length(), 8 * KIBI, 0);
            if gap.is_empty() {
                break;
            }

            let read = unsafe { buf[..buf_len].assume_init_ref() };
            let (input_advance, output_advance) = c.convert(read, slice_as_uninit_mut(gap))?;

            self.buffer.commit_gap(output_advance);

            let flush = done && buf_len == 0;
            buf_len -= input_advance;
            buf.copy_within(input_advance.., 0);

            if flush {
                break;
            }
        }

        Ok(())
    }

    /// Writes the text buffer contents to a file, handling BOM and encoding.
    /// Does not mark the buffer as clean.
    pub fn write_file_contents(&mut self, file: &mut File) -> apperr::Result<()> {
        let mut offset = 0;

        if self.encoding.starts_with("UTF-8") {
            if self.encoding == "UTF-8 BOM" {
                file.write_all(b"\xEF\xBB\xBF")?;
            }
            loop {
                let chunk = self.read_forward(offset);
                if chunk.is_empty() {
                    break;
                }
                file.write_all(chunk)?;
                offset += chunk.len();
            }
        } else {
            self.write_file_with_icu(file)?;
        }

        Ok(())
    }

    /// Writes the text buffer contents to a file, handling BOM and encoding.
    pub fn write_file(&mut self, file: &mut File) -> apperr::Result<()> {
        self.write_file_contents(file)?;
        self.mark_as_clean();
        Ok(())
    }

    fn write_file_with_icu(&mut self, file: &mut File) -> apperr::Result<()> {
        let scratch = scratch_arena(None);
        let pivot_buffer = scratch.alloc_uninit_slice(4 * KIBI);
        let buf = scratch.alloc_uninit_slice(4 * KIBI);
        let mut c = icu::Converter::new(pivot_buffer, "UTF-8", self.encoding)?;
        let mut offset = 0;

        // Write the BOM for the encodings we know need it.
        if self.encoding.starts_with("UTF-16")
            || self.encoding.starts_with("UTF-32")
            || self.encoding == "GB18030"
        {
            let (_, output_advance) = c.convert(b"\xEF\xBB\xBF", buf)?;
            let chunk = unsafe { buf[..output_advance].assume_init_ref() };
            file.write_all(chunk)?;
        }

        loop {
            let chunk = self.read_forward(offset);
            let (input_advance, output_advance) = c.convert(chunk, buf)?;
            let chunk = unsafe { buf[..output_advance].assume_init_ref() };

            file.write_all(chunk)?;
            offset += input_advance;

            if chunk.is_empty() {
                break;
            }
        }

        Ok(())
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

    /// Find the next occurrence of the given `pattern` and select it.
    pub fn find_and_select(&mut self, pattern: &str, options: SearchOptions) -> apperr::Result<()> {
        if let Some(search) = &mut self.search {
            let search = search.get_mut();
            // When the search input changes we must reset the search.
            if search.pattern != pattern || search.options != options {
                self.search = None;
            }

            // When transitioning from some search to no search, we must clear the selection.
            if pattern.is_empty()
                && let Some(TextBufferSelection { beg, .. }) = self.selection
            {
                self.cursor_move_to_logical(beg);
            }
        }

        if pattern.is_empty() {
            return Ok(());
        }

        let search = match &self.search {
            Some(search) => unsafe { &mut *search.get() },
            None => {
                let search = self.find_construct_search(pattern, options)?;
                self.search = Some(UnsafeCell::new(search));
                unsafe { &mut *self.search.as_ref().unwrap().get() }
            }
        };

        // If we previously searched through the entire document and found 0 matches,
        // then we can avoid searching again.
        if search.no_matches {
            return Ok(());
        }

        // If the user moved the cursor since the last search, but the needle remained the same,
        // we still need to move the start of the search to the new cursor position.
        let next_search_offset = match self.selection {
            Some(TextBufferSelection { beg, end }) => {
                if self.selection_generation == search.selection_generation {
                    search.next_search_offset
                } else {
                    self.cursor_move_to_logical_internal(self.cursor, beg.min(end)).offset
                }
            }
            _ => self.cursor.offset,
        };

        self.find_select_next(search, next_search_offset, true);
        Ok(())
    }

    /// Find the next occurrence of the given `pattern` and replace it with `replacement`.
    pub fn find_and_replace(
        &mut self,
        pattern: &str,
        options: SearchOptions,
        replacement: &[u8],
    ) -> apperr::Result<()> {
        // Editors traditionally replace the previous search hit, not the next possible one.
        if let (Some(search), Some(..)) = (&self.search, &self.selection) {
            let search = unsafe { &mut *search.get() };
            if search.selection_generation == self.selection_generation {
                let scratch = scratch_arena(None);
                let parsed_replacements =
                    Self::find_parse_replacement(&scratch, &mut *search, replacement);
                let replacement =
                    self.find_fill_replacement(&mut *search, replacement, &parsed_replacements);
                self.write(&replacement, self.cursor, true);
            }
        }

        self.find_and_select(pattern, options)
    }

    /// Find all occurrences of the given `pattern` and replace them with `replacement`.
    pub fn find_and_replace_all(
        &mut self,
        pattern: &str,
        options: SearchOptions,
        replacement: &[u8],
    ) -> apperr::Result<()> {
        let scratch = scratch_arena(None);
        let mut search = self.find_construct_search(pattern, options)?;
        let mut offset = 0;
        let parsed_replacements = Self::find_parse_replacement(&scratch, &mut search, replacement);

        loop {
            self.find_select_next(&mut search, offset, false);
            if !self.has_selection() {
                break;
            }

            let replacement =
                self.find_fill_replacement(&mut search, replacement, &parsed_replacements);
            self.write(&replacement, self.cursor, true);
            offset = self.cursor.offset;
        }

        Ok(())
    }

    fn find_construct_search(
        &self,
        pattern: &str,
        options: SearchOptions,
    ) -> apperr::Result<ActiveSearch> {
        if pattern.is_empty() {
            return Err(apperr::Error::Icu(1)); // U_ILLEGAL_ARGUMENT_ERROR
        }

        let sanitized_pattern = if options.whole_word && options.use_regex {
            Cow::Owned(format!(r"\b(?:{pattern})\b"))
        } else if options.whole_word {
            let mut p = String::with_capacity(pattern.len() + 16);
            p.push_str(r"\b");

            // Escape regex special characters.
            let b = unsafe { p.as_mut_vec() };
            for &byte in pattern.as_bytes() {
                match byte {
                    b'*' | b'?' | b'+' | b'[' | b'(' | b')' | b'{' | b'}' | b'^' | b'$' | b'|'
                    | b'\\' | b'.' => {
                        b.push(b'\\');
                        b.push(byte);
                    }
                    _ => b.push(byte),
                }
            }

            p.push_str(r"\b");
            Cow::Owned(p)
        } else {
            Cow::Borrowed(pattern)
        };

        let mut flags = icu::Regex::MULTILINE;
        if !options.match_case {
            flags |= icu::Regex::CASE_INSENSITIVE;
        }
        if !options.use_regex && !options.whole_word {
            flags |= icu::Regex::LITERAL;
        }

        // Move the start of the search to the start of the selection,
        // or otherwise to the current cursor position.

        let text = unsafe { icu::Text::new(self)? };
        let regex = unsafe { icu::Regex::new(&sanitized_pattern, flags, &text)? };

        Ok(ActiveSearch {
            pattern: pattern.to_string(),
            options,
            text,
            regex,
            buffer_generation: self.buffer.generation(),
            selection_generation: 0,
            next_search_offset: 0,
            no_matches: false,
        })
    }

    fn find_select_next(&mut self, search: &mut ActiveSearch, offset: usize, wrap: bool) {
        if search.buffer_generation != self.buffer.generation() {
            unsafe { search.regex.set_text(&mut search.text, offset) };
            search.buffer_generation = self.buffer.generation();
            search.next_search_offset = offset;
        } else if search.next_search_offset != offset {
            search.next_search_offset = offset;
            search.regex.reset(offset);
        }

        let mut hit = search.regex.next();

        // If we hit the end of the buffer, and we know that there's something to find,
        // start the search again from the beginning (= wrap around).
        if wrap && hit.is_none() && search.next_search_offset != 0 {
            search.next_search_offset = 0;
            search.regex.reset(0);
            hit = search.regex.next();
        }

        search.selection_generation = if let Some(range) = hit {
            // Now the search offset is no more at the start of the buffer.
            search.next_search_offset = range.end;

            let beg = self.cursor_move_to_offset_internal(self.cursor, range.start);
            let end = self.cursor_move_to_offset_internal(beg, range.end);

            unsafe { self.set_cursor(end) };
            self.make_cursor_visible();

            self.set_selection(Some(TextBufferSelection {
                beg: beg.logical_pos,
                end: end.logical_pos,
            }))
        } else {
            // Avoid searching through the entire document again if we know there's nothing to find.
            search.no_matches = true;
            self.set_selection(None)
        };
    }

    fn find_parse_replacement<'a>(
        arena: &'a Arena,
        search: &mut ActiveSearch,
        replacement: &[u8],
    ) -> Vec<RegexReplacement<'a>, &'a Arena> {
        let mut res = Vec::new_in(arena);

        if !search.options.use_regex {
            return res;
        }

        let group_count = search.regex.group_count();
        let mut text = Vec::new_in(arena);
        let mut text_beg = 0;

        loop {
            let mut off = memchr2(b'$', b'\\', replacement, text_beg);

            // Push the raw, unescaped text, if any.
            if text_beg < off {
                text.extend_from_slice(&replacement[text_beg..off]);
            }

            // Unescape any escaped characters.
            while off < replacement.len() && replacement[off] == b'\\' {
                off += 2;

                // If this backslash is the last character (e.g. because
                // `replacement` is just 1 byte long, holding just b"\\"),
                // we can't unescape it. In that case, we map it to `b'\\'` here.
                // This results in us appending a literal backslash to the text.
                let ch = replacement.get(off - 1).map_or(b'\\', |&c| c);

                // Unescape and append the character.
                text.push(match ch {
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    ch => ch,
                });
            }

            // Parse out a group number, if any.
            let mut group = -1;
            if off < replacement.len() && replacement[off] == b'$' {
                let mut beg = off;
                let mut end = off + 1;
                let mut acc = 0i32;
                let mut acc_bad = true;

                if end < replacement.len() {
                    let ch = replacement[end];

                    if ch == b'$' {
                        // Translate "$$" to "$".
                        beg += 1;
                        end += 1;
                    } else if ch.is_ascii_digit() {
                        // Parse "$1234" into 1234i32.
                        // If the number is larger than the group count,
                        // we flag `acc_bad` which causes us to treat it as text.
                        acc_bad = false;
                        while {
                            acc =
                                acc.wrapping_mul(10).wrapping_add((replacement[end] - b'0') as i32);
                            acc_bad |= acc > group_count;
                            end += 1;
                            end < replacement.len() && replacement[end].is_ascii_digit()
                        } {}
                    }
                }

                if !acc_bad {
                    group = acc;
                } else {
                    text.extend_from_slice(&replacement[beg..end]);
                }

                off = end;
            }

            if !text.is_empty() {
                res.push(RegexReplacement::Text(text));
                text = Vec::new_in(arena);
            }
            if group >= 0 {
                res.push(RegexReplacement::Group(group));
            }

            text_beg = off;
            if text_beg >= replacement.len() {
                break;
            }
        }

        res
    }

    fn find_fill_replacement<'a>(
        &self,
        search: &mut ActiveSearch,
        replacement: &'a [u8],
        parsed_replacements: &[RegexReplacement],
    ) -> Cow<'a, [u8]> {
        if !search.options.use_regex {
            Cow::Borrowed(replacement)
        } else {
            let mut res = Vec::new();

            for replacement in parsed_replacements {
                match replacement {
                    RegexReplacement::Text(text) => res.extend_from_slice(text),
                    RegexReplacement::Group(group) => {
                        if let Some(range) = search.regex.group(*group) {
                            self.buffer.extract_raw(range, &mut res, usize::MAX);
                        }
                    }
                }
            }

            Cow::Owned(res)
        }
    }

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

        // Find matching bracket for highlighting (if cursor is on a bracket)
        let matching_bracket_offset = if focused { self.find_matching_bracket_offset() } else { None };
        // Also highlight the bracket under cursor if there's a match
        let cursor_bracket_offset = if matching_bracket_offset.is_some() { Some(self.cursor.offset) } else { None };

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

            let mut selection_off = 0..0;

            // Figure out the selection range on this line, if any.
            if cursor_beg.visual_pos.y == visual_line
                && selection_beg <= cursor_end.logical_pos
                && selection_end >= cursor_beg.logical_pos
            {
                let mut cursor = cursor_beg;

                // By default, we assume the entire line is selected.
                let mut selection_pos_beg = 0;
                let mut selection_pos_end = COORD_TYPE_SAFE_MAX;
                selection_off.start = cursor_beg.offset;
                selection_off.end = cursor_end.offset;

                // The start of the selection is within this line. We need to update selection_beg.
                if selection_beg <= cursor_end.logical_pos
                    && selection_beg >= cursor_beg.logical_pos
                {
                    cursor = self.cursor_move_to_logical_internal(cursor, selection_beg);
                    selection_off.start = cursor.offset;
                    selection_pos_beg = cursor.visual_pos.x;
                }

                // The end of the selection is within this line. We need to update selection_end.
                if selection_end <= cursor_end.logical_pos
                    && selection_end >= cursor_beg.logical_pos
                {
                    cursor = self.cursor_move_to_logical_internal(cursor, selection_end);
                    selection_off.end = cursor.offset;
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
                            let visualize = self.show_whitespace || selection_off.contains(&global_off);
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
                    let matches =
                        find_search_matches(content, &highlight.needle, highlight.options);
                    if !matches.is_empty() {
                        let base_left = destination.left + self.margin_width;
                        let top = destination.top + y;
                        let color = fb.indexed_alpha(IndexedColor::BrightYellow, 1, 3);

                        for range in matches {
                            let left =
                                base_left + count_columns(content, range.start) as CoordType;
                            let right =
                                base_left + count_columns(content, range.end) as CoordType;
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
            for bracket_off in [matching_bracket_offset, cursor_bracket_offset].into_iter().flatten() {
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
                        col += if ch == '\t' {
                            self.tab_size - (col % self.tab_size)
                        } else {
                            1
                        };
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
            let mut x = self.cursor.visual_pos.x;
            let mut y = self.cursor.visual_pos.y;

            if self.word_wrap_column > 0 && x >= self.word_wrap_column {
                // The line the cursor is on wraps exactly on the word wrap column which
                // means the cursor is invisible. We need to move it to the next line.
                x = 0;
                y += 1;
            }

            // Move the cursor into screen space.
            x += destination.left - origin.x + self.margin_width;
            y += destination.top - origin.y;

            let cursor = Point { x, y };
            let text = Rect {
                left: destination.left + self.margin_width,
                top: destination.top,
                right: destination.right,
                bottom: destination.bottom,
            };

            if text.contains(cursor) {
                fb.set_cursor(cursor, self.overtype);

                if self.line_highlight_enabled && selection_beg >= selection_end {
                    fb.blend_bg(
                        Rect {
                            left: destination.left,
                            top: cursor.y,
                            right: destination.right,
                            bottom: cursor.y + 1,
                        },
                        StraightRgba::from_le(0x7f7f7f7f),
                    );
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
        let start_cursor = self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y: beg_y });
        let end_cursor = self.cursor_move_to_logical_internal(start_cursor, Point { x: 0, y: end_y + 1 });

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
        let end_cursor = self.cursor_move_to_logical_internal(start_cursor, Point { x: 0, y: y + 1 });

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
            let line_end = self.cursor_move_to_logical_internal(
                self.cursor,
                Point { x: CoordType::MAX, y },
            );

            // Find start of next line
            let next_line_start = self.cursor_move_to_logical_internal(
                line_end,
                Point { x: 0, y: y + 1 },
            );

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

    /// Sorts the selected lines (or all lines if no selection) alphabetically.
    pub fn sort_lines(&mut self, descending: bool) {
        let cursor = self.cursor;

        // Get the line range
        let [beg_y, end_y] = match self.selection {
            Some(s) => minmax(s.beg.y, s.end.y),
            None => [0, self.stats.logical_lines - 1],
        };

        if end_y <= beg_y {
            return;
        }

        // Get offsets for the lines
        let start_cursor = self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y: beg_y });
        let end_cursor = self.cursor_move_to_logical_internal(start_cursor, Point { x: 0, y: end_y + 1 });

        // Extract the content
        let mut content = Vec::new();
        self.buffer.extract_raw(start_cursor.offset..end_cursor.offset, &mut content, 0);

        // Split into lines and sort
        let content_str = String::from_utf8_lossy(&content);
        let mut lines: Vec<&str> = content_str.lines().collect();

        if descending {
            lines.sort_by(|a, b| b.cmp(a));
        } else {
            lines.sort();
        }

        // Rejoin with newlines
        let newline = if self.newlines_are_crlf { "\r\n" } else { "\n" };
        let mut sorted = lines.join(newline);
        sorted.push_str(newline);

        // Replace the content
        self.edit_begin_grouping();

        self.cursor_move_to_offset(start_cursor.offset);
        self.edit_begin(HistoryType::Delete, self.cursor);
        self.edit_delete(end_cursor);
        self.edit_end();

        self.edit_begin(HistoryType::Write, self.cursor);
        self.write_raw(sorted.as_bytes());
        self.edit_end();

        self.edit_end_grouping();

        // Restore cursor position
        self.cursor_move_to_logical(cursor.logical_pos);
        self.set_selection(None);
    }

    /// Removes duplicate lines from the selection (or all lines if no selection),
    /// keeping the first occurrence of each unique line.
    pub fn remove_duplicate_lines(&mut self) {
        let cursor = self.cursor;

        // Get the line range
        let [beg_y, end_y] = match self.selection {
            Some(s) => minmax(s.beg.y, s.end.y),
            None => [0, self.stats.logical_lines - 1],
        };

        if end_y < beg_y {
            return;
        }

        // Get offsets for the lines
        let start_cursor = self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y: beg_y });
        let end_cursor = self.cursor_move_to_logical_internal(start_cursor, Point { x: 0, y: end_y + 1 });

        // Extract the content
        let mut content = Vec::new();
        self.buffer.extract_raw(start_cursor.offset..end_cursor.offset, &mut content, 0);

        // Split into lines and remove duplicates (preserving order)
        let content_str = String::from_utf8_lossy(&content);
        let mut seen = std::collections::HashSet::new();
        let unique_lines: Vec<&str> = content_str
            .lines()
            .filter(|line| seen.insert(*line))
            .collect();

        // If no duplicates were removed, do nothing
        if unique_lines.len() == content_str.lines().count() {
            return;
        }

        // Rejoin with newlines
        let newline = if self.newlines_are_crlf { "\r\n" } else { "\n" };
        let mut result = unique_lines.join(newline);
        result.push_str(newline);

        // Replace the content
        self.edit_begin_grouping();

        self.cursor_move_to_offset(start_cursor.offset);
        self.edit_begin(HistoryType::Delete, self.cursor);
        self.edit_delete(end_cursor);
        self.edit_end();

        self.edit_begin(HistoryType::Write, self.cursor);
        self.write_raw(result.as_bytes());
        self.edit_end();

        self.edit_end_grouping();

        // Restore cursor position (clamped to new bounds)
        let new_y = cursor.logical_pos.y.min(beg_y + unique_lines.len() as CoordType - 1);
        self.cursor_move_to_logical(Point { x: cursor.logical_pos.x, y: new_y });
        self.set_selection(None);
    }

    /// Removes empty lines from the selection (or all lines if no selection).
    /// Lines containing only whitespace are also considered empty.
    pub fn remove_empty_lines(&mut self) {
        let cursor = self.cursor;

        // Get the line range
        let [beg_y, end_y] = match self.selection {
            Some(s) => minmax(s.beg.y, s.end.y),
            None => [0, self.stats.logical_lines - 1],
        };

        if end_y < beg_y {
            return;
        }

        // Get offsets for the lines
        let start_cursor = self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y: beg_y });
        let end_cursor = self.cursor_move_to_logical_internal(start_cursor, Point { x: 0, y: end_y + 1 });

        // Extract the content
        let mut content = Vec::new();
        self.buffer.extract_raw(start_cursor.offset..end_cursor.offset, &mut content, 0);

        // Split into lines and filter out empty ones
        let content_str = String::from_utf8_lossy(&content);
        let non_empty_lines: Vec<&str> = content_str
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();

        // If no empty lines were removed, do nothing
        if non_empty_lines.len() == content_str.lines().count() {
            return;
        }

        // Rejoin with newlines
        let newline = if self.newlines_are_crlf { "\r\n" } else { "\n" };
        let mut result = non_empty_lines.join(newline);
        if !non_empty_lines.is_empty() {
            result.push_str(newline);
        }

        // Replace the content
        self.edit_begin_grouping();

        self.cursor_move_to_offset(start_cursor.offset);
        self.edit_begin(HistoryType::Delete, self.cursor);
        self.edit_delete(end_cursor);
        self.edit_end();

        if !result.is_empty() {
            self.edit_begin(HistoryType::Write, self.cursor);
            self.write_raw(result.as_bytes());
            self.edit_end();
        }

        self.edit_end_grouping();

        // Restore cursor position (clamped to new bounds)
        let new_y = cursor.logical_pos.y.min(beg_y + non_empty_lines.len().saturating_sub(1) as CoordType);
        self.cursor_move_to_logical(Point { x: cursor.logical_pos.x, y: new_y });
        self.set_selection(None);
    }

    /// Trims trailing whitespace from all lines.
    pub fn trim_trailing_whitespace(&mut self) {
        self.edit_begin_grouping();

        // Work backwards from last line to first
        for y in (0..self.stats.logical_lines).rev() {
            // Find end of line content (before any trailing whitespace)
            let line_start = self.cursor_move_to_logical_internal(
                self.cursor,
                Point { x: 0, y },
            );
            let line_end = self.cursor_move_to_logical_internal(
                line_start,
                Point { x: CoordType::MAX, y },
            );

            // Read backwards from line_end to find trailing whitespace
            if line_start.offset >= line_end.offset {
                continue;
            }

            let mut content = Vec::new();
            self.buffer.extract_raw(line_start.offset..line_end.offset, &mut content, 0);

            // Find where trailing whitespace starts
            let trimmed_len = content.iter().rposition(|&b| b != b' ' && b != b'\t').map(|i| i + 1).unwrap_or(0);

            if trimmed_len < content.len() {
                // There's trailing whitespace to remove
                let ws_start_offset = line_start.offset + trimmed_len;

                self.cursor_move_to_offset(ws_start_offset);
                self.edit_begin(HistoryType::Delete, self.cursor);
                self.edit_delete(line_end);
                self.edit_end();
            }
        }

        self.edit_end_grouping();
    }

    /// Gets the line comment prefix for the current language.
    fn line_comment_prefix(&self) -> Option<&'static str> {
        match self.language {
            Language::Rust | Language::C | Language::Cpp | Language::CSharp
            | Language::Go | Language::Java | Language::JavaScript
            | Language::TypeScript | Language::Swift | Language::Kotlin
            | Language::Dart | Language::Scala => Some("//"),
            Language::Python | Language::Ruby | Language::Shell
            | Language::Yaml | Language::Toml | Language::Ini
            | Language::R | Language::Perl => Some("#"),
            Language::Sql | Language::Lua | Language::Haskell => Some("--"),
            Language::Latex => Some("%"),
            Language::Clojure => Some(";"),
            Language::VbNet => Some("'"),
            _ => None,
        }
    }

    /// Gets the block comment delimiters for the current language.
    fn block_comment_delimiters(&self) -> Option<(&'static str, &'static str)> {
        match self.language {
            Language::C | Language::Cpp | Language::CSharp | Language::Java
            | Language::JavaScript | Language::TypeScript | Language::Go
            | Language::Rust | Language::Swift | Language::Kotlin | Language::Css
            | Language::Dart | Language::Scala | Language::Php => Some(("/*", "*/")),
            Language::Html | Language::Xml => Some(("<!--", "-->")),
            Language::Lua => Some(("--[[", "]]")),
            Language::Haskell => Some(("{-", "-}")),
            _ => None,
        }
    }

    /// Toggles line comments on the current line or selection.
    pub fn toggle_line_comment(&mut self) {
        let Some(prefix) = self.line_comment_prefix() else {
            return;
        };

        let cursor = self.cursor;
        let prefix_bytes = prefix.as_bytes();
        let prefix_len = prefix_bytes.len();

        // Get the line range
        let [beg_y, end_y] = match self.selection {
            Some(s) => minmax(s.beg.y, s.end.y),
            None => [cursor.logical_pos.y, cursor.logical_pos.y],
        };

        // Check if all lines are already commented
        let mut all_commented = true;
        for y in beg_y..=end_y {
            let line_start = self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y });
            let line_end = self.cursor_move_to_logical_internal(line_start, Point { x: CoordType::MAX, y });

            let mut content = Vec::new();
            self.buffer.extract_raw(line_start.offset..line_end.offset, &mut content, 0);

            // Skip leading whitespace
            let trimmed = content.iter().position(|&b| b != b' ' && b != b'\t').unwrap_or(content.len());
            if !content[trimmed..].starts_with(prefix_bytes) {
                all_commented = false;
                break;
            }
        }

        self.edit_begin_grouping();

        // Process lines in reverse order to preserve offsets
        for y in (beg_y..=end_y).rev() {
            let line_start = self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y });
            let line_end = self.cursor_move_to_logical_internal(line_start, Point { x: CoordType::MAX, y });

            let mut content = Vec::new();
            self.buffer.extract_raw(line_start.offset..line_end.offset, &mut content, 0);

            let ws_len = content.iter().position(|&b| b != b' ' && b != b'\t').unwrap_or(content.len());

            if all_commented {
                // Remove comment prefix (and optional space after it)
                let remove_start = line_start.offset + ws_len;
                let mut remove_end = remove_start + prefix_len;

                // Check if there's a space after the prefix to remove
                if content.get(ws_len + prefix_len) == Some(&b' ') {
                    remove_end += 1;
                }

                let remove_end_cursor = Cursor {
                    offset: remove_end,
                    logical_pos: Point { x: (remove_end - line_start.offset) as CoordType, y },
                    visual_pos: Point::default(),
                    column: 0,
                    wrap_opp: false,
                };

                self.cursor_move_to_offset(remove_start);
                self.edit_begin(HistoryType::Delete, self.cursor);
                self.edit_delete(remove_end_cursor);
                self.edit_end();
            } else {
                // Add comment prefix at start of content (after whitespace)
                let insert_offset = line_start.offset + ws_len;
                self.cursor_move_to_offset(insert_offset);
                self.edit_begin(HistoryType::Write, self.cursor);
                self.write_raw(prefix_bytes);
                self.write_raw(b" ");
                self.edit_end();
            }
        }

        self.edit_end_grouping();
        self.cursor_move_to_logical(cursor.logical_pos);
    }

    /// Toggles block comment around the selection.
    pub fn toggle_block_comment(&mut self) {
        let Some((open, close)) = self.block_comment_delimiters() else {
            return;
        };

        let Some((beg, end)) = self.selection_range() else {
            return; // Need a selection for block comment
        };

        let open_bytes = open.as_bytes();
        let close_bytes = close.as_bytes();

        // Extract selected content
        let mut content = Vec::new();
        self.buffer.extract_raw(beg.offset..end.offset, &mut content, 0);

        // Check if already wrapped in block comment
        let already_wrapped = content.starts_with(open_bytes) && content.ends_with(close_bytes);

        self.edit_begin_grouping();

        if already_wrapped {
            // Remove block comment delimiters
            // Remove closing delimiter first (working backwards)
            let close_start = end.offset - close_bytes.len();

            self.cursor_move_to_offset(close_start);
            self.edit_begin(HistoryType::Delete, self.cursor);
            self.edit_delete(end);
            self.edit_end();

            // Remove opening delimiter
            self.cursor_move_to_offset(beg.offset);
            let open_end = Cursor {
                offset: beg.offset + open_bytes.len(),
                logical_pos: Point::default(),
                visual_pos: Point::default(),
                column: 0,
                wrap_opp: false,
            };
            self.edit_begin(HistoryType::Delete, self.cursor);
            self.edit_delete(open_end);
            self.edit_end();
        } else {
            // Add block comment delimiters
            // Add closing delimiter first
            self.cursor_move_to_offset(end.offset);
            self.edit_begin(HistoryType::Write, self.cursor);
            self.write_raw(close_bytes);
            self.edit_end();

            // Add opening delimiter
            self.cursor_move_to_offset(beg.offset);
            self.edit_begin(HistoryType::Write, self.cursor);
            self.write_raw(open_bytes);
            self.edit_end();
        }

        self.edit_end_grouping();
        self.set_selection(None);
    }

    /// Finds the offset of the matching bracket, if the cursor is on a bracket.
    /// Returns None if not on a bracket or no match found.
    fn find_matching_bracket_offset(&self) -> Option<usize> {
        let offset = self.cursor.offset;
        let text = self.buffer.read_forward(offset);

        if text.is_empty() {
            return None;
        }

        let ch = text[0];
        let (target, forward) = match ch {
            b'(' => (b')', true),
            b')' => (b'(', false),
            b'[' => (b']', true),
            b']' => (b'[', false),
            b'{' => (b'}', true),
            b'}' => (b'{', false),
            _ => return None, // Not on a bracket (excluding < > as they're often comparison operators)
        };

        let mut depth = 1i32;

        if forward {
            // Search forward
            for (i, &b) in text[1..].iter().enumerate() {
                if b == ch {
                    depth += 1;
                } else if b == target {
                    depth -= 1;
                    if depth == 0 {
                        return Some(offset + i + 1);
                    }
                }
            }
        } else {
            // Search backward
            if offset == 0 {
                return None;
            }

            let mut search_offset = offset;
            while search_offset > 0 {
                search_offset -= 1;
                let prev_text = self.buffer.read_forward(search_offset);
                if prev_text.is_empty() {
                    break;
                }
                let b = prev_text[0];
                if b == ch {
                    depth += 1;
                } else if b == target {
                    depth -= 1;
                    if depth == 0 {
                        return Some(search_offset);
                    }
                }
            }
        }

        None
    }

    /// Moves the cursor to the matching bracket.
    pub fn goto_matching_bracket(&mut self) {
        if let Some(match_offset) = self.find_matching_bracket_offset() {
            self.cursor_move_to_offset(match_offset);
        }
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

    fn edit_begin_grouping(&mut self) {
        self.active_edit_group = Some(ActiveEditGroupInfo {
            cursor_before: self.cursor.logical_pos,
            selection_before: self.selection,
            stats_before: self.stats,
            generation_before: self.buffer.generation(),
        });
    }

    fn edit_end_grouping(&mut self) {
        self.active_edit_group = None;
    }

    /// Starts a new edit operation.
    /// This is used for tracking the undo/redo history.
    fn edit_begin(&mut self, history_type: HistoryType, cursor: Cursor) {
        self.active_edit_depth += 1;
        if self.active_edit_depth > 1 {
            return;
        }

        let cursor_before = self.cursor;
        self.set_cursor_internal(cursor);

        // If both the last and this are a Write/Delete operation, we skip allocating a new undo history item.
        if history_type != self.last_history_type
            || !matches!(history_type, HistoryType::Write | HistoryType::Delete)
        {
            self.redo_stack.clear();
            while self.undo_stack.len() > 1000 {
                self.undo_stack.pop_front();
            }

            self.last_history_type = history_type;
            self.undo_stack.push_back(SemiRefCell::new(HistoryEntry {
                cursor_before: cursor_before.logical_pos,
                selection_before: self.selection,
                stats_before: self.stats,
                generation_before: self.buffer.generation(),
                cursor: cursor.logical_pos,
                deleted: Vec::new(),
                added: Vec::new(),
            }));

            if let Some(info) = &self.active_edit_group
                && let Some(entry) = self.undo_stack.back()
            {
                let mut entry = entry.borrow_mut();
                entry.cursor_before = info.cursor_before;
                entry.selection_before = info.selection_before;
                entry.stats_before = info.stats_before;
                entry.generation_before = info.generation_before;
            }
        }

        self.active_edit_off = cursor.offset;

        // If word-wrap is enabled, the visual layout of all logical lines affected by the write
        // may have changed. This includes even text before the insertion point up to the line
        // start, because this write may have joined with a word before the initial cursor.
        // See other uses of `word_wrap_cursor_next_line` in this function.
        if self.word_wrap_column > 0 {
            let safe_start = self.goto_line_start(cursor, cursor.logical_pos.y);
            let next_line = self.cursor_move_to_logical_internal(
                cursor,
                Point { x: 0, y: cursor.logical_pos.y + 1 },
            );
            self.active_edit_line_info = Some(ActiveEditLineInfo {
                safe_start,
                line_height_in_rows: next_line.visual_pos.y - safe_start.visual_pos.y,
                distance_next_line_start: next_line.offset - cursor.offset,
            });
        }
    }

    /// Writes `text` into the buffer at the current cursor position.
    /// It records the change in the undo stack.
    fn edit_write(&mut self, text: &[u8]) {
        let logical_y_before = self.cursor.logical_pos.y;

        // Copy the written portion into the undo entry.
        {
            let mut undo = self.undo_stack.back_mut().unwrap().borrow_mut();
            undo.added.extend_from_slice(text);
        }

        // Write!
        self.buffer.replace(self.active_edit_off..self.active_edit_off, text);

        // Move self.cursor to the end of the newly written text. Can't use `self.set_cursor_internal`,
        // because we're still in the progress of recalculating the line stats.
        self.active_edit_off += text.len();
        self.cursor = self.cursor_move_to_offset_internal(self.cursor, self.active_edit_off);
        self.stats.logical_lines += self.cursor.logical_pos.y - logical_y_before;
    }

    /// Deletes the text between the current cursor position and `to`.
    /// It records the change in the undo stack.
    fn edit_delete(&mut self, to: Cursor) {
        debug_assert!(to.offset >= self.active_edit_off);

        let logical_y_before = self.cursor.logical_pos.y;
        let off = self.active_edit_off;
        let mut out_off = usize::MAX;

        let mut undo = self.undo_stack.back_mut().unwrap().borrow_mut();

        // If this is a continued backspace operation,
        // we need to prepend the deleted portion to the undo entry.
        if self.cursor.logical_pos < undo.cursor {
            out_off = 0;
            undo.cursor = self.cursor.logical_pos;
        }

        // Copy the deleted portion into the undo entry.
        let deleted = &mut undo.deleted;
        self.buffer.extract_raw(off..to.offset, deleted, out_off);

        // Delete the portion from the buffer by enlarging the gap.
        let count = to.offset - off;
        self.buffer.allocate_gap(off, 0, count);

        self.stats.logical_lines += logical_y_before - to.logical_pos.y;
    }

    /// Finalizes the current edit operation
    /// and recalculates the line statistics.
    fn edit_end(&mut self) {
        self.active_edit_depth -= 1;
        debug_assert!(self.active_edit_depth >= 0);
        if self.active_edit_depth > 0 {
            return;
        }

        #[cfg(debug_assertions)]
        {
            let entry = self.undo_stack.back_mut().unwrap().borrow_mut();
            debug_assert!(!entry.deleted.is_empty() || !entry.added.is_empty());
        }

        if let Some(info) = self.active_edit_line_info.take() {
            let deleted_count = self.undo_stack.back_mut().unwrap().borrow_mut().deleted.len();
            let target = self.cursor.logical_pos;

            // From our safe position we can measure the actual visual position of the cursor.
            self.set_cursor_internal(self.cursor_move_to_logical_internal(info.safe_start, target));

            // If content is added at the insertion position, that's not a problem:
            // We can just remeasure the height of this one line and calculate the delta.
            // `deleted_count` is 0 in this case.
            //
            // The problem is when content is deleted, because it may affect lines
            // beyond the end of the `next_line`. In that case we have to measure
            // the entire buffer contents until the end to compute `self.stats.visual_lines`.
            if deleted_count < info.distance_next_line_start {
                // Now we can measure how many more visual rows this logical line spans.
                let next_line = self
                    .cursor_move_to_logical_internal(self.cursor, Point { x: 0, y: target.y + 1 });
                let lines_before = info.line_height_in_rows;
                let lines_after = next_line.visual_pos.y - info.safe_start.visual_pos.y;
                self.stats.visual_lines += lines_after - lines_before;
            } else {
                let end = self.cursor_move_to_logical_internal(self.cursor, Point::MAX);
                self.stats.visual_lines = end.visual_pos.y + 1;
            }
        } else {
            // If word-wrap is disabled the visual line count always matches the logical one.
            self.stats.visual_lines = self.stats.logical_lines;
        }

        self.recalc_after_content_changed();
    }

    /// Undo the last edit operation.
    pub fn undo(&mut self) {
        self.undo_redo(true);
    }

    /// Redo the last undo operation.
    pub fn redo(&mut self) {
        self.undo_redo(false);
    }

    fn undo_redo(&mut self, undo: bool) {
        let buffer_generation = self.buffer.generation();
        let mut entry_buffer_generation = None;

        loop {
            // Transfer the last entry from the undo stack to the redo stack or vice versa.
            {
                let (from, to) = if undo {
                    (&mut self.undo_stack, &mut self.redo_stack)
                } else {
                    (&mut self.redo_stack, &mut self.undo_stack)
                };

                if let Some(g) = entry_buffer_generation
                    && from.back().is_none_or(|c| c.borrow().generation_before != g)
                {
                    break;
                }

                let Some(list) = from.cursor_back_mut().remove_current_as_list() else {
                    break;
                };

                to.cursor_back_mut().splice_after(list);
            }

            let change = {
                let to = if undo { &self.redo_stack } else { &self.undo_stack };
                to.back().unwrap()
            };

            // Remember the buffer generation of the change so we can stop popping undos/redos.
            // Also, move to the point where the modification took place.
            let cursor = {
                let change = change.borrow();
                entry_buffer_generation = Some(change.generation_before);
                self.cursor_move_to_logical_internal(self.cursor, change.cursor)
            };

            let safe_cursor = if self.word_wrap_column > 0 {
                // If word-wrap is enabled, we need to move the cursor to the beginning of the line.
                // This is because the undo/redo operation may have changed the visual position of the cursor.
                self.goto_line_start(cursor, cursor.logical_pos.y)
            } else {
                cursor
            };

            {
                let mut change = change.borrow_mut();
                let change = &mut *change;

                // Undo: Whatever was deleted is now added and vice versa.
                mem::swap(&mut change.deleted, &mut change.added);

                // Delete the inserted portion.
                self.buffer.allocate_gap(cursor.offset, 0, change.deleted.len());

                // Reinsert the deleted portion.
                {
                    let added = &change.added[..];
                    let mut beg = 0;
                    let mut offset = cursor.offset;

                    while beg < added.len() {
                        let (end, line) = simd::lines_fwd(added, beg, 0, 1);
                        let has_newline = line != 0;
                        let link = &added[beg..end];
                        let line = unicode::strip_newline(link);
                        let mut written;

                        {
                            let gap = self.buffer.allocate_gap(offset, line.len() + 2, 0);
                            written = slice_copy_safe(gap, line);

                            if has_newline {
                                if self.newlines_are_crlf && written < gap.len() {
                                    gap[written] = b'\r';
                                    written += 1;
                                }
                                if written < gap.len() {
                                    gap[written] = b'\n';
                                    written += 1;
                                }
                            }

                            self.buffer.commit_gap(written);
                        }

                        beg = end;
                        offset += written;
                    }
                }

                // Restore the previous line statistics.
                mem::swap(&mut self.stats, &mut change.stats_before);

                // Restore the previous selection.
                mem::swap(&mut self.selection, &mut change.selection_before);

                // Pretend as if the buffer was never modified.
                self.buffer.set_generation(change.generation_before);
                change.generation_before = buffer_generation;

                // Restore the previous cursor.
                let cursor_before =
                    self.cursor_move_to_logical_internal(safe_cursor, change.cursor_before);
                change.cursor_before = self.cursor.logical_pos;
                // Can't use `set_cursor_internal` here, because we haven't updated the line stats yet.
                self.cursor = cursor_before;

                if self.undo_stack.is_empty() {
                    self.last_history_type = HistoryType::Other;
                }
            }
        }

        if entry_buffer_generation.is_some() {
            self.recalc_after_content_changed();
        }
    }

    /// For interfacing with ICU.
    pub(crate) fn read_backward(&self, off: usize) -> &[u8] {
        self.buffer.read_backward(off)
    }

    /// For interfacing with ICU.
    pub fn read_forward(&self, off: usize) -> &[u8] {
        self.buffer.read_forward(off)
    }

    // ========== Encode/Decode Methods ==========

    /// Returns the current selection as byte offsets (start, end).
    fn selection_offsets(&self) -> Option<(usize, usize)> {
        let (beg, end) = self.selection_range()?;
        Some((beg.offset, end.offset))
    }

    /// Replaces the current selection with new content.
    fn replace_selection_with(&mut self, content: &[u8]) {
        let Some((beg_off, end_off)) = self.selection_offsets() else {
            return;
        };

        self.edit_begin_grouping();

        // Delete the selection
        self.cursor_move_to_offset(beg_off);
        let end_cursor = self.cursor_move_to_offset_internal(self.cursor, end_off);
        self.edit_begin(HistoryType::Delete, self.cursor);
        self.edit_delete(end_cursor);
        self.edit_end();

        // Insert the new content
        self.edit_begin(HistoryType::Write, self.cursor);
        self.write_raw(content);
        self.edit_end();

        self.edit_end_grouping();
        self.set_selection(None);
    }

    /// Encodes the selected text as Base64.
    pub fn encode_base64(&mut self) {
        let Some((beg, end)) = self.selection_offsets() else {
            return;
        };

        let mut content = Vec::new();
        self.buffer.extract_raw(beg..end, &mut content, 0);

        let encoded = base64_encode(&content);
        self.replace_selection_with(encoded.as_bytes());
    }

    /// Decodes the selected text from Base64.
    pub fn decode_base64(&mut self) {
        let Some((beg, end)) = self.selection_offsets() else {
            return;
        };

        let mut content = Vec::new();
        self.buffer.extract_raw(beg..end, &mut content, 0);

        if let Some(decoded) = base64_decode(&content) {
            self.replace_selection_with(&decoded);
        }
    }

    /// URL-encodes the selected text.
    pub fn encode_url(&mut self) {
        let Some((beg, end)) = self.selection_offsets() else {
            return;
        };

        let mut content = Vec::new();
        self.buffer.extract_raw(beg..end, &mut content, 0);

        let encoded = url_encode(&content);
        self.replace_selection_with(&encoded);
    }

    /// URL-decodes the selected text.
    pub fn decode_url(&mut self) {
        let Some((beg, end)) = self.selection_offsets() else {
            return;
        };

        let mut content = Vec::new();
        self.buffer.extract_raw(beg..end, &mut content, 0);

        if let Some(decoded) = url_decode(&content) {
            self.replace_selection_with(&decoded);
        }
    }

    /// Encodes the selected text as hexadecimal.
    pub fn encode_hex(&mut self) {
        let Some((beg, end)) = self.selection_offsets() else {
            return;
        };

        let mut content = Vec::new();
        self.buffer.extract_raw(beg..end, &mut content, 0);

        let encoded = hex_encode(&content);
        self.replace_selection_with(encoded.as_bytes());
    }

    /// Decodes the selected text from hexadecimal.
    pub fn decode_hex(&mut self) {
        let Some((beg, end)) = self.selection_offsets() else {
            return;
        };

        let mut content = Vec::new();
        self.buffer.extract_raw(beg..end, &mut content, 0);

        if let Some(decoded) = hex_decode(&content) {
            self.replace_selection_with(&decoded);
        }
    }

    // ========== Case Conversion Methods ==========

    /// Converts the selected text to uppercase.
    pub fn convert_to_uppercase(&mut self) {
        let Some((beg, end)) = self.selection_offsets() else {
            return;
        };

        let mut content = Vec::new();
        self.buffer.extract_raw(beg..end, &mut content, 0);

        if let Ok(text) = std::str::from_utf8(&content) {
            let upper = text.to_uppercase();
            self.replace_selection_with(upper.as_bytes());
        }
    }

    /// Converts the selected text to lowercase.
    pub fn convert_to_lowercase(&mut self) {
        let Some((beg, end)) = self.selection_offsets() else {
            return;
        };

        let mut content = Vec::new();
        self.buffer.extract_raw(beg..end, &mut content, 0);

        if let Ok(text) = std::str::from_utf8(&content) {
            let lower = text.to_lowercase();
            self.replace_selection_with(lower.as_bytes());
        }
    }

    /// Converts the selected text to title case.
    pub fn convert_to_title_case(&mut self) {
        let Some((beg, end)) = self.selection_offsets() else {
            return;
        };

        let mut content = Vec::new();
        self.buffer.extract_raw(beg..end, &mut content, 0);

        if let Ok(text) = std::str::from_utf8(&content) {
            let title = to_title_case(text);
            self.replace_selection_with(title.as_bytes());
        }
    }
}

impl TextBuffer {
    fn highlight_line(language: Language, line: &str) -> Vec<HighlightSpan> {
        if line.is_empty() {
            return Vec::new();
        }

        let mut spans = Vec::new();

        match language {
            Language::PlainText => {}
            Language::Csv => {
                let mut column: u8 = 0;
                let mut field_start: usize = 0;
                let mut in_quotes = false;
                let bytes = line.as_bytes();
                let mut i = 0;

                while i < bytes.len() {
                    let b = bytes[i];
                    if in_quotes {
                        if b == b'"' {
                            // Handle escaped quotes ("")
                            if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                                i += 2;
                                continue;
                            }
                            in_quotes = false;
                        }
                        i += 1;
                    } else {
                        match b {
                            b'"' => {
                                in_quotes = true;
                                i += 1;
                            }
                            b',' => {
                                if i > field_start {
                                    spans.push(HighlightSpan {
                                        start: field_start,
                                        end: i,
                                        kind: HighlightKind::CsvColumn(column),
                                    });
                                }
                                column = column.wrapping_add(1);
                                field_start = i + 1;
                                i += 1;
                            }
                            _ => i += 1,
                        }
                    }
                }
                // Final field
                if field_start < bytes.len() {
                    spans.push(HighlightSpan {
                        start: field_start,
                        end: bytes.len(),
                        kind: HighlightKind::CsvColumn(column),
                    });
                }
            }
            Language::Ini => {
                let trimmed = line.trim_start();
                let indent = line.len() - trimmed.len();

                // Section header: [section_name]
                if trimmed.starts_with('[') {
                    if let Some(end) = trimmed.find(']') {
                        spans.push(HighlightSpan {
                            start: indent,
                            end: indent + end + 1,
                            kind: HighlightKind::Section,
                        });
                        return spans;
                    }
                }

                // Comment (# or ;)
                let comment_start = trimmed
                    .find('#')
                    .or_else(|| trimmed.find(';'))
                    .map(|p| indent + p);

                if let Some(cs) = comment_start {
                    spans.push(HighlightSpan {
                        start: cs,
                        end: line.len(),
                        kind: HighlightKind::Comment,
                    });
                }

                // Key = value
                let line_end = comment_start.unwrap_or(line.len());
                if let Some(eq_pos) = line[..line_end].find('=') {
                    let key_part = &line[indent..eq_pos];
                    let key_end = indent + key_part.trim_end().len();
                    if key_end > indent {
                        spans.push(HighlightSpan {
                            start: indent,
                            end: key_end,
                            kind: HighlightKind::Key,
                        });
                    }

                    // Check for strings in value
                    let value_start = eq_pos + 1;
                    let value_part = &line[value_start..line_end];
                    for range in Self::scan_strings(value_part, '"') {
                        spans.push(HighlightSpan {
                            start: value_start + range.start,
                            end: value_start + range.end,
                            kind: HighlightKind::String,
                        });
                    }

                    // Check for keywords in value
                    let value_trimmed = value_part.trim();
                    let value_lower = value_trimmed.to_ascii_lowercase();
                    if matches!(value_lower.as_str(), "true" | "false" | "on" | "off" | "yes" | "no") {
                        let kw_start = value_start + value_part.find(value_trimmed).unwrap_or(0);
                        spans.push(HighlightSpan {
                            start: kw_start,
                            end: kw_start + value_trimmed.len(),
                            kind: HighlightKind::Keyword,
                        });
                    }
                }
            }
            Language::Toml => {
                let trimmed = line.trim_start();
                let indent = line.len() - trimmed.len();

                // Table header: [table] or [[array]]
                if trimmed.starts_with('[') {
                    if let Some(end) = trimmed.rfind(']') {
                        spans.push(HighlightSpan {
                            start: indent,
                            end: indent + end + 1,
                            kind: HighlightKind::Section,
                        });
                        return spans;
                    }
                }

                // Comment
                let comment_start = Self::find_comment_start(line, "#", &[]);
                if let Some(cs) = comment_start {
                    spans.push(HighlightSpan {
                        start: cs,
                        end: line.len(),
                        kind: HighlightKind::Comment,
                    });
                }

                // Key = value
                let line_end = comment_start.unwrap_or(line.len());
                if let Some(eq_pos) = line[..line_end].find('=') {
                    let key_part = &line[indent..eq_pos];
                    let key_end = indent + key_part.trim_end().len();
                    if key_end > indent {
                        spans.push(HighlightSpan {
                            start: indent,
                            end: key_end,
                            kind: HighlightKind::Key,
                        });
                    }

                    // Check for strings in value
                    let value_start = eq_pos + 1;
                    let value_part = &line[value_start..line_end];
                    let mut string_ranges = Self::scan_strings(value_part, '"');
                    string_ranges.extend(Self::scan_strings(value_part, '\''));
                    for range in string_ranges {
                        spans.push(HighlightSpan {
                            start: value_start + range.start,
                            end: value_start + range.end,
                            kind: HighlightKind::String,
                        });
                    }

                    // Check for keywords in value
                    let value_trimmed = value_part.trim();
                    if matches!(value_trimmed, "true" | "false") {
                        let kw_start = value_start + value_part.find(value_trimmed).unwrap_or(0);
                        spans.push(HighlightSpan {
                            start: kw_start,
                            end: kw_start + value_trimmed.len(),
                            kind: HighlightKind::Keyword,
                        });
                    }
                }
            }
            Language::Yaml => {
                let trimmed = line.trim_start();
                let indent = line.len() - trimmed.len();

                // Comment
                let comment_start = Self::find_comment_start(line, "#", &[]);
                if let Some(cs) = comment_start {
                    spans.push(HighlightSpan {
                        start: cs,
                        end: line.len(),
                        kind: HighlightKind::Comment,
                    });
                }

                let line_end = comment_start.unwrap_or(line.len());
                let working_line = &line[..line_end];

                // Key: value (find first colon not inside quotes)
                let mut in_quotes = false;
                let mut quote_char = '"';
                let mut colon_pos = None;

                for (i, ch) in working_line.char_indices() {
                    if !in_quotes && (ch == '"' || ch == '\'') {
                        in_quotes = true;
                        quote_char = ch;
                    } else if in_quotes && ch == quote_char {
                        in_quotes = false;
                    } else if !in_quotes && ch == ':' {
                        colon_pos = Some(i);
                        break;
                    }
                }

                if let Some(cp) = colon_pos {
                    // Key is from indent to colon
                    let key_part = &line[indent..cp];
                    if !key_part.trim().is_empty() && !key_part.starts_with('-') {
                        spans.push(HighlightSpan {
                            start: indent,
                            end: cp,
                            kind: HighlightKind::Key,
                        });
                    }

                    // Check for strings and keywords in value
                    let value_start = cp + 1;
                    if value_start < line_end {
                        let value_part = &line[value_start..line_end];
                        let mut string_ranges = Self::scan_strings(value_part, '"');
                        string_ranges.extend(Self::scan_strings(value_part, '\''));
                        for range in string_ranges {
                            spans.push(HighlightSpan {
                                start: value_start + range.start,
                                end: value_start + range.end,
                                kind: HighlightKind::String,
                            });
                        }

                        // Keywords
                        let value_trimmed = value_part.trim();
                        if matches!(value_trimmed, "true" | "false" | "null" | "~") {
                            let kw_start = value_start + value_part.find(value_trimmed).unwrap_or(0);
                            spans.push(HighlightSpan {
                                start: kw_start,
                                end: kw_start + value_trimmed.len(),
                                kind: HighlightKind::Keyword,
                            });
                        }
                    }
                } else {
                    // No colon - might be a list item or just a value
                    let mut string_ranges = Self::scan_strings(working_line, '"');
                    string_ranges.extend(Self::scan_strings(working_line, '\''));
                    for range in string_ranges {
                        spans.push(HighlightSpan {
                            start: range.start,
                            end: range.end,
                            kind: HighlightKind::String,
                        });
                    }
                }
            }
            Language::Json => {
                // Scan all strings first
                let string_ranges = Self::scan_strings(line, '"');

                // Check each string - if followed by `:`, it's a key
                for range in &string_ranges {
                    let after = line[range.end..].trim_start();
                    if after.starts_with(':') {
                        spans.push(HighlightSpan {
                            start: range.start,
                            end: range.end,
                            kind: HighlightKind::Key,
                        });
                    } else {
                        spans.push(HighlightSpan {
                            start: range.start,
                            end: range.end,
                            kind: HighlightKind::String,
                        });
                    }
                }

                // Keywords and numbers
                let bytes = line.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    // Skip if inside a string
                    if string_ranges.iter().any(|r| i >= r.start && i < r.end) {
                        i += 1;
                        continue;
                    }

                    // Check for keywords
                    if line[i..].starts_with("true") {
                        spans.push(HighlightSpan {
                            start: i,
                            end: i + 4,
                            kind: HighlightKind::Keyword,
                        });
                        i += 4;
                    } else if line[i..].starts_with("false") {
                        spans.push(HighlightSpan {
                            start: i,
                            end: i + 5,
                            kind: HighlightKind::Keyword,
                        });
                        i += 5;
                    } else if line[i..].starts_with("null") {
                        spans.push(HighlightSpan {
                            start: i,
                            end: i + 4,
                            kind: HighlightKind::Keyword,
                        });
                        i += 4;
                    } else {
                        i += 1;
                    }
                }
            }
            Language::Markdown | Language::Mdx => {
                let trimmed = line.trim_start();
                if trimmed.starts_with('#') {
                    spans.push(HighlightSpan {
                        start: 0,
                        end: line.len(),
                        kind: HighlightKind::Keyword,
                    });
                }
            }
            Language::Rust
            | Language::Python
            | Language::JavaScript
            | Language::TypeScript
            | Language::Html
            | Language::Css
            | Language::Shell
            | Language::C
            | Language::Cpp
            | Language::CSharp
            | Language::Go
            | Language::Java
            | Language::Kotlin
            | Language::Ruby
            | Language::Php
            | Language::Sql
            | Language::Xml
            | Language::Lua
            | Language::Makefile
            | Language::PowerShell
            | Language::R
            | Language::Swift
            | Language::ObjectiveC
            | Language::Dart
            | Language::Scala
            | Language::Haskell
            | Language::Elixir
            | Language::Erlang
            | Language::Clojure
            | Language::FSharp
            | Language::VbNet
            | Language::Perl
            | Language::Groovy
            | Language::Terraform
            | Language::Nix
            | Language::Assembly
            | Language::Latex
            | Language::Graphql => {
                let string_ranges = match language {
                    Language::Rust => Self::scan_strings(line, '"'),
                    Language::Markdown | Language::Mdx => Vec::new(),
                    Language::Html => {
                        let mut ranges = Self::scan_strings(line, '"');
                        ranges.extend(Self::scan_strings(line, '\''));
                        ranges
                    }
                    Language::Python => {
                        let mut ranges = Self::scan_strings(line, '"');
                        ranges.extend(Self::scan_strings(line, '\''));
                        ranges
                    }
                    Language::JavaScript | Language::TypeScript | Language::Css => {
                        let mut ranges = Self::scan_strings(line, '"');
                        ranges.extend(Self::scan_strings(line, '\''));
                        if matches!(language, Language::JavaScript | Language::TypeScript) {
                            ranges.extend(Self::scan_strings(line, '`'));
                        }
                        ranges
                    }
                    Language::Shell
                    | Language::Ruby
                    | Language::Php
                    | Language::Lua
                    | Language::Makefile
                    | Language::PowerShell
                    | Language::R
                    | Language::Sql
                    | Language::Xml
                    | Language::C
                    | Language::Cpp
                    | Language::CSharp
                    | Language::Go
                    | Language::Java
                    | Language::Kotlin
                    | Language::Swift
                    | Language::ObjectiveC
                    | Language::Dart
                    | Language::Scala
                    | Language::Haskell
                    | Language::Elixir
                    | Language::Erlang
                    | Language::Clojure
                    | Language::FSharp
                    | Language::VbNet
                    | Language::Perl
                    | Language::Groovy
                    | Language::Terraform
                    | Language::Nix
                    | Language::Assembly
                    | Language::Latex
                    | Language::Graphql => {
                        let mut ranges = Self::scan_strings(line, '"');
                        ranges.extend(Self::scan_strings(line, '\''));
                        ranges
                    }
                    _ => Vec::new(),
                };

                let comment_start = match language {
                    Language::Rust => Self::find_comment_start(line, "//", &string_ranges),
                    Language::Python => Self::find_comment_start(line, "#", &string_ranges),
                    Language::JavaScript | Language::TypeScript => {
                        Self::find_comment_start(line, "//", &string_ranges)
                    }
                    Language::Css => Self::find_comment_start(line, "/*", &string_ranges),
                    Language::Html => Self::find_comment_start(line, "<!--", &string_ranges),
                    Language::Shell
                    | Language::Ruby
                    | Language::Makefile
                    | Language::R
                    | Language::PowerShell => Self::find_comment_start(line, "#", &string_ranges),
                    Language::C
                    | Language::Cpp
                    | Language::CSharp
                    | Language::Go
                    | Language::Java
                    | Language::Kotlin
                    | Language::Swift
                    | Language::ObjectiveC
                    | Language::Dart
                    | Language::Scala
                    | Language::FSharp
                    | Language::Groovy
                    | Language::Php => Self::find_comment_start(line, "//", &string_ranges),
                    Language::Sql => Self::find_comment_start(line, "--", &string_ranges),
                    Language::Lua => Self::find_comment_start(line, "--", &string_ranges),
                    Language::Xml => Self::find_comment_start(line, "<!--", &string_ranges),
                    Language::Haskell => Self::find_comment_start(line, "--", &string_ranges),
                    Language::Elixir => Self::find_comment_start(line, "#", &string_ranges),
                    Language::Erlang => Self::find_comment_start(line, "%", &string_ranges),
                    Language::Clojure => Self::find_comment_start(line, ";", &string_ranges),
                    Language::Terraform | Language::Nix => {
                        Self::find_comment_start(line, "#", &string_ranges)
                    }
                    Language::VbNet => Self::find_comment_start(line, "'", &string_ranges),
                    Language::Perl => Self::find_comment_start(line, "#", &string_ranges),
                    Language::Assembly => Self::find_comment_start(line, ";", &string_ranges),
                    Language::Latex => Self::find_comment_start(line, "%", &string_ranges),
                    Language::Graphql => Self::find_comment_start(line, "#", &string_ranges),
                    _ => None,
                };

                if let Some(start) = comment_start {
                    spans.push(HighlightSpan {
                        start,
                        end: line.len(),
                        kind: HighlightKind::Comment,
                    });
                }

                for range in &string_ranges {
                    if comment_start.map_or(true, |c| range.start < c) {
                        spans.push(HighlightSpan {
                            start: range.start,
                            end: range.end,
                            kind: HighlightKind::String,
                        });
                    }
                }

                let word_limit = comment_start.unwrap_or(line.len());
                let bytes = line.as_bytes();

                let keywords = match language {
                    Language::Rust => &[
                        "fn", "let", "mut", "pub", "struct", "enum", "impl", "use", "mod",
                        "trait", "match", "if", "else", "for", "while", "loop", "return", "self",
                        "Self", "crate", "super", "const", "static", "ref", "as", "in", "where",
                        "async", "await",
                    ][..],
                    Language::Python => &[
                        "def", "class", "self", "return", "import", "from", "as", "if", "elif",
                        "else", "for", "while", "try", "except", "finally", "with", "lambda",
                        "yield", "True", "False", "None", "and", "or", "not", "in", "is",
                    ][..],
                    Language::JavaScript | Language::TypeScript => &[
                        "function", "return", "const", "let", "var", "if", "else", "for", "while",
                        "do", "switch", "case", "break", "continue", "try", "catch", "finally",
                        "throw", "class", "extends", "new", "this", "super", "import", "from",
                        "export", "default", "async", "await", "true", "false", "null", "undefined",
                    ][..],
                    Language::Html => &[
                        "html", "head", "body", "div", "span", "p", "a", "ul", "ol", "li",
                        "header", "footer", "section", "nav", "main", "img", "input", "button",
                        "form", "label", "table", "tr", "td", "th", "thead", "tbody", "script",
                        "style",
                    ][..],
                    Language::Css => &[
                        "color", "background", "display", "flex", "grid", "margin", "padding",
                        "font", "position", "absolute", "relative", "fixed", "border", "width",
                        "height", "gap",
                    ][..],
                    Language::Shell => &[
                        "if", "then", "fi", "for", "do", "done", "case", "esac", "function", "in",
                    ][..],
                    Language::C | Language::Cpp => &[
                        "int", "char", "void", "struct", "class", "namespace", "if", "else", "for",
                        "while", "return", "const", "static", "typedef", "enum",
                    ][..],
                    Language::CSharp => &[
                        "class", "struct", "interface", "using", "namespace", "public", "private",
                        "protected", "static", "async", "await", "return", "new",
                    ][..],
                    Language::Go => &[
                        "package", "import", "func", "type", "struct", "interface", "if", "else",
                        "for", "return", "go", "defer",
                    ][..],
                    Language::Java => &[
                        "class", "interface", "extends", "implements", "public", "private",
                        "protected", "static", "final", "return", "new", "import",
                    ][..],
                    Language::Kotlin => &[
                        "class", "interface", "fun", "val", "var", "object", "when", "if", "else",
                        "for", "while", "return", "import",
                    ][..],
                    Language::Ruby => &[
                        "def", "class", "module", "end", "if", "elsif", "else", "true", "false",
                        "nil", "require", "return",
                    ][..],
                    Language::Php => &[
                        "function", "class", "public", "private", "protected", "echo", "return",
                        "true", "false", "null", "new",
                    ][..],
                    Language::Sql => &[
                        "select", "from", "where", "join", "insert", "update", "delete", "create",
                        "table", "into", "values", "and", "or", "null",
                    ][..],
                    Language::Xml => &["xml", "doctype"][..],
                    Language::Lua => &[
                        "function", "local", "end", "if", "then", "elseif", "else", "for", "while",
                        "return", "true", "false", "nil",
                    ][..],
                    Language::Makefile => &["if", "else", "endif", "include"][..],
                    Language::PowerShell => &[
                        "function", "param", "if", "else", "foreach", "return", "$true", "$false",
                        "$null",
                    ][..],
                    Language::R => &[
                        "function", "if", "else", "for", "while", "return", "TRUE", "FALSE",
                        "NULL",
                    ][..],
                    Language::Swift => &[
                        "class", "struct", "enum", "protocol", "func", "let", "var", "if", "else",
                        "for", "while", "return", "import",
                    ][..],
                    Language::ObjectiveC => &[
                        "interface", "implementation", "end", "class", "void", "int", "return",
                        "if", "else", "for", "while", "nil",
                    ][..],
                    Language::Dart => &[
                        "class", "enum", "extends", "implements", "import", "library", "void",
                        "final", "var", "if", "else", "for", "while", "return",
                    ][..],
                    Language::Scala => &[
                        "class", "object", "trait", "def", "val", "var", "if", "else", "for",
                        "while", "match", "case", "return",
                    ][..],
                    Language::Haskell => &[
                        "module", "where", "import", "data", "type", "let", "in", "if", "then",
                        "else", "case", "of",
                    ][..],
                    Language::Elixir => &[
                        "def", "defmodule", "do", "end", "if", "else", "case", "when", "true",
                        "false", "nil",
                    ][..],
                    Language::Erlang => &[
                        "module", "export", "fun", "if", "case", "of", "end", "true", "false",
                    ][..],
                    Language::Clojure => &[
                        "def", "defn", "let", "if", "do", "fn", "true", "false", "nil",
                    ][..],
                    Language::FSharp => &[
                        "let", "module", "type", "open", "if", "then", "else", "match", "with",
                        "fun", "true", "false",
                    ][..],
                    Language::VbNet => &[
                        "Class", "Module", "Sub", "Function", "End", "If", "Then", "Else",
                        "Dim", "As", "Return",
                    ][..],
                    Language::Perl => &[
                        "my", "our", "sub", "use", "if", "else", "elsif", "return", "undef",
                    ][..],
                    Language::Groovy => &[
                        "class", "def", "import", "if", "else", "for", "while", "return", "new",
                    ][..],
                    Language::Terraform => &[
                        "resource", "variable", "output", "module", "provider", "data", "true",
                        "false",
                    ][..],
                    Language::Nix => &[
                        "let", "in", "with", "rec", "if", "then", "else", "true", "false", "null",
                    ][..],
                    Language::Assembly => &["mov", "add", "sub", "jmp", "call", "ret"][..],
                    Language::Latex => &[
                        "documentclass", "begin", "end", "usepackage", "section", "subsection",
                        "title", "author",
                    ][..],
                    Language::Graphql => &[
                        "query", "mutation", "subscription", "fragment", "on", "true", "false",
                        "null",
                    ][..],
                    _ => &[][..],
                };

                let mut i = 0;
                while i < word_limit {
                    let b = bytes[i];
                    let is_word_start = b.is_ascii_alphabetic() || b == b'_';
                    if !is_word_start {
                        i += 1;
                        continue;
                    }

                    let start = i;
                    i += 1;
                    while i < word_limit {
                        let b = bytes[i];
                        if !(b.is_ascii_alphanumeric() || b == b'_') {
                            break;
                        }
                        i += 1;
                    }

                    if !Self::is_in_ranges(start, &string_ranges) {
                        let word = &line[start..i];
                        if keywords.iter().any(|&kw| kw == word) {
                            spans.push(HighlightSpan {
                                start,
                                end: i,
                                kind: HighlightKind::Keyword,
                            });
                        }
                    }
                }

                let mut i = 0;
                while i < word_limit {
                    let b = bytes[i];
                    if !b.is_ascii_digit() || Self::is_in_ranges(i, &string_ranges) {
                        i += 1;
                        continue;
                    }
                    let start = i;
                    i += 1;
                    while i < word_limit {
                        let b = bytes[i];
                        if !(b.is_ascii_digit() || b == b'.' || b == b'_') {
                            break;
                        }
                        i += 1;
                    }
                    spans.push(HighlightSpan {
                        start,
                        end: i,
                        kind: HighlightKind::Number,
                    });
                }
            }
        }

        spans
    }

    fn scan_strings(line: &str, quote: char) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let bytes = line.as_bytes();
        let quote = quote as u8;
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] != quote || Self::is_escaped(bytes, i) {
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == quote && !Self::is_escaped(bytes, i) {
                    ranges.push(start..(i + 1));
                    i += 1;
                    break;
                }
                i += 1;
            }
        }

        ranges
    }

    fn is_escaped(bytes: &[u8], index: usize) -> bool {
        if index == 0 {
            return false;
        }

        let mut i = index;
        let mut count = 0;
        while i > 0 {
            i -= 1;
            if bytes[i] != b'\\' {
                break;
            }
            count += 1;
        }
        count % 2 == 1
    }

    fn is_in_ranges(index: usize, ranges: &[Range<usize>]) -> bool {
        ranges.iter().any(|r| r.start <= index && index < r.end)
    }

    fn find_comment_start(
        line: &str,
        needle: &str,
        string_ranges: &[Range<usize>],
    ) -> Option<usize> {
        for (idx, _) in line.match_indices(needle) {
            if !Self::is_in_ranges(idx, string_ranges) {
                return Some(idx);
            }
        }
        None
    }
}

fn find_search_matches(line: &str, needle: &str, options: SearchOptions) -> Vec<Range<usize>> {
    if line.is_empty() || needle.is_empty() {
        return Vec::new();
    }

    let (haystack, needle_cmp): (Cow<'_, str>, Cow<'_, str>) = if options.match_case {
        (Cow::Borrowed(line), Cow::Borrowed(needle))
    } else {
        let line_lower = String::from_utf8(
            line.as_bytes().iter().map(|b| b.to_ascii_lowercase()).collect(),
        )
        .unwrap();
        let needle_lower = String::from_utf8(
            needle.as_bytes().iter().map(|b| b.to_ascii_lowercase()).collect(),
        )
        .unwrap();
        (Cow::Owned(line_lower), Cow::Owned(needle_lower))
    };
    let haystack = haystack.as_ref();
    let needle_cmp = needle_cmp.as_ref();

    let mut matches = Vec::new();
    let needle_len = needle_cmp.len();
    let mut search_start = 0;
    while search_start < haystack.len() {
        let Some(pos) = haystack[search_start..].find(needle_cmp) else {
            break;
        };
        let start = search_start + pos;
        let end = start + needle_len;
        if options.whole_word && !is_word_boundary(line.as_bytes(), start, end) {
            search_start = start + 1;
            continue;
        }
        matches.push(start..end);
        if needle_len == 0 {
            search_start = start + 1;
        } else {
            search_start = end;
        }
    }

    matches
}

fn is_word_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    let before = start > 0 && is_word_byte(bytes[start - 1]);
    let after = end < bytes.len() && is_word_byte(bytes[end]);
    !before && !after
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn count_columns(text: &str, byte_off: usize) -> usize {
    let bytes = text.as_bytes();
    let mut cfg = MeasurementConfig::new(&bytes);
    let cursor = cfg.goto_offset(byte_off.min(text.len()));
    cursor.visual_pos.x as usize
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

const BOM_MAX_LEN: usize = 4;

fn detect_bom(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 4 {
        if bytes.starts_with(b"\xFF\xFE\x00\x00") {
            return Some("UTF-32LE");
        }
        if bytes.starts_with(b"\x00\x00\xFE\xFF") {
            return Some("UTF-32BE");
        }
        if bytes.starts_with(b"\x84\x31\x95\x33") {
            return Some("GB18030");
        }
    }
    if bytes.len() >= 3 && bytes.starts_with(b"\xEF\xBB\xBF") {
        return Some("UTF-8");
    }
    if bytes.len() >= 2 {
        if bytes.starts_with(b"\xFF\xFE") {
            return Some("UTF-16LE");
        }
        if bytes.starts_with(b"\xFE\xFF") {
            return Some("UTF-16BE");
        }
    }
    None
}

// ========== Encoding/Decoding Helper Functions ==========

const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8]) -> String {
    let mut result = String::with_capacity((input.len() + 2) / 3 * 4);

    for chunk in input.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;

        result.push(BASE64_CHARS[b0 >> 2] as char);
        result.push(BASE64_CHARS[((b0 & 0x03) << 4) | (b1 >> 4)] as char);

        if chunk.len() > 1 {
            result.push(BASE64_CHARS[((b1 & 0x0f) << 2) | (b2 >> 6)] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(BASE64_CHARS[b2 & 0x3f] as char);
        } else {
            result.push('=');
        }
    }

    result
}

fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    // Filter out whitespace and validate
    let filtered: Vec<u8> = input.iter().copied().filter(|&b| !b.is_ascii_whitespace()).collect();

    if filtered.is_empty() {
        return Some(Vec::new());
    }

    // Must be multiple of 4
    if filtered.len() % 4 != 0 {
        return None;
    }

    let mut result = Vec::with_capacity(filtered.len() / 4 * 3);

    for chunk in filtered.chunks(4) {
        let mut vals = [0u8; 4];
        let mut padding = 0;

        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                vals[i] = 0;
                padding += 1;
            } else {
                vals[i] = match b {
                    b'A'..=b'Z' => b - b'A',
                    b'a'..=b'z' => b - b'a' + 26,
                    b'0'..=b'9' => b - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => return None,
                };
            }
        }

        result.push((vals[0] << 2) | (vals[1] >> 4));
        if padding < 2 {
            result.push((vals[1] << 4) | (vals[2] >> 2));
        }
        if padding < 1 {
            result.push((vals[2] << 6) | vals[3]);
        }
    }

    Some(result)
}

const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

fn hex_encode(input: &[u8]) -> String {
    let mut result = String::with_capacity(input.len() * 2);
    for &byte in input {
        result.push(HEX_CHARS[(byte >> 4) as usize] as char);
        result.push(HEX_CHARS[(byte & 0x0f) as usize] as char);
    }
    result
}

fn hex_decode(input: &[u8]) -> Option<Vec<u8>> {
    // Filter out whitespace
    let filtered: Vec<u8> = input.iter().copied().filter(|&b| !b.is_ascii_whitespace()).collect();

    if filtered.len() % 2 != 0 {
        return None;
    }

    let mut result = Vec::with_capacity(filtered.len() / 2);

    for chunk in filtered.chunks(2) {
        let high = hex_digit_value(chunk[0])?;
        let low = hex_digit_value(chunk[1])?;
        result.push((high << 4) | low);
    }

    Some(result)
}

fn hex_digit_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

fn url_encode(input: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(input.len() * 3);
    for &byte in input {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte);
            }
            _ => {
                result.push(b'%');
                result.push(HEX_CHARS[(byte >> 4) as usize]);
                result.push(HEX_CHARS[(byte & 0x0f) as usize]);
            }
        }
    }
    result
}

fn url_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut result = Vec::with_capacity(input.len());
    let mut i = 0;

    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            let high = hex_digit_value(input[i + 1])?;
            let low = hex_digit_value(input[i + 2])?;
            result.push((high << 4) | low);
            i += 3;
        } else if input[i] == b'+' {
            result.push(b' ');
            i += 1;
        } else {
            result.push(input[i]);
            i += 1;
        }
    }

    Some(result)
}

fn to_title_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = true;

    for c in s.chars() {
        if c.is_whitespace() || c == '-' || c == '_' {
            result.push(c);
            capitalize_next = true;
        } else if capitalize_next {
            for uc in c.to_uppercase() {
                result.push(uc);
            }
            capitalize_next = false;
        } else {
            for lc in c.to_lowercase() {
                result.push(lc);
            }
        }
    }

    result
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
