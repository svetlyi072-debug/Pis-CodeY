//! Turns the raw [`Config`] into values the editor and renderer use
//! directly: per-language editing [`Settings`], and parsed interface
//! styling in [`UiSettings`].

use super::{
    BraceStyle, CaseMode, Config, CursorStyle, IndentStyle, LanguageOverride, LineEnding,
    StatusPosition,
};
use crate::language::{BraceSplit, Language};
use ratatui::style::{Color, Modifier, Style};

/// Mouse handling; the same for every language.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MouseSettings {
    pub enabled: bool,
    /// Lines the wheel scrolls per notch (1-50).
    pub scroll_lines: usize,
    /// Two clicks closer together than this select a word, three a line.
    /// 0 turns multi-click off.
    pub double_click: std::time::Duration,
}

/// How the find bar (Ctrl+F) behaves; the same for every language.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchSettings {
    pub case: CaseMode,
    /// Whether the bar starts in regular-expression mode / whole-word mode.
    pub regex: bool,
    pub whole_word: bool,
    pub wrap_around: bool,
    pub incremental: bool,
    pub select_on_close: bool,
    pub highlight_matches: bool,
}

/// Editing behavior for one file, with every layer already applied. For
/// each option the most specific layer wins:
///
/// 1. `[languages.<id>]` in the config,
/// 2. the global `[editor]`/`[files]` value,
/// 3. the language's built-in default (where it has one).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub tab_width: usize,
    pub indent_style: IndentStyle,
    pub indent_width: Option<usize>,
    pub detect_indentation: bool,
    pub auto_indent: bool,
    pub smart_enter: bool,
    pub auto_close_brackets: bool,
    pub auto_close_quotes: bool,
    pub auto_close_single_quote: bool,
    pub auto_close_tags: bool,
    pub brace_split: BraceSplit,
    pub scroll_off: usize,
    /// 0 = a screenful.
    pub page_size: usize,
    pub word_chars: String,
    pub undo_limit: usize,
    pub line_ending: LineEnding,
    pub insert_final_newline: bool,
    pub trim_trailing_whitespace: bool,
    pub highlight: bool,
    pub check: bool,
    pub search: SearchSettings,
    pub mouse: MouseSettings,
}

impl Settings {
    /// Built-in behavior for `language`, as if the config were empty.
    #[cfg(test)]
    pub fn for_language(language: Language) -> Self {
        Self::resolve(&Config::default(), language)
    }

    pub fn resolve(config: &Config, language: Language) -> Self {
        let e = &config.editor;
        let f = &config.files;
        let none = LanguageOverride::default();
        let o = config.languages.get(language.id()).unwrap_or(&none);

        let brace_style = o.brace_style.unwrap_or(e.brace_style);
        Settings {
            tab_width: o.tab_width.unwrap_or(e.tab_width).clamp(1, 16),
            indent_style: o.indent_style.unwrap_or(e.indent_style),
            indent_width: o.indent_width.or(e.indent_width).map(|n| n.clamp(1, 16)),
            detect_indentation: e.detect_indentation,
            auto_indent: o.auto_indent.unwrap_or(e.auto_indent),
            smart_enter: o.smart_enter.unwrap_or(e.smart_enter),
            auto_close_brackets: o.auto_close_brackets.unwrap_or(e.auto_close_brackets),
            auto_close_quotes: o.auto_close_quotes.unwrap_or(e.auto_close_quotes),
            auto_close_single_quote: o
                .auto_close_single_quote
                .or(e.auto_close_single_quote)
                .unwrap_or_else(|| language.autocloses_single_quote()),
            // Closing a tag only makes sense where there are tags, so the
            // setting can switch it off but not on for, say, Rust.
            auto_close_tags: o.auto_close_tags.unwrap_or(e.auto_close_tags) && language.has_tags(),
            brace_split: match brace_style {
                BraceStyle::Auto => language.brace_split(),
                BraceStyle::Allman => BraceSplit::Allman,
                BraceStyle::KAndR => BraceSplit::KAndR,
                BraceStyle::Off => BraceSplit::None,
            },
            scroll_off: e.scroll_off.min(50),
            page_size: e.page_size,
            word_chars: o.word_chars.clone().unwrap_or_else(|| e.word_chars.clone()),
            undo_limit: e.undo_limit.clamp(1, 100_000),
            line_ending: o.line_ending.unwrap_or(f.line_ending),
            insert_final_newline: o.insert_final_newline.unwrap_or(f.insert_final_newline),
            trim_trailing_whitespace: o
                .trim_trailing_whitespace
                .unwrap_or(f.trim_trailing_whitespace),
            highlight: o.highlight.unwrap_or(true) && config.syntax.enabled,
            check: o.check.unwrap_or(true) && config.check.enabled && language.has_checker(),
            mouse: MouseSettings {
                enabled: config.mouse.enabled,
                scroll_lines: config.mouse.scroll_lines.clamp(1, 50),
                double_click: std::time::Duration::from_millis(
                    config.mouse.double_click_ms.min(5000),
                ),
            },
            search: SearchSettings {
                case: config.search.case,
                regex: config.search.regex,
                whole_word: config.search.whole_word,
                wrap_around: config.search.wrap_around,
                incremental: config.search.incremental,
                select_on_close: config.search.select_on_close,
                highlight_matches: config.search.highlight_matches,
            },
        }
    }
}

