// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::env;
use std::path::PathBuf;
use std::{fs, io};

use edit::apperr;
use edit::framebuffer;
use edit::helpers::CoordType;
use edit::input::{kbmod, vk, InputKey};
use edit::oklab::StraightRgba;

use crate::commands::CommandId;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThemeId {
    Terminal,
    Nord,
    OneDark,
    Gruvbox,
    Monokai,
    SolarizedDark,
    SolarizedLight,
    Dracula,
    TokyoNight,
    Midnight,
    Paperwhite,
    Custom,
}

impl Default for ThemeId {
    fn default() -> Self {
        ThemeId::Terminal
    }
}

impl ThemeId {
    pub fn config_value(self) -> &'static str {
        match self {
            ThemeId::Terminal => "terminal",
            ThemeId::Nord => "nord",
            ThemeId::OneDark => "one-dark",
            ThemeId::Gruvbox => "gruvbox",
            ThemeId::Monokai => "monokai",
            ThemeId::SolarizedDark => "solarized-dark",
            ThemeId::SolarizedLight => "solarized-light",
            ThemeId::Dracula => "dracula",
            ThemeId::TokyoNight => "tokyo-night",
            ThemeId::Midnight => "midnight",
            ThemeId::Paperwhite => "paperwhite",
            ThemeId::Custom => "custom",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ThemeId::Terminal => "Terminal",
            ThemeId::Nord => "Nord",
            ThemeId::OneDark => "One Dark",
            ThemeId::Gruvbox => "Gruvbox",
            ThemeId::Monokai => "Monokai",
            ThemeId::SolarizedDark => "Solarized Dark",
            ThemeId::SolarizedLight => "Solarized Light",
            ThemeId::Dracula => "Dracula",
            ThemeId::TokyoNight => "Tokyo Night",
            ThemeId::Midnight => "Midnight",
            ThemeId::Paperwhite => "Paperwhite",
            ThemeId::Custom => "Custom",
        }
    }
}

#[derive(Clone, Copy)]
pub struct EditorSettings {
    pub word_wrap: bool,
    pub tab_size: CoordType,
    pub indent_with_tabs: bool,
    pub high_contrast: bool,
    pub theme: ThemeId,
    pub custom_theme: Option<[StraightRgba; framebuffer::INDEXED_COLORS_COUNT]>,
    pub large_file_threshold_bytes: u64,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            word_wrap: false,
            tab_size: 4,
            indent_with_tabs: false,
            high_contrast: false,
            theme: ThemeId::default(),
            custom_theme: None,
            large_file_threshold_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Default, Clone)]
pub struct Keybindings {
    overrides: Vec<(CommandId, InputKey)>,
}

impl Keybindings {
    pub fn shortcut(&self, id: CommandId) -> InputKey {
        if let Some((_, key)) = self.overrides.iter().find(|(cid, _)| *cid == id) {
            *key
        } else {
            crate::commands::shortcut(id)
        }
    }

    pub fn set_override(&mut self, id: CommandId, key: InputKey) {
        if let Some((_, existing)) = self.overrides.iter_mut().find(|(cid, _)| *cid == id) {
            *existing = key;
        } else {
            self.overrides.push((id, key));
        }
    }

    pub fn overrides(&self) -> &[(CommandId, InputKey)] {
        &self.overrides
    }
}

#[derive(Clone)]
pub struct Config {
    pub editor: EditorSettings,
    pub keybindings: Keybindings,
    pub recent_files: Vec<PathBuf>,
    pub recent_folders: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            editor: Default::default(),
            keybindings: Default::default(),
            recent_files: Vec::new(),
            recent_folders: Vec::new(),
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    if cfg!(windows) {
        env::var_os("APPDATA").map(|root| PathBuf::from(root).join("edit32").join("config.ini"))
    } else {
        let base = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("edit32").join("config.ini"))
    }
}

pub fn load_config() -> Config {
    let mut cfg = Config::default();
    let Some(path) = config_path() else {
        return cfg;
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return cfg;
    };

    for line in contents.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        let mut parts = line.splitn(2, '=');
        let key = parts.next().unwrap_or("").trim();
        let value = parts.next().unwrap_or("").trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }

        apply_config_kv(&mut cfg, key, value);
    }

    cfg
}

pub fn ensure_config_file() -> Option<PathBuf> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if !path.exists() {
        let default = default_config_text();
        let _ = std::fs::write(&path, default);
    }
    Some(path)
}

