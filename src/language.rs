//! What Pis Code knows about each file type it supports: which grammar to
//! highlight with, and how the editor's "smart" behaviors apply.

use std::path::Path;

/// How Enter splits an autoclosed bracket pair when the cursor sits
/// between the two halves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BraceSplit {
    /// No split: just the generic deepen-after-an-open-bracket rule. Used
    /// where `{}` is a data literal rather than a block (Python, GDScript).
    None,
    /// The opening bracket stays on its line; an indented line and the
    /// closing bracket on its own line follow. How JSON is written.
    KAndR,
    /// The opening brace moves to its own line, matching the signature's
    /// indent (Visual Studio's C# style — and the one chosen here for every
    /// language whose `{}` is a block).
    Allman,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Language {
    CSharp,
    C,
    Cpp,
    ObjectiveC,
    Java,
    Go,
    Php,
    TypeScript,
    Kotlin,
    Swift,
    Dart,
    Scala,
    JavaScript,
    Rust,
    Python,
    GdScript,
    Html,
    Css,
    Json,
    Xml,
    Yaml,
    Markdown,
    Other,
}

/// Every language, in the order they're documented.
pub const ALL: &[Language] = &[
    Language::CSharp,
    Language::C,
    Language::Cpp,
    Language::ObjectiveC,
    Language::Java,
    Language::Go,
    Language::Php,
    Language::TypeScript,
    Language::Kotlin,
    Language::Swift,
    Language::Dart,
    Language::Scala,
    Language::JavaScript,
    Language::Rust,
    Language::Python,
    Language::GdScript,
    Language::Html,
    Language::Css,
    Language::Json,
    Language::Xml,
    Language::Yaml,
    Language::Markdown,
    Language::Other,
];

impl Language {
    /// The name used for this language in the config file
    /// (`[languages.<id>]`, `[associations]`).
    pub fn id(self) -> &'static str {
        match self {
            Language::CSharp => "csharp",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::ObjectiveC => "objc",
            Language::Java => "java",
            Language::Go => "go",
            Language::Php => "php",
            Language::TypeScript => "typescript",
            Language::Kotlin => "kotlin",
            Language::Swift => "swift",
            Language::Dart => "dart",
            Language::Scala => "scala",
            Language::JavaScript => "javascript",
            Language::Rust => "rust",
            Language::Python => "python",
            Language::GdScript => "gdscript",
            Language::Html => "html",
            Language::Css => "css",
            Language::Json => "json",
            Language::Xml => "xml",
            Language::Yaml => "yaml",
            Language::Markdown => "markdown",
            Language::Other => "plain",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        ALL.iter().copied().find(|l| l.id() == id)
    }

