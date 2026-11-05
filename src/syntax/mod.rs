use crate::language::Language;
use ratatui::style::{Color, Modifier, Style as RtStyle};
use std::path::Path;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SynStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxDefinition, SyntaxReference, SyntaxSet, SyntaxSetBuilder};

/// The theme used when `syntax.theme` names none that exists.
const DEFAULT_THEME: &str = "base16-ocean.dark";

/// Syntax highlighting for the languages Pis Code knows (C#, JavaScript,
/// Rust, Python, GDScript, HTML, ...). Any other file type — and every
/// file when highlighting is switched off — is deliberately left unstyled
/// so its text renders in the terminal's own configured colors instead of
/// a fixed theme foreground.
pub struct Highlighter {
    /// `None` means plain: no styling at all.
    backend: Option<SyntectBackend>,
}

struct SyntectBackend {
    syntax_set: SyntaxSet,
    theme: Theme,
    syntax: SyntaxReference,
}

/// Names of the color themes that ship with the editor.
pub fn theme_names() -> Vec<String> {
    let mut names: Vec<String> = ThemeSet::load_defaults().themes.into_keys().collect();
    names.sort();
    names
}

/// Finds `name` among the built-in themes, or loads it as a `.tmTheme`
/// file when it looks like a path. On failure returns a message and the
/// default theme.
fn load_theme(name: &str) -> (Theme, Option<String>) {
    let mut themes = ThemeSet::load_defaults().themes;
    let name = name.trim();
    if let Some(theme) = themes.remove(name) {
        return (theme, None);
    }
    // Built-in names are matched case-insensitively as a convenience.
    if let Some(key) = themes
        .keys()
        .find(|k| k.eq_ignore_ascii_case(name))
        .cloned()
    {
        return (themes.remove(&key).expect("key just found"), None);
    }
    let fallback = themes.remove(DEFAULT_THEME).unwrap_or_else(|| {
        ThemeSet::load_defaults()
            .themes
            .into_values()
            .next()
            .unwrap()
    });
    let looks_like_file = name.ends_with(".tmTheme") || Path::new(name).is_file();
    if looks_like_file {
        return match ThemeSet::get_theme(name) {
            Ok(theme) => (theme, None),
            Err(e) => (
                fallback,
                Some(format!(
                    "syntax.theme: can't load {name:?}: {e}; using {DEFAULT_THEME}"
                )),
            ),
        };
    }
    (
        fallback,
        Some(format!(
            "syntax.theme: unknown theme {name:?} (built in: {}; or a path to a .tmTheme file); using {DEFAULT_THEME}",
            theme_names().join(", ")
        )),
    )
}

impl Highlighter {
    /// Highlighter for `language` in the named `theme`. With `enabled`
    /// false the text is left unstyled. The second value is a warning when
    /// the theme couldn't be found or loaded.
    pub fn new(language: Language, enabled: bool, theme: &str) -> (Self, Option<String>) {
        if !enabled {
            return (Self { backend: None }, None);
        }
        let (theme, warning) = load_theme(theme);
        let backend = match language {
            // syntect's bundled grammars have no GDScript, so it ships its
            // own (gdscript.sublime-syntax) in a set of its own.
            Language::GdScript => Some(SyntectBackend::gdscript(theme)),
            // Likewise for the C-family languages built from one template.
            l if clike_spec(l).is_some() => {
                clike_spec(l).map(|spec| SyntectBackend::clike(spec, theme))
            }
            // `None` for the types left unstyled (see the struct docs).
            other => other
                .bundled_syntax_extension()
                .map(|ext| SyntectBackend::bundled(ext, theme)),
        };
        (Self { backend }, warning)
    }

    /// Highlight `lines[0..upto]` from the start of the buffer so
    /// multi-line constructs (block comments, verbatim strings) stay
    /// consistent, and return styled spans per line.
    pub fn highlight(&self, lines: &[String], upto: usize) -> Vec<Vec<(RtStyle, String)>> {
        let upto = upto.min(lines.len());

        let Some(backend) = &self.backend else {
            return lines[..upto]
                .iter()
                .map(|line| {
                    if line.is_empty() {
                        Vec::new()
                    } else {
                        vec![(RtStyle::default(), line.clone())]
                    }
                })
                .collect();
        };

        let mut h = HighlightLines::new(&backend.syntax, &backend.theme);
        let mut out = Vec::with_capacity(upto);
        for line in &lines[..upto] {
            let with_nl = format!("{line}\n");
            let ranges: Vec<(SynStyle, &str)> = h
                .highlight_line(&with_nl, &backend.syntax_set)
                .unwrap_or_default();
            let spans = ranges
                .into_iter()
                .map(|(style, text)| {
                    (
                        to_ratatui_style(style),
                        text.trim_end_matches('\n').to_string(),
                    )
                })
                .filter(|(_, text)| !text.is_empty())
                .collect();
            out.push(spans);
        }
        out
    }
}

