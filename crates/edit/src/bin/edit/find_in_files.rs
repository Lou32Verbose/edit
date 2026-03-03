// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use edit::buffer::{SearchOptions, TextBuffer};
use edit::{apperr, sys};

use crate::documents::DocumentManager;
use crate::state::{DisplayablePathBuf, ReplacePreviewItem};

const DEFAULT_MAX_RESULTS: usize = 500;
const DEFAULT_MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;
const DEFAULT_MAX_RECURSION_DEPTH: usize = 24;
const DEFAULT_MAX_FILES_SCANNED: usize = 50_000;

fn env_usize(name: &str, default: usize, min: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|v| v.max(min))
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64, min: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| v.max(min))
        .unwrap_or(default)
}

pub fn max_results_limit() -> usize {
    env_usize("EDIT_FIND_MAX_RESULTS", DEFAULT_MAX_RESULTS, 1)
}

fn max_file_size_limit() -> u64 {
    env_u64("EDIT_FIND_MAX_FILE_SIZE", DEFAULT_MAX_FILE_SIZE, 1024)
}

fn max_recursion_depth_limit() -> usize {
    env_usize("EDIT_FIND_MAX_RECURSION_DEPTH", DEFAULT_MAX_RECURSION_DEPTH, 1)
}

fn max_files_scanned_limit() -> usize {
    env_usize("EDIT_FIND_MAX_FILES", DEFAULT_MAX_FILES_SCANNED, 1)
}

pub struct FindInFilesResult {
    pub path: DisplayablePathBuf,
    pub line: usize,
    pub column: usize,
    pub preview: String,
}

pub struct ReplaceStats {
    pub files_changed: usize,
    pub replacements: usize,
    pub skipped_dirty: usize,
}

pub fn search(root: &Path, query: &str, options: SearchOptions) -> Vec<FindInFilesResult> {
    if query.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut files = Vec::new();
    collect_files(root, &mut files);

    for path in files {
        if results.len() >= max_results_limit() {
            break;
        }

        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() > max_file_size_limit() {
            continue;
        }

        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };

        let available = max_results_limit().saturating_sub(results.len());
        let file_results = search_file(&path, &contents, query, options, available);
        results.extend(file_results);
        if results.len() >= max_results_limit() {
            break;
        }
    }

    results
}

pub fn replace_all(
    root: &Path,
    query: &str,
    replacement: &str,
    options: SearchOptions,
    documents: &mut DocumentManager,
) -> apperr::Result<ReplaceStats> {
    let mut stats = ReplaceStats { files_changed: 0, replacements: 0, skipped_dirty: 0 };

    if query.is_empty() {
        return Ok(stats);
    }

    let mut files = Vec::new();
    collect_files(root, &mut files);

    for path in files {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() > max_file_size_limit() {
            continue;
        }

        if documents.is_open_and_dirty(&path) {
            stats.skipped_dirty += 1;
            continue;
        }

        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        let (updated, replacements) =
            replace_all_in_text(&contents, query, replacement, options)?;
        if replacements == 0 || updated == contents {
            continue;
        }

        sys::atomic_write(&path, |file| {
            file.write_all(updated.as_bytes()).map_err(apperr::Error::from)
        })?;

        documents.reload_if_open_and_clean(&path)?;
        stats.files_changed += 1;
        stats.replacements += replacements;
    }

    Ok(stats)
}

pub fn preview_replace(
    root: &Path,
    query: &str,
    replacement: &str,
    options: SearchOptions,
) -> (Vec<ReplacePreviewItem>, String) {
    let query = query.trim_ascii();
    if query.is_empty() {
        return (Vec::new(), "No search text provided.".to_string());
    }

    let mut results = Vec::new();
    let mut files = Vec::new();
    collect_files(root, &mut files);

    for path in files {
        if results.len() >= max_results_limit() {
            break;
        }

        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() > max_file_size_limit() {
            continue;
        }

        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };

        let available = max_results_limit().saturating_sub(results.len());
        let file_preview = preview_file_replacements(&path, &contents, query, replacement, options, available);
        results.extend(file_preview);
        if results.len() >= max_results_limit() {
            break;
        }
    }

    let status = format!("{} preview item(s)", results.len());
    (results, status)
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) {
    let mut visited_dirs = HashSet::new();
    collect_files_inner(root, out, &mut visited_dirs, 0);
}

fn collect_files_inner(
    root: &Path,
    out: &mut Vec<PathBuf>,
    visited_dirs: &mut HashSet<PathBuf>,
    depth: usize,
) {
    if depth >= max_recursion_depth_limit() || out.len() >= max_files_scanned_limit() {
        return;
    }

    let canonical = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if !visited_dirs.insert(canonical) {
        return;
    }

    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        if out.len() >= max_files_scanned_limit() {
            break;
        }

        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_symlink() {
            continue;
        }

        if file_type.is_dir() {
            if should_skip_dir(&path) {
                continue;
            }
            collect_files_inner(&path, out, visited_dirs, depth + 1);
        } else if file_type.is_file() {
            out.push(path);
        }
    }
}

fn should_skip_dir(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    matches!(name, ".git" | ".hg" | ".svn" | "target" | "node_modules")
}

fn trim_preview(line: &str) -> String {
    let mut preview = line.replace('\t', "    ");
    let max_len = 120;
    if preview.len() > max_len {
        preview.truncate(max_len);
        preview.push_str("...");
    }
    preview
}

