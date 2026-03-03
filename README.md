# ![Application Icon for Edit32](./assets/edit.svg) Edit32

A simple editor for simple needs.

This editor pays homage to the classic [MS-DOS Editor](https://en.wikipedia.org/wiki/MS-DOS_Editor), but with a modern interface and input controls similar to VS Code. The goal is to provide an accessible editor that even users largely unfamiliar with terminals can easily use.

![Screenshot of Edit32 with the About dialog in the foreground](./assets/edit_hero_image.png)

## Features

- **Syntax Highlighting** for 40+ languages including Rust, Python, JavaScript, TypeScript, Go, C/C++, and more
- **Multiple Cursors** - Edit multiple locations simultaneously (VS Code-style)
- **Block/Column Selection** - Select and edit rectangular regions of text
- **Find and Replace** with regex support
- **Find in Files** - Search across your entire project
- **Multiple Themes** - Terminal, Nord, Gruvbox, Solarized, Dracula, Tokyo Night, and more
- **Word Wrap** - Toggle soft wrapping for long lines
- **Line Operations** - Duplicate, delete, move, join, and sort lines
- **Encoding Support** - UTF-8, UTF-16, and other encodings with auto-detection
- **Large File Support** - Optimized for editing large files
- **Customizable Keybindings** - Remap shortcuts to your preference
- **Command Palette** - Quick access to all commands (F1)

## Keyboard Shortcuts

### File Operations

| Shortcut | Action |
|----------|--------|
| `Ctrl+N` | New file |
| `Ctrl+O` | Open file |
| `Ctrl+Shift+O` | Open folder |
| `Ctrl+S` | Save |
| `Ctrl+Shift+S` | Save as |
| `Ctrl+W` | Close file |
| `Ctrl+Q` | Exit |

### Editing

| Shortcut | Action |
|----------|--------|
| `Ctrl+Z` | Undo |
| `Ctrl+Y` | Redo |
| `Ctrl+X` | Cut |
| `Ctrl+C` | Copy |
| `Ctrl+V` | Paste |
| `Ctrl+A` | Select all |
| `Ctrl+L` | Select line |
| `Ctrl+Shift+D` | Duplicate line |
| `Ctrl+Shift+K` | Delete line |
| `Ctrl+J` | Join lines |
| `Alt+Up` | Move line up |
| `Alt+Down` | Move line down |
| `Tab` | Indent |
| `Shift+Tab` | Unindent |

### Multiple Cursors

| Shortcut | Action |
|----------|--------|
| `Ctrl+D` | Select word, then add next occurrence |
| `Ctrl+Alt+Up` | Add cursor above |
| `Ctrl+Alt+Down` | Add cursor below |
| `Escape` | Collapse to single cursor |

Multiple cursors allow you to edit several locations at once. Press `Ctrl+D` to select the current word, then press it again to select the next occurrence. All cursors receive the same input when you type.

### Block/Column Selection

| Shortcut | Action |
|----------|--------|
| `Alt+Shift+Up` | Extend block selection up |
| `Alt+Shift+Down` | Extend block selection down |
| `Alt+Shift+Left` | Extend block selection left |
| `Alt+Shift+Right` | Extend block selection right |
| `Escape` | Cancel block selection |

Block selection creates a rectangular selection across multiple lines. This is useful for:
- Editing CSV columns
- Adding or removing text at the same position on multiple lines
- Working with aligned or tabular data

When you type with a block selection active, it automatically converts to multiple cursors.

### Navigation

| Shortcut | Action |
|----------|--------|
| `Ctrl+G` | Go to line |
| `Ctrl+P` | Go to file |
| `Ctrl+M` | Go to matching bracket |
| `Ctrl+Home` | Go to beginning of file |
| `Ctrl+End` | Go to end of file |
| `Home` | Go to beginning of line |
| `End` | Go to end of line |
| `Ctrl+Left` | Move word left |
| `Ctrl+Right` | Move word right |
| `Page Up` | Page up |
| `Page Down` | Page down |

### Search

| Shortcut | Action |
|----------|--------|
| `Ctrl+F` | Find |
| `Ctrl+R` | Replace |
| `F4` | Find in files |

### View

| Shortcut | Action |
|----------|--------|
| `Alt+Z` | Toggle word wrap |
| `Alt+W` | Toggle whitespace visibility |
| `F1` | Command palette |
| `Ctrl+T` | Theme picker |
| `Ctrl+E` | Quick switcher |

### Text Transformations

| Shortcut | Action |
|----------|--------|
| `Alt+Shift+U` | Convert to uppercase |
| `Alt+U` | Convert to lowercase |

Additional transformations available via Command Palette (F1):
- Convert to Title Case
- Encode/Decode Base64
- URL Encode/Decode
- Hex Encode/Decode
- Sort Lines (Ascending/Descending)
- Remove Duplicate Lines
- Remove Empty Lines
- Trim Trailing Whitespace

### Themes

| Shortcut | Action |
|----------|--------|
| `Ctrl+Alt+1` | Terminal theme |
| `Ctrl+Alt+2` | Nord theme |
| `Ctrl+Alt+3` | Gruvbox theme |
| `Ctrl+Alt+4` | Solarized Light theme |
| `Ctrl+Alt+5` | Dracula theme |
| `Ctrl+Alt+6` | Tokyo Night theme |
| `Ctrl+Alt+9` | Previous theme |
| `Ctrl+Alt+0` | Cycle themes |

### Settings

| Shortcut | Action |
|----------|--------|
| `Ctrl+K` | Edit keybindings |

## Installation

[![Packaging status](https://repology.org/badge/vertical-allrepos/microsoft-edit.svg?exclude_unsupported=1)](https://repology.org/project/microsoft-edit/versions)

You can also download binaries from [our Releases page](https://github.com/microsoft/edit/releases/latest).

### Windows

You can install the latest version with WinGet:
```powershell
winget install Edit32
```

### macOS

Using Homebrew:
```bash
brew install edit32
```

Or download the binary from [Releases](https://github.com/microsoft/edit/releases/latest).

### Linux

Check your distribution's package manager, or download the binary from [Releases](https://github.com/microsoft/edit/releases/latest).

For Debian/Ubuntu-based systems, you can install the `.deb` package:
```bash
sudo dpkg -i edit32_*.deb
```

## Build Instructions

* [Install Rust](https://www.rust-lang.org/tools/install)
* Install the nightly toolchain: `rustup install nightly`
  * Alternatively, set the environment variable `RUSTC_BOOTSTRAP=1`
* Clone the repository
* For a release build, run:
  * Rust 1.90 or earlier: `cargo build --config .cargo/release.toml --release`
  * otherwise: `cargo build --config .cargo/release-nightly.toml --release`
* To run locally: `cargo run --bin edit32`

### Build Configuration

During compilation you can set various environment variables to configure the build. The following table lists the available configuration options:

Environment variable | Description
--- | ---
`EDIT_CFG_ICU*` | See [ICU library name (SONAME)](#icu-library-name-soname) for details.
`EDIT_CFG_LANGUAGES` | A comma-separated list of languages to include in the build. See [i18n/edit.toml](i18n/edit.toml) for available languages.

## Configuration

Edit32 stores its configuration in a platform-specific location:
- **Windows**: `%APPDATA%\edit32\config.ini`
- **macOS**: `~/.config/edit32/config.ini` (or `$XDG_CONFIG_HOME/edit32/config.ini` when set)
- **Linux**: `~/.config/edit32/config.ini` (or `$XDG_CONFIG_HOME/edit32/config.ini` when set)

### Customizing Keybindings

Press `Ctrl+K` to open the keybindings editor. You can remap any command to a different key combination.

## Notes to Package Maintainers

### Package Naming

The canonical executable name is "edit32" and the alternative name is "msedit32".
We're aware of the potential conflict of "edit32" with existing commands and recommend alternatively naming packages and executables "msedit32".
Names such as "ms-edit32" should be avoided.
Assigning an "edit32" alias is recommended, if possible.

### ICU library name (SONAME)

This project _optionally_ depends on the ICU library for its Search and Replace functionality.
By default, the project will look for a SONAME without version suffix:
* Windows: `icuuc.dll`
* macOS: `libicuuc.dylib`
* UNIX, and other OS: `libicuuc.so`

If your installation uses a different SONAME, please set the following environment variable at build time:
* `EDIT_CFG_ICUUC_SONAME`:
  For instance, `libicuuc.so.76`.
* `EDIT_CFG_ICUI18N_SONAME`:
  For instance, `libicui18n.so.76`.

Additionally, this project assumes that the ICU exports are exported without `_` prefix and without version suffix, such as `u_errorName`.
If your installation uses versioned exports, please set:
* `EDIT_CFG_ICU_CPP_EXPORTS`:
  If set to `true`, it'll look for C++ symbols such as `_u_errorName`.
  Enabled by default on macOS.
* `EDIT_CFG_ICU_RENAMING_VERSION`:
  If set to a version number, such as `76`, it'll look for symbols such as `u_errorName_76`.

Finally, you can set the following environment variables:
* `EDIT_CFG_ICU_RENAMING_AUTO_DETECT`:
  If set to `true`, the executable will try to detect the `EDIT_CFG_ICU_RENAMING_VERSION` value at runtime.
  The way it does this is not officially supported by ICU and as such is not recommended to be relied upon.
  Enabled by default on UNIX (excluding macOS) if no other options are set.

To test your settings, run `cargo test` again but with the `--ignored` flag. For instance:
```sh
cargo test -- --ignored
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development guidelines and architecture overview.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