/// What differs between the C-family languages that share the
/// `clike.sublime-syntax` template: the file extension and the keyword
/// groups, each a `|`-joined list of whole words.
struct CLikeSpec {
    name: &'static str,
    extension: &'static str,
    /// The keyword that introduces a function (`fun`, `func`, `function`),
    /// if the language has one.
    function_keyword: Option<&'static str>,
    type_keywords: &'static str,
    /// `var`, `let`, `val`, `import`, ...
    storage: &'static str,
    modifiers: &'static str,
    control: &'static str,
    operator_words: &'static str,
    self_names: &'static str,
    constants: &'static str,
    /// Built-in types written in lowercase (the capitalized ones are
    /// picked up as class names anyway).
    builtin_types: &'static str,
}

const TYPESCRIPT: CLikeSpec = CLikeSpec {
    name: "TypeScript",
    extension: "ts",
    function_keyword: Some("function"),
    type_keywords: "class|interface|enum|type|namespace|module",
    storage: "var|let|const|import|export|from|declare|default",
    modifiers: "public|private|protected|static|readonly|abstract|async|override|get|set",
    control: "if|else|for|while|do|switch|case|break|continue|return|try|catch|finally|throw|await|yield|with",
    operator_words: "in|of|instanceof|typeof|keyof|is|as|new|delete|void|extends|implements|infer|satisfies",
    self_names: "this|super",
    constants: "true|false|null|undefined|NaN|Infinity",
    builtin_types: "string|number|boolean|any|unknown|never|object|symbol|bigint",
};

const KOTLIN: CLikeSpec = CLikeSpec {
    name: "Kotlin",
    extension: "kt",
    function_keyword: Some("fun"),
    type_keywords: "class|interface|object|typealias",
    storage: "val|var|import|package",
    modifiers: "public|private|protected|internal|open|abstract|final|override|sealed|data|inner|enum|companion|inline|noinline|crossinline|reified|suspend|operator|infix|tailrec|lateinit|const|vararg|external|expect|actual|annotation|by|init|constructor|get|set",
    control: "if|else|when|for|while|do|try|catch|finally|throw|return|break|continue",
    operator_words: "in|is|as",
    self_names: "this|super",
    constants: "true|false|null",
    builtin_types: "",
};

const SWIFT: CLikeSpec = CLikeSpec {
    name: "Swift",
    extension: "swift",
    function_keyword: Some("func"),
    type_keywords: "class|struct|enum|protocol|extension|actor|typealias|associatedtype",
    storage: "let|var|import|init|deinit|subscript|operator|precedencegroup",
    modifiers: "public|private|fileprivate|internal|open|final|static|override|mutating|nonmutating|lazy|weak|unowned|convenience|required|dynamic|inout|async|throws|rethrows|indirect|optional|some|any",
    control: "if|else|guard|switch|case|default|for|while|repeat|do|try|catch|throw|return|break|continue|fallthrough|defer|where|await",
    operator_words: "in|is|as",
    self_names: "self|Self|super",
    constants: "true|false|nil",
    builtin_types: "",
};

const DART: CLikeSpec = CLikeSpec {
    name: "Dart",
    extension: "dart",
    function_keyword: None,
    type_keywords: "class|enum|mixin|extension|typedef",
    storage: "var|final|const|late|import|export|library|part",
    modifiers: "abstract|static|external|factory|covariant|required|async|sync|get|set|operator|implements|extends|with|on",
    control: "if|else|for|while|do|switch|case|default|break|continue|return|try|catch|finally|throw|rethrow|await|yield|assert",
    operator_words: "in|is|as|new",
    self_names: "this|super",
    constants: "true|false|null",
    builtin_types: "int|double|num|bool|void|dynamic",
};

