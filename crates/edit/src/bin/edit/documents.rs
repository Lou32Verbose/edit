// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::collections::LinkedList;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use edit::buffer::{Language, RcTextBuffer, TextBuffer};
use crate::config::EditorSettings;
use edit::helpers::{CoordType, Point};
use edit::{apperr, path, sys};

use crate::state::DisplayablePathBuf;

pub struct Document {
    pub buffer: RcTextBuffer,
    pub path: Option<PathBuf>,
    pub dir: Option<DisplayablePathBuf>,
    pub filename: String,
    pub file_id: Option<sys::FileId>,
    pub new_file_counter: usize,
    pub mode: DocumentMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DocumentMode {
    Text,
    LargeText,
    Hex,
}

impl Document {
    pub fn save(&mut self, new_path: Option<PathBuf>) -> apperr::Result<()> {
        let path = new_path.as_deref().unwrap_or_else(|| self.path.as_ref().unwrap().as_path());

        let buffer = self.buffer.clone();
        sys::atomic_write(path, |file| buffer.borrow_mut().write_file_contents(file))?;
        self.buffer.borrow_mut().mark_as_clean();

        if let Ok(id) = sys::file_id(None, path) {
            self.file_id = Some(id);
        }

        if let Some(path) = new_path {
            self.set_path(path);
        }

        Ok(())
    }

    pub fn reread(&mut self, encoding: Option<&'static str>) -> apperr::Result<()> {
        let path = self.path.as_ref().unwrap().as_path();
        let mut file = DocumentManager::open_for_reading(path)?;

        {
            let mut tb = self.buffer.borrow_mut();
            tb.read_file(&mut file, encoding)?;
        }

        if let Ok(id) = sys::file_id(None, path) {
            self.file_id = Some(id);
        }

        Ok(())
    }

    fn set_path(&mut self, path: PathBuf) {
        let filename = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let dir = path.parent().map(ToOwned::to_owned).unwrap_or_default();
        self.filename = filename;
        self.dir = Some(DisplayablePathBuf::from_path(dir));
        self.path = Some(path);
        self.update_file_mode();
        self.update_language();
    }

    fn update_file_mode(&mut self) {
        let mut tb = self.buffer.borrow_mut();
        tb.set_ruler(if self.filename == "COMMIT_EDITMSG" { 72 } else { 0 });
    }

    fn update_language(&mut self) {
        let language = if self.mode != DocumentMode::Text {
            Language::PlainText
        } else {
            self.path
                .as_ref()
                .map(|path| language_for_path(path))
                .unwrap_or(Language::PlainText)
        };
        self.buffer.borrow_mut().set_language(language);
    }

    fn apply_mode_settings(&mut self, settings: &EditorSettings) {
        let mut tb = self.buffer.borrow_mut();
        match self.mode {
            DocumentMode::Text => {
                tb.set_word_wrap(settings.word_wrap);
                tb.set_line_highlight_enabled(true);
                tb.set_margin_enabled(true);
            }
            DocumentMode::LargeText => {
                tb.set_word_wrap(false);
                tb.set_line_highlight_enabled(false);
                tb.set_margin_enabled(false);
            }
            DocumentMode::Hex => {
                tb.set_word_wrap(false);
                tb.set_line_highlight_enabled(false);
                tb.set_margin_enabled(false);
            }
        }
    }
}

fn language_for_path(path: &Path) -> Language {
    if let Some(name) = path.file_name().and_then(OsStr::to_str) {
        if name == "Makefile" {
            return Language::Makefile;
        }
    }
    let ext = path.extension().and_then(OsStr::to_str).unwrap_or_default();
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Language::Rust,
        "toml" => Language::Toml,
        "json" => Language::Json,
        "md" | "markdown" => Language::Markdown,
        "py" => Language::Python,
        "js" | "mjs" | "cjs" => Language::JavaScript,
        "ts" | "tsx" | "jsx" => Language::TypeScript,
        "html" | "htm" => Language::Html,
        "css" => Language::Css,
        "yml" | "yaml" => Language::Yaml,
        "sh" | "bash" | "zsh" => Language::Shell,
        "c" | "h" => Language::C,
        "cpp" | "hpp" | "cc" | "cxx" | "hh" => Language::Cpp,
        "cs" => Language::CSharp,
        "go" => Language::Go,
        "java" => Language::Java,
        "kt" => Language::Kotlin,
        "rb" => Language::Ruby,
        "php" => Language::Php,
        "sql" => Language::Sql,
        "xml" => Language::Xml,
        "ini" => Language::Ini,
        "lua" => Language::Lua,
        "ps1" => Language::PowerShell,
        "r" => Language::R,
        "swift" => Language::Swift,
        "m" | "mm" => Language::ObjectiveC,
        "dart" => Language::Dart,
        "scala" => Language::Scala,
        "hs" => Language::Haskell,
        "ex" | "exs" => Language::Elixir,
        "erl" | "hrl" => Language::Erlang,
        "clj" | "cljs" | "cljc" => Language::Clojure,
        "fs" | "fsi" | "fsx" => Language::FSharp,
        "vb" => Language::VbNet,
        "pl" | "pm" | "t" => Language::Perl,
        "groovy" | "gvy" | "gradle" => Language::Groovy,
        "tf" | "tfvars" => Language::Terraform,
        "nix" => Language::Nix,
        "asm" | "s" => Language::Assembly,
        "tex" | "sty" | "cls" => Language::Latex,
        "mdx" => Language::Mdx,
        "graphql" | "gql" => Language::Graphql,
        "csv" => Language::Csv,
        _ => Language::PlainText,
    }
}