pub fn persist_theme(theme: ThemeId) -> apperr::Result<()> {
    let Some(path) = config_path() else {
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let contents = fs::read_to_string(&path).unwrap_or_default();
    let mut lines = Vec::new();
    let mut replaced = false;
    let theme_value = theme.config_value();

    for line in contents.lines() {
        let (content, comment) = match line.split_once('#') {
            Some((left, right)) => (left, Some(right)),
            None => (line, None),
        };
        let key = content.splitn(2, '=').next().unwrap_or("").trim();
        if key == "editor.theme" {
            let mut updated = format!("editor.theme = {theme_value}");
            if let Some(comment) = comment {
                updated.push_str(" #");
                updated.push_str(comment);
            }
            lines.push(updated);
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }

    if !replaced {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(format!("editor.theme = {theme_value}"));
    }

    let mut output = lines.join("\n");
    if !output.ends_with('\n') {
        output.push('\n');
    }
    if output.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty config contents").into());
    }

    fs::write(&path, output)?;
    Ok(())
}

pub fn persist_recents(files: &[PathBuf], folders: &[PathBuf]) -> apperr::Result<()> {
    let Some(path) = config_path() else {
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let contents = fs::read_to_string(&path).unwrap_or_default();
    let mut lines = Vec::new();
    for line in contents.lines() {
        let key = line.split('#').next().unwrap_or("").splitn(2, '=').next().unwrap_or("").trim();
        if key == "recent.files" || key == "recent.folders" {
            continue;
        }
        lines.push(line.to_string());
    }

    let files_value = join_recent_list(files);
    let folders_value = join_recent_list(folders);

    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(format!("recent.files = {files_value}"));
    lines.push(format!("recent.folders = {folders_value}"));

    let mut output = lines.join("\n");
    if !output.ends_with('\n') {
        output.push('\n');
    }

    fs::write(&path, output)?;
    Ok(())
}

fn join_recent_list(items: &[PathBuf]) -> String {
    items
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join(";")
}

pub fn persist_keybindings(keybindings: &Keybindings) -> apperr::Result<()> {
    let Some(path) = config_path() else {
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let overrides = keybindings
        .overrides()
        .iter()
        .filter_map(|(id, key)| {
            let key_name = keybinding_config_key(*id)?;
            let value = format_keybinding_ascii(*key)?;
            Some((key_name.to_string(), value))
        })
        .collect::<Vec<_>>();

    let contents = fs::read_to_string(&path).unwrap_or_default();
    let mut lines = Vec::new();

    for line in contents.lines() {
        let key = line.split('#').next().unwrap_or("").splitn(2, '=').next().unwrap_or("").trim();
        if overrides.iter().any(|(name, _)| name == key) {
            continue;
        }
        lines.push(line.to_string());
    }

    if !overrides.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        for (name, value) in overrides {
            lines.push(format!("{name} = {value}"));
        }
    }

    let mut output = lines.join("\n");
    if !output.ends_with('\n') {
        output.push('\n');
    }
    fs::write(&path, output)?;
    Ok(())
}

fn keybinding_config_key(id: CommandId) -> Option<&'static str> {
    Some(match id {
        CommandId::CommandPalette => "keybindings.command_palette",
        CommandId::FileNew => "keybindings.file_new",
        CommandId::FileOpen => "keybindings.file_open",
        CommandId::FileOpenFolder => "keybindings.file_open_folder",
        CommandId::FileOpenRecentFolder => "keybindings.file_open_recent_folder",
        CommandId::FileSave => "keybindings.file_save",
        CommandId::FileSaveAs => "keybindings.file_save_as",
        CommandId::FileClose => "keybindings.file_close",
        CommandId::FileExit => "keybindings.file_exit",
        CommandId::EditFind => "keybindings.edit_find",
        CommandId::EditReplace => "keybindings.edit_replace",
        CommandId::FindInFiles => "keybindings.find_in_files",
        CommandId::ViewGoToFile => "keybindings.view_goto_file",
        CommandId::FileGoto => "keybindings.file_goto",
        CommandId::QuickSwitcher => "keybindings.quick_switcher",
        CommandId::HelpContext => "keybindings.help_context",
        CommandId::ThemePicker => "keybindings.theme_picker",
        CommandId::SettingsOpenConfig => "keybindings.settings_open",
        CommandId::SettingsReload => "keybindings.settings_reload",
        CommandId::SettingsToggleHighContrast => "keybindings.settings_toggle_high_contrast",
        CommandId::SettingsEditKeybindings => "keybindings.settings_edit_keybindings",
        CommandId::SettingsThemeTerminal => "keybindings.theme_terminal",
        CommandId::SettingsThemeNord => "keybindings.theme_nord",
        CommandId::SettingsThemeOneDark => "keybindings.theme_one_dark",
        CommandId::SettingsThemeGruvbox => "keybindings.theme_gruvbox",
        CommandId::SettingsThemeMonokai => "keybindings.theme_monokai",
        CommandId::SettingsThemeSolarizedDark => "keybindings.theme_solarized_dark",
        CommandId::SettingsThemeSolarizedLight => "keybindings.theme_solarized_light",
        CommandId::SettingsThemeDracula => "keybindings.theme_dracula",
        CommandId::SettingsThemeTokyoNight => "keybindings.theme_tokyo_night",
        CommandId::SettingsThemeMidnight => "keybindings.theme_midnight",
        CommandId::SettingsThemePaperwhite => "keybindings.theme_paperwhite",
        CommandId::SettingsThemeCustom => "keybindings.theme_custom",
        CommandId::SettingsThemeCycle => "keybindings.theme_cycle",
        CommandId::SettingsThemePrevious => "keybindings.theme_previous",
        _ => return None,
    })
}

fn format_keybinding_ascii(key: InputKey) -> Option<String> {
    if key == vk::NULL {
        return None;
    }
    let modifiers = key.modifiers();
    let base = key.key();
    let mut parts = Vec::new();

    if modifiers.contains(kbmod::CTRL) {
        parts.push("Ctrl");
    }
    if modifiers.contains(kbmod::ALT) {
        parts.push("Alt");
    }
    if modifiers.contains(kbmod::SHIFT) {
        parts.push("Shift");
    }

    let name = key_name_ascii(base)?;
    parts.push(name);
    Some(parts.join("+"))
}

fn key_name_ascii(key: InputKey) -> Option<&'static str> {
    Some(match key {
        vk::A => "A",
        vk::B => "B",
        vk::C => "C",
        vk::D => "D",
        vk::E => "E",
        vk::F => "F",
        vk::G => "G",
        vk::H => "H",
        vk::I => "I",
        vk::J => "J",
        vk::K => "K",
        vk::L => "L",
        vk::M => "M",
        vk::N => "N",
        vk::O => "O",
        vk::P => "P",
        vk::Q => "Q",
        vk::R => "R",
        vk::S => "S",
        vk::T => "T",
        vk::U => "U",
        vk::V => "V",
        vk::W => "W",
        vk::X => "X",
        vk::Y => "Y",
        vk::Z => "Z",
        vk::N0 => "0",
        vk::N1 => "1",
        vk::N2 => "2",
        vk::N3 => "3",
        vk::N4 => "4",
        vk::N5 => "5",
        vk::N6 => "6",
        vk::N7 => "7",
        vk::N8 => "8",
        vk::N9 => "9",
        vk::F1 => "F1",
        vk::F2 => "F2",
        vk::F3 => "F3",
        vk::F4 => "F4",
        vk::F5 => "F5",
        vk::F6 => "F6",
        vk::F7 => "F7",
        vk::F8 => "F8",
        vk::F9 => "F9",
        vk::F10 => "F10",
        vk::F11 => "F11",
        vk::F12 => "F12",
        vk::RETURN => "Enter",
        vk::ESCAPE => "Esc",
        vk::SPACE => "Space",
        vk::TAB => "Tab",
        vk::BACK => "Backspace",
        vk::DELETE => "Delete",
        vk::UP => "Up",
        vk::DOWN => "Down",
        vk::LEFT => "Left",
        vk::RIGHT => "Right",
        vk::HOME => "Home",
        vk::END => "End",
        vk::PRIOR => "PageUp",
        vk::NEXT => "PageDown",
        _ => return None,
    })
}

