// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parses VT sequences into input events.
//!
//! In the future this allows us to take apart the application and
//! support input schemes that aren't VT, such as UEFI, or GUI.

use std::mem;

use crate::helpers::{CoordType, Point, Size};
use crate::vt;

/// Represents a key/modifier combination.
///
/// The value is bit-packed for ABI stability and compactness:
/// - low 24 bits: key code
/// - high 8 bits: modifiers
///
/// Prefer [`InputKey::from_parts`] and [`InputKey::key_code`] for typed access.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct InputKey(u32);

impl InputKey {
    pub(crate) const fn new(v: u32) -> Self {
        Self(v)
    }

    pub(crate) const fn from_ascii(ch: char) -> Option<Self> {
        if ch == ' ' || ch == '-' || ch == '/' || (ch >= '0' && ch <= '9') {
            Some(Self(ch as u32))
        } else if ch >= 'a' && ch <= 'z' {
            Some(Self(ch as u32 & !0x20)) // Shift a-z to A-Z
        } else if ch >= 'A' && ch <= 'Z' {
            Some(Self(kbmod::SHIFT.0 | ch as u32))
        } else {
            None
        }
    }

    pub(crate) const fn value(&self) -> u32 {
        self.0
    }

    pub const fn from_parts(key: KeyCode, modifiers: InputKeyMod) -> Self {
        Self((key.value() & 0x00FFFFFF) | modifiers.0)
    }

    pub const fn key_code(&self) -> KeyCode {
        KeyCode::new(self.0 & 0x00FFFFFF)
    }

    pub const fn key(&self) -> Self {
        Self(self.key_code().value())
    }

    pub const fn modifiers(&self) -> InputKeyMod {
        InputKeyMod(self.0 & 0xFF000000)
    }

    pub const fn modifiers_contains(&self, modifier: InputKeyMod) -> bool {
        (self.0 & modifier.0) != 0
    }

    pub const fn with_modifiers(&self, modifiers: InputKeyMod) -> Self {
        Self::from_parts(self.key_code(), modifiers)
    }
}

/// A keyboard key code without modifiers.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KeyCode(u32);

impl KeyCode {
    pub(crate) const fn new(v: u32) -> Self {
        Self(v)
    }

    pub const fn value(self) -> u32 {
        self.0
    }
}

/// A keyboard modifier. Ctrl/Alt/Shift.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct InputKeyMod(u32);

impl InputKeyMod {
    const fn new(v: u32) -> Self {
        Self(v)
    }

    pub const fn contains(&self, modifier: Self) -> bool {
        (self.0 & modifier.0) != 0
    }
}

impl std::ops::BitOr<InputKeyMod> for InputKey {
    type Output = Self;

    fn bitor(self, rhs: InputKeyMod) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOr<InputKeyMod> for KeyCode {
    type Output = InputKey;

    fn bitor(self, rhs: InputKeyMod) -> InputKey {
        InputKey::from_parts(self, rhs)
    }
}

impl std::ops::BitOr<InputKey> for InputKeyMod {
    type Output = InputKey;