#[derive(Default)]
pub struct DocumentManager {
    list: LinkedList<Document>,
}

pub enum OpenOutcome {
    Opened,
    BinaryDetected { path: PathBuf, goto: Option<Point> },
}

impl DocumentManager {
    #[inline]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    #[inline]
    pub fn active(&self) -> Option<&Document> {
        self.list.front()
    }

    #[inline]
    pub fn active_mut(&mut self) -> Option<&mut Document> {
        self.list.front_mut()
    }

    #[inline]
    pub fn iter(&self) -> std::collections::linked_list::Iter<'_, Document> {
        self.list.iter()
    }

    #[inline]
    pub fn update_active<F: FnMut(&Document) -> bool>(&mut self, mut func: F) -> bool {
        let mut cursor = self.list.cursor_front_mut();
        while let Some(doc) = cursor.current() {
            if func(doc) {
                let list = cursor.remove_current_as_list().unwrap();
                self.list.cursor_front_mut().splice_before(list);
                return true;
            }
            cursor.move_next();
        }
        false
    }

    pub fn is_open_and_dirty(&self, path: &Path) -> bool {
        let path = path::normalize(path);
        self.list.iter().any(|doc| {
            doc.path.as_deref() == Some(path.as_path()) && doc.buffer.borrow().is_dirty()
        })
    }

    pub fn reload_if_open_and_clean(&mut self, path: &Path) -> apperr::Result<()> {
        let path = path::normalize(path);
        for doc in &mut self.list {
            if doc.path.as_deref() == Some(path.as_path()) {
                if doc.buffer.borrow().is_dirty() {
                    return Ok(());
                }
                doc.reread(None)?;
                doc.buffer.borrow_mut().mark_as_clean();
            }
        }
        Ok(())
    }

    pub fn remove_active(&mut self) {
        self.list.pop_front();
    }

    pub fn add_untitled(&mut self, settings: &EditorSettings) -> apperr::Result<&mut Document> {
        let buffer = Self::create_buffer(settings)?;
        let mut doc = Document {
            buffer,
            path: None,
            dir: Default::default(),
            filename: Default::default(),
            file_id: None,
            new_file_counter: 0,
            mode: DocumentMode::Text,
        };
        self.gen_untitled_name(&mut doc);
        doc.apply_mode_settings(settings);

        self.list.push_front(doc);
        Ok(self.list.front_mut().unwrap())
    }

    pub fn gen_untitled_name(&self, doc: &mut Document) {
        let mut new_file_counter = 0;
        for doc in &self.list {
            new_file_counter = new_file_counter.max(doc.new_file_counter);
        }
        new_file_counter += 1;

        doc.filename = format!("Untitled-{new_file_counter}.txt");
        doc.new_file_counter = new_file_counter;
    }

    pub fn add_file_path(
        &mut self,
        path: &Path,
        settings: &EditorSettings,
    ) -> apperr::Result<OpenOutcome> {
        let (path, goto) = Self::parse_filename_goto(path);
        let path = path::normalize(path);

        let mut file = match Self::open_for_reading(&path) {
            Ok(file) => Some(file),
            Err(err) if sys::apperr_is_not_found(err) => None,
            Err(err) => return Err(err),
        };

        let file_id = if file.is_some() { Some(sys::file_id(file.as_ref(), &path)?) } else { None };
        let mut is_large = false;
        let mut is_binary = false;

        // Check if the file is already open.
        if file_id.is_some() && self.update_active(|doc| doc.file_id == file_id) {
            let doc = self.active_mut().unwrap();
            if let Some(goto) = goto {
                doc.buffer.borrow_mut().cursor_move_to_logical(goto);
            }
            return Ok(OpenOutcome::Opened);
        }

        if let Some(file) = &mut file {
            if let Ok(meta) = file.metadata() {
                is_large = meta.len() > settings.large_file_threshold_bytes;
            }

            let mut probe = [0u8; 4096];
            let read = file.read(&mut probe)?;
            is_binary = probe[..read].iter().any(|&b| b == 0);
            file.seek(SeekFrom::Start(0))?;
        }

        if is_binary {
            return Ok(OpenOutcome::BinaryDetected { path: path.to_path_buf(), goto });
        }

        let buffer = Self::create_buffer(settings)?;
        {
            if let Some(file) = &mut file {
                let mut tb = buffer.borrow_mut();
                tb.read_file(file, None)?;

                if let Some(goto) = goto
                    && goto != Default::default()
                {
                    tb.cursor_move_to_logical(goto);
                }
            }
        }

        let mut doc = Document {
            buffer,
            path: None,
            dir: None,
            filename: Default::default(),
            file_id,
            new_file_counter: 0,
            mode: if is_large { DocumentMode::LargeText } else { DocumentMode::Text },
        };
        doc.set_path(path);
        doc.apply_mode_settings(settings);

        if let Some(active) = self.active()
            && active.path.is_none()
            && active.file_id.is_none()
            && !active.buffer.borrow().is_dirty()
        {
            // If the current document is a pristine Untitled document with no
            // name and no ID, replace it with the new document.
            self.remove_active();
        }

        self.list.push_front(doc);
        Ok(OpenOutcome::Opened)
    }

    pub fn reflow_all(&self) {
        for doc in &self.list {
            let mut tb = doc.buffer.borrow_mut();
            tb.reflow();
        }
    }

    pub fn open_for_reading(path: &Path) -> apperr::Result<File> {
        File::open(path).map_err(apperr::Error::from)
    }

    pub fn add_file_path_hex(
        &mut self,
        path: &Path,
        settings: &EditorSettings,
    ) -> apperr::Result<()> {
        let path = path::normalize(path);
        let mut file = Self::open_for_reading(&path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;

        let buffer = Self::create_buffer(settings)?;
        {
            let mut tb = buffer.borrow_mut();
            let hex = format_hex_view(&bytes);
            tb.copy_from_str(&hex);
            tb.mark_as_clean();
        }

        let file_id = Some(sys::file_id(Some(&file), &path)?);
        let mut doc = Document {
            buffer,
            path: None,
            dir: None,
            filename: Default::default(),
            file_id,
            new_file_counter: 0,
            mode: DocumentMode::Hex,
        };
        doc.set_path(path);
        doc.apply_mode_settings(settings);

        if let Some(active) = self.active()
            && active.path.is_none()
            && active.file_id.is_none()
            && !active.buffer.borrow().is_dirty()
        {
            self.remove_active();
        }

        self.list.push_front(doc);
        Ok(())
    }

    fn create_buffer(settings: &EditorSettings) -> apperr::Result<RcTextBuffer> {
        let buffer = TextBuffer::new_rc(false)?;
        {
            let mut tb = buffer.borrow_mut();
            tb.set_insert_final_newline(!cfg!(windows)); // As mandated by POSIX.
            tb.set_margin_enabled(true);
            tb.set_line_highlight_enabled(true);
            tb.set_word_wrap(settings.word_wrap);
            tb.set_tab_size(settings.tab_size);
            tb.set_indent_with_tabs(settings.indent_with_tabs);
        }
        Ok(buffer)
    }

    // Parse a filename in the form of "filename:line:char".
    // Returns the position of the first colon and the line/char coordinates.
    fn parse_filename_goto(path: &Path) -> (&Path, Option<Point>) {
        fn parse(s: &[u8]) -> Option<CoordType> {
            if s.is_empty() {
                return None;
            }

            let mut num: CoordType = 0;
            for &b in s {
                if !b.is_ascii_digit() {
                    return None;
                }
                let digit = (b - b'0') as CoordType;
                num = num.checked_mul(10)?.checked_add(digit)?;
            }
            Some(num)
        }

        fn find_colon_rev(bytes: &[u8], offset: usize) -> Option<usize> {
            (0..offset.min(bytes.len())).rev().find(|&i| bytes[i] == b':')
        }

        let bytes = path.as_os_str().as_encoded_bytes();
        let colend = match find_colon_rev(bytes, bytes.len()) {
            // Reject filenames that would result in an empty filename after stripping off the :line:char suffix.
            // For instance, a filename like ":123:456" will not be processed by this function.
            Some(colend) if colend > 0 => colend,
            _ => return (path, None),
        };

        let last = match parse(&bytes[colend + 1..]) {
            Some(last) => last,
            None => return (path, None),
        };
        let last = (last - 1).max(0);
        let mut len = colend;
        let mut goto = Point { x: 0, y: last };

        if let Some(colbeg) = find_colon_rev(bytes, colend) {
            // Same here: Don't allow empty filenames.
            if colbeg != 0
                && let Some(first) = parse(&bytes[colbeg + 1..colend])
            {
                let first = (first - 1).max(0);
                len = colbeg;
                goto = Point { x: last, y: first };
            }
        }

        // Strip off the :line:char suffix.
        let path = &bytes[..len];
        let path = unsafe { OsStr::from_encoded_bytes_unchecked(path) };
        let path = Path::new(path);
        (path, Some(goto))
    }
}