fn search_file(
    path: &Path,
    contents: &str,
    query: &str,
    options: SearchOptions,
    max_results: usize,
) -> Vec<FindInFilesResult> {
    if max_results == 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    let ranges = collect_match_ranges_in_text(contents, query, options, max_results);
    let line_starts = build_line_starts(contents);

    for (start, _) in ranges {
        let (line, column, line_text) = offset_to_line_col_and_text(contents, &line_starts, start);
        out.push(FindInFilesResult {
            path: DisplayablePathBuf::from_path(path.to_path_buf()),
            line,
            column,
            preview: trim_preview(line_text),
        });
        if out.len() >= max_results {
            break;
        }
    }

    out
}

fn preview_file_replacements(
    path: &Path,
    contents: &str,
    query: &str,
    replacement: &str,
    options: SearchOptions,
    max_results: usize,
) -> Vec<ReplacePreviewItem> {
    if max_results == 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    let ranges = collect_match_ranges_in_text(contents, query, options, max_results);
    let line_starts = build_line_starts(contents);

    for (start, _) in ranges {
        let (line, column, line_text) = offset_to_line_col_and_text(contents, &line_starts, start);
        let after = replace_line_preview(line_text, query, replacement, options);

        out.push(ReplacePreviewItem {
            path: DisplayablePathBuf::from_path(path.to_path_buf()),
            line,
            column,
            before: trim_preview(line_text),
            after: trim_preview(&after),
        });
        if out.len() >= max_results {
            break;
        }
    }

    out
}

fn replace_line_preview(
    line: &str,
    query: &str,
    replacement: &str,
    options: SearchOptions,
) -> String {
    match replace_all_in_text(line, query, replacement, options) {
        Ok((updated, _)) => updated,
        Err(_) => line.to_string(),
    }
}

fn replace_all_in_text(
    contents: &str,
    query: &str,
    replacement: &str,
    options: SearchOptions,
) -> apperr::Result<(String, usize)> {
    let ranges = collect_match_ranges_in_text(contents, query, options, usize::MAX);
    if ranges.is_empty() {
        return Ok((contents.to_string(), 0));
    }

    let buffer = TextBuffer::new_rc(false)?;
    let mut tb = buffer.borrow_mut();
    tb.copy_from_str(&contents.as_bytes());
    tb.find_and_replace_all(query, options, replacement.as_bytes())?;
    let updated = extract_buffer_text(&tb);
    Ok((updated, ranges.len()))
}

fn extract_buffer_text(tb: &TextBuffer) -> String {
    let mut out = Vec::with_capacity(tb.text_length());
    let mut offset = 0usize;
    while offset < tb.text_length() {
        let chunk = tb.read_forward(offset);
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(chunk);
        offset += chunk.len();
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn collect_match_ranges_in_text(
    contents: &str,
    query: &str,
    options: SearchOptions,
    max_results: usize,
) -> Vec<(usize, usize)> {
    if query.is_empty() || max_results == 0 {
        return Vec::new();
    }

    let buffer = match TextBuffer::new_rc(false) {
        Ok(buffer) => buffer,
        Err(_) => return Vec::new(),
    };
    let mut tb = buffer.borrow_mut();
    tb.copy_from_str(&contents.as_bytes());

    let mut first_hit: Option<(usize, usize)> = None;
    let mut hits = Vec::new();

    loop {
        if tb.find_and_select(query, options).is_err() {
            break;
        }

        let Some((beg, end)) = tb.selection_range() else {
            break;
        };

        let hit = (beg.offset, end.offset);
        if first_hit.is_none() {
            first_hit = Some(hit);
        } else if first_hit == Some(hit) {
            break;
        }

        hits.push(hit);
        if hits.len() >= max_results {
            break;
        }

        let next = if end.offset > beg.offset {
            end.offset
        } else {
            end.offset.saturating_add(1).min(tb.text_length())
        };
        if next >= tb.text_length() && end.offset >= tb.text_length() {
            break;
        }
        tb.clear_selection();
        tb.cursor_move_to_offset(next);
    }

    hits
}

fn build_line_starts(contents: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    starts.push(0);
    for (idx, b) in contents.as_bytes().iter().enumerate() {
        if *b == b'\n' && idx + 1 < contents.len() {
            starts.push(idx + 1);
        }
    }
    starts
}

fn offset_to_line_col_and_text<'a>(
    contents: &'a str,
    line_starts: &[usize],
    offset: usize,
) -> (usize, usize, &'a str) {
    let idx = line_starts.partition_point(|start| *start <= offset).saturating_sub(1);
    let line_start = line_starts[idx];
    let line_end = contents[line_start..]
        .find('\n')
        .map_or(contents.len(), |pos| line_start + pos);
    let line_text = contents[line_start..line_end].trim_end_matches('\r');
    let column = contents[line_start..offset].chars().count() + 1;
    (idx + 1, column, line_text)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        path.push(format!("edit32-find-{name}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    #[test]
    fn search_respects_recursion_depth_limit() {
        let root = temp_dir("depth");
        let mut current = root.clone();
        for idx in 0..(DEFAULT_MAX_RECURSION_DEPTH + 2) {
            current.push(format!("d{idx}"));
            fs::create_dir_all(&current).expect("failed to create nested dir");
        }
        fs::write(current.join("deep.txt"), b"needle").expect("failed to write deep file");

        let results = search(&root, "needle", SearchOptions::default());
        assert!(results.is_empty(), "deep file should not be reachable past recursion depth limit");

        let _ = fs::remove_dir_all(root);
    }
}
