// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use edit::buffer::SearchOptions;
use edit::{apperr, sys};

use crate::documents::DocumentManager;
use crate::state::{DisplayablePathBuf, ReplacePreviewItem};

const MAX_RESULTS: usize = 500;
const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;
const MAX_RECURSION_DEPTH: usize = 24;
const MAX_FILES_SCANNED: usize = 50_000;

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

pub fn search(root: &Path, query: &str) -> Vec<FindInFilesResult> {
    if query.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut files = Vec::new();
    collect_files(root, &mut files);

    for path in files {
        if results.len() >= MAX_RESULTS {
            break;
        }

        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() > MAX_FILE_SIZE {
            continue;
        }

        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };

        for (line_idx, line) in contents.lines().enumerate() {
            let mut start = 0;
            while let Some(pos) = line[start..].find(query) {
                let col = start + pos;
                results.push(FindInFilesResult {
                    path: DisplayablePathBuf::from_path(path.clone()),
                    line: line_idx + 1,
                    column: col + 1,
                    preview: trim_preview(line),
                });
                if results.len() >= MAX_RESULTS {
                    break;
                }
                start = col + query.len();
            }
            if results.len() >= MAX_RESULTS {
                break;
            }
        }
    }

    results
}

pub fn replace_all(
    root: &Path,
    query: &str,
    replacement: &str,
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
        if meta.len() > MAX_FILE_SIZE {
            continue;
        }

        if documents.is_open_and_dirty(&path) {
            stats.skipped_dirty += 1;
            continue;
        }

        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        if !contents.contains(query) {
            continue;
        }

        let updated = contents.replace(query, replacement);
        let replacements = count_occurrences(&contents, query);
        if updated == contents {
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
    if options.use_regex {
        return (Vec::new(), "Preview not available for regex search.".to_string());
    }

    let mut results = Vec::new();
    let mut files = Vec::new();
    collect_files(root, &mut files);

    for path in files {
        if results.len() >= MAX_RESULTS {
            break;
        }

        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() > MAX_FILE_SIZE {
            continue;
        }

        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };

        for (line_idx, line) in contents.lines().enumerate() {
            let matches = preview_find_matches(line, query, options);
            if matches.is_empty() {
                continue;
            }

            let after = replace_line_with_matches(line, &matches, replacement);
            let column = matches.first().map_or(1, |m| m.start + 1);

            results.push(ReplacePreviewItem {
                path: DisplayablePathBuf::from_path(path.clone()),
                line: line_idx + 1,
                column,
                before: trim_preview(line),
                after: trim_preview(&after),
            });

            if results.len() >= MAX_RESULTS {
                break;
            }
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
    if depth >= MAX_RECURSION_DEPTH || out.len() >= MAX_FILES_SCANNED {
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
        if out.len() >= MAX_FILES_SCANNED {
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

fn preview_find_matches(
    line: &str,
    needle: &str,
    options: SearchOptions,
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

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
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
        for idx in 0..(MAX_RECURSION_DEPTH + 2) {
            current.push(format!("d{idx}"));
            fs::create_dir_all(&current).expect("failed to create nested dir");
        }
        fs::write(current.join("deep.txt"), b"needle").expect("failed to write deep file");

        let results = search(&root, "needle");
        assert!(results.is_empty(), "deep file should not be reachable past recursion depth limit");

        let _ = fs::remove_dir_all(root);
    }
}
