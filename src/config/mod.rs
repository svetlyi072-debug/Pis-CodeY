//! User configuration.
//!
//! Three layers, later ones overriding earlier ones key by key:
//! built-in defaults, the user's `config.toml`, and a `.pis-code.toml`
//! found in the edited file's directory or any parent (a per-project
//! override, like `.editorconfig`). `default.toml` next to this file lists
//! every option with its default and documentation; it is embedded in the
//! binary and is what `PC --init-config` writes.

mod keymap;
mod settings;

pub use keymap::{Action, Keymap};
pub use settings::{HintSetting, SearchSettings, SelectionStyle, Settings, UiSettings};

use crate::language::Language;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The fully documented default configuration, as written by
/// `PC --init-config`.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("default.toml");

/// Name of the per-project override file.
pub const PROJECT_CONFIG_NAME: &str = ".pis-code.toml";

/// Environment variable naming a config file to use instead of the
/// default location.
pub const CONFIG_ENV_VAR: &str = "PIS_CODE_CONFIG";

// ---------------------------------------------------------------- enums

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndentStyle {
    Auto,
    Spaces,
    Tabs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BraceStyle {
    /// Whatever suits the language (see `language.rs`).
    Auto,
    Allman,
    #[serde(rename = "k&r", alias = "kr")]
    KAndR,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    /// Keep whatever the file already uses (LF for a new file).
    Auto,
    Lf,
    Crlf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    Default,
    BlinkingBlock,
    Block,
    BlinkingUnderline,
    Underline,
    BlinkingBar,
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaseMode {
    /// Ignore case unless the search text has a capital letter.
    Smart,
    Sensitive,
    Insensitive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusPosition {
    Bottom,
    Top,
}

// -------------------------------------------------------------- sections

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EditorConfig {
    pub tab_width: usize,
    pub indent_style: IndentStyle,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indent_width: Option<usize>,
    pub detect_indentation: bool,
    pub auto_indent: bool,
    pub smart_enter: bool,
    pub auto_close_brackets: bool,
    pub auto_close_quotes: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_close_single_quote: Option<bool>,
    pub auto_close_tags: bool,
    pub brace_style: BraceStyle,
    pub scroll_off: usize,
    pub page_size: usize,
    pub word_chars: String,
    pub undo_limit: usize,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            tab_width: 4,
            indent_style: IndentStyle::Auto,
            indent_width: None,
            detect_indentation: true,
            auto_indent: true,
            smart_enter: true,
            auto_close_brackets: true,
            auto_close_quotes: true,
            auto_close_single_quote: None,
            auto_close_tags: true,
            brace_style: BraceStyle::Auto,
            scroll_off: 0,
            page_size: 0,
            word_chars: "_".to_string(),
            undo_limit: 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilesConfig {
    pub line_ending: LineEnding,
    pub insert_final_newline: bool,
    pub trim_trailing_whitespace: bool,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            line_ending: LineEnding::Auto,
            insert_final_newline: true,
            trim_trailing_whitespace: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayConfig {
    pub line_numbers: bool,
    pub relative_line_numbers: bool,
    pub gutter_min_width: usize,
    pub cursor_style: CursorStyle,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            line_numbers: true,
            relative_line_numbers: false,
            gutter_min_width: 4,
            cursor_style: CursorStyle::Default,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StatusBarConfig {
    pub position: StatusPosition,
    pub show_file_name: bool,
    pub show_modified_marker: bool,
    pub show_cursor_position: bool,
    pub show_char_count: bool,
    pub show_line_count: bool,
    /// `"auto"` builds the shortcut hint from the real key bindings; an
    /// empty string hides it; anything else is shown verbatim.
    pub hint: String,
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self {
            position: StatusPosition::Bottom,
            show_file_name: true,
            show_modified_marker: true,
            show_cursor_position: true,
            show_char_count: true,
            show_line_count: true,
            hint: "auto".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ColorsConfig {
    pub gutter: String,
    pub gutter_current_line: String,
    pub gutter_error: String,
    pub gutter_warning: String,
    pub diagnostic_error: String,
    pub diagnostic_warning: String,
    pub status_fg: String,
    pub status_bg: String,
    pub status_error_fg: String,
    pub status_error_bg: String,
    pub selection: String,
    pub search_match: String,
    pub search_current: String,
    pub search_current_text: String,
}

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            gutter: "darkgray".into(),
            gutter_current_line: "darkgray".into(),
            gutter_error: "red".into(),
            gutter_warning: "yellow".into(),
            diagnostic_error: "red".into(),
            diagnostic_warning: "yellow".into(),
            status_fg: "black".into(),
            status_bg: "#7aa2f7".into(),
            status_error_fg: "white".into(),
            status_error_bg: "#8b2f2f".into(),
            selection: "reverse".into(),
            search_match: "#58511f".into(),
            search_current: "#f0a030".into(),
            search_current_text: "black".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MouseConfig {
    pub enabled: bool,
    pub scroll_lines: usize,
    pub double_click_ms: u64,
}

impl Default for MouseConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scroll_lines: 3,
            double_click_ms: 400,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    pub case: CaseMode,
    pub regex: bool,
    pub whole_word: bool,
    pub wrap_around: bool,
    pub incremental: bool,
    pub select_on_close: bool,
    pub highlight_matches: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            case: CaseMode::Smart,
            regex: false,
            whole_word: false,
            wrap_around: true,
            incremental: true,
            select_on_close: true,
            highlight_matches: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyntaxConfig {
    pub enabled: bool,
    pub theme: String,
}

impl Default for SyntaxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: "base16-ocean.dark".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckConfig {
    pub enabled: bool,
    pub on_save: bool,
    pub timeout_seconds: u64,
    pub mark_gutter: bool,
    pub underline: bool,
    pub dotnet_target_framework: String,
    pub rust_edition: String,
    /// Extra arguments for the C / C++ compiler, e.g. `["-Iinclude", "-std=c11"]`.
    pub c_flags: Vec<String>,
    pub cpp_flags: Vec<String>,
}

impl Default for CheckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            on_save: false,
            timeout_seconds: 300,
            mark_gutter: true,
            underline: true,
            dotnet_target_framework: "auto".to_string(),
            rust_edition: "2021".to_string(),
            c_flags: Vec::new(),
            cpp_flags: Vec::new(),
        }
    }
}

/// Executables the checkers run. An empty string means "find it": Python
/// tries `python3` then `python`, Godot tries `godot4` then `godot`, the C
/// compiler `cc`, `gcc`, `clang`, and the C++ one `c++`, `g++`, `clang++`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    pub dotnet: String,
    pub cargo: String,
    pub rustc: String,
    pub node: String,
    pub python: String,
    pub godot: String,
    pub cc: String,
    pub cxx: String,
    pub go: String,
    pub javac: String,
    pub php: String,
    pub tsc: String,
    pub kotlinc: String,
    pub swiftc: String,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            dotnet: "dotnet".into(),
            cargo: "cargo".into(),
            rustc: "rustc".into(),
            node: "node".into(),
            python: String::new(),
            godot: String::new(),
            cc: String::new(),
            cxx: String::new(),
            go: "go".into(),
            javac: "javac".into(),
            php: "php".into(),
            tsc: "tsc".into(),
            kotlinc: "kotlinc".into(),
            swiftc: "swiftc".into(),
        }
    }
}

/// One list of chords per action; replacing an action's list rebinds it,
/// and an empty list unbinds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeysConfig {
    pub save: Vec<String>,
    pub save_and_check: Vec<String>,
    pub quit: Vec<String>,
    pub undo: Vec<String>,
    pub redo: Vec<String>,
    pub select_all: Vec<String>,
    pub copy: Vec<String>,
    pub cut: Vec<String>,
    pub paste: Vec<String>,
    pub newline: Vec<String>,
    pub backspace: Vec<String>,
    pub delete: Vec<String>,
    pub delete_word_backward: Vec<String>,
    pub delete_word_forward: Vec<String>,
    pub indent: Vec<String>,
    pub dedent: Vec<String>,
    pub move_left: Vec<String>,
    pub move_right: Vec<String>,
    pub move_up: Vec<String>,
    pub move_down: Vec<String>,
    pub move_home: Vec<String>,
    pub move_end: Vec<String>,
    pub page_up: Vec<String>,
    pub page_down: Vec<String>,
    pub word_left: Vec<String>,
    pub word_right: Vec<String>,
    pub find: Vec<String>,
    pub find_next: Vec<String>,
    pub find_previous: Vec<String>,
    pub find_toggle_case: Vec<String>,
    pub find_toggle_whole_word: Vec<String>,
    pub find_toggle_regex: Vec<String>,
}

fn chords(specs: &[&str]) -> Vec<String> {
    specs.iter().map(|s| s.to_string()).collect()
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            save: chords(&["ctrl+s"]),
            save_and_check: chords(&["ctrl+shift+s"]),
            quit: chords(&["ctrl+q"]),
            undo: chords(&["ctrl+z"]),
            redo: chords(&["ctrl+y"]),
            select_all: chords(&["ctrl+a"]),
            copy: chords(&["ctrl+c"]),
            cut: chords(&["ctrl+x"]),
            paste: chords(&["ctrl+v"]),
            newline: chords(&["enter"]),
            backspace: chords(&["backspace"]),
            delete: chords(&["delete"]),
            // A real Ctrl+Backspace only reaches us from terminals with the
            // Kitty keyboard protocol; the others send it as Ctrl+H, and
            // Ctrl+W / Alt+Backspace are the classic shell spellings.
            delete_word_backward: chords(&["ctrl+backspace", "alt+backspace", "ctrl+h", "ctrl+w"]),
            delete_word_forward: chords(&["ctrl+delete"]),
            indent: chords(&["tab"]),
            dedent: chords(&["shift+tab"]),
            move_left: chords(&["left"]),
            move_right: chords(&["right"]),
            move_up: chords(&["up"]),
            move_down: chords(&["down"]),
            move_home: chords(&["home"]),
            move_end: chords(&["end"]),
            page_up: chords(&["pageup"]),
            page_down: chords(&["pagedown"]),
            word_left: chords(&["ctrl+left"]),
            word_right: chords(&["ctrl+right"]),
            find: chords(&["ctrl+f"]),
            find_next: chords(&["f3"]),
            find_previous: chords(&["shift+f3"]),
            find_toggle_case: chords(&["alt+c"]),
            find_toggle_whole_word: chords(&["alt+w"]),
            find_toggle_regex: chords(&["alt+r"]),
        }
    }
}

