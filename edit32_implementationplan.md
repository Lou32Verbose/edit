  Implementation Plan: Line Operations & Statistics

  Overview

  This plan covers implementing four features:
  1. Remove Duplicate Lines - Delete duplicate lines keeping first occurrence
  2. Remove Empty Lines - Delete all blank lines
  3. Word Count - Show word count in statusbar
  4. Character Count - Show character/byte count in statusbar

  ---
  1. Remove Duplicate Lines

  Overview

  Removes duplicate lines from the selection (or entire document if no selection), keeping the first occurrence of each unique line.

  Why This Approach

  Follow the established pattern from sort_lines() at buffer/mod.rs:2828: extract lines, process them, replace content. Using an IndexSet preserves insertion order while deduplicating.

  Implementation

  Step 1: Add method to TextBuffer (crates/edit/src/buffer/mod.rs)

  Add after sort_lines() (around line 2880):

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

  Step 2: Add CommandId (crates/edit/src/bin/edit/commands.rs)

  EditRemoveDuplicateLines,

  Step 3: Add shortcut (vk::NULL - command palette only)

  EditRemoveDuplicateLines => vk::NULL,

  Step 4: Add to command_list

  Command { id: EditRemoveDuplicateLines, label: "Remove Duplicate Lines", requires_document: true, show_in_palette: true },

  Step 5: Add handler in run_command

  EditRemoveDuplicateLines => {
      if let Some(doc) = state.documents.active() {
          doc.buffer.borrow_mut().remove_duplicate_lines();
          ctx.needs_rerender();
      }
  }

  Step 6: Add to command_group (Edit group)

  | CommandId::EditRemoveDuplicateLines

  ---
  2. Remove Empty Lines

  Overview

  Removes all empty or whitespace-only lines from the selection (or entire document).

  Why This Approach

  Similar to remove_duplicate_lines() but filters out empty/whitespace-only lines.

  Implementation

  Step 1: Add method to TextBuffer (crates/edit/src/buffer/mod.rs)

  Add after remove_duplicate_lines():

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

  Step 2: Add CommandId

  EditRemoveEmptyLines,

  Step 3: Add shortcut

  EditRemoveEmptyLines => vk::NULL,

  Step 4: Add to command_list

  Command { id: EditRemoveEmptyLines, label: "Remove Empty Lines", requires_document: true, show_in_palette: true },

  Step 5: Add handler

  EditRemoveEmptyLines => {
      if let Some(doc) = state.documents.active() {
          doc.buffer.borrow_mut().remove_empty_lines();
          ctx.needs_rerender();
      }
  }

  Step 6: Add to command_group

  | CommandId::EditRemoveEmptyLines

  ---
  3 & 4. Word Count & Character Count (Statusbar)

  Overview

  Display word count and character count in the statusbar. Character count shows bytes for the whole document. Word count shows words for selection (if any) or whole document.

  Why This Approach

  Add a word_count() method to TextBuffer that efficiently counts words. Display both statistics as labels in the statusbar using the existing ctx.label() pattern. For selections, show selection stats; otherwise show document stats.

  Implementation

  Step 1: Add word_count method to TextBuffer (crates/edit/src/buffer/mod.rs)

  Add after text_length() (around line 380):

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

  Step 2: Add helper function at module level (crates/edit/src/buffer/mod.rs)

  Add at the end of the file with other helper functions:

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

  Step 3: Update statusbar (crates/edit/src/bin/edit/draw_statusbar.rs)

  After the location label (around line 170), add:

  // Word and character count
  if let Some(sel_len) = tb.selection_length() {
      // Show selection stats
      let sel_words = tb.selection_word_count().unwrap_or(0);
      ctx.label(
          "selection-stats",
          &arena_format!(
              ctx.arena(),
              "Sel: {} words, {} chars",
              sel_words,
              sel_len
          ),
      );
  } else {
      // Show document stats
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

  ---
  Summary of Changes by File
  ┌────────────────────────────────────────────┬────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
  │                    File                    │                                                                Changes                                                                 │
  ├────────────────────────────────────────────┼────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
  │ crates/edit/src/buffer/mod.rs              │ Add remove_duplicate_lines(), remove_empty_lines(), word_count(), selection_word_count(), selection_length(), and count_words() helper │
  ├────────────────────────────────────────────┼────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
  │ crates/edit/src/bin/edit/commands.rs       │ Add 2 CommandIds, shortcuts, handlers, command_list entries, command_group assignments                                                 │
  ├────────────────────────────────────────────┼────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
  │ crates/edit/src/bin/edit/draw_statusbar.rs │ Add word/character count display                                                                                                       │
  └────────────────────────────────────────────┴────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
  New Commands
  ┌────────────────────────┬──────────────────────┬──────────────────────────────────────────────────┐
  │        Command         │       Shortcut       │                   Description                    │
  ├────────────────────────┼──────────────────────┼──────────────────────────────────────────────────┤
  │ Remove Duplicate Lines │ Command Palette (F1) │ Remove duplicate lines, keeping first occurrence │
  ├────────────────────────┼──────────────────────┼──────────────────────────────────────────────────┤
  │ Remove Empty Lines     │ Command Palette (F1) │ Remove empty/whitespace-only lines               │
  └────────────────────────┴──────────────────────┴──────────────────────────────────────────────────┘
  Statusbar Display

  The statusbar will show:
  - When text is selected: Sel: X words, Y chars
  - When no selection: X words, Y chars

  This appears after the line:column indicator and before the dirty indicator.