/// How the status-bar hint line is chosen.
#[derive(Debug, Clone, PartialEq)]
pub enum HintSetting {
    /// Built from the real key bindings.
    Auto,
    Off,
    Text(String),
}

/// How selected text is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectionStyle {
    Reverse,
    Underline,
    Bold,
    Background(Color),
}

impl SelectionStyle {
    pub fn apply(self, style: Style) -> Style {
        match self {
            SelectionStyle::Reverse => style.add_modifier(Modifier::REVERSED),
            SelectionStyle::Underline => style.add_modifier(Modifier::UNDERLINED),
            SelectionStyle::Bold => style.add_modifier(Modifier::BOLD),
            SelectionStyle::Background(c) => style.bg(c),
        }
    }
}

/// Everything the renderer needs from the config, with colors parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct UiSettings {
    pub line_numbers: bool,
    pub relative_line_numbers: bool,
    pub gutter_min_width: usize,
    pub cursor_style: CursorStyle,

    pub status_position: StatusPosition,
    pub show_file_name: bool,
    pub show_modified_marker: bool,
    pub show_cursor_position: bool,
    pub show_char_count: bool,
    pub show_line_count: bool,
    pub hint: HintSetting,

    pub gutter: Color,
    pub gutter_current_line: Color,
    pub gutter_error: Color,
    pub gutter_warning: Color,
    pub diagnostic_error: Color,
    pub diagnostic_warning: Color,
    pub status_fg: Color,
    pub status_bg: Color,
    pub status_error_fg: Color,
    pub status_error_bg: Color,
    pub selection: SelectionStyle,
    pub search_match: Color,
    pub search_current: Color,
    pub search_current_text: Color,

    pub mark_gutter: bool,
    pub underline_diagnostics: bool,
}

/// Parses a color: a name (`red`, `lightblue`, `darkgray`, ...), `#rrggbb`,
/// a 0-255 palette index, or `default` for the terminal's own color.
pub fn parse_color(text: &str) -> Result<Color, String> {
    let t = text.trim().to_lowercase().replace(['-', '_', ' '], "");
    if t == "default" || t == "none" {
        return Ok(Color::Reset);
    }
    t.parse::<Color>().map_err(|_| {
        format!(
            "{text:?} is not a color (use a name like \"red\", \"#rrggbb\", 0-255, or \"default\")"
        )
    })
}