impl KeysConfig {
    /// Every action with its config-file name and its chords, in the order
    /// conflicts are resolved (earlier wins).
    pub fn bindings(&self) -> Vec<(&'static str, Action, &[String])> {
        vec![
            ("save", Action::Save, &self.save),
            ("save_and_check", Action::SaveAndCheck, &self.save_and_check),
            ("quit", Action::Quit, &self.quit),
            ("undo", Action::Undo, &self.undo),
            ("redo", Action::Redo, &self.redo),
            ("select_all", Action::SelectAll, &self.select_all),
            ("copy", Action::Copy, &self.copy),
            ("cut", Action::Cut, &self.cut),
            ("paste", Action::Paste, &self.paste),
            ("newline", Action::Newline, &self.newline),
            ("backspace", Action::Backspace, &self.backspace),
            ("delete", Action::Delete, &self.delete),
            (
                "delete_word_backward",
                Action::DeleteWordBackward,
                &self.delete_word_backward,
            ),
            (
                "delete_word_forward",
                Action::DeleteWordForward,
                &self.delete_word_forward,
            ),
            ("indent", Action::Indent, &self.indent),
            ("dedent", Action::Dedent, &self.dedent),
            ("move_left", Action::MoveLeft, &self.move_left),
            ("move_right", Action::MoveRight, &self.move_right),
            ("move_up", Action::MoveUp, &self.move_up),
            ("move_down", Action::MoveDown, &self.move_down),
            ("move_home", Action::MoveHome, &self.move_home),
            ("move_end", Action::MoveEnd, &self.move_end),
            ("page_up", Action::PageUp, &self.page_up),
            ("page_down", Action::PageDown, &self.page_down),
            ("word_left", Action::WordLeft, &self.word_left),
            ("word_right", Action::WordRight, &self.word_right),
            ("find", Action::Find, &self.find),
            ("find_next", Action::FindNext, &self.find_next),
            ("find_previous", Action::FindPrevious, &self.find_previous),
            (
                "find_toggle_case",
                Action::FindToggleCase,
                &self.find_toggle_case,
            ),
            (
                "find_toggle_whole_word",
                Action::FindToggleWholeWord,
                &self.find_toggle_whole_word,
            ),
            (
                "find_toggle_regex",
                Action::FindToggleRegex,
                &self.find_toggle_regex,
            ),
        ]
        .into_iter()
        .map(|(n, a, v)| (n, a, v.as_slice()))
        .collect()
    }

