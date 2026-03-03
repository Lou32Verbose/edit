// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

impl TextBuffer {
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

    /// Applies a byte transform to the current selection and replaces it
    /// only when the transform yields a value.
    fn transform_selection_bytes<F>(&mut self, transform: F)
    where
        F: FnOnce(&[u8]) -> Option<Vec<u8>>,
    {
        let Some((beg, end)) = self.selection_offsets() else {
            return;
        };

        let mut content = Vec::new();
        self.buffer.extract_raw(beg..end, &mut content, 0);

        if let Some(output) = transform(&content) {
            self.replace_selection_with(&output);
        }
    }

    /// Encodes the selected text as Base64.
    pub fn encode_base64(&mut self) {
        self.transform_selection_bytes(|content| Some(base64_encode(content).into_bytes()));
    }

    /// Decodes the selected text from Base64.
    pub fn decode_base64(&mut self) {
        self.transform_selection_bytes(base64_decode);
    }

    /// URL-encodes the selected text.
    pub fn encode_url(&mut self) {
        self.transform_selection_bytes(|content| Some(url_encode(content)));
    }

    /// URL-decodes the selected text.
    pub fn decode_url(&mut self) {
        self.transform_selection_bytes(url_decode);
    }

    /// Encodes the selected text as hexadecimal.
    pub fn encode_hex(&mut self) {
        self.transform_selection_bytes(|content| Some(hex_encode(content).into_bytes()));
    }

    /// Decodes the selected text from hexadecimal.
    pub fn decode_hex(&mut self) {
        self.transform_selection_bytes(hex_decode);
    }

    /// Converts the selected text to uppercase.
    pub fn convert_to_uppercase(&mut self) {
        self.transform_selection_bytes(|content| {
            std::str::from_utf8(content).ok().map(|text| text.to_uppercase().into_bytes())
        });
    }

    /// Converts the selected text to lowercase.
    pub fn convert_to_lowercase(&mut self) {
        self.transform_selection_bytes(|content| {
            std::str::from_utf8(content).ok().map(|text| text.to_lowercase().into_bytes())
        });
    }

    /// Converts the selected text to title case.
    pub fn convert_to_title_case(&mut self) {
        self.transform_selection_bytes(|content| {
            std::str::from_utf8(content).ok().map(|text| to_title_case(text).into_bytes())
        });
    }
}

const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

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