    fn bitor(self, rhs: InputKey) -> InputKey {
        InputKey(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for InputKeyMod {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Keyboard keys.
///
/// The codes defined here match the VK_* constants on Windows.
/// It's a convenient way to handle keyboard input, even on other platforms.
pub mod vk {
    use super::InputKey;

    pub const NULL: InputKey = InputKey::new('\0' as u32);
    pub const BACK: InputKey = InputKey::new(0x08);
    pub const TAB: InputKey = InputKey::new('\t' as u32);
    pub const RETURN: InputKey = InputKey::new('\r' as u32);
    pub const ESCAPE: InputKey = InputKey::new(0x1B);
    pub const SPACE: InputKey = InputKey::new(' ' as u32);
    pub const MINUS: InputKey = InputKey::new('-' as u32);
    pub const SLASH: InputKey = InputKey::new('/' as u32);
    pub const PRIOR: InputKey = InputKey::new(0x21);
    pub const NEXT: InputKey = InputKey::new(0x22);

    pub const END: InputKey = InputKey::new(0x23);
    pub const HOME: InputKey = InputKey::new(0x24);

    pub const LEFT: InputKey = InputKey::new(0x25);
    pub const UP: InputKey = InputKey::new(0x26);
    pub const RIGHT: InputKey = InputKey::new(0x27);
    pub const DOWN: InputKey = InputKey::new(0x28);

    pub const INSERT: InputKey = InputKey::new(0x2D);
    pub const DELETE: InputKey = InputKey::new(0x2E);

    pub const N0: InputKey = InputKey::new('0' as u32);
    pub const N1: InputKey = InputKey::new('1' as u32);
    pub const N2: InputKey = InputKey::new('2' as u32);
    pub const N3: InputKey = InputKey::new('3' as u32);
    pub const N4: InputKey = InputKey::new('4' as u32);
    pub const N5: InputKey = InputKey::new('5' as u32);
    pub const N6: InputKey = InputKey::new('6' as u32);
    pub const N7: InputKey = InputKey::new('7' as u32);
    pub const N8: InputKey = InputKey::new('8' as u32);
    pub const N9: InputKey = InputKey::new('9' as u32);

    pub const A: InputKey = InputKey::new('A' as u32);
    pub const B: InputKey = InputKey::new('B' as u32);
    pub const C: InputKey = InputKey::new('C' as u32);
    pub const D: InputKey = InputKey::new('D' as u32);
    pub const E: InputKey = InputKey::new('E' as u32);
    pub const F: InputKey = InputKey::new('F' as u32);
    pub const G: InputKey = InputKey::new('G' as u32);
    pub const H: InputKey = InputKey::new('H' as u32);
    pub const I: InputKey = InputKey::new('I' as u32);
    pub const J: InputKey = InputKey::new('J' as u32);
    pub const K: InputKey = InputKey::new('K' as u32);
    pub const L: InputKey = InputKey::new('L' as u32);
    pub const M: InputKey = InputKey::new('M' as u32);
    pub const N: InputKey = InputKey::new('N' as u32);
    pub const O: InputKey = InputKey::new('O' as u32);
    pub const P: InputKey = InputKey::new('P' as u32);
    pub const Q: InputKey = InputKey::new('Q' as u32);
    pub const R: InputKey = InputKey::new('R' as u32);
    pub const S: InputKey = InputKey::new('S' as u32);
    pub const T: InputKey = InputKey::new('T' as u32);
    pub const U: InputKey = InputKey::new('U' as u32);
    pub const V: InputKey = InputKey::new('V' as u32);
    pub const W: InputKey = InputKey::new('W' as u32);
    pub const X: InputKey = InputKey::new('X' as u32);
    pub const Y: InputKey = InputKey::new('Y' as u32);
    pub const Z: InputKey = InputKey::new('Z' as u32);

    pub const NUMPAD0: InputKey = InputKey::new(0x60);
    pub const NUMPAD1: InputKey = InputKey::new(0x61);
    pub const NUMPAD2: InputKey = InputKey::new(0x62);
    pub const NUMPAD3: InputKey = InputKey::new(0x63);
    pub const NUMPAD4: InputKey = InputKey::new(0x64);
    pub const NUMPAD5: InputKey = InputKey::new(0x65);
    pub const NUMPAD6: InputKey = InputKey::new(0x66);
    pub const NUMPAD7: InputKey = InputKey::new(0x67);
    pub const NUMPAD8: InputKey = InputKey::new(0x68);
    pub const NUMPAD9: InputKey = InputKey::new(0x69);
    pub const MULTIPLY: InputKey = InputKey::new(0x6A);
    pub const ADD: InputKey = InputKey::new(0x6B);
    pub const SEPARATOR: InputKey = InputKey::new(0x6C);
    pub const SUBTRACT: InputKey = InputKey::new(0x6D);
    pub const DECIMAL: InputKey = InputKey::new(0x6E);
    pub const DIVIDE: InputKey = InputKey::new(0x6F);

    pub const F1: InputKey = InputKey::new(0x70);
    pub const F2: InputKey = InputKey::new(0x71);
    pub const F3: InputKey = InputKey::new(0x72);
    pub const F4: InputKey = InputKey::new(0x73);
    pub const F5: InputKey = InputKey::new(0x74);
    pub const F6: InputKey = InputKey::new(0x75);
    pub const F7: InputKey = InputKey::new(0x76);
    pub const F8: InputKey = InputKey::new(0x77);
    pub const F9: InputKey = InputKey::new(0x78);
    pub const F10: InputKey = InputKey::new(0x79);
    pub const F11: InputKey = InputKey::new(0x7A);
    pub const F12: InputKey = InputKey::new(0x7B);
    pub const F13: InputKey = InputKey::new(0x7C);
    pub const F14: InputKey = InputKey::new(0x7D);
    pub const F15: InputKey = InputKey::new(0x7E);
    pub const F16: InputKey = InputKey::new(0x7F);
    pub const F17: InputKey = InputKey::new(0x80);
    pub const F18: InputKey = InputKey::new(0x81);
    pub const F19: InputKey = InputKey::new(0x82);
    pub const F20: InputKey = InputKey::new(0x83);
    pub const F21: InputKey = InputKey::new(0x84);
    pub const F22: InputKey = InputKey::new(0x85);
    pub const F23: InputKey = InputKey::new(0x86);
    pub const F24: InputKey = InputKey::new(0x87);
}

/// Typed key constants. Equivalent key set as [`vk`] but without modifiers.
pub mod key {
    use super::KeyCode;

    pub const NULL: KeyCode = KeyCode::new('\0' as u32);
    pub const BACK: KeyCode = KeyCode::new(0x08);
    pub const TAB: KeyCode = KeyCode::new('\t' as u32);
    pub const RETURN: KeyCode = KeyCode::new('\r' as u32);
    pub const ESCAPE: KeyCode = KeyCode::new(0x1B);
    pub const SPACE: KeyCode = KeyCode::new(' ' as u32);
    pub const MINUS: KeyCode = KeyCode::new('-' as u32);
    pub const SLASH: KeyCode = KeyCode::new('/' as u32);
    pub const PRIOR: KeyCode = KeyCode::new(0x21);
    pub const NEXT: KeyCode = KeyCode::new(0x22);

    pub const END: KeyCode = KeyCode::new(0x23);
    pub const HOME: KeyCode = KeyCode::new(0x24);

    pub const LEFT: KeyCode = KeyCode::new(0x25);
    pub const UP: KeyCode = KeyCode::new(0x26);
    pub const RIGHT: KeyCode = KeyCode::new(0x27);
    pub const DOWN: KeyCode = KeyCode::new(0x28);

    pub const INSERT: KeyCode = KeyCode::new(0x2D);
    pub const DELETE: KeyCode = KeyCode::new(0x2E);

    pub const N0: KeyCode = KeyCode::new('0' as u32);
    pub const N1: KeyCode = KeyCode::new('1' as u32);
    pub const N2: KeyCode = KeyCode::new('2' as u32);
    pub const N3: KeyCode = KeyCode::new('3' as u32);
    pub const N4: KeyCode = KeyCode::new('4' as u32);
    pub const N5: KeyCode = KeyCode::new('5' as u32);
    pub const N6: KeyCode = KeyCode::new('6' as u32);
    pub const N7: KeyCode = KeyCode::new('7' as u32);
    pub const N8: KeyCode = KeyCode::new('8' as u32);
    pub const N9: KeyCode = KeyCode::new('9' as u32);

    pub const A: KeyCode = KeyCode::new('A' as u32);
    pub const B: KeyCode = KeyCode::new('B' as u32);
    pub const C: KeyCode = KeyCode::new('C' as u32);
    pub const D: KeyCode = KeyCode::new('D' as u32);
    pub const E: KeyCode = KeyCode::new('E' as u32);
    pub const F: KeyCode = KeyCode::new('F' as u32);
    pub const G: KeyCode = KeyCode::new('G' as u32);
    pub const H: KeyCode = KeyCode::new('H' as u32);
    pub const I: KeyCode = KeyCode::new('I' as u32);
    pub const J: KeyCode = KeyCode::new('J' as u32);
    pub const K: KeyCode = KeyCode::new('K' as u32);
    pub const L: KeyCode = KeyCode::new('L' as u32);
    pub const M: KeyCode = KeyCode::new('M' as u32);
    pub const N: KeyCode = KeyCode::new('N' as u32);
    pub const O: KeyCode = KeyCode::new('O' as u32);
    pub const P: KeyCode = KeyCode::new('P' as u32);
    pub const Q: KeyCode = KeyCode::new('Q' as u32);
    pub const R: KeyCode = KeyCode::new('R' as u32);
    pub const S: KeyCode = KeyCode::new('S' as u32);
    pub const T: KeyCode = KeyCode::new('T' as u32);
    pub const U: KeyCode = KeyCode::new('U' as u32);
    pub const V: KeyCode = KeyCode::new('V' as u32);
    pub const W: KeyCode = KeyCode::new('W' as u32);
    pub const X: KeyCode = KeyCode::new('X' as u32);
    pub const Y: KeyCode = KeyCode::new('Y' as u32);
    pub const Z: KeyCode = KeyCode::new('Z' as u32);
}

/// Keyboard modifiers.
pub mod kbmod {
    use super::InputKeyMod;

    pub const NONE: InputKeyMod = InputKeyMod::new(0x00000000);
    pub const CTRL: InputKeyMod = InputKeyMod::new(0x01000000);
    pub const ALT: InputKeyMod = InputKeyMod::new(0x02000000);
    pub const SHIFT: InputKeyMod = InputKeyMod::new(0x04000000);

    pub const CTRL_ALT: InputKeyMod = InputKeyMod::new(0x03000000);
    pub const CTRL_SHIFT: InputKeyMod = InputKeyMod::new(0x05000000);
    pub const ALT_SHIFT: InputKeyMod = InputKeyMod::new(0x06000000);
    pub const CTRL_ALT_SHIFT: InputKeyMod = InputKeyMod::new(0x07000000);
}

/// Mouse input state. Up/Down, Left/Right, etc.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum InputMouseState {
    #[default]
    None,

    // These 3 carry their state between frames.
    Left,
    Middle,
    Right,

    // These 2 get reset to None on the next frame.
    Release,
    Scroll,
}

/// Mouse input.
#[derive(Clone, Copy)]
pub struct InputMouse {
    /// The state of the mouse.Up/Down, Left/Right, etc.
    pub state: InputMouseState,
    /// Any keyboard modifiers that are held down.
    pub modifiers: InputKeyMod,
    /// Position of the mouse in the viewport.
    pub position: Point,
    /// Scroll delta.
    pub scroll: Point,
}

/// Primary result type of the parser.
pub enum Input<'input> {
    /// Window resize event.
    Resize(Size),
    /// Text input.
    /// Note that [`Input::Keyboard`] events can also be text.
    Text(&'input str),
    /// A clipboard paste.
    Paste(Vec<u8>),
    /// Keyboard input.
    Keyboard(InputKey),
    /// Mouse input.
    Mouse(InputMouse),
}

/// Parses VT sequences into input events.
pub struct Parser {
    bracketed_paste: bool,
    bracketed_paste_buf: Vec<u8>,
    x10_mouse_want: bool,
    x10_mouse_buf: [char; 3],
    x10_mouse_len: usize,
}

impl Parser {
    /// Creates a new parser that turns VT sequences into input events.
    ///
    /// Keep the instance alive for the lifetime of the input stream.
    pub fn new() -> Self {
        Self {
            bracketed_paste: false,
            bracketed_paste_buf: Vec::new(),
            x10_mouse_want: false,
            x10_mouse_buf: ['\0'; 3],
            x10_mouse_len: 0,
        }
    }

    /// Takes an [`vt::Stream`] and returns a [`Stream`]
    /// that turns VT sequences into input events.
    pub fn parse<'parser, 'vt, 'input>(
        &'parser mut self,
        stream: vt::Stream<'vt, 'input>,
    ) -> Stream<'parser, 'vt, 'input> {
        Stream { parser: self, stream }
    }
}

/// An iterator that parses VT sequences into input events.
pub struct Stream<'parser, 'vt, 'input> {
    parser: &'parser mut Parser,
    stream: vt::Stream<'vt, 'input>,
}

impl<'input> Iterator for Stream<'_, '_, 'input> {
    type Item = Input<'input>;