    pub fn name_of(&self, action: Action) -> &'static str {
        self.bindings()
            .into_iter()
            .find(|(_, a, _)| *a == action)
            .map(|(n, _, _)| n)
            .unwrap_or("?")
    }
}

/// `[languages.<id>]`: any editing option set here applies to that
/// language only, overriding both the global value and the language's
/// built-in default. Everything is optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LanguageOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_width: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indent_style: Option<IndentStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indent_width: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_indent: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smart_enter: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_close_brackets: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_close_quotes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_close_single_quote: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_close_tags: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brace_style: Option<BraceStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_chars: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_ending: Option<LineEnding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert_final_newline: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trim_trailing_whitespace: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlight: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<bool>,
}

// ----------------------------------------------------------------- root

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub editor: EditorConfig,
    pub files: FilesConfig,
    pub display: DisplayConfig,
    pub status_bar: StatusBarConfig,
    pub colors: ColorsConfig,
    pub mouse: MouseConfig,
    pub search: SearchConfig,
    pub syntax: SyntaxConfig,
    pub check: CheckConfig,
    pub tools: ToolsConfig,
    pub keys: KeysConfig,
    /// Extension (no dot) or exact file name -> language id.
    pub associations: BTreeMap<String, String>,
    /// Per-language overrides, keyed by language id.
    pub languages: BTreeMap<String, LanguageOverride>,
}