/// The table entry for a language that uses the template.
fn clike_spec(language: Language) -> Option<&'static CLikeSpec> {
    match language {
        Language::TypeScript => Some(&TYPESCRIPT),
        Language::Kotlin => Some(&KOTLIN),
        Language::Swift => Some(&SWIFT),
        Language::Dart => Some(&DART),
        _ => None,
    }
}

/// An alternation that can never match, for keyword groups a language
/// doesn't have (an empty `(?:)` would match everywhere).
const NEVER: &str = "(?!)";

/// Fills in the template. The keyword lists are regex alternations as they
/// stand; an empty one becomes a pattern that never matches.
fn clike_grammar(spec: &CLikeSpec) -> String {
    let words = |list: &str| {
        if list.is_empty() {
            NEVER.to_string()
        } else {
            list.to_string()
        }
    };
    let scope = spec.name.to_lowercase();
    include_str!("clike.sublime-syntax")
        .replace("@@NAME@@", spec.name)
        .replace("@@EXT@@", spec.extension)
        .replace("@@SCOPE@@", &scope)
        .replace("@@FUNC_KW@@", spec.function_keyword.unwrap_or(NEVER))
        .replace("@@CLASS_KW@@", &words(spec.type_keywords))
        .replace("@@STORAGE@@", &words(spec.storage))
        .replace("@@MODIFIERS@@", &words(spec.modifiers))
        .replace("@@CONTROL@@", &words(spec.control))
        .replace("@@OPERATOR_WORDS@@", &words(spec.operator_words))
        .replace("@@SELF@@", &words(spec.self_names))
        .replace("@@CONSTANTS@@", &words(spec.constants))
        .replace("@@BUILTIN_TYPES@@", &words(spec.builtin_types))
}

impl SyntectBackend {
    fn bundled(extension: &str, theme: Theme) -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let syntax = syntax_set
            .find_syntax_by_extension(extension)
            .cloned()
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text().clone());
        Self {
            syntax_set,
            theme,
            syntax,
        }
    }

    fn clike(spec: &CLikeSpec, theme: Theme) -> Self {
        let definition = SyntaxDefinition::load_from_str(&clike_grammar(spec), true, None)
            .unwrap_or_else(|e| panic!("bundled {} grammar is valid: {e}", spec.name));
        let mut builder = SyntaxSetBuilder::new();
        builder.add(definition);
        let syntax_set = builder.build();
        let syntax = syntax_set
            .find_syntax_by_extension(spec.extension)
            .cloned()
            .expect("the grammar registers its own extension");
        Self {
            syntax_set,
            theme,
            syntax,
        }
    }

    fn gdscript(theme: Theme) -> Self {
        let definition =
            SyntaxDefinition::load_from_str(include_str!("gdscript.sublime-syntax"), true, None)
                .expect("bundled GDScript grammar is valid");
        let mut builder = SyntaxSetBuilder::new();
        builder.add(definition);
        let syntax_set = builder.build();
        let syntax = syntax_set
            .find_syntax_by_extension("gd")
            .cloned()
            .expect("GDScript grammar registers the .gd extension");
        Self {
            syntax_set,
            theme,
            syntax,
        }
    }
}