fn default_config_text() -> &'static str {
    concat!(
        "# Edit32 configuration\n",
        "#\n",
        "# editor.word_wrap = true\n",
        "# editor.tab_size = 4\n",
        "# editor.indent_with_tabs = false\n",
        "# editor.high_contrast = false\n",
        "# editor.theme = terminal  # terminal, nord, one-dark, gruvbox, monokai, solarized-dark, solarized-light, dracula, tokyo-night, midnight, paperwhite, custom\n",
        "# editor.large_file_threshold_bytes = 8388608\n",
        "# theme.custom.palette = #000000,#ff0000,#00ff00,#ffff00,#0000ff,#ff00ff,#00ffff,#ffffff,#555555,#ff5555,#55ff55,#ffff55,#5555ff,#ff55ff,#55ffff,#ffffff,#000000,#ffffff\n",
        "# recent.files = /path/to/file.txt;/path/to/other.txt\n",
        "# recent.folders = /path/to/project;/path/to/other\n",
        "#\n",
        "# keybindings.command_palette = F1\n",
        "# keybindings.find_in_files = F4\n",
        "# keybindings.file_save = Ctrl+S\n",
        "# keybindings.file_open = Ctrl+O\n",
        "# keybindings.file_open_folder = Ctrl+Shift+O\n",
        "# keybindings.file_open_recent_folder = Ctrl+Shift+R\n",
        "# keybindings.quick_switcher = Ctrl+E\n",
        "# keybindings.help_context = Shift+F1\n",
        "# keybindings.settings_edit_keybindings = Ctrl+K\n",
        "# keybindings.theme_picker = Ctrl+Shift+T\n",
        "# keybindings.theme_terminal = Ctrl+Alt+1\n",
        "# keybindings.theme_nord = Ctrl+Alt+2\n",
        "# keybindings.theme_gruvbox = Ctrl+Alt+3\n",
        "# keybindings.theme_solarized_light = Ctrl+Alt+4\n",
        "# keybindings.theme_dracula = Ctrl+Alt+5\n",
        "# keybindings.theme_tokyo_night = Ctrl+Alt+6\n",
        "# keybindings.theme_cycle = Ctrl+Alt+0\n",
        "# keybindings.theme_previous = Ctrl+Alt+9\n",
        "# keybindings.theme_one_dark = Ctrl+Alt+7\n",
        "# keybindings.theme_monokai = Ctrl+Alt+8\n",
        "# keybindings.theme_solarized_dark = Ctrl+Alt+-\n",
        "# keybindings.theme_midnight = Ctrl+Alt+M\n",
        "# keybindings.theme_paperwhite = Ctrl+Alt+P\n",
        "# keybindings.theme_custom = Ctrl+Alt+C\n",
    )
}