impl UiSettings {
    pub fn from_config(config: &Config) -> (UiSettings, Vec<String>) {
        let mut warnings = Vec::new();
        let defaults = super::ColorsConfig::default();
        let mut color = |name: &str, value: &str, fallback: &str| -> Color {
            parse_color(value).unwrap_or_else(|why| {
                warnings.push(format!("colors.{name}: {why}"));
                parse_color(fallback).expect("built-in default colors parse")
            })
        };
        let c = &config.colors;
        let gutter = color("gutter", &c.gutter, &defaults.gutter);
        let gutter_current_line = color(
            "gutter_current_line",
            &c.gutter_current_line,
            &defaults.gutter_current_line,
        );
        let gutter_error = color("gutter_error", &c.gutter_error, &defaults.gutter_error);
        let gutter_warning = color(
            "gutter_warning",
            &c.gutter_warning,
            &defaults.gutter_warning,
        );
        let diagnostic_error = color(
            "diagnostic_error",
            &c.diagnostic_error,
            &defaults.diagnostic_error,
        );
        let diagnostic_warning = color(
            "diagnostic_warning",
            &c.diagnostic_warning,
            &defaults.diagnostic_warning,
        );
        let status_fg = color("status_fg", &c.status_fg, &defaults.status_fg);
        let status_bg = color("status_bg", &c.status_bg, &defaults.status_bg);
        let status_error_fg = color(
            "status_error_fg",
            &c.status_error_fg,
            &defaults.status_error_fg,
        );
        let status_error_bg = color(
            "status_error_bg",
            &c.status_error_bg,
            &defaults.status_error_bg,
        );

        let search_match = color("search_match", &c.search_match, &defaults.search_match);
        let search_current = color(
            "search_current",
            &c.search_current,
            &defaults.search_current,
        );
        let search_current_text = color(
            "search_current_text",
            &c.search_current_text,
            &defaults.search_current_text,
        );

        let selection = match c.selection.trim().to_lowercase().as_str() {
            "reverse" => SelectionStyle::Reverse,
            "underline" => SelectionStyle::Underline,
            "bold" => SelectionStyle::Bold,
            other => match parse_color(other) {
                Ok(bg) => SelectionStyle::Background(bg),
                Err(why) => {
                    warnings.push(format!(
                        "colors.selection: {why}; also accepts \"reverse\", \"underline\", \"bold\""
                    ));
                    SelectionStyle::Reverse
                }
            },
        };

        let hint = match config.status_bar.hint.as_str() {
            h if h.eq_ignore_ascii_case("auto") => HintSetting::Auto,
            "" => HintSetting::Off,
            text => HintSetting::Text(text.to_string()),
        };

        let s = &config.status_bar;
        let d = &config.display;
        (
            UiSettings {
                line_numbers: d.line_numbers,
                relative_line_numbers: d.relative_line_numbers,
                gutter_min_width: d.gutter_min_width.clamp(1, 12),
                cursor_style: d.cursor_style,
                status_position: s.position,
                show_file_name: s.show_file_name,
                show_modified_marker: s.show_modified_marker,
                show_cursor_position: s.show_cursor_position,
                show_char_count: s.show_char_count,
                show_line_count: s.show_line_count,
                hint,
                gutter,
                gutter_current_line,
                gutter_error,
                gutter_warning,
                diagnostic_error,
                diagnostic_warning,
                status_fg,
                status_bg,
                status_error_fg,
                status_error_bg,
                selection,
                search_match,
                search_current,
                search_current_text,
                mark_gutter: config.check.mark_gutter,
                underline_diagnostics: config.check.underline,
            },
            warnings,
        )
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings::from_config(&Config::default()).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LanguageOverride;

    fn lang_cfg(id: &str, over: LanguageOverride) -> Config {
        let mut c = Config::default();
        c.languages.insert(id.to_string(), over);
        c
    }

    #[test]
    fn built_in_behavior_matches_what_the_editor_always_did() {
        // The defaults must reproduce the previously hard-coded behavior.
        let cs = Settings::for_language(Language::CSharp);
        assert!(cs.brace_split == BraceSplit::Allman);
        assert!(cs.auto_close_single_quote && cs.auto_close_brackets && cs.smart_enter);
        assert!(!cs.auto_close_tags, "tags only auto-close in HTML/XML");
        assert_eq!(cs.tab_width, 4);

        assert!(Settings::for_language(Language::Json).brace_split == BraceSplit::KAndR);
        assert!(Settings::for_language(Language::Python).brace_split == BraceSplit::None);
        assert!(!Settings::for_language(Language::Rust).auto_close_single_quote);
        assert!(!Settings::for_language(Language::Markdown).auto_close_single_quote);
        assert!(Settings::for_language(Language::Html).auto_close_tags);
        assert!(Settings::for_language(Language::Xml).auto_close_tags);
        assert!(Settings::for_language(Language::Rust).check);
        assert!(
            !Settings::for_language(Language::Css).check,
            "no checker for CSS"
        );
    }

    #[test]
    fn language_override_beats_global_beats_builtin() {
        // built-in: Rust doesn't autoclose '
        assert!(!Settings::for_language(Language::Rust).auto_close_single_quote);

        // global setting beats the built-in default...
        let mut global = Config::default();
        global.editor.auto_close_single_quote = Some(true);
        assert!(Settings::resolve(&global, Language::Rust).auto_close_single_quote);

        // ...and the per-language override beats the global setting.
        let mut both = global.clone();
        both.languages.insert(
            "rust".into(),
            LanguageOverride {
                auto_close_single_quote: Some(false),
                ..Default::default()
            },
        );
        assert!(!Settings::resolve(&both, Language::Rust).auto_close_single_quote);
        assert!(
            Settings::resolve(&both, Language::Python).auto_close_single_quote,
            "other languages keep the global value"
        );
    }

    #[test]
    fn brace_style_override_forces_a_split_style_or_turns_it_off() {
        let mut c = Config::default();
        c.editor.brace_style = BraceStyle::KAndR;
        assert!(Settings::resolve(&c, Language::CSharp).brace_split == BraceSplit::KAndR);
        c.editor.brace_style = BraceStyle::Off;
        assert!(Settings::resolve(&c, Language::CSharp).brace_split == BraceSplit::None);

        let c = lang_cfg(
            "rust",
            LanguageOverride {
                brace_style: Some(BraceStyle::KAndR),
                ..Default::default()
            },
        );
        assert!(Settings::resolve(&c, Language::Rust).brace_split == BraceSplit::KAndR);
        assert!(Settings::resolve(&c, Language::CSharp).brace_split == BraceSplit::Allman);
    }

    #[test]
    fn tag_autoclose_can_be_disabled_but_not_forced_onto_other_languages() {
        let c = lang_cfg(
            "html",
            LanguageOverride {
                auto_close_tags: Some(false),
                ..Default::default()
            },
        );
        assert!(!Settings::resolve(&c, Language::Html).auto_close_tags);
        assert!(Settings::resolve(&c, Language::Xml).auto_close_tags);

        let c = lang_cfg(
            "rust",
            LanguageOverride {
                auto_close_tags: Some(true),
                ..Default::default()
            },
        );
        assert!(!Settings::resolve(&c, Language::Rust).auto_close_tags);
    }

    #[test]
    fn global_switches_gate_highlighting_and_checking() {
        let mut c = Config::default();
        c.syntax.enabled = false;
        c.check.enabled = false;
        let s = Settings::resolve(&c, Language::Rust);
        assert!(!s.highlight && !s.check);

        let c = lang_cfg(
            "python",
            LanguageOverride {
                highlight: Some(false),
                check: Some(false),
                ..Default::default()
            },
        );
        assert!(!Settings::resolve(&c, Language::Python).highlight);
        assert!(!Settings::resolve(&c, Language::Python).check);
        assert!(Settings::resolve(&c, Language::Rust).highlight);
    }

    #[test]
    fn search_settings_and_colors_come_from_the_config() {
        let mut c = Config::default();
        c.search.case = CaseMode::Sensitive;
        c.search.wrap_around = false;
        c.search.regex = true;
        c.colors.search_match = "#112233".into();
        c.colors.search_current = "red".into();
        c.colors.search_current_text = "bad-color".into();
        let s = Settings::resolve(&c, Language::Rust);
        assert_eq!(s.search.case, CaseMode::Sensitive);
        assert!(!s.search.wrap_around && s.search.regex && s.search.incremental);
        let (ui, warnings) = UiSettings::from_config(&c);
        assert_eq!(ui.search_match, Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(ui.search_current, Color::Red);
        assert_eq!(
            ui.search_current_text,
            Color::Black,
            "fell back to the default"
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("colors.search_current_text"));
    }

    #[test]
    fn mouse_settings_are_clamped() {
        let mut c = Config::default();
        c.mouse.scroll_lines = 0;
        c.mouse.double_click_ms = 999_999;
        let s = Settings::resolve(&c, Language::Rust).mouse;
        assert_eq!(s.scroll_lines, 1);
        assert_eq!(s.double_click, std::time::Duration::from_millis(5000));
        assert!(
            Settings::resolve(&Config::default(), Language::Rust)
                .mouse
                .enabled
        );
        assert_eq!(c.warnings().len(), 2, "{:?}", c.warnings());
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let mut c = Config::default();
        c.editor.tab_width = 0;
        c.editor.indent_width = Some(500);
        c.editor.scroll_off = 9999;
        c.editor.undo_limit = 0;
        let s = Settings::resolve(&c, Language::Rust);
        assert_eq!(
            (s.tab_width, s.indent_width, s.scroll_off, s.undo_limit),
            (1, Some(16), 50, 1)
        );
    }

    #[test]
    fn colors_parse_names_hex_indexes_and_default() {
        assert_eq!(parse_color("red"), Ok(Color::Red));
        assert_eq!(parse_color("Light-Blue"), Ok(Color::LightBlue));
        assert_eq!(parse_color("dark_gray"), Ok(Color::DarkGray));
        assert_eq!(parse_color("#7aa2f7"), Ok(Color::Rgb(0x7a, 0xa2, 0xf7)));
        assert_eq!(parse_color("42"), Ok(Color::Indexed(42)));
        assert_eq!(parse_color("default"), Ok(Color::Reset));
        assert!(parse_color("not-a-color")
            .unwrap_err()
            .contains("not-a-color"));
        assert!(parse_color("#12345").is_err());
    }

    #[test]
    fn default_ui_matches_the_previously_hard_coded_look() {
        let (ui, warnings) = UiSettings::from_config(&Config::default());
        assert!(warnings.is_empty());
        assert_eq!(ui.gutter, Color::DarkGray);
        assert_eq!(ui.gutter_error, Color::Red);
        assert_eq!(ui.gutter_warning, Color::Yellow);
        assert_eq!(ui.status_bg, Color::Rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(ui.status_error_bg, Color::Rgb(0x8b, 0x2f, 0x2f));
        assert_eq!(ui.status_fg, Color::Black);
        assert_eq!(ui.status_error_fg, Color::White);
        assert_eq!(ui.selection, SelectionStyle::Reverse);
        assert_eq!(ui.hint, HintSetting::Auto);
        assert!(ui.line_numbers && !ui.relative_line_numbers);
        assert_eq!(ui.gutter_min_width, 4);
    }

    #[test]
    fn a_bad_color_warns_and_falls_back_instead_of_failing() {
        let mut c = Config::default();
        c.colors.status_bg = "chartreuse-ish".into();
        c.colors.selection = "nope".into();
        let (ui, warnings) = UiSettings::from_config(&c);
        assert_eq!(
            ui.status_bg,
            Color::Rgb(0x7a, 0xa2, 0xf7),
            "fell back to the default"
        );
        assert_eq!(ui.selection, SelectionStyle::Reverse);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].starts_with("colors.status_bg"));
        assert!(warnings[1].starts_with("colors.selection"));
    }

    #[test]
    fn selection_can_be_a_background_color_or_a_text_attribute() {
        let mut c = Config::default();
        c.colors.selection = "#264f78".into();
        let (ui, _) = UiSettings::from_config(&c);
        assert_eq!(
            ui.selection,
            SelectionStyle::Background(Color::Rgb(0x26, 0x4f, 0x78))
        );
        assert_eq!(
            ui.selection.apply(Style::default()).bg,
            Some(Color::Rgb(0x26, 0x4f, 0x78))
        );
        assert!(SelectionStyle::Reverse
            .apply(Style::default())
            .add_modifier
            .contains(Modifier::REVERSED));
        assert!(SelectionStyle::Underline
            .apply(Style::default())
            .add_modifier
            .contains(Modifier::UNDERLINED));
    }

    #[test]
    fn hint_setting_has_three_modes() {
        let mut c = Config::default();
        c.status_bar.hint = "AUTO".into();
        assert_eq!(UiSettings::from_config(&c).0.hint, HintSetting::Auto);
        c.status_bar.hint = "".into();
        assert_eq!(UiSettings::from_config(&c).0.hint, HintSetting::Off);
        c.status_bar.hint = "F1 for help".into();
        assert_eq!(
            UiSettings::from_config(&c).0.hint,
            HintSetting::Text("F1 for help".into())
        );
    }
}