fn to_ratatui_style(style: SynStyle) -> RtStyle {
    let fg = style.foreground;
    let mut rt = RtStyle::default().fg(Color::Rgb(fg.r, fg.g, fg.b));
    if style.font_style.contains(FontStyle::BOLD) {
        rt = rt.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        rt = rt.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        rt = rt.add_modifier(Modifier::UNDERLINED);
    }
    rt
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans_for(path: &str, source: &str) -> Vec<Vec<(RtStyle, String)>> {
        let lines: Vec<String> = source.lines().map(String::from).collect();
        highlighter_for(path).highlight(&lines, lines.len())
    }

    fn highlighter_for(path: &str) -> Highlighter {
        let language = Language::from_path(Path::new(path));
        Highlighter::new(language, true, DEFAULT_THEME).0
    }

    /// The foreground color of the first span whose text, ignoring the
    /// whitespace syntect merges into neighboring same-colored spans, is
    /// exactly `text`.
    fn fg_of(row: &[(RtStyle, String)], text: &str) -> Option<Color> {
        row.iter()
            .find(|(_, t)| t.trim() == text)
            .and_then(|(s, _)| s.fg)
    }

    #[test]
    fn unsupported_file_types_are_unstyled_so_terminal_colors_apply() {
        for path in [
            "notes.txt",
            "data.csv",
            "config.toml",
            "main.zig",
            "Makefile",
        ] {
            let rows = spans_for(path, "hello world 123 \"quoted\"");
            assert_eq!(rows.len(), 1, "{path}");
            for (style, _) in &rows[0] {
                assert_eq!(*style, RtStyle::default(), "{path} must not set colors");
            }
            let joined: String = rows[0].iter().map(|(_, t)| t.as_str()).collect();
            assert_eq!(joined, "hello world 123 \"quoted\"", "{path}");
        }
    }

    #[test]
    fn supported_languages_are_still_colored() {
        for (path, source) in [
            ("a.cs", "using System;"),
            ("a.js", "const x = 1;"),
            ("a.rs", "fn main() {}"),
            ("a.py", "def f(): pass"),
            ("a.gd", "func _ready():"),
        ] {
            let rows = spans_for(path, source);
            assert!(
                rows[0].iter().any(|(s, _)| s.fg.is_some()),
                "{path} should have colored spans"
            );
        }
    }

    /// How many different colors a highlighted row uses. A grammar that
    /// really loaded colors tokens differently; the plain-text fallback
    /// paints everything one color.
    fn distinct_colors(row: &[(RtStyle, String)]) -> usize {
        let mut colors: Vec<_> = row.iter().filter_map(|(s, _)| s.fg).collect();
        colors.dedup();
        colors.sort_by_key(|c| format!("{c:?}"));
        colors.dedup();
        colors.len()
    }

    #[test]
    fn web_and_markup_languages_have_real_grammars() {
        for (path, source) in [
            ("a.html", "<div class=\"a\">x</div>"),
            ("a.css", "a { color: red; }"),
            ("a.json", "{\"a\": 1, \"b\": \"x\"}"),
            ("a.xml", "<a b=\"c\">d</a>"),
            ("a.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>"),
            ("a.yaml", "key: \"v\" # c"),
            ("a.md", "# Title"),
        ] {
            let rows = spans_for(path, source);
            assert!(
                distinct_colors(&rows[0]) >= 2,
                "{path} fell back to plain text: {:?}",
                rows[0]
            );
        }
    }

    #[test]
    fn gdscript_grammar_loads_and_every_context_compiles() {
        // Touches every context in the grammar (comments, all four string
        // kinds + escapes + placeholders, annotations, declarations,
        // keywords, constants, every number form, node paths, types,
        // calls, operators), so a bad regex anywhere surfaces here rather
        // than as a panic while someone is typing.
        let source = r#"@tool
@export_range(0, 10) var speed: float = 1.5e3
class_name Player extends CharacterBody2D
signal died(reason)
enum State { IDLE, RUN }
const MAX_HP := 0xFF
var mask = 0b1010 + .5 + 1_000
# a comment
func _ready() -> void:
	var s = "esc\n %s %d" % ["a", 1]
	var t = 'single \' quote'
	var n = &"name"
	var raw = r"C:\raw"
	var path = $"Sprite/Child"
	var label = %Unique
	var body = $Body/Collision
	var v = Vector2(1, 2) * 3
	if x and not y or z in w:
		return null
	"""
	triple
	"""
	'''
	triple single
	'''
"#;
        let rows = spans_for("player.gd", source);
        assert!(rows.len() > 20);
    }

    #[test]
    fn gdscript_scopes_get_distinct_colors() {
        let rows = spans_for(
            "a.gd",
            "func _ready():\n\tvar speed = 10\n\treturn \"s\" # c\nextends Node2D\n@export var x\nsignal hit\nvar y = true",
        );
        let default = fg_of(&rows[1], "speed").expect("plain identifier has a color");

        // Every one of these should differ from plain-identifier color.
        for (row, text, what) in [
            (0, "func", "func keyword"),
            (0, "_ready", "function name"),
            (1, "var", "var keyword"),
            (1, "10", "number"),
            (2, "return", "control keyword"),
            (2, "s", "string contents"),
            (3, "extends", "extends"),
            (3, "Node2D", "class name"),
            (4, "@export", "annotation"),
            (5, "signal", "signal keyword"),
            (6, "true", "constant"),
        ] {
            let c = fg_of(&rows[row], text)
                .unwrap_or_else(|| panic!("no span for {what}: {text:?} in {:?}", rows[row]));
            assert_ne!(
                c, default,
                "{what} ({text:?}) should not be the plain color"
            );
        }
        // Comments get their own color too.
        assert!(rows[2]
            .iter()
            .any(|(s, t)| t.contains("# c") && s.fg.is_some() && s.fg != Some(default)));
    }

    #[test]
    fn gdscript_comment_hides_keywords_and_strings_hide_comments() {
        let rows = spans_for("a.gd", "# func var if\nvar s = \"# not a comment\"");

        // The commented-out keywords are one comment span, not
        // individually keyword-colored.
        assert_eq!(rows[0].len(), 1, "{:?}", rows[0]);
        let comment_color = rows[0][0].0.fg;

        // The `#` inside a string is colored as string content, not as a
        // comment.
        let in_string = rows[1]
            .iter()
            .find(|(_, t)| t.contains("not a comment"))
            .expect("string content span");
        assert_ne!(in_string.0.fg, comment_color);
    }

    #[test]
    fn gdscript_unterminated_string_does_not_swallow_following_lines() {
        let rows = spans_for("a.gd", "var s = \"oops\nfunc f():\n\tpass");
        let default = highlighter_for("a.gd").highlight(&["x".to_string()], 1)[0][0]
            .0
            .fg;
        // Line 2's `func` is still a keyword (colored, not default).
        let func = fg_of(&rows[1], "func").expect("func span");
        assert_ne!(Some(func), default);
    }

    fn colors_of(theme: &str, enabled: bool) -> (Vec<Option<Color>>, Option<String>) {
        let (h, warning) = Highlighter::new(Language::Rust, enabled, theme);
        let rows = h.highlight(&["fn main() { let x = 1; }".to_string()], 1);
        (rows[0].iter().map(|(s, _)| s.fg).collect(), warning)
    }

    #[test]
    fn highlighting_can_be_switched_off() {
        let (off, warning) = colors_of(DEFAULT_THEME, false);
        assert!(warning.is_none());
        assert!(off.iter().all(|c| c.is_none()), "unstyled: {off:?}");
        let (on, _) = colors_of(DEFAULT_THEME, true);
        assert!(on.iter().any(|c| c.is_some()));
    }

    #[test]
    fn the_theme_setting_changes_the_colors() {
        let (dark, w1) = colors_of("base16-ocean.dark", true);
        let (light, w2) = colors_of("InspiredGitHub", true);
        assert!(w1.is_none() && w2.is_none());
        assert_ne!(dark, light, "different themes, different colors");
        let (case_insensitive, w3) = colors_of("inspiredgithub", true);
        assert!(w3.is_none());
        assert_eq!(light, case_insensitive);
    }

    #[test]
    fn an_unknown_theme_warns_and_falls_back_to_the_default() {
        let (colors, warning) = colors_of("no-such-theme", true);
        let (default, _) = colors_of(DEFAULT_THEME, true);
        assert_eq!(colors, default);
        let warning = warning.expect("a warning");
        assert!(
            warning.contains("no-such-theme") && warning.contains("base16-ocean.dark"),
            "{warning}"
        );
    }

    #[test]
    fn a_tmtheme_file_can_be_used_as_the_theme() {
        let path = std::env::temp_dir().join(format!("pc_theme_{}.tmTheme", std::process::id()));
        std::fs::write(
            &path,
            r##"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>name</key><string>Test</string>
<key>settings</key><array>
<dict><key>settings</key><dict>
<key>foreground</key><string>#112233</string>
<key>background</key><string>#000000</string>
</dict></dict>
<dict><key>scope</key><string>keyword</string><key>settings</key><dict>
<key>foreground</key><string>#ff0000</string>
</dict></dict>
</array></dict></plist>"##,
        )
        .unwrap();
        let (colors, warning) = colors_of(path.to_str().unwrap(), true);
        assert!(warning.is_none(), "{warning:?}");
        assert!(
            colors.contains(&Some(Color::Rgb(0xff, 0, 0))),
            "keywords red: {colors:?}"
        );
        assert!(
            colors.contains(&Some(Color::Rgb(0x11, 0x22, 0x33))),
            "rest uses the theme foreground: {colors:?}"
        );

        std::fs::write(&path, "not a theme").unwrap();
        let (_, warning) = colors_of(path.to_str().unwrap(), true);
        assert!(warning.expect("warns").contains("can't load"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_language_decides_the_grammar_not_the_file_name() {
        // `[associations]` maps odd extensions to a language; the
        // highlighter follows the language it is given.
        let (h, _) = Highlighter::new(Language::Rust, true, DEFAULT_THEME);
        let rows = h.highlight(&["fn main() {}".to_string()], 1);
        assert!(rows[0].iter().any(|(s, _)| s.fg.is_some()));
    }

    #[test]
    fn every_language_with_a_bundled_grammar_really_has_one() {
        let set = SyntaxSet::load_defaults_newlines();
        let plain = set.find_syntax_plain_text().name.clone();
        for &language in crate::language::ALL {
            if let Some(extension) = language.bundled_syntax_extension() {
                let syntax = set
                    .find_syntax_by_extension(extension)
                    .unwrap_or_else(|| panic!("no bundled grammar for .{extension}"));
                assert_ne!(
                    syntax.name,
                    plain,
                    "{} fell back to plain text",
                    language.id()
                );
            }
        }
    }

    #[test]
    fn c_family_languages_are_colored() {
        for (path, source) in [
            ("a.c", "#include <stdio.h>\nint main(void) { return 0; }"),
            ("a.h", "typedef struct { int x; } P;"),
            ("a.cpp", "#include <vector>\nstd::vector<int> v = {1, 2};"),
            ("A.java", "public class A { int x = 1; }"),
            ("main.go", "package main\nfunc main() { x := 1 }"),
            ("a.php", "<?php function f() { return 1; }"),
            ("A.scala", "object A { val x = 1 }"),
            ("a.m", "@interface A : NSObject\n@end"),
        ] {
            let rows = spans_for(path, source);
            assert!(
                rows.iter().any(|r| distinct_colors(r) >= 2),
                "{path} looks unstyled: {rows:?}"
            );
        }
    }

    /// One snippet per template language that uses every kind of token the
    /// template knows.
    fn template_samples() -> Vec<(&'static str, String)> {
        let ts = [
            "import { A } from \"./a\";",
            "@Component({})",
            "export class Foo extends Bar implements Baz {",
            "  private readonly x: number = 0x1F + 1_000 + 1.5e3;",
            "  async run(name: string): Promise<void> {",
            "    const s = `hi ${name} \\n` + 'single' + \"double\";",
            "    if (this.x > 0 && name !== undefined) { return; } // note",
            "    /* block",
            "       comment */",
            "  }",
            "}",
            "function helper(a: any) { return a ?? null; }",
            "interface Shape { area(): number }",
        ];
        let kt = [
            "package demo",
            "import kotlin.math.*",
            "@JvmStatic",
            "data class User(val name: String, var age: Int = 0x10)",
            "fun greet(u: User?): String {",
            "    val s = \"Hello ${u?.name} $x\" + 'c' + \"\"\"raw",
            "text\"\"\"",
            "    when (u) { is User -> println(s) else -> return \"none\" }",
            "    for (i in 1..10) { if (i % 2 == 0) continue } // even",
            "    return s /* done */",
            "}",
        ];
        let swift = [
            "import Foundation",
            "@available(iOS 13, *)",
            "public struct Point: Equatable {",
            "    var x: Double = 1.5e3",
            "    private(set) var y = 0b101",
            "    mutating func move(by d: Int) -> Self {",
            "        guard let v = Optional(d) else { return self }",
            "        let s = \"moved \\(v) times\" + \"\"\"",
            "multi",
            "\"\"\"",
            "        switch v { case 1: break; default: fallthrough }",
            "        return self // done",
            "    }",
            "}",
            "enum Dir { case north, south }",
        ];
        let dart = [
            "import 'package:flutter/material.dart';",
            "@override",
            "class Foo extends StatelessWidget with Bar {",
            "  final int count = 0xFF;",
            "  static const double pi = 3.14e0;",
            "  Future<void> run(String name) async {",
            "    var s = 'hi $name ${name.length}' + \"x\" + '''multi",
            "line''';",
            "    if (name is String && count > 0) { await Future.delayed(d); } // c",
            "    /* block */ return;",
            "  }",
            "}",
        ];
        vec![
            ("a.ts", ts.join("\n")),
            ("a.kt", kt.join("\n")),
            ("a.swift", swift.join("\n")),
            ("a.dart", dart.join("\n")),
        ]
    }

    #[test]
    fn template_grammars_load_and_every_context_compiles() {
        for (path, source) in template_samples() {
            let rows = spans_for(path, &source);
            assert!(rows.len() > 8, "{path}");
            assert!(
                rows.iter().any(|r| distinct_colors(r) >= 3),
                "{path} has no richly colored line: {rows:?}"
            );
        }
    }

    type Check = (usize, &'static str, &'static str);

    #[test]
    fn template_grammars_color_each_kind_of_token_differently() {
        // Each listed token must differ in color from a plain identifier.
        let cases: Vec<(&str, &str, &str, Vec<Check>)> = vec![
            (
                "a.kt",
                "val x = foo(\"s\", 10) // c\nreturn null",
                "x",
                vec![
                    (0, "val", "storage keyword"),
                    (0, "foo", "call"),
                    (0, "10", "number"),
                    (1, "return", "control"),
                    (1, "null", "constant"),
                ],
            ),
            (
                "a.swift",
                "let x = foo(1) // c\nguard true else { return }",
                "x",
                vec![
                    (0, "let", "storage keyword"),
                    (0, "foo", "call"),
                    (0, "1", "number"),
                    (1, "guard", "control"),
                    (1, "true", "constant"),
                ],
            ),
            (
                "a.dart",
                "final x = foo(1); // c\nreturn null;",
                "x",
                vec![
                    (0, "final", "storage keyword"),
                    (0, "foo", "call"),
                    (0, "1", "number"),
                    (1, "return", "control"),
                    (1, "null", "constant"),
                ],
            ),
            (
                "a.ts",
                "function foo(a: number) { return undefined } // c",
                "", // the plain text between tokens
                vec![
                    (0, "function", "function keyword"),
                    (0, "foo", "function name"),
                    (0, "number", "builtin type"),
                    (0, "return", "control"),
                    (0, "undefined", "constant"),
                ],
            ),
        ];
        for (path, source, plain_text, checks) in cases {
            let rows = spans_for(path, source);
            let plain = fg_of(&rows[0], plain_text)
                .unwrap_or_else(|| panic!("{path}: no span for {plain_text:?}: {:?}", rows[0]));
            for (row, text, what) in checks {
                let c = fg_of(&rows[row], text).unwrap_or_else(|| {
                    panic!("{path}: no span for {what} {text:?} in {:?}", rows[row])
                });
                assert_ne!(
                    c, plain,
                    "{path}: {what} ({text:?}) should not look like a plain identifier"
                );
            }
            assert!(
                rows[0]
                    .iter()
                    .any(|(s, t)| t.contains("// c") && s.fg.is_some() && s.fg != Some(plain)),
                "{path}: comment not colored: {:?}",
                rows[0]
            );
        }
    }

    #[test]
    fn template_strings_hide_comments_and_comments_hide_keywords() {
        for path in ["a.ts", "a.kt", "a.swift", "a.dart"] {
            let rows = spans_for(path, "// if else return\nlet s = \"// not a comment\"");
            assert_eq!(
                rows[0].len(),
                1,
                "{path}: one comment span, got {:?}",
                rows[0]
            );
            let comment = rows[0][0].0.fg;
            let inside = rows[1]
                .iter()
                .find(|(_, t)| t.contains("not a comment"))
                .expect("string span");
            assert_ne!(inside.0.fg, comment, "{path}");
        }
    }

    #[test]
    fn template_unterminated_string_does_not_swallow_following_lines() {
        for (path, line2, keyword) in [
            ("a.kt", "fun f() {}", "fun"),
            ("a.swift", "func f() {}", "func"),
            ("a.ts", "function f() {}", "function"),
        ] {
            let rows = spans_for(path, &format!("var s = \"oops\n{line2}"));
            let plain = highlighter_for(path).highlight(&["x".to_string()], 1)[0][0]
                .0
                .fg;
            let kw = fg_of(&rows[1], keyword).unwrap_or_else(|| panic!("{path}: {:?}", rows[1]));
            assert_ne!(
                Some(kw),
                plain,
                "{path}: `{keyword}` should still be a keyword"
            );
        }
    }
}