fn apply_config_kv(cfg: &mut Config, key: &str, value: &str) {
    match key {
        "editor.word_wrap" => {
            if let Some(v) = parse_bool(value) {
                cfg.editor.word_wrap = v;
            }
        }
        "editor.tab_size" => {
            if let Ok(v) = value.parse::<u8>() {
                let v = v.clamp(1, 8) as CoordType;
                cfg.editor.tab_size = v;
            }
        }
        "editor.indent_with_tabs" => {
            if let Some(v) = parse_bool(value) {
                cfg.editor.indent_with_tabs = v;
            }
        }
        "editor.high_contrast" => {
            if let Some(v) = parse_bool(value) {
                cfg.editor.high_contrast = v;
            }
        }
        "editor.theme" => {
            if let Some(theme) = parse_theme(value) {
                cfg.editor.theme = theme;
            }
        }
        "editor.large_file_threshold_bytes" => {
            if let Ok(v) = value.parse::<u64>() {
                cfg.editor.large_file_threshold_bytes = v.max(1024);
            }
        }
        "theme.custom.palette" => {
            if let Some(palette) = parse_palette(value) {
                cfg.editor.custom_theme = Some(palette);
            }
        }
        "recent.files" => {
            cfg.recent_files = parse_recent_list(value);
        }
        "recent.folders" => {
            cfg.recent_folders = parse_recent_list(value);
        }
        _ => {
            if let Some((id, keybinding)) = parse_keybinding(key, value) {
                cfg.keybindings.set_override(id, keybinding);
            }
        }
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

fn parse_keybinding(key: &str, value: &str) -> Option<(CommandId, InputKey)> {
    let id = match key {
        "keybindings.command_palette" => CommandId::CommandPalette,
        "keybindings.file_new" => CommandId::FileNew,
        "keybindings.file_open" => CommandId::FileOpen,
        "keybindings.file_open_folder" => CommandId::FileOpenFolder,
        "keybindings.file_open_recent_folder" => CommandId::FileOpenRecentFolder,
        "keybindings.file_save" => CommandId::FileSave,
        "keybindings.file_save_as" => CommandId::FileSaveAs,
        "keybindings.file_close" => CommandId::FileClose,
        "keybindings.file_exit" => CommandId::FileExit,
        "keybindings.edit_find" => CommandId::EditFind,
        "keybindings.edit_replace" => CommandId::EditReplace,
        "keybindings.find_in_files" => CommandId::FindInFiles,
        "keybindings.view_goto_file" => CommandId::ViewGoToFile,
        "keybindings.file_goto" => CommandId::FileGoto,
        "keybindings.quick_switcher" => CommandId::QuickSwitcher,
        "keybindings.help_context" => CommandId::HelpContext,
        "keybindings.theme_picker" => CommandId::ThemePicker,
        "keybindings.settings_open" => CommandId::SettingsOpenConfig,
        "keybindings.settings_reload" => CommandId::SettingsReload,
        "keybindings.settings_toggle_high_contrast" => CommandId::SettingsToggleHighContrast,
        "keybindings.settings_edit_keybindings" => CommandId::SettingsEditKeybindings,
        "keybindings.theme_terminal" => CommandId::SettingsThemeTerminal,
        "keybindings.theme_nord" => CommandId::SettingsThemeNord,
        "keybindings.theme_gruvbox" => CommandId::SettingsThemeGruvbox,
        "keybindings.theme_solarized_light" => CommandId::SettingsThemeSolarizedLight,
        "keybindings.theme_dracula" => CommandId::SettingsThemeDracula,
        "keybindings.theme_tokyo_night" => CommandId::SettingsThemeTokyoNight,
        "keybindings.theme_cycle" => CommandId::SettingsThemeCycle,
        "keybindings.theme_previous" => CommandId::SettingsThemePrevious,
        "keybindings.theme_one_dark" => CommandId::SettingsThemeOneDark,
        "keybindings.theme_monokai" => CommandId::SettingsThemeMonokai,
        "keybindings.theme_solarized_dark" => CommandId::SettingsThemeSolarizedDark,
        "keybindings.theme_midnight" => CommandId::SettingsThemeMidnight,
        "keybindings.theme_paperwhite" => CommandId::SettingsThemePaperwhite,
        "keybindings.theme_custom" => CommandId::SettingsThemeCustom,
        _ => return None,
    };

    parse_key(value).map(|key| (id, key))
}

fn parse_key(value: &str) -> Option<InputKey> {
    let mut mods = kbmod::NONE;
    let mut key = None;

    for part in value.split(|c| c == '+' || c == '-') {
        let token = part.trim();
        if token.is_empty() {
            continue;
        }
        match token.to_ascii_uppercase().as_str() {
            "CTRL" | "CONTROL" => mods |= kbmod::CTRL,
            "ALT" => mods |= kbmod::ALT,
            "SHIFT" => mods |= kbmod::SHIFT,
            other => {
                key = parse_key_token(other);
            }
        }
    }

    key.map(|k| mods | k)
}

fn parse_theme(value: &str) -> Option<ThemeId> {
    let mut normalized = value.trim().to_ascii_lowercase();
    normalized.retain(|ch| ch != ' ');
    let normalized = normalized.replace('_', "-");
    match normalized.as_str() {
        "terminal" | "default" => Some(ThemeId::Terminal),
        "nord" => Some(ThemeId::Nord),
        "one-dark" | "one_dark" | "onedark" => Some(ThemeId::OneDark),
        "gruvbox" | "gruvbox-dark" => Some(ThemeId::Gruvbox),
        "monokai" => Some(ThemeId::Monokai),
        "solarized-dark" => Some(ThemeId::SolarizedDark),
        "solarized-light" | "solarized" => Some(ThemeId::SolarizedLight),
        "dracula" => Some(ThemeId::Dracula),
        "tokyo-night" | "tokyonight" => Some(ThemeId::TokyoNight),
        "midnight" => Some(ThemeId::Midnight),
        "paperwhite" => Some(ThemeId::Paperwhite),
        "custom" => Some(ThemeId::Custom),
        _ => None,
    }
}

fn parse_palette(value: &str) -> Option<[StraightRgba; framebuffer::INDEXED_COLORS_COUNT]> {
    let mut colors = Vec::new();
    for token in value.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let color = parse_hex_color(token)?;
        colors.push(color);
    }
    if colors.len() != framebuffer::INDEXED_COLORS_COUNT {
        return None;
    }
    let mut palette = [StraightRgba::zero(); framebuffer::INDEXED_COLORS_COUNT];
    for (idx, color) in colors.into_iter().enumerate() {
        palette[idx] = color;
    }
    Some(palette)
}

fn parse_hex_color(value: &str) -> Option<StraightRgba> {
    let value = value.trim().trim_start_matches('#');
    let (rgb, alpha) = match value.len() {
        6 => (value, 0xff),
        8 => (&value[..6], u8::from_str_radix(&value[6..8], 16).ok()?),
        _ => return None,
    };
    let rgb = u32::from_str_radix(rgb, 16).ok()?;
    let rgba = (rgb << 8) | (alpha as u32);
    Some(StraightRgba::from_be(rgba))
}

fn parse_recent_list(value: &str) -> Vec<PathBuf> {
    value
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn parse_key_token(token: &str) -> Option<InputKey> {
    if token.len() == 1 {
        let ch = token.as_bytes()[0];
        return match ch {
            b'A' => Some(vk::A),
            b'B' => Some(vk::B),
            b'C' => Some(vk::C),
            b'D' => Some(vk::D),
            b'E' => Some(vk::E),
            b'F' => Some(vk::F),
            b'G' => Some(vk::G),
            b'H' => Some(vk::H),
            b'I' => Some(vk::I),
            b'J' => Some(vk::J),
            b'K' => Some(vk::K),
            b'L' => Some(vk::L),
            b'M' => Some(vk::M),
            b'N' => Some(vk::N),
            b'O' => Some(vk::O),
            b'P' => Some(vk::P),
            b'Q' => Some(vk::Q),
            b'R' => Some(vk::R),
            b'S' => Some(vk::S),
            b'T' => Some(vk::T),
            b'U' => Some(vk::U),
            b'V' => Some(vk::V),
            b'W' => Some(vk::W),
            b'X' => Some(vk::X),
            b'Y' => Some(vk::Y),
            b'Z' => Some(vk::Z),
            b'0' => Some(vk::N0),
            b'1' => Some(vk::N1),
            b'2' => Some(vk::N2),
            b'3' => Some(vk::N3),
            b'4' => Some(vk::N4),
            b'5' => Some(vk::N5),
            b'6' => Some(vk::N6),
            b'7' => Some(vk::N7),
            b'8' => Some(vk::N8),
            b'9' => Some(vk::N9),
            _ => None,
        };
    }

    match token {
        "ENTER" | "RETURN" => Some(vk::RETURN),
        "ESC" | "ESCAPE" => Some(vk::ESCAPE),
        "SPACE" => Some(vk::SPACE),
        "TAB" => Some(vk::TAB),
        "BACKSPACE" => Some(vk::BACK),
        "DELETE" => Some(vk::DELETE),
        "UP" => Some(vk::UP),
        "DOWN" => Some(vk::DOWN),
        "LEFT" => Some(vk::LEFT),
        "RIGHT" => Some(vk::RIGHT),
        "HOME" => Some(vk::HOME),
        "END" => Some(vk::END),
        "PAGEUP" | "PGUP" => Some(vk::PRIOR),
        "PAGEDOWN" | "PGDN" => Some(vk::NEXT),
        "F1" => Some(vk::F1),
        "F2" => Some(vk::F2),
        "F3" => Some(vk::F3),
        "F4" => Some(vk::F4),
        "F5" => Some(vk::F5),
        "F6" => Some(vk::F6),
        "F7" => Some(vk::F7),
        "F8" => Some(vk::F8),
        "F9" => Some(vk::F9),
        "F10" => Some(vk::F10),
        "F11" => Some(vk::F11),
        "F12" => Some(vk::F12),
        _ => None,
    }
}

pub fn apply_settings_to_document(settings: &EditorSettings, doc: &crate::documents::Document) {
    if doc.mode != crate::documents::DocumentMode::Text {
        return;
    }
    let mut tb = doc.buffer.borrow_mut();
    tb.set_word_wrap(settings.word_wrap);
    tb.set_tab_size(settings.tab_size);
    tb.set_indent_with_tabs(settings.indent_with_tabs);
}

pub fn apply_settings_to_all(settings: &EditorSettings, documents: &crate::documents::DocumentManager) {
    for doc in documents.iter() {
        apply_settings_to_document(settings, doc);
    }
}