fn format_hex_view(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut offset = 0usize;
    for chunk in bytes.chunks(16) {
        out.push_str(&format!("{offset:08x}  "));
        for i in 0..16 {
            if let Some(b) = chunk.get(i) {
                out.push_str(&format!("{b:02x} "));
            } else {
                out.push_str("   ");
            }
            if i == 7 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        for &b in chunk {
            let ch = if (0x20..=0x7e).contains(&b) { b as char } else { '.' };
            out.push(ch);
        }
        out.push('|');
        out.push('\n');
        offset += chunk.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_last_numbers() {
        fn parse(s: &str) -> (&str, Option<Point>) {
            let (p, g) = DocumentManager::parse_filename_goto(Path::new(s));
            (p.to_str().unwrap(), g)
        }

        assert_eq!(parse("123"), ("123", None));
        assert_eq!(parse("abc"), ("abc", None));
        assert_eq!(parse(":123"), (":123", None));
        assert_eq!(parse("abc:123"), ("abc", Some(Point { x: 0, y: 122 })));
        assert_eq!(parse("45:123"), ("45", Some(Point { x: 0, y: 122 })));
        assert_eq!(parse(":45:123"), (":45", Some(Point { x: 0, y: 122 })));
        assert_eq!(parse("abc:45:123"), ("abc", Some(Point { x: 122, y: 44 })));
        assert_eq!(parse("abc:def:123"), ("abc:def", Some(Point { x: 0, y: 122 })));
        assert_eq!(parse("1:2:3"), ("1", Some(Point { x: 2, y: 1 })));
        assert_eq!(parse("::3"), (":", Some(Point { x: 0, y: 2 })));
        assert_eq!(parse("1::3"), ("1:", Some(Point { x: 0, y: 2 })));
        assert_eq!(parse(""), ("", None));
        assert_eq!(parse(":"), (":", None));
        assert_eq!(parse("::"), ("::", None));
        assert_eq!(parse("a:1"), ("a", Some(Point { x: 0, y: 0 })));
        assert_eq!(parse("1:a"), ("1:a", None));
        assert_eq!(parse("file.txt:10"), ("file.txt", Some(Point { x: 0, y: 9 })));
        assert_eq!(parse("file.txt:10:5"), ("file.txt", Some(Point { x: 4, y: 9 })));
    }
}