impl Config {
    /// Problems that don't stop the config from loading but that the user
    /// should hear about: out-of-range numbers, unknown language ids,
    /// unparsable colors and key chords. Out-of-range values are clamped
    /// when settings are resolved.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();

        let range = |w: &mut Vec<String>, name: &str, value: usize, lo: usize, hi: usize| {
            if value < lo || value > hi {
                w.push(format!(
                    "{name} = {value} is out of range {lo}..={hi}; clamped"
                ));
            }
        };
        range(&mut w, "editor.tab_width", self.editor.tab_width, 1, 16);
        if let Some(n) = self.editor.indent_width {
            range(&mut w, "editor.indent_width", n, 1, 16);
        }
        range(&mut w, "editor.scroll_off", self.editor.scroll_off, 0, 50);
        range(
            &mut w,
            "editor.undo_limit",
            self.editor.undo_limit,
            1,
            100_000,
        );
        range(
            &mut w,
            "display.gutter_min_width",
            self.display.gutter_min_width,
            1,
            12,
        );
        if !["2015", "2018", "2021", "2024"].contains(&self.check.rust_edition.trim()) {
            w.push(format!(
                "check.rust_edition = {:?} is not a Rust edition (2015, 2018, 2021 or 2024)",
                self.check.rust_edition
            ));
        }
        let framework = self.check.dotnet_target_framework.trim();
        if !framework.eq_ignore_ascii_case("auto") && !framework.starts_with("net") {
            w.push(format!(
                "check.dotnet_target_framework = {framework:?} should be \"auto\" or a framework like \"net8.0\""
            ));
        }
        range(&mut w, "mouse.scroll_lines", self.mouse.scroll_lines, 1, 50);
        if self.mouse.double_click_ms > 5000 {
            w.push(format!(
                "mouse.double_click_ms = {} is out of range 0..=5000; clamped",
                self.mouse.double_click_ms
            ));
        }
        if self.check.timeout_seconds == 0 {
            w.push("check.timeout_seconds = 0 would time out immediately; using 1".to_string());
        }

        for (id, over) in &self.languages {
            if Language::from_id(id).is_none() {
                w.push(format!(
                    "languages.{id}: unknown language (known: {})",
                    language_ids()
                ));
            }
            if let Some(n) = over.tab_width {
                range(&mut w, &format!("languages.{id}.tab_width"), n, 1, 16);
            }
            if let Some(n) = over.indent_width {
                range(&mut w, &format!("languages.{id}.indent_width"), n, 1, 16);
            }
        }
        for (key, id) in &self.associations {
            if Language::from_id(id).is_none() {
                w.push(format!(
                    "associations.{key:?}: unknown language {id:?} (known: {})",
                    language_ids()
                ));
            }
        }

        w.extend(UiSettings::from_config(self).1);
        w.extend(Keymap::from_config(&self.keys).1);
        w
    }

    /// The language of `path`, honoring `[associations]`.
    pub fn language_of(&self, path: &Path) -> Language {
        Language::from_path_with(path, &self.associations)
    }
}