    fn next(&mut self) -> Option<Input<'input>> {
        loop {
            if self.parser.bracketed_paste {
                return self.handle_bracketed_paste();
            }

            if self.parser.x10_mouse_want {
                return self.parse_x10_mouse_coordinates();
            }

            const KEYPAD_LUT: [u8; 8] = [
                vk::UP.value() as u8,    // A
                vk::DOWN.value() as u8,  // B
                vk::RIGHT.value() as u8, // C
                vk::LEFT.value() as u8,  // D
                0,                       // E
                vk::END.value() as u8,   // F
                0,                       // G
                vk::HOME.value() as u8,  // H
            ];

            match self.stream.next()? {
                vt::Token::Text(text) => {
                    return Some(Input::Text(text));
                }
                vt::Token::Ctrl(ch) => match ch {
                    '\0' | '\t' | '\r' => return Some(Input::Keyboard(InputKey::new(ch as u32))),
                    '\n' => return Some(Input::Keyboard(kbmod::CTRL | vk::RETURN)),
                    '\x1f' => return Some(Input::Keyboard(kbmod::CTRL | vk::SLASH)),
                    ..='\x1a' => {
                        // Shift control code to A-Z
                        let key = ch as u32 | 0x40;
                        return Some(Input::Keyboard(kbmod::CTRL | InputKey::new(key)));
                    }
                    '\x7f' => return Some(Input::Keyboard(vk::BACK)),
                    _ => {}
                },
                vt::Token::Esc(ch) => {
                    match ch {
                        '\0' => return Some(Input::Keyboard(vk::ESCAPE)),
                        '\n' => return Some(Input::Keyboard(kbmod::CTRL_ALT | vk::RETURN)),
                        ' '..='~' => {
                            let ch = ch as u32;
                            let key = ch & !0x20; // Shift a-z to A-Z
                            let modifiers =
                                if (ch & 0x20) != 0 { kbmod::ALT } else { kbmod::ALT_SHIFT };
                            return Some(Input::Keyboard(modifiers | InputKey::new(key)));
                        }
                        _ => {}
                    }
                }
                vt::Token::SS3(ch) => match ch {
                    'A'..='H' => {
                        let vk = KEYPAD_LUT[ch as usize - 'A' as usize];
                        if vk != 0 {
                            return Some(Input::Keyboard(InputKey::new(vk as u32)));
                        }
                    }
                    'P'..='S' => {
                        let key = vk::F1.value() + ch as u32 - 'P' as u32;
                        return Some(Input::Keyboard(InputKey::new(key)));
                    }
                    _ => {}
                },
                vt::Token::Csi(csi) => {
                    match csi.final_byte {
                        'A'..='H' => {
                            let vk = KEYPAD_LUT[csi.final_byte as usize - 'A' as usize];
                            if vk != 0 {
                                return Some(Input::Keyboard(
                                    InputKey::new(vk as u32) | Self::parse_modifiers(csi),
                                ));
                            }
                        }
                        'Z' => return Some(Input::Keyboard(kbmod::SHIFT | vk::TAB)),
                        '~' => {
                            const LUT: [u8; 35] = [
                                0,
                                vk::HOME.value() as u8,   // 1
                                vk::INSERT.value() as u8, // 2
                                vk::DELETE.value() as u8, // 3
                                vk::END.value() as u8,    // 4
                                vk::PRIOR.value() as u8,  // 5
                                vk::NEXT.value() as u8,   // 6
                                0,
                                0,
                                0,
                                0,
                                0,
                                0,
                                0,
                                0,
                                vk::F5.value() as u8, // 15
                                0,
                                vk::F6.value() as u8,  // 17
                                vk::F7.value() as u8,  // 18
                                vk::F8.value() as u8,  // 19
                                vk::F9.value() as u8,  // 20
                                vk::F10.value() as u8, // 21
                                0,
                                vk::F11.value() as u8, // 23
                                vk::F12.value() as u8, // 24
                                vk::F13.value() as u8, // 25
                                vk::F14.value() as u8, // 26
                                0,
                                vk::F15.value() as u8, // 28
                                vk::F16.value() as u8, // 29
                                0,
                                vk::F17.value() as u8, // 31
                                vk::F18.value() as u8, // 32
                                vk::F19.value() as u8, // 33
                                vk::F20.value() as u8, // 34
                            ];
                            const LUT_LEN: u16 = LUT.len() as u16;

                            match csi.params[0] {
                                0..LUT_LEN => {
                                    let vk = LUT[csi.params[0] as usize];
                                    if vk != 0 {
                                        return Some(Input::Keyboard(
                                            InputKey::new(vk as u32) | Self::parse_modifiers(csi),
                                        ));
                                    }
                                }
                                200 => self.parser.bracketed_paste = true,
                                _ => {}
                            }
                        }
                        'm' | 'M' if csi.private_byte == '<' => {
                            let btn = csi.params[0];
                            let mut mouse = InputMouse {
                                state: InputMouseState::None,
                                modifiers: kbmod::NONE,
                                position: Default::default(),
                                scroll: Default::default(),
                            };

                            mouse.state = InputMouseState::None;
                            if (btn & 0x40) != 0 {
                                mouse.state = InputMouseState::Scroll;
                                mouse.scroll.y += if (btn & 0x01) != 0 { 3 } else { -3 };
                            } else if csi.final_byte == 'M' {
                                const STATES: [InputMouseState; 4] = [
                                    InputMouseState::Left,
                                    InputMouseState::Middle,
                                    InputMouseState::Right,
                                    InputMouseState::None,
                                ];
                                mouse.state = STATES[(btn as usize) & 0x03];
                            }

                            mouse.modifiers = kbmod::NONE;
                            mouse.modifiers |=
                                if (btn & 0x04) != 0 { kbmod::SHIFT } else { kbmod::NONE };
                            mouse.modifiers |=
                                if (btn & 0x08) != 0 { kbmod::ALT } else { kbmod::NONE };
                            mouse.modifiers |=
                                if (btn & 0x10) != 0 { kbmod::CTRL } else { kbmod::NONE };

                            mouse.position.x = csi.params[1] as CoordType - 1;
                            mouse.position.y = csi.params[2] as CoordType - 1;
                            return Some(Input::Mouse(mouse));
                        }
                        'M' if csi.param_count == 0 => {
                            self.parser.x10_mouse_want = true;
                        }
                        't' if csi.params[0] == 8 => {
                            // Window Size
                            let width = (csi.params[2] as CoordType).clamp(1, 32767);
                            let height = (csi.params[1] as CoordType).clamp(1, 32767);
                            return Some(Input::Resize(Size { width, height }));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

impl<'input> Stream<'_, '_, 'input> {
    /// Once we encounter the start of a bracketed paste
    /// we seek to the end of the paste in this function.
    ///
    /// A bracketed paste is basically:
    /// ```text
    /// <ESC>[201~    lots of text    <ESC>[201~
    /// ```
    ///
    /// That in between text is then expected to be taken literally.
    /// It can be in between anything though, including other escape sequences.
    /// This is the reason why this is a separate method.
    #[cold]
    fn handle_bracketed_paste(&mut self) -> Option<Input<'input>> {
        let beg = self.stream.offset();
        let mut end = beg;

        while let Some(token) = self.stream.next() {
            if let vt::Token::Csi(csi) = token
                && csi.final_byte == '~'
                && csi.params[0] == 201
            {
                self.parser.bracketed_paste = false;
                break;
            }
            end = self.stream.offset();
        }

        if end != beg {
            self.parser
                .bracketed_paste_buf
                .extend_from_slice(&self.stream.input().as_bytes()[beg..end]);
        }

        if !self.parser.bracketed_paste {
            Some(Input::Paste(mem::take(&mut self.parser.bracketed_paste_buf)))
        } else {
            None
        }
    }

    /// Implements the X10 mouse protocol via `CSI M CbCxCy`.
    ///
    /// You want to send numeric mouse coordinates.
    /// You have CSI sequences with numeric parameters.
    /// So, of course you put the coordinates as shifted ASCII characters after
    /// the end of the sequence. Limited coordinate range and complicated parsing!
    /// This is so puzzling to me. The existence of this function makes me unhappy.
    #[cold]
    fn parse_x10_mouse_coordinates(&mut self) -> Option<Input<'input>> {
        while self.parser.x10_mouse_len < 3 && !self.stream.done() {
            self.parser.x10_mouse_buf[self.parser.x10_mouse_len] = self.stream.next_char();
            self.parser.x10_mouse_len += 1;
        }
        if self.parser.x10_mouse_len < 3 {
            return None;
        }

        let b = self.parser.x10_mouse_buf[0] as u32;
        let x = self.parser.x10_mouse_buf[1] as CoordType - 0x21;
        let y = self.parser.x10_mouse_buf[2] as CoordType - 0x21;
        let action = match b & 0b11 {
            0 => InputMouseState::Left,
            1 => InputMouseState::Middle,
            2 => InputMouseState::Right,
            _ => InputMouseState::None,
        };
        let modifiers = {
            let mut m = kbmod::NONE;
            if (b & 0b00100) != 0 {
                m |= kbmod::SHIFT;
            }
            if (b & 0b01000) != 0 {
                m |= kbmod::ALT;
            }
            if (b & 0b10000) != 0 {
                m |= kbmod::CTRL;
            }
            m
        };

        self.parser.x10_mouse_want = false;
        self.parser.x10_mouse_len = 0;

        Some(Input::Mouse(InputMouse {
            state: action,
            modifiers,
            position: Point { x, y },
            scroll: Default::default(),
        }))
    }

    fn parse_modifiers(csi: &vt::Csi) -> InputKeyMod {
        let mut modifiers = kbmod::NONE;
        let p1 = csi.params[1].saturating_sub(1);
        if (p1 & 0x01) != 0 {
            modifiers |= kbmod::SHIFT;
        }
        if (p1 & 0x02) != 0 {
            modifiers |= kbmod::ALT;
        }
        if (p1 & 0x04) != 0 {
            modifiers |= kbmod::CTRL;
        }
        modifiers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_facade_roundtrips() {
        let key = InputKey::from_parts(key::SLASH, kbmod::CTRL_SHIFT);
        assert!(key.key_code() == key::SLASH);
        assert!(key.modifiers() == kbmod::CTRL_SHIFT);
    }

    #[test]
    fn typed_facade_bitor_builds_input_key() {
        let key = key::A | kbmod::ALT;
        assert!(key.key_code() == key::A);
        assert!(key.modifiers() == kbmod::ALT);
    }
}
