// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

impl TextBuffer {
    pub(super) fn edit_begin_grouping(&mut self) {
        self.active_edit_group = Some(ActiveEditGroupInfo {
            cursor_before: self.cursor.logical_pos,
            selection_before: self.selection,
            stats_before: self.stats,
            generation_before: self.buffer.generation(),
        });
    }

    pub(super) fn edit_end_grouping(&mut self) {
        self.active_edit_group = None;
    }

    /// Starts a new edit operation.
    /// This is used for tracking the undo/redo history.
    pub(super) fn edit_begin(&mut self, history_type: HistoryType, cursor: Cursor) {
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
    pub(super) fn edit_write(&mut self, text: &[u8]) {
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
    pub(super) fn edit_delete(&mut self, to: Cursor) {
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
    pub(super) fn edit_end(&mut self) {
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
}
