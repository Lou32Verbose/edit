// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

impl TextBuffer {
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
        let start_cursor =
            self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y: beg_y });
        let end_cursor =
            self.cursor_move_to_logical_internal(start_cursor, Point { x: 0, y: end_y + 1 });

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
        let start_cursor =
            self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y: beg_y });
        let end_cursor =
            self.cursor_move_to_logical_internal(start_cursor, Point { x: 0, y: end_y + 1 });

        // Extract the content
        let mut content = Vec::new();
        self.buffer.extract_raw(start_cursor.offset..end_cursor.offset, &mut content, 0);

        // Split into lines and remove duplicates (preserving order)
        let content_str = String::from_utf8_lossy(&content);
        let mut seen = std::collections::HashSet::new();
        let unique_lines: Vec<&str> =
            content_str.lines().filter(|line| seen.insert(*line)).collect();

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
        let start_cursor =
            self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y: beg_y });
        let end_cursor =
            self.cursor_move_to_logical_internal(start_cursor, Point { x: 0, y: end_y + 1 });

        // Extract the content
        let mut content = Vec::new();
        self.buffer.extract_raw(start_cursor.offset..end_cursor.offset, &mut content, 0);

        // Split into lines and filter out empty ones
        let content_str = String::from_utf8_lossy(&content);
        let non_empty_lines: Vec<&str> =
            content_str.lines().filter(|line| !line.trim().is_empty()).collect();

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
        let new_y =
            cursor.logical_pos.y.min(beg_y + non_empty_lines.len().saturating_sub(1) as CoordType);
        self.cursor_move_to_logical(Point { x: cursor.logical_pos.x, y: new_y });
        self.set_selection(None);
    }

    /// Trims trailing whitespace from all lines.
    pub fn trim_trailing_whitespace(&mut self) {
        self.edit_begin_grouping();

        // Work backwards from last line to first
        for y in (0..self.stats.logical_lines).rev() {
            // Find end of line content (before any trailing whitespace)
            let line_start = self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y });
            let line_end =
                self.cursor_move_to_logical_internal(line_start, Point { x: CoordType::MAX, y });

            // Read backwards from line_end to find trailing whitespace
            if line_start.offset >= line_end.offset {
                continue;
            }

            let mut content = Vec::new();
            self.buffer.extract_raw(line_start.offset..line_end.offset, &mut content, 0);

            // Find where trailing whitespace starts
            let trimmed_len =
                content.iter().rposition(|&b| b != b' ' && b != b'\t').map(|i| i + 1).unwrap_or(0);

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
            Language::Rust
            | Language::C
            | Language::Cpp
            | Language::CSharp
            | Language::Go
            | Language::Java
            | Language::JavaScript
            | Language::TypeScript
            | Language::Swift
            | Language::Kotlin
            | Language::Dart
            | Language::Scala => Some("//"),
            Language::Python
            | Language::Ruby
            | Language::Shell
            | Language::Yaml
            | Language::Toml
            | Language::Ini
            | Language::R
            | Language::Perl => Some("#"),
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
            Language::C
            | Language::Cpp
            | Language::CSharp
            | Language::Java
            | Language::JavaScript
            | Language::TypeScript
            | Language::Go
            | Language::Rust
            | Language::Swift
            | Language::Kotlin
            | Language::Css
            | Language::Dart
            | Language::Scala
            | Language::Php => Some(("/*", "*/")),
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
            let line_end =
                self.cursor_move_to_logical_internal(line_start, Point { x: CoordType::MAX, y });

            let mut content = Vec::new();
            self.buffer.extract_raw(line_start.offset..line_end.offset, &mut content, 0);

            // Skip leading whitespace
            let trimmed =
                content.iter().position(|&b| b != b' ' && b != b'\t').unwrap_or(content.len());
            if !content[trimmed..].starts_with(prefix_bytes) {
                all_commented = false;
                break;
            }
        }

        self.edit_begin_grouping();

        // Process lines in reverse order to preserve offsets
        for y in (beg_y..=end_y).rev() {
            let line_start = self.cursor_move_to_logical_internal(self.cursor, Point { x: 0, y });
            let line_end =
                self.cursor_move_to_logical_internal(line_start, Point { x: CoordType::MAX, y });

            let mut content = Vec::new();
            self.buffer.extract_raw(line_start.offset..line_end.offset, &mut content, 0);

            let ws_len =
                content.iter().position(|&b| b != b' ' && b != b'\t').unwrap_or(content.len());

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
    pub(super) fn find_matching_bracket_offset(&self) -> Option<usize> {
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
}
