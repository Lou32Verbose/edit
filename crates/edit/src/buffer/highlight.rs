// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::borrow::Cow;
use std::ops::Range;

use super::{HighlightKind, HighlightSpan, Language, SearchOptions, TextBuffer};
use crate::unicode::MeasurementConfig;

impl TextBuffer {
    pub(super) fn highlight_line(language: Language, line: &str) -> Vec<HighlightSpan> {
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
                let comment_start =
                    trimmed.find('#').or_else(|| trimmed.find(';')).map(|p| indent + p);

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
                    if matches!(
                        value_lower.as_str(),
                        "true" | "false" | "on" | "off" | "yes" | "no"
                    ) {
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
                            let kw_start =
                                value_start + value_part.find(value_trimmed).unwrap_or(0);
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
                        "fn", "let", "mut", "pub", "struct", "enum", "impl", "use", "mod", "trait",
                        "match", "if", "else", "for", "while", "loop", "return", "self", "Self",
                        "crate", "super", "const", "static", "ref", "as", "in", "where", "async",
                        "await",
                    ][..],
                    Language::Python => &[
                        "def", "class", "self", "return", "import", "from", "as", "if", "elif",
                        "else", "for", "while", "try", "except", "finally", "with", "lambda",
                        "yield", "True", "False", "None", "and", "or", "not", "in", "is",
                    ][..],
                    Language::JavaScript | Language::TypeScript => &[
                        "function",
                        "return",
                        "const",
                        "let",
                        "var",
                        "if",
                        "else",
                        "for",
                        "while",
                        "do",
                        "switch",
                        "case",
                        "break",
                        "continue",
                        "try",
                        "catch",
                        "finally",
                        "throw",
                        "class",
                        "extends",
                        "new",
                        "this",
                        "super",
                        "import",
                        "from",
                        "export",
                        "default",
                        "async",
                        "await",
                        "true",
                        "false",
                        "null",
                        "undefined",
                    ][..],
                    Language::Html => &[
                        "html", "head", "body", "div", "span", "p", "a", "ul", "ol", "li",
                        "header", "footer", "section", "nav", "main", "img", "input", "button",
                        "form", "label", "table", "tr", "td", "th", "thead", "tbody", "script",
                        "style",
                    ][..],
                    Language::Css => &[
                        "color",
                        "background",
                        "display",
                        "flex",
                        "grid",
                        "margin",
                        "padding",
                        "font",
                        "position",
                        "absolute",
                        "relative",
                        "fixed",
                        "border",
                        "width",
                        "height",
                        "gap",
                    ][..],
                    Language::Shell => {
                        &["if", "then", "fi", "for", "do", "done", "case", "esac", "function", "in"]
                            [..]
                    }
                    Language::C | Language::Cpp => &[
                        "int",
                        "char",
                        "void",
                        "struct",
                        "class",
                        "namespace",
                        "if",
                        "else",
                        "for",
                        "while",
                        "return",
                        "const",
                        "static",
                        "typedef",
                        "enum",
                    ][..],
                    Language::CSharp => &[
                        "class",
                        "struct",
                        "interface",
                        "using",
                        "namespace",
                        "public",
                        "private",
                        "protected",
                        "static",
                        "async",
                        "await",
                        "return",
                        "new",
                    ][..],
                    Language::Go => &[
                        "package",
                        "import",
                        "func",
                        "type",
                        "struct",
                        "interface",
                        "if",
                        "else",
                        "for",
                        "return",
                        "go",
                        "defer",
                    ][..],
                    Language::Java => &[
                        "class",
                        "interface",
                        "extends",
                        "implements",
                        "public",
                        "private",
                        "protected",
                        "static",
                        "final",
                        "return",
                        "new",
                        "import",
                    ][..],
                    Language::Kotlin => &[
                        "class",
                        "interface",
                        "fun",
                        "val",
                        "var",
                        "object",
                        "when",
                        "if",
                        "else",
                        "for",
                        "while",
                        "return",
                        "import",
                    ][..],
                    Language::Ruby => &[
                        "def", "class", "module", "end", "if", "elsif", "else", "true", "false",
                        "nil", "require", "return",
                    ][..],
                    Language::Php => &[
                        "function",
                        "class",
                        "public",
                        "private",
                        "protected",
                        "echo",
                        "return",
                        "true",
                        "false",
                        "null",
                        "new",
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
                        "function", "if", "else", "for", "while", "return", "TRUE", "FALSE", "NULL",
                    ][..],
                    Language::Swift => &[
                        "class", "struct", "enum", "protocol", "func", "let", "var", "if", "else",
                        "for", "while", "return", "import",
                    ][..],
                    Language::ObjectiveC => &[
                        "interface",
                        "implementation",
                        "end",
                        "class",
                        "void",
                        "int",
                        "return",
                        "if",
                        "else",
                        "for",
                        "while",
                        "nil",
                    ][..],
                    Language::Dart => &[
                        "class",
                        "enum",
                        "extends",
                        "implements",
                        "import",
                        "library",
                        "void",
                        "final",
                        "var",
                        "if",
                        "else",
                        "for",
                        "while",
                        "return",
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
                        "def",
                        "defmodule",
                        "do",
                        "end",
                        "if",
                        "else",
                        "case",
                        "when",
                        "true",
                        "false",
                        "nil",
                    ][..],
                    Language::Erlang => {
                        &["module", "export", "fun", "if", "case", "of", "end", "true", "false"][..]
                    }
                    Language::Clojure => {
                        &["def", "defn", "let", "if", "do", "fn", "true", "false", "nil"][..]
                    }
                    Language::FSharp => &[
                        "let", "module", "type", "open", "if", "then", "else", "match", "with",
                        "fun", "true", "false",
                    ][..],
                    Language::VbNet => &[
                        "Class", "Module", "Sub", "Function", "End", "If", "Then", "Else", "Dim",
                        "As", "Return",
                    ][..],
                    Language::Perl => {
                        &["my", "our", "sub", "use", "if", "else", "elsif", "return", "undef"][..]
                    }
                    Language::Groovy => {
                        &["class", "def", "import", "if", "else", "for", "while", "return", "new"][..]
                    }
                    Language::Terraform => &[
                        "resource", "variable", "output", "module", "provider", "data", "true",
                        "false",
                    ][..],
                    Language::Nix => {
                        &["let", "in", "with", "rec", "if", "then", "else", "true", "false", "null"]
                            [..]
                    }
                    Language::Assembly => &["mov", "add", "sub", "jmp", "call", "ret"][..],
                    Language::Latex => &[
                        "documentclass",
                        "begin",
                        "end",
                        "usepackage",
                        "section",
                        "subsection",
                        "title",
                        "author",
                    ][..],
                    Language::Graphql => &[
                        "query",
                        "mutation",
                        "subscription",
                        "fragment",
                        "on",
                        "true",
                        "false",
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
                    spans.push(HighlightSpan { start, end: i, kind: HighlightKind::Number });
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

pub(super) fn find_search_matches(
    line: &str,
    needle: &str,
    options: SearchOptions,
) -> Vec<Range<usize>> {
    if line.is_empty() || needle.is_empty() {
        return Vec::new();
    }

    let (haystack, needle_cmp): (Cow<'_, str>, Cow<'_, str>) = if options.match_case {
        (Cow::Borrowed(line), Cow::Borrowed(needle))
    } else {
        let line_lower =
            String::from_utf8(line.as_bytes().iter().map(|b| b.to_ascii_lowercase()).collect())
                .unwrap();
        let needle_lower =
            String::from_utf8(needle.as_bytes().iter().map(|b| b.to_ascii_lowercase()).collect())
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

pub(super) fn count_columns(text: &str, byte_off: usize) -> usize {
    let bytes = text.as_bytes();
    let mut cfg = MeasurementConfig::new(&bytes);
    let cursor = cfg.goto_offset(byte_off.min(text.len()));
    cursor.visual_pos.x as usize
}