fn language_ids() -> String {
    crate::language::ALL
        .iter()
        .map(|l| l.id())
        .collect::<Vec<_>>()
        .join(", ")
}

// -------------------------------------------------------------- loading

#[derive(Default)]
pub struct LoadOptions {
    /// `--config FILE`: must exist and parse, or loading fails.
    pub explicit: Option<PathBuf>,
    /// `--no-config`: built-in defaults only.
    pub disabled: bool,
}

#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    /// Every file that contributed, lowest precedence first.
    pub sources: Vec<PathBuf>,
    /// Non-fatal problems, ready to show the user.
    pub warnings: Vec<String>,
}

/// The user-level config file: `$PIS_CODE_CONFIG`, else `config.toml` in
/// the platform's config directory (`~/.config/pis-code` on Linux,
/// `%APPDATA%\pis-code` on Windows, `~/Library/Application
/// Support/pis-code` on macOS).
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(CONFIG_ENV_VAR).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    dirs::config_dir().map(|d| d.join("pis-code").join("config.toml"))
}

/// The nearest `.pis-code.toml` in `file`'s directory or above.
pub fn find_project_config(file: &Path) -> Option<PathBuf> {
    let start = file.parent().filter(|p| !p.as_os_str().is_empty());
    let start = match start {
        Some(p) => std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()),
        None => std::env::current_dir().ok()?,
    };
    start
        .ancestors()
        .map(|dir| dir.join(PROJECT_CONFIG_NAME))
        .find(|candidate| candidate.is_file())
}

/// Recursively merges `over` into `base`: tables merge key by key, any
/// other value (including arrays) replaces.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Reads and validates one config file. The error is just the reason; the
/// caller adds the file name, after the reason, so a narrow status bar
/// clips the path rather than the explanation.
fn read_layer(path: &Path) -> Result<toml::Table, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("can't read it: {e}"))?;
    let table = text
        .parse::<toml::Table>()
        .map_err(|e| one_line(&e.to_string()))?;
    // Reject unknown keys / wrong types here, so the error names this file
    // rather than the merged result.
    table
        .clone()
        .try_into::<Config>()
        .map_err(|e| one_line(&e.to_string()))?;
    Ok(table)
}

/// toml's errors are multi-line (they quote the offending source); the
/// status bar wants one line.
fn one_line(message: &str) -> String {
    let mut parts = message.lines().filter(|l| {
        let t = l.trim();
        !t.is_empty()
            && !t.starts_with('|')
            && !t
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() && t.contains('|'))
    });
    let head = parts.next().unwrap_or(message).trim().to_string();
    let rest: Vec<_> = parts.map(|l| l.trim().to_string()).collect();
    if rest.is_empty() {
        head
    } else {
        format!("{head} ({})", rest.join(" "))
    }
}