    /// Like [`Language::from_path`], but first consults the user's
    /// `[associations]`: an exact file name (`Jenkinsfile`) or an extension
    /// without the dot (`vue`, case-insensitive) mapped to a language id.
    /// An association naming an unknown language is ignored.
    pub fn from_path_with(
        path: &Path,
        associations: &std::collections::BTreeMap<String, String>,
    ) -> Self {
        let by_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| associations.get(n));
        let by_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .and_then(|e| {
                associations
                    .iter()
                    .find(|(k, _)| k.trim_start_matches('.').eq_ignore_ascii_case(&e))
                    .map(|(_, v)| v)
            });
        by_name
            .or(by_ext)
            .and_then(|id| Language::from_id(id))
            .unwrap_or_else(|| Language::from_path(path))
    }

    pub fn from_path(path: &Path) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("cs") => Language::CSharp,
            // `.h` is ambiguous (C or C++); C is the safer guess, and
            // `[associations]` can say otherwise per project.
            Some("c" | "h") => Language::C,
            Some(
                "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ipp" | "tpp" | "inl",
            ) => Language::Cpp,
            Some("m" | "mm") => Language::ObjectiveC,
            Some("java") => Language::Java,
            Some("go") => Language::Go,
            Some("php" | "phtml" | "php5" | "php7" | "phps") => Language::Php,
            Some("ts" | "tsx" | "mts" | "cts") => Language::TypeScript,
            Some("kt" | "kts") => Language::Kotlin,
            Some("swift") => Language::Swift,
            Some("dart") => Language::Dart,
            Some("scala" | "sc" | "sbt") => Language::Scala,
            Some("js" | "mjs" | "cjs" | "jsx") => Language::JavaScript,
            Some("rs") => Language::Rust,
            Some("py" | "py3" | "pyw" | "pyi") => Language::Python,
            Some("gd") => Language::GdScript,
            Some("html" | "htm" | "xhtml") => Language::Html,
            Some("css") => Language::Css,
            Some("json") => Language::Json,
            // Besides plain XML: the XML-based formats a C# project is
            // full of (.csproj and friends) and common SVG/XAML/XSL.
            Some(
                "xml" | "svg" | "xaml" | "xsl" | "xslt" | "xsd" | "csproj" | "vbproj" | "fsproj"
                | "props" | "targets" | "resx" | "plist",
            ) => Language::Xml,
            Some("yaml" | "yml") => Language::Yaml,
            Some("md" | "markdown" | "mdown") => Language::Markdown,
            _ => Language::Other,
        }
    }

    /// The extension to look the grammar up by in syntect's bundled set.
    /// `None` for GDScript (which ships its own grammar) and for types we
    /// deliberately leave unstyled.
    pub fn bundled_syntax_extension(self) -> Option<&'static str> {
        match self {
            Language::CSharp => Some("cs"),
            Language::C => Some("c"),
            Language::Cpp => Some("cpp"),
            Language::ObjectiveC => Some("m"),
            Language::Java => Some("java"),
            Language::Go => Some("go"),
            Language::Php => Some("php"),
            Language::Scala => Some("scala"),
            Language::JavaScript => Some("js"),
            Language::Rust => Some("rs"),
            Language::Python => Some("py"),
            Language::Html => Some("html"),
            Language::Css => Some("css"),
            Language::Json => Some("json"),
            Language::Xml => Some("xml"),
            Language::Yaml => Some("yaml"),
            Language::Markdown => Some("md"),
            // Written for the editor (see `syntax/`): syntect's bundled set
            // has no grammar for these.
            Language::GdScript
            | Language::TypeScript
            | Language::Kotlin
            | Language::Swift
            | Language::Dart
            | Language::Other => None,
        }
    }

    /// `{}` is a block in every C-family language, so Enter between an
    /// autoclosed pair splits it Allman-style. Go is the exception: its
    /// grammar forbids a brace on its own line (a newline there inserts a
    /// semicolon), so it gets the K&R shape, as does JSON, where `{}` is a
    /// data literal. Elsewhere there's no split.
    pub fn brace_split(self) -> BraceSplit {
        match self {
            Language::CSharp
            | Language::C
            | Language::Cpp
            | Language::ObjectiveC
            | Language::Java
            | Language::Php
            | Language::TypeScript
            | Language::Kotlin
            | Language::Swift
            | Language::Dart
            | Language::Scala
            | Language::JavaScript
            | Language::Rust
            | Language::Css => BraceSplit::Allman,
            Language::Go | Language::Json => BraceSplit::KAndR,
            _ => BraceSplit::None,
        }
    }

    /// Whether Enter between `[` and `]` splits them onto three lines (when
    /// the brace style is K&R): right for JSON arrays, wrong for `[]int`.
    pub fn splits_square_brackets(self) -> bool {
        self == Language::Json
    }

    /// Indentation-based blocks: a trailing `:` opens one.
    pub fn colon_deepens_indent(self) -> bool {
        matches!(self, Language::Python | Language::GdScript | Language::Yaml)
    }

    /// `'` is a lifetime marker in Rust and an apostrophe in prose, and
    /// Swift has no single-quoted literals at all, so autoclosing it into
    /// `''` there is more nuisance than help.
    pub fn autocloses_single_quote(self) -> bool {
        !matches!(self, Language::Rust | Language::Markdown | Language::Swift)
    }

    /// Whether files of this type are made of tags (HTML, XML) — what tag
    /// auto-closing and the Enter-between-tags split work on.
    pub fn has_tags(self) -> bool {
        matches!(self, Language::Html | Language::Xml)
    }

    pub fn is_html(self) -> bool {
        self == Language::Html
    }

    /// Indent unit for a file that doesn't show one yet. GDScript's style
    /// guide (and Godot's editor) uses tabs, and mixing tabs with spaces is
    /// a parse error there; Go's formatter insists on tabs. YAML
    /// conventionally uses two spaces (and forbids tabs outright), and so
    /// does Dart's formatter.
    pub fn default_indent(self) -> &'static str {
        match self {
            Language::GdScript | Language::Go => "\t",
            Language::Yaml | Language::Dart => "  ",
            _ => "    ",
        }
    }

    /// Tabs aren't legal indentation in YAML.
    pub fn allows_tab_indent(self) -> bool {
        self != Language::Yaml
    }

    /// Whether Ctrl+Shift+S has a checker to run for this file type.
    pub fn has_checker(self) -> bool {
        !matches!(
            self,
            Language::Css
                | Language::Markdown
                | Language::Other
                // No dependable command-line syntax check for these.
                | Language::ObjectiveC
                | Language::Dart
                | Language::Scala
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lang(path: &str) -> Language {
        Language::from_path(Path::new(path))
    }

    #[test]
    fn detects_language_from_extension_ignoring_case() {
        assert!(lang("a.cs") == Language::CSharp);
        assert!(lang("a.HTML") == Language::Html);
        assert!(lang("a.htm") == Language::Html);
        assert!(lang("a.css") == Language::Css);
        assert!(lang("a.JSON") == Language::Json);
        assert!(lang("a.yml") == Language::Yaml);
        assert!(lang("a.md") == Language::Markdown);
        assert!(lang("a.gd") == Language::GdScript);
        assert!(lang("a.txt") == Language::Other);
        assert!(lang("Makefile") == Language::Other);
    }

    #[test]
    fn c_family_extensions() {
        for (path, want) in [
            ("a.c", Language::C),
            ("a.h", Language::C),
            ("a.cpp", Language::Cpp),
            ("a.CC", Language::Cpp),
            ("a.cxx", Language::Cpp),
            ("a.hpp", Language::Cpp),
            ("a.m", Language::ObjectiveC),
            ("a.mm", Language::ObjectiveC),
            ("A.java", Language::Java),
            ("main.go", Language::Go),
            ("index.php", Language::Php),
            ("a.ts", Language::TypeScript),
            ("a.tsx", Language::TypeScript),
            ("a.kt", Language::Kotlin),
            ("build.gradle.kts", Language::Kotlin),
            ("a.swift", Language::Swift),
            ("main.dart", Language::Dart),
            ("A.scala", Language::Scala),
            ("a.js", Language::JavaScript),
        ] {
            assert!(lang(path) == want, "{path}");
        }
    }

    #[test]
    fn every_block_language_splits_braces_allman_except_go() {
        for l in [
            Language::C,
            Language::Cpp,
            Language::ObjectiveC,
            Language::Java,
            Language::Php,
            Language::TypeScript,
            Language::Kotlin,
            Language::Swift,
            Language::Dart,
            Language::Scala,
        ] {
            assert!(l.brace_split() == BraceSplit::Allman, "{}", l.id());
        }
        // A brace on its own line is a syntax error in Go.
        assert!(Language::Go.brace_split() == BraceSplit::KAndR);
        assert!(
            !Language::Go.splits_square_brackets(),
            "[]int is not an array literal"
        );
        assert!(Language::Json.splits_square_brackets());
    }

    #[test]
    fn indent_and_quote_conventions_of_the_newer_languages() {
        assert_eq!(Language::Go.default_indent(), "\t");
        assert_eq!(Language::Dart.default_indent(), "  ");
        assert_eq!(Language::Java.default_indent(), "    ");
        assert!(Language::C.autocloses_single_quote(), "char literals");
        assert!(!Language::Swift.autocloses_single_quote());
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = ALL.iter().map(|l| l.id()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len());
    }

    #[test]
    fn xml_based_project_files_are_xml() {
        for p in ["App.csproj", "a.svg", "Main.xaml", "Directory.Build.props"] {
            assert!(lang(p) == Language::Xml, "{p}");
        }
    }

    #[test]
    fn ids_round_trip() {
        for &l in ALL {
            assert!(Language::from_id(l.id()) == Some(l), "{}", l.id());
        }
        assert!(Language::from_id("cobol").is_none());
    }

    #[test]
    fn associations_override_detection() {
        let mut assoc = std::collections::BTreeMap::new();
        assoc.insert("vue".to_string(), "html".to_string());
        assoc.insert(".JSONC".to_string(), "json".to_string());
        assoc.insert("Jenkinsfile".to_string(), "python".to_string());
        assoc.insert("cs".to_string(), "plain".to_string()); // beats the built-in
        assoc.insert("zzz".to_string(), "nonsense".to_string()); // ignored

        let l = |p: &str| Language::from_path_with(Path::new(p), &assoc);
        assert!(l("App.vue") == Language::Html);
        assert!(
            l("a/b/settings.jsonc") == Language::Json,
            "leading dot and case ignored"
        );
        assert!(l("Jenkinsfile") == Language::Python, "exact file name");
        assert!(l("Program.cs") == Language::Other, "user association wins");
        assert!(
            l("a.zzz") == Language::Other,
            "unknown language id falls through"
        );
        assert!(l("a.rs") == Language::Rust, "unrelated files unaffected");
    }
}