/// Loads the layered configuration for editing `file` (`None` when there's
/// no file yet, e.g. for `--print-config`).
pub fn load(options: &LoadOptions, file: Option<&Path>) -> Result<Loaded, String> {
    let mut warnings = Vec::new();
    let mut sources = Vec::new();
    let mut merged = toml::Table::new();

    if !options.disabled {
        let mut paths: Vec<(PathBuf, bool)> = Vec::new(); // (path, must exist)
        match (&options.explicit, user_config_path()) {
            (Some(p), _) => paths.push((p.clone(), true)),
            (None, Some(p)) => paths.push((p, false)),
            (None, None) => {}
        }
        if let Some(project) = file.and_then(find_project_config) {
            if !paths.iter().any(|(p, _)| *p == project) {
                paths.push((project, false));
            }
        }

        for (path, must_exist) in paths {
            if !path.exists() {
                if must_exist {
                    return Err(format!("config file not found: {}", path.display()));
                }
                continue;
            }
            match read_layer(&path) {
                Ok(table) => {
                    merge(&mut merged, table);
                    sources.push(path);
                }
                Err(e) if options.explicit.as_deref() == Some(path.as_path()) => {
                    return Err(format!("{e} in {}", path.display()))
                }
                Err(e) => warnings.push(format!("{e} — file ignored: {}", path.display())),
            }
        }
    }

    // Every layer validated on its own, so the merge can't fail; fall back
    // to defaults rather than panic if that reasoning is ever wrong.
    let config = merged.try_into::<Config>().unwrap_or_else(|e| {
        warnings.push(format!("config ignored — {}", one_line(&e.to_string())));
        Config::default()
    });
    warnings.extend(config.warnings());
    Ok(Loaded {
        config,
        sources,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_default_file_is_exactly_the_built_in_defaults() {
        // default.toml is the documentation users read and what
        // --init-config writes; if it drifts from the code, this fails.
        let parsed: Config = toml::from_str(DEFAULT_CONFIG_TOML)
            .unwrap_or_else(|e| panic!("default.toml doesn't parse: {e}"));
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn the_default_config_produces_no_warnings() {
        assert_eq!(Config::default().warnings(), Vec::<String>::new());
    }

    #[test]
    fn the_effective_config_round_trips_through_toml() {
        // What --print-config emits must load back to the same thing.
        let mut c = Config::default();
        c.editor.indent_width = Some(2);
        c.editor.auto_close_single_quote = Some(false);
        c.languages.insert(
            "python".into(),
            LanguageOverride {
                indent_width: Some(4),
                brace_style: Some(BraceStyle::KAndR),
                ..Default::default()
            },
        );
        c.associations.insert("vue".into(), "html".into());
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(back, c);
    }

    #[test]
    fn an_empty_or_partial_file_fills_in_defaults() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c, Config::default());
        let c: Config = toml::from_str("[editor]\ntab_width = 8\n").unwrap();
        assert_eq!(c.editor.tab_width, 8);
        assert_eq!(c.editor.undo_limit, EditorConfig::default().undo_limit);
        assert_eq!(c.keys, KeysConfig::default());
    }

    #[test]
    fn unknown_keys_are_rejected_so_typos_are_not_silent() {
        let err = toml::from_str::<Config>("[editor]\ntab_widht = 8\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("tab_widht"), "{err}");
        assert!(toml::from_str::<Config>("[nonsense]\nx = 1\n").is_err());
        assert!(toml::from_str::<Config>("[editor]\ntab_width = \"wide\"\n").is_err());
    }

    #[test]
    fn enum_values_parse_from_their_documented_spellings() {
        let c: Config = toml::from_str(
            "[editor]\nindent_style = \"tabs\"\nbrace_style = \"k&r\"\n\
             [files]\nline_ending = \"crlf\"\n\
             [display]\ncursor_style = \"blinking_bar\"\n\
             [status_bar]\nposition = \"top\"\n",
        )
        .unwrap();
        assert_eq!(c.editor.indent_style, IndentStyle::Tabs);
        assert_eq!(c.editor.brace_style, BraceStyle::KAndR);
        assert_eq!(c.files.line_ending, LineEnding::Crlf);
        assert_eq!(c.display.cursor_style, CursorStyle::BlinkingBar);
        assert_eq!(c.status_bar.position, StatusPosition::Top);
        let kr: Config = toml::from_str("[editor]\nbrace_style = \"kr\"\n").unwrap();
        assert_eq!(
            kr.editor.brace_style,
            BraceStyle::KAndR,
            "`kr` is accepted too"
        );
    }

    #[test]
    fn merging_overrides_key_by_key_and_replaces_arrays() {
        let mut base: toml::Table =
            "[editor]\ntab_width = 8\nscroll_off = 3\n[keys]\nsave = [\"f2\"]\n"
                .parse()
                .unwrap();
        let over: toml::Table = "[editor]\ntab_width = 2\n[keys]\nsave = [\"f3\", \"f4\"]\n"
            .parse()
            .unwrap();
        merge(&mut base, over);
        let c: Config = base.try_into().unwrap();
        assert_eq!(c.editor.tab_width, 2, "overridden");
        assert_eq!(c.editor.scroll_off, 3, "untouched sibling survives");
        assert_eq!(
            c.keys.save,
            vec!["f3", "f4"],
            "arrays are replaced, not appended"
        );
    }

    #[test]
    fn warnings_cover_ranges_ids_colors_and_chords() {
        let c: Config = toml::from_str(
            "[editor]\ntab_width = 99\n\
             [colors]\ngutter = \"not-a-color\"\n\
             [keys]\nsave = [\"ctrl+banana\"]\n\
             [associations]\nvue = \"cobol\"\n\
             [languages.klingon]\ntab_width = 2\n",
        )
        .unwrap();
        let w = c.warnings().join("\n");
        for needle in [
            "tab_width = 99",
            "not-a-color",
            "banana",
            "cobol",
            "klingon",
        ] {
            assert!(w.contains(needle), "missing {needle:?} in:\n{w}");
        }
    }

    #[test]
    fn check_settings_are_validated() {
        let c: Config = toml::from_str(
            "[check]\nrust_edition = \"1999\"\ndotnet_target_framework = \"java8\"\ntimeout_seconds = 0\n",
        )
        .unwrap();
        let w = c.warnings().join("\n");
        for needle in ["1999", "java8", "timeout_seconds"] {
            assert!(w.contains(needle), "missing {needle:?} in:\n{w}");
        }
        let ok: Config = toml::from_str(
            "[check]\nrust_edition = \"2024\"\ndotnet_target_framework = \"net8.0\"\n",
        )
        .unwrap();
        assert!(ok.warnings().is_empty(), "{:?}", ok.warnings());
    }

    #[test]
    fn language_of_honors_associations() {
        let mut c = Config::default();
        c.associations.insert("vue".into(), "html".into());
        assert!(c.language_of(Path::new("a.vue")) == Language::Html);
        assert!(c.language_of(Path::new("a.rs")) == Language::Rust);
    }

    // ---- file loading (real files in a temp dir) ----

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pc_cfg_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn explicit_config_must_exist_and_be_valid() {
        let dir = temp_dir("explicit");
        let missing = LoadOptions {
            explicit: Some(dir.join("nope.toml")),
            disabled: false,
        };
        assert!(load(&missing, None).unwrap_err().contains("not found"));

        let bad = dir.join("bad.toml");
        std::fs::write(&bad, "[editor]\ntab_widht = 8\n").unwrap();
        let err = load(
            &LoadOptions {
                explicit: Some(bad),
                disabled: false,
            },
            None,
        )
        .unwrap_err();
        assert!(err.contains("tab_widht"), "{err}");

        let good = dir.join("good.toml");
        std::fs::write(&good, "[editor]\ntab_width = 2\n").unwrap();
        let loaded = load(
            &LoadOptions {
                explicit: Some(good.clone()),
                disabled: false,
            },
            None,
        )
        .unwrap();
        assert_eq!(loaded.config.editor.tab_width, 2);
        assert_eq!(loaded.sources, vec![good]);
        assert!(loaded.warnings.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn project_config_overrides_user_config_and_is_found_in_parent_dirs() {
        let dir = temp_dir("project");
        let user = dir.join("user.toml");
        std::fs::write(&user, "[editor]\ntab_width = 8\nscroll_off = 5\n").unwrap();
        std::fs::write(dir.join(PROJECT_CONFIG_NAME), "[editor]\ntab_width = 2\n").unwrap();
        let nested = dir.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("a.rs");

        let found = find_project_config(&file).expect("found by walking up");
        assert_eq!(found.file_name().unwrap(), PROJECT_CONFIG_NAME);

        // The explicit file stands in for the user layer.
        let opts = LoadOptions {
            explicit: Some(user),
            disabled: false,
        };
        let loaded = load(&opts, Some(&file)).unwrap();
        assert_eq!(loaded.config.editor.tab_width, 2, "project wins");
        assert_eq!(
            loaded.config.editor.scroll_off, 5,
            "user value kept where project is silent"
        );
        assert_eq!(loaded.sources.len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_broken_project_config_is_skipped_with_a_warning_not_fatal() {
        let dir = temp_dir("broken");
        std::fs::write(dir.join(PROJECT_CONFIG_NAME), "[editor\nbroken").unwrap();
        let file = dir.join("a.rs");
        // Use --no-config-style isolation for the user layer by pointing
        // the explicit layer at an empty file.
        let empty = dir.join("empty.toml");
        std::fs::write(&empty, "").unwrap();
        let loaded = load(
            &LoadOptions {
                explicit: Some(empty),
                disabled: false,
            },
            Some(&file),
        )
        .unwrap();
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert!(
            loaded.warnings[0].contains("file ignored"),
            "{:?}",
            loaded.warnings
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_config_ignores_every_file() {
        let dir = temp_dir("disabled");
        std::fs::write(dir.join(PROJECT_CONFIG_NAME), "[editor]\ntab_width = 2\n").unwrap();
        let loaded = load(
            &LoadOptions {
                explicit: None,
                disabled: true,
            },
            Some(&dir.join("a.rs")),
        )
        .unwrap();
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.sources.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
