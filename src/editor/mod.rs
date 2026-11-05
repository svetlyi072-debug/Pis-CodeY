use crate::check::Diagnostic;
use crate::config::{IndentStyle, LineEnding, Settings};
use crate::file_io;
use crate::language::{BraceSplit, Language};
use crate::markup;
use anyhow::Result;
use std::ops::Range;
use std::path::PathBuf;

pub mod mouse;
pub mod search;
pub use mouse::View;
pub use search::{Search, SearchStatus};

/// What the file itself shows about its indentation: the first indented
/// line decides tabs vs. spaces (a single leading space is ignored — that's
/// usually the ` * ` of a block comment, not indentation); for spaces, the
/// narrowest indentation seen is the unit (2, 3, 4 or 8), so a two-space
/// file stays two-space. `None` for a file with no indentation yet.
fn detect_indent(lines: &[String], language: Language) -> Option<String> {
    let leading_spaces = |l: &String| l.chars().take_while(|c| *c == ' ').count();
    let is_indented =
        |l: &String| l.starts_with('\t') || (leading_spaces(l) >= 2 && !l.trim().is_empty());

    let first = lines.iter().find(|l| is_indented(l))?;
    if first.starts_with('\t') && language.allows_tab_indent() {
        return Some("\t".to_string());
    }
    let narrowest = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(leading_spaces)
        .filter(|&n| n >= 2)
        .min();
    match narrowest {
        Some(n @ (2 | 3 | 4 | 8)) => Some(" ".repeat(n)),
        _ => None, // odd widths are alignment, not an indent unit
    }
}

/// One indent level for a file, from the `indent_style` / `indent_width` /
/// `detect_indentation` settings, the file's own content, and the
/// language's default (tabs for GDScript, two spaces for YAML, otherwise
/// four spaces).
fn choose_indent(lines: &[String], language: Language, s: &Settings) -> String {
    let default = language.default_indent();
    // Width in spaces when the default is spaces; GDScript's tab default
    // has no width, so an explicit "spaces" there means four.
    let default_width = if default == "\t" { 4 } else { default.len() };
    let spaces = |width: Option<usize>| " ".repeat(width.unwrap_or(default_width));

    match s.indent_style {
        IndentStyle::Tabs => "\t".to_string(),
        IndentStyle::Spaces => spaces(s.indent_width),
        IndentStyle::Auto => {
            let detected = if s.detect_indentation {
                detect_indent(lines, language)
            } else {
                None
            };
            detected.unwrap_or_else(|| {
                if default == "\t" {
                    default.to_string()
                } else {
                    spaces(s.indent_width)
                }
            })
        }
    }
}

/// Screen column (in cells) at which each character of `line` starts, plus
/// one final entry for the line's total width, expanding tabs to the next
/// multiple of `tab_width`.
pub fn display_columns(line: &str, tab_width: usize) -> Vec<usize> {
    let tab_width = tab_width.max(1);
    let mut cols = Vec::with_capacity(line.len() + 1);
    let mut col = 0;
    for ch in line.chars() {
        cols.push(col);
        col += if ch == '\t' {
            tab_width - col % tab_width
        } else {
            1
        };
    }
    cols.push(col);
    cols
}

/// What kind of edit was last performed, used to decide whether a new
/// keystroke should be coalesced into the current undo step or start a
/// fresh one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditGroup {
    None,
    Insert,
    Delete,
    Newline,
    Paste,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Whitespace,
    Word,
    Other,
}

// The real OS clipboard is global, external, mutable state shared with
// everything else on the machine — reading/writing it from unit tests
// would make them flaky (parallel test threads racing on it, or picking up
// whatever a developer last copied). Tests always go through the in-app
// `Editor::clipboard` fallback instead; the real integration is exercised
// manually.
#[cfg(not(test))]
fn set_system_clipboard(text: &str) -> Result<()> {
    let mut cb = arboard::Clipboard::new()?;
    cb.set_text(text.to_string())?;
    Ok(())
}

#[cfg(test)]
fn set_system_clipboard(_text: &str) -> Result<()> {
    anyhow::bail!("system clipboard disabled in tests")
}

#[cfg(not(test))]
fn get_system_clipboard() -> Result<String> {
    let mut cb = arboard::Clipboard::new()?;
    Ok(cb.get_text()?)
}

#[cfg(test)]
fn get_system_clipboard() -> Result<String> {
    anyhow::bail!("system clipboard disabled in tests")
}

#[derive(Clone)]
struct Snapshot {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

pub struct Editor {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub file_path: PathBuf,
    pub language: Language,
    pub settings: Settings,
    /// The find bar (Ctrl+F): its text, options and matches.
    pub search: Search,
    /// Where the last draw put the text area, for turning mouse positions
    /// into text positions.
    pub view: View,
    mouse: mouse::MouseState,
    /// One level of indentation, see [`choose_indent`].
    indent: String,
    /// Whether the file used CRLF line endings when it was loaded.
    file_crlf: bool,
    /// Height of the text area as of the last draw; PageUp/PageDown and
    /// `scroll_off` need it.
    viewport_height: usize,
    pub modified: bool,

    /// Top-left visible cell, in (line, char) space.
    pub scroll: usize,
    pub col_scroll: usize,

    pub confirm_quit: bool,
    pub status_message: String,
    pub status_is_error: bool,

    /// The other end of an active selection; `None` means no selection.
    /// The current cursor position is always the live end.
    pub selection_anchor: Option<(usize, usize)>,
    clipboard: String,

    /// Compiler diagnostics from the last Ctrl+S check, kept until the
    /// next check completes.
    pub diagnostics: Vec<Diagnostic>,

    desired_col: usize,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    last_edit_group: EditGroup,
}

impl Editor {
    /// Opens `path` with the built-in behavior for its language.
    #[cfg(test)]
    pub fn open(path: PathBuf) -> Result<Self> {
        let language = Language::from_path(&path);
        Self::open_with(path, language, Settings::for_language(language))
    }

    /// Opens `path` as `language` with already-resolved `settings`.
    pub fn open_with(path: PathBuf, language: Language, settings: Settings) -> Result<Self> {
        let loaded = file_io::load_or_create(&path)?;
        let indent = choose_indent(&loaded.lines, language, &settings);
        Ok(Self {
            lines: loaded.lines,
            cursor_row: 0,
            cursor_col: 0,
            file_path: path,
            language,
            search: Search::new(&settings.search),
            view: View::default(),
            mouse: mouse::MouseState::default(),
            settings,
            indent,
            file_crlf: loaded.crlf,
            viewport_height: 0,
            modified: false,
            scroll: 0,
            col_scroll: 0,
            confirm_quit: false,
            status_message: String::from(
                "^S save  ^⇧S save+check  ^Q quit  ^Z undo  ^Y redo  ^A select all  ^C copy  ^X cut  ^V paste  ^F find",
            ),
            status_is_error: false,
            selection_anchor: None,
            clipboard: String::new(),
            diagnostics: Vec::new(),
            desired_col: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit_group: EditGroup::None,
        })
    }

    /// How wide a tab is drawn, in columns.
    pub fn tab_width(&self) -> usize {
        self.settings.tab_width
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Total character count across the whole buffer (not counting line
    /// terminators).
    pub fn char_count(&self) -> usize {
        self.lines.iter().map(|l| l.chars().count()).sum()
    }

    fn current_line(&self) -> &str {
        &self.lines[self.cursor_row]
    }

    fn current_line_char_len(&self) -> usize {
        self.current_line().chars().count()
    }

    fn char_before_cursor(&self) -> Option<char> {
        if self.cursor_col == 0 {
            None
        } else {
            self.current_line().chars().nth(self.cursor_col - 1)
        }
    }

    fn char_at_cursor(&self) -> Option<char> {
        self.current_line().chars().nth(self.cursor_col)
    }

    fn byte_idx(line: &str, char_idx: usize) -> usize {
        line.char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(line.len())
    }

    fn leading_whitespace(line: &str) -> String {
        line.chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect()
    }

    /// `extra` lists the non-alphanumeric characters that count as part of
    /// a word (the `word_chars` setting).
    fn char_class(c: char, extra: &str) -> CharClass {
        if c.is_whitespace() {
            CharClass::Whitespace
        } else if c.is_alphanumeric() || extra.contains(c) {
            CharClass::Word
        } else {
            CharClass::Other
        }
    }

    /// The column one word to the left of `col` on `line`: skip any
    /// whitespace immediately before the cursor, then skip the run of
    /// same-class characters before that (identifiers and runs of
    /// punctuation are treated as separate word stops).
    fn prev_word_boundary(line: &str, col: usize, extra: &str) -> usize {
        let chars: Vec<char> = line.chars().collect();
        let mut i = col.min(chars.len());
        while i > 0 && Self::char_class(chars[i - 1], extra) == CharClass::Whitespace {
            i -= 1;
        }
        if i == 0 {
            return 0;
        }
        let class = Self::char_class(chars[i - 1], extra);
        while i > 0 && Self::char_class(chars[i - 1], extra) == class {
            i -= 1;
        }
        i
    }

    /// The column one word to the right of `col` on `line`; mirrors
    /// `prev_word_boundary`.
    fn next_word_boundary(line: &str, col: usize, extra: &str) -> usize {
        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();
        let mut i = col.min(len);
        while i < len && Self::char_class(chars[i], extra) == CharClass::Whitespace {
            i += 1;
        }
        if i == len {
            return len;
        }
        let class = Self::char_class(chars[i], extra);
        while i < len && Self::char_class(chars[i], extra) == class {
            i += 1;
        }
        i
    }

    // ---- low level buffer mutation (no undo bookkeeping) ----

    fn insert_raw(&mut self, c: char) {
        let row = self.cursor_row;
        let bi = Self::byte_idx(&self.lines[row], self.cursor_col);
        self.lines[row].insert(bi, c);
        self.cursor_col += 1;
    }

    fn remove_chars(&mut self, row: usize, start: usize, end: usize) {
        let line = &self.lines[row];
        let bstart = Self::byte_idx(line, start);
        let bend = Self::byte_idx(line, end);
        self.lines[row].replace_range(bstart..bend, "");
    }

    // ---- selection ----

    /// Returns `(start, end)` in row-major order, or `None` if there is no
    /// active (non-empty) selection.
    #[cfg(test)]
    pub fn selection_bounds_for_test(&self) -> Option<((usize, usize), (usize, usize))> {
        self.selection_bounds()
    }

    fn selection_bounds(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.selection_anchor?;
        let cursor = (self.cursor_row, self.cursor_col);
        if anchor == cursor {
            return None;
        }
        Some(if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        })
    }

    pub fn has_selection(&self) -> bool {
        self.selection_bounds().is_some()
    }

    /// The selected character-column range within `row`, for rendering.
    pub fn selection_col_range(&self, row: usize) -> Option<Range<usize>> {
        let (start, end) = self.selection_bounds()?;
        if row < start.0 || row > end.0 {
            return None;
        }
        let line_len = self.lines.get(row)?.chars().count();
        let s = if row == start.0 { start.1 } else { 0 };
        let e = if row == end.0 { end.1 } else { line_len };
        Some(s..e)
    }

    fn extract_range(&self, start: (usize, usize), end: (usize, usize)) -> String {
        if start.0 == end.0 {
            let line = &self.lines[start.0];
            let bs = Self::byte_idx(line, start.1);
            let be = Self::byte_idx(line, end.1);
            line[bs..be].to_string()
        } else {
            let mut out = String::new();
            let first = &self.lines[start.0];
            out.push_str(&first[Self::byte_idx(first, start.1)..]);
            for r in start.0 + 1..end.0 {
                out.push('\n');
                out.push_str(&self.lines[r]);
            }
            out.push('\n');
            let last = &self.lines[end.0];
            out.push_str(&last[..Self::byte_idx(last, end.1)]);
            out
        }
    }

    /// Deletes the active selection as part of edit `group`, moving the
    /// cursor to where the selection started. Returns `false` if there was
    /// nothing selected.
    fn delete_selection_as(&mut self, group: EditGroup) -> bool {
        let Some((start, end)) = self.selection_bounds() else {
            return false;
        };
        self.begin_edit(group);
        if start.0 == end.0 {
            self.remove_chars(start.0, start.1, end.1);
        } else {
            let end_line_tail = {
                let last = &self.lines[end.0];
                last[Self::byte_idx(last, end.1)..].to_string()
            };
            let bstart = Self::byte_idx(&self.lines[start.0], start.1);
            self.lines[start.0].truncate(bstart);
            self.lines[start.0].push_str(&end_line_tail);
            self.lines.drain(start.0 + 1..=end.0);
        }
        self.cursor_row = start.0;
        self.cursor_col = start.1;
        self.selection_anchor = None;
        self.modified = true;
        true
    }

    // ---- undo bookkeeping ----

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
        }
    }

    fn begin_edit(&mut self, group: EditGroup) {
        if self.last_edit_group != group {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > self.settings.undo_limit {
                let excess = self.undo_stack.len() - self.settings.undo_limit;
                self.undo_stack.drain(..excess);
            }
            self.redo_stack.clear();
            self.last_edit_group = group;
        }
    }

    /// Breaks undo coalescing; call this on cursor moves and other
    /// non-edit actions so the next edit starts a fresh undo step.
    fn reset_edit_group(&mut self) {
        self.last_edit_group = EditGroup::None;
    }

    pub fn undo(&mut self) {
        if let Some(state) = self.undo_stack.pop() {
            self.redo_stack.push(self.snapshot());
            self.lines = state.lines;
            self.cursor_row = state.cursor_row.min(self.lines.len().saturating_sub(1));
            self.cursor_col = state.cursor_col;
            self.clamp_cursor();
            self.last_edit_group = EditGroup::None;
            self.modified = true;
            self.status_message = "Undo".to_string();
            self.status_is_error = false;
        } else {
            self.status_message = "Nothing to undo".to_string();
            self.status_is_error = false;
        }
    }

    pub fn redo(&mut self) {
        if let Some(state) = self.redo_stack.pop() {
            self.undo_stack.push(self.snapshot());
            self.lines = state.lines;
            self.cursor_row = state.cursor_row.min(self.lines.len().saturating_sub(1));
            self.cursor_col = state.cursor_col;
            self.clamp_cursor();
            self.last_edit_group = EditGroup::None;
            self.modified = true;
            self.status_message = "Redo".to_string();
            self.status_is_error = false;
        } else {
            self.status_message = "Nothing to redo".to_string();
            self.status_is_error = false;
        }
    }

    fn clamp_cursor(&mut self) {
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len() - 1;
        }
        let len = self.current_line_char_len();
        if self.cursor_col > len {
            self.cursor_col = len;
        }
    }

    // ---- editing ----

    /// Counts (unescaped) occurrences of `quote` before the cursor on the
    /// current line — an odd count means we're sitting inside an open
    /// string, which is how we tell "close this string" apart from "open a
    /// new one" when the same character is used for both ends.
    fn quote_count_before_cursor(&self, quote: char) -> usize {
        let mut count = 0;
        let mut escaped = false;
        for (i, ch) in self.current_line().chars().enumerate() {
            if i >= self.cursor_col {
                break;
            }
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                count += 1;
            }
        }
        count
    }

    /// Insert a typed character, applying VS Code-style smart pairing for
    /// `{`, `(`, `[`, `"`, `'` and type-over for their closers.
    pub fn insert_char(&mut self, c: char) {
        self.confirm_quit = false;
        if self.has_selection() {
            self.delete_selection_as(EditGroup::Insert);
        }
        match c {
            '{' | '(' | '[' if self.settings.auto_close_brackets => {
                self.begin_edit(EditGroup::Insert);
                let close = match c {
                    '{' => '}',
                    '(' => ')',
                    '[' => ']',
                    _ => unreachable!(),
                };
                self.insert_raw(c);
                self.insert_raw(close);
                self.cursor_col -= 1;
            }
            '}' | ')' | ']'
                if self.settings.auto_close_brackets && self.char_at_cursor() == Some(c) =>
            {
                // Type over an already-present closing bracket instead of
                // inserting a duplicate.
                self.begin_edit(EditGroup::Insert);
                self.cursor_col += 1;
            }
            '>' if self.settings.auto_close_tags => {
                // HTML/XML: finishing an opening tag inserts its closing
                // tag after the cursor, leaving the cursor between them.
                self.begin_edit(EditGroup::Insert);
                let before: String = self.current_line().chars().take(self.cursor_col).collect();
                let closing = markup::closing_tag_for(&before, self.language.is_html());
                self.insert_raw('>');
                if let Some(closing) = closing {
                    let between = self.cursor_col;
                    for ch in closing.chars() {
                        self.insert_raw(ch);
                    }
                    self.cursor_col = between;
                }
            }
            '"' | '\'' | '`'
                if self.settings.auto_close_quotes
                    && (c != '\'' || self.settings.auto_close_single_quote) =>
            {
                self.begin_edit(EditGroup::Insert);
                if self.char_at_cursor() == Some(c) {
                    // Type over an already-present closing quote.
                    self.cursor_col += 1;
                } else if self.quote_count_before_cursor(c) % 2 == 1 {
                    // We're inside an open string; this closes it, so
                    // don't also autoclose a fresh pair.
                    self.insert_raw(c);
                } else {
                    self.insert_raw(c);
                    self.insert_raw(c);
                    self.cursor_col -= 1;
                }
            }
            _ => {
                self.begin_edit(EditGroup::Insert);
                self.insert_raw(c);
            }
        }
        self.desired_col = self.cursor_col;
        self.modified = true;
    }

    /// Tab: with an active selection, indents every line the selection
    /// touches by one level (VS Code-style block indent) instead of
    /// replacing the selected text. Otherwise inserts one indent unit
    /// (a tab, or four spaces) at the cursor.
    pub fn insert_tab(&mut self) {
        self.confirm_quit = false;
        if self.has_selection() {
            self.indent_selection();
            return;
        }
        self.begin_edit(EditGroup::Insert);
        for c in self.indent.clone().chars() {
            self.insert_raw(c);
        }
        self.desired_col = self.cursor_col;
        self.modified = true;
    }

    /// Shift+Tab: with an active selection, dedents every line it
    /// touches by up to one level; otherwise dedents just the current
    /// line.
    pub fn dedent(&mut self) {
        self.confirm_quit = false;
        if self.has_selection() {
            self.dedent_selection();
        } else {
            self.dedent_line();
        }
    }

    /// How many leading spaces one Shift+Tab can remove: the file's indent
    /// width, or a tab-width's worth when the file is tab-indented.
    fn dedent_width(&self) -> usize {
        if self.indent == "\t" {
            self.settings.tab_width
        } else {
            self.indent.len()
        }
    }

    fn selection_line_span(&self) -> Option<(usize, usize)> {
        let (start, end) = self.selection_bounds()?;
        Some((start.0, end.0))
    }

    fn indent_selection(&mut self) {
        let Some((start_row, end_row)) = self.selection_line_span() else {
            return;
        };
        self.begin_edit(EditGroup::Insert);
        let unit_chars = self.indent.chars().count();
        for row in start_row..=end_row {
            self.lines[row].insert_str(0, &self.indent);
        }
        // Every line in range shifted right by the same amount, so both
        // ends of the selection shift identically regardless of which
        // row each sits on.
        self.cursor_col += unit_chars;
        if let Some(anchor) = self.selection_anchor.as_mut() {
            anchor.1 += unit_chars;
        }
        self.desired_col = self.cursor_col;
        self.modified = true;
    }

    fn dedent_selection(&mut self) {
        let Some((start_row, end_row)) = self.selection_line_span() else {
            return;
        };
        self.begin_edit(EditGroup::Delete);
        let width = self.dedent_width();
        let anchor_row = self.selection_anchor.map(|a| a.0);
        let mut cursor_removed = 0;
        let mut anchor_removed = 0;
        for row in start_row..=end_row {
            let removed = Self::dedent_line_at(&mut self.lines[row], width);
            if row == self.cursor_row {
                cursor_removed = removed;
            }
            if Some(row) == anchor_row {
                anchor_removed = removed;
            }
        }
        self.cursor_col = self.cursor_col.saturating_sub(cursor_removed);
        if let Some(anchor) = self.selection_anchor.as_mut() {
            anchor.1 = anchor.1.saturating_sub(anchor_removed);
        }
        self.desired_col = self.cursor_col;
        self.modified = true;
    }

    fn dedent_line(&mut self) {
        self.begin_edit(EditGroup::Delete);
        let width = self.dedent_width();
        let row = self.cursor_row;
        let removed = Self::dedent_line_at(&mut self.lines[row], width);
        self.cursor_col = self.cursor_col.saturating_sub(removed);
        self.desired_col = self.cursor_col;
        self.modified = true;
    }

    /// Removes one indent level from the start of `line` — a leading tab,
    /// or up to `width` leading spaces, whichever the line actually uses, so
    /// Shift+Tab works on lines regardless of the file's dominant style.
    /// Returns how many characters were removed.
    fn dedent_line_at(line: &mut String, width: usize) -> usize {
        let remove = if line.starts_with('\t') {
            1
        } else {
            line.chars().take(width).take_while(|c| *c == ' ').count()
        };
        if remove > 0 {
            let byte_idx = Self::byte_idx(line, remove);
            line.replace_range(..byte_idx, "");
        }
        remove
    }

    /// Enter key. For languages where `{}` is a block (C#, JavaScript,
    /// Rust), if the cursor sits directly between an autoclosed `{` and
    /// `}`, expand Allman-style into four lines — the code before `{`
    /// stays on its own line, `{` moves to its own line at the same
    /// indent, an empty line one level deeper is inserted for the cursor,
    /// and `}` goes on its own line matching `{`'s indent. Other
    /// languages (`{}` is just a dict/set literal there, not a block) get
    /// no split at all. Otherwise it's plain autoindent — deepened after
    /// an unclosed bracket for any language, or after a trailing `:` for
    /// Python/GDScript's indentation-based blocks.
    pub fn newline(&mut self) {
        self.confirm_quit = false;
        if self.has_selection() {
            self.delete_selection_as(EditGroup::Newline);
        }
        self.begin_edit(EditGroup::Newline);

        let row = self.cursor_row;
        let line = self.lines[row].clone();
        // With auto_indent off, new lines start at column 0: nothing is
        // copied from this line and nothing is added for a deeper level.
        let auto_indent = self.settings.auto_indent;
        let indent = if auto_indent {
            Self::leading_whitespace(&line)
        } else {
            String::new()
        };
        let unit = if auto_indent {
            self.indent.clone()
        } else {
            String::new()
        };

        let before = self.char_before_cursor();
        let after = self.char_at_cursor();

        let bi = Self::byte_idx(&line, self.cursor_col);
        let smart = self.settings.smart_enter;
        let brace_split = if smart {
            self.settings.brace_split
        } else {
            BraceSplit::None
        };
        let between_brackets = match (before, after) {
            (Some('{'), Some('}')) => true,
            // Arrays split too, but only where `[]` is data (JSON).
            (Some('['), Some(']')) => {
                brace_split == BraceSplit::KAndR && self.language.splits_square_brackets()
            }
            _ => false,
        };
        let between_tags = smart
            && self.language.has_tags()
            && markup::between_tags(&line[..bi], &line[bi..], self.language.is_html());

        if brace_split == BraceSplit::Allman && between_brackets {
            // `left` ends with the '{' itself since the cursor sits right
            // after it; strip that brace back off to get the code before it.
            let before_brace = line[..bi - 1].trim_end().to_string();
            let right = line[bi..].to_string();
            let inner_indent = format!("{indent}{unit}");

            // A brace that already starts its line stays where it is,
            // instead of leaving a blank line above it.
            let brace_on_own_line = before_brace.trim().is_empty();
            let first = if brace_on_own_line { row } else { row + 1 };
            if brace_on_own_line {
                self.lines[row] = format!("{indent}{{");
            } else {
                self.lines[row] = before_brace;
                self.lines.insert(first, format!("{indent}{{"));
            }
            self.lines.insert(first + 1, inner_indent.clone());
            self.lines.insert(first + 2, format!("{indent}{right}"));

            self.cursor_row = first + 1;
            self.cursor_col = inner_indent.chars().count();
        } else if (brace_split == BraceSplit::KAndR && between_brackets) || between_tags {
            // The opening bracket/tag stays where it is; an indented line
            // for the cursor and the closing half on its own line follow.
            let left = line[..bi].to_string();
            let right = line[bi..].to_string();
            let inner_indent = format!("{indent}{unit}");

            self.lines[row] = left;
            self.lines.insert(row + 1, inner_indent.clone());
            self.lines.insert(row + 2, format!("{indent}{right}"));

            self.cursor_row = row + 1;
            self.cursor_col = inner_indent.chars().count();
        } else {
            let left = line[..bi].to_string();
            let right = line[bi..].to_string();

            let trimmed_left = left.trim_end();
            let deepen = trimmed_left.ends_with('{')
                || trimmed_left.ends_with('(')
                || trimmed_left.ends_with('[')
                || (self.language.colon_deepens_indent() && trimmed_left.ends_with(':'));

            let new_indent = if deepen && auto_indent {
                format!("{indent}{unit}")
            } else {
                indent
            };

            self.lines[row] = left;
            self.lines.insert(row + 1, format!("{new_indent}{right}"));

            self.cursor_row = row + 1;
            self.cursor_col = new_indent.chars().count();
        }

        self.desired_col = self.cursor_col;
        self.modified = true;
    }

    /// Backspace: deletes an adjacent auto-paired `{}`/`()`/`[]` as a unit,
    /// otherwise removes one character, otherwise merges with the previous
    /// line.
    pub fn backspace(&mut self) {
        self.confirm_quit = false;
        if self.delete_selection_as(EditGroup::Delete) {
            self.desired_col = self.cursor_col;
            return;
        }
        if self.cursor_col == 0 {
            if self.cursor_row == 0 {
                return;
            }
            self.begin_edit(EditGroup::Delete);
            let row = self.cursor_row;
            let current = self.lines.remove(row);
            let prev_len = self.lines[row - 1].chars().count();
            self.lines[row - 1].push_str(&current);
            self.cursor_row = row - 1;
            self.cursor_col = prev_len;
        } else {
            self.begin_edit(EditGroup::Delete);
            let before = self.char_before_cursor();
            let after = self.char_at_cursor();
            // Only pairs the editor would itself have auto-closed.
            let brackets = self.settings.auto_close_brackets;
            let quotes = self.settings.auto_close_quotes;
            let single = quotes && self.settings.auto_close_single_quote;
            let is_pair = match (before, after) {
                (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']')) => {
                    brackets
                }
                (Some('"'), Some('"')) | (Some('`'), Some('`')) => quotes,
                (Some('\''), Some('\'')) => single,
                _ => false,
            };
            if is_pair {
                let row = self.cursor_row;
                self.remove_chars(row, self.cursor_col - 1, self.cursor_col + 1);
                self.cursor_col -= 1;
            } else {
                let row = self.cursor_row;
                self.remove_chars(row, self.cursor_col - 1, self.cursor_col);
                self.cursor_col -= 1;
            }
        }
        self.desired_col = self.cursor_col;
        self.modified = true;
    }

    /// Delete key: removes the character under the cursor, or merges with
    /// the next line if at end of line.
    pub fn delete_forward(&mut self) {
        self.confirm_quit = false;
        if self.delete_selection_as(EditGroup::Delete) {
            self.desired_col = self.cursor_col;
            return;
        }
        let len = self.current_line_char_len();
        if self.cursor_col >= len {
            if self.cursor_row + 1 >= self.lines.len() {
                return;
            }
            self.begin_edit(EditGroup::Delete);
            let row = self.cursor_row;
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
        } else {
            self.begin_edit(EditGroup::Delete);
            let row = self.cursor_row;
            self.remove_chars(row, self.cursor_col, self.cursor_col + 1);
        }
        self.desired_col = self.cursor_col;
        self.modified = true;
    }

    // ---- cursor movement ----

    /// Call at the top of every movement method: breaks undo coalescing,
    /// and either starts a selection (if `select`) or drops one.
    fn begin_move(&mut self, select: bool) {
        self.confirm_quit = false;
        self.reset_edit_group();
        if select {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some((self.cursor_row, self.cursor_col));
            }
        } else {
            self.selection_anchor = None;
        }
    }

    fn move_left_impl(&mut self, select: bool) {
        self.begin_move(select);
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.current_line_char_len();
        }
        self.desired_col = self.cursor_col;
    }

    fn move_right_impl(&mut self, select: bool) {
        self.begin_move(select);
        let len = self.current_line_char_len();
        if self.cursor_col < len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.desired_col = self.cursor_col;
    }

    fn move_up_impl(&mut self, select: bool) {
        self.begin_move(select);
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.desired_col.min(self.current_line_char_len());
        }
    }

    fn move_down_impl(&mut self, select: bool) {
        self.begin_move(select);
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.desired_col.min(self.current_line_char_len());
        }
    }

    fn move_home_impl(&mut self, select: bool) {
        self.begin_move(select);
        self.cursor_col = 0;
        self.desired_col = 0;
    }

    fn move_end_impl(&mut self, select: bool) {
        self.begin_move(select);
        self.cursor_col = self.current_line_char_len();
        self.desired_col = self.cursor_col;
    }

    fn page_up_impl(&mut self, page: usize, select: bool) {
        self.begin_move(select);
        self.cursor_row = self.cursor_row.saturating_sub(page);
        self.cursor_col = self.desired_col.min(self.current_line_char_len());
    }

    fn page_down_impl(&mut self, page: usize, select: bool) {
        self.begin_move(select);
        self.cursor_row = (self.cursor_row + page).min(self.lines.len() - 1);
        self.cursor_col = self.desired_col.min(self.current_line_char_len());
    }

    pub fn move_left(&mut self) {
        self.move_left_impl(false);
    }
    pub fn move_left_select(&mut self) {
        self.move_left_impl(true);
    }
    pub fn move_right(&mut self) {
        self.move_right_impl(false);
    }
    pub fn move_right_select(&mut self) {
        self.move_right_impl(true);
    }
    pub fn move_up(&mut self) {
        self.move_up_impl(false);
    }
    pub fn move_up_select(&mut self) {
        self.move_up_impl(true);
    }
    pub fn move_down(&mut self) {
        self.move_down_impl(false);
    }
    pub fn move_down_select(&mut self) {
        self.move_down_impl(true);
    }
    pub fn move_home(&mut self) {
        self.move_home_impl(false);
    }
    pub fn move_home_select(&mut self) {
        self.move_home_impl(true);
    }
    pub fn move_end(&mut self) {
        self.move_end_impl(false);
    }
    pub fn move_end_select(&mut self) {
        self.move_end_impl(true);
    }
    /// Lines a PageUp/PageDown moves: the `page_size` setting, or one line
    /// short of a screenful when that is 0.
    fn page_lines(&self) -> usize {
        match self.settings.page_size {
            0 => self.viewport_height.saturating_sub(1).max(1),
            n => n,
        }
    }
    pub fn page_up(&mut self) {
        self.page_up_impl(self.page_lines(), false);
    }
    pub fn page_up_select(&mut self) {
        self.page_up_impl(self.page_lines(), true);
    }
    pub fn page_down(&mut self) {
        self.page_down_impl(self.page_lines(), false);
    }
    pub fn page_down_select(&mut self) {
        self.page_down_impl(self.page_lines(), true);
    }

    fn move_word_left_impl(&mut self, select: bool) {
        self.begin_move(select);
        if self.cursor_col == 0 {
            if self.cursor_row > 0 {
                self.cursor_row -= 1;
                self.cursor_col = self.current_line_char_len();
            }
        } else {
            let line = self.current_line().to_string();
            self.cursor_col =
                Self::prev_word_boundary(&line, self.cursor_col, &self.settings.word_chars);
        }
        self.desired_col = self.cursor_col;
    }

    fn move_word_right_impl(&mut self, select: bool) {
        self.begin_move(select);
        let len = self.current_line_char_len();
        if self.cursor_col >= len {
            if self.cursor_row + 1 < self.lines.len() {
                self.cursor_row += 1;
                self.cursor_col = 0;
            }
        } else {
            let line = self.current_line().to_string();
            self.cursor_col =
                Self::next_word_boundary(&line, self.cursor_col, &self.settings.word_chars);
        }
        self.desired_col = self.cursor_col;
    }

    pub fn move_word_left(&mut self) {
        self.move_word_left_impl(false);
    }
    pub fn move_word_left_select(&mut self) {
        self.move_word_left_impl(true);
    }
    pub fn move_word_right(&mut self) {
        self.move_word_right_impl(false);
    }
    pub fn move_word_right_select(&mut self) {
        self.move_word_right_impl(true);
    }

    /// Ctrl+Backspace: deletes back to the previous word boundary, or
    /// merges with the previous line at column 0 (same as plain
    /// backspace there).
    pub fn delete_word_backward(&mut self) {
        self.confirm_quit = false;
        if self.delete_selection_as(EditGroup::Delete) {
            self.desired_col = self.cursor_col;
            return;
        }
        if self.cursor_col == 0 {
            self.backspace();
            return;
        }
        self.begin_edit(EditGroup::Delete);
        let line = self.current_line().to_string();
        let new_col = Self::prev_word_boundary(&line, self.cursor_col, &self.settings.word_chars);
        let row = self.cursor_row;
        self.remove_chars(row, new_col, self.cursor_col);
        self.cursor_col = new_col;
        self.desired_col = self.cursor_col;
        self.modified = true;
    }

    /// Ctrl+Delete: deletes forward to the next word boundary, or merges
    /// with the next line at end-of-line (same as plain delete there).
    pub fn delete_word_forward(&mut self) {
        self.confirm_quit = false;
        if self.delete_selection_as(EditGroup::Delete) {
            self.desired_col = self.cursor_col;
            return;
        }
        let len = self.current_line_char_len();
        if self.cursor_col >= len {
            self.delete_forward();
            return;
        }
        self.begin_edit(EditGroup::Delete);
        let line = self.current_line().to_string();
        let new_col = Self::next_word_boundary(&line, self.cursor_col, &self.settings.word_chars);
        let row = self.cursor_row;
        self.remove_chars(row, self.cursor_col, new_col);
        self.desired_col = self.cursor_col;
        self.modified = true;
    }

    pub fn select_all(&mut self) {
        self.confirm_quit = false;
        self.reset_edit_group();
        self.selection_anchor = Some((0, 0));
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.current_line_char_len();
        self.desired_col = self.cursor_col;
    }

    // ---- clipboard ----

    /// Copies the selection, or the current line if nothing is selected
    /// (matching VS Code's "empty selection" convention). Mirrors to the OS
    /// clipboard on a best-effort basis; always keeps an in-app copy too so
    /// paste still works when the system clipboard is unavailable.
    pub fn copy(&mut self) {
        self.confirm_quit = false;
        let text = if let Some((start, end)) = self.selection_bounds() {
            self.extract_range(start, end)
        } else {
            format!("{}\n", self.lines[self.cursor_row])
        };
        self.clipboard = text.clone();
        self.status_is_error = false;
        self.status_message = match set_system_clipboard(&text) {
            Ok(()) => "Copied".to_string(),
            Err(_) => "Copied (in-app clipboard only)".to_string(),
        };
    }

    pub fn cut(&mut self) {
        self.confirm_quit = false;
        if let Some((start, end)) = self.selection_bounds() {
            let text = self.extract_range(start, end);
            self.clipboard = text.clone();
            let _ = set_system_clipboard(&text);
            self.delete_selection_as(EditGroup::Delete);
            self.status_message = "Cut".to_string();
        } else {
            self.begin_edit(EditGroup::Delete);
            let text = format!("{}\n", self.lines[self.cursor_row]);
            self.clipboard = text.clone();
            let _ = set_system_clipboard(&text);
            if self.lines.len() > 1 {
                self.lines.remove(self.cursor_row);
                if self.cursor_row >= self.lines.len() {
                    self.cursor_row = self.lines.len() - 1;
                }
            } else {
                self.lines[0].clear();
            }
            self.cursor_col = 0;
            self.desired_col = 0;
            self.modified = true;
            self.status_message = "Cut line".to_string();
        }
        self.status_is_error = false;
    }

    /// Pastes from the OS clipboard, falling back to the in-app clipboard
    /// if the system one can't be read. Handles multi-line text.
    pub fn paste(&mut self) {
        self.confirm_quit = false;
        let text = get_system_clipboard().unwrap_or_else(|_| self.clipboard.clone());
        if text.is_empty() {
            self.status_message = "Clipboard empty".to_string();
            self.status_is_error = false;
            return;
        }

        self.last_edit_group = EditGroup::None;
        self.begin_edit(EditGroup::Paste);
        self.delete_selection_as(EditGroup::Paste);

        let parts: Vec<&str> = text.split('\n').collect();
        if parts.len() == 1 {
            for c in parts[0].chars() {
                self.insert_raw(c);
            }
        } else {
            let row = self.cursor_row;
            let bi = Self::byte_idx(&self.lines[row], self.cursor_col);
            let tail = self.lines[row][bi..].to_string();
            self.lines[row].truncate(bi);
            self.lines[row].push_str(parts[0]);
            for (i, part) in parts[1..parts.len() - 1].iter().enumerate() {
                self.lines.insert(row + 1 + i, part.to_string());
            }
            let last_idx = row + parts.len() - 1;
            let mut last_line = parts[parts.len() - 1].to_string();
            last_line.push_str(&tail);
            self.lines.insert(last_idx, last_line);
            self.cursor_row = last_idx;
            self.cursor_col = parts[parts.len() - 1].chars().count();
        }

        self.desired_col = self.cursor_col;
        self.modified = true;
        self.status_message = "Pasted".to_string();
        self.status_is_error = false;
    }

    /// The cursor's horizontal position in screen cells, i.e. with tabs
    /// before it expanded (`cursor_col` itself counts characters).
    pub fn cursor_display_col(&self) -> usize {
        let cols = display_columns(self.current_line(), self.settings.tab_width);
        cols[self.cursor_col.min(cols.len() - 1)]
    }

    /// Scrolls so the cursor is on screen. `col_scroll` is in screen
    /// cells (not characters), so tab-indented lines scroll correctly.
    pub fn ensure_visible(&mut self, width: usize, height: usize) {
        self.viewport_height = height;
        if height > 0 {
            // `scroll_off` lines of context above and below the cursor,
            // but never more than fits (so it can't make the view jump).
            let margin = self.settings.scroll_off.min(height.saturating_sub(1) / 2);
            if self.cursor_row < self.scroll + margin {
                self.scroll = self.cursor_row.saturating_sub(margin);
            } else if self.cursor_row + margin >= self.scroll + height {
                self.scroll = self.cursor_row + margin + 1 - height;
            }
        }
        if width > 0 {
            let col = self.cursor_display_col();
            if col < self.col_scroll {
                self.col_scroll = col;
            } else if col >= self.col_scroll + width {
                self.col_scroll = col + 1 - width;
            }
        }
    }

    // ---- persistence ----

    /// Returns `true` if the save succeeded.
    pub fn save(&mut self) -> bool {
        if self.settings.trim_trailing_whitespace {
            self.trim_trailing_whitespace();
        }
        let crlf = match self.settings.line_ending {
            LineEnding::Lf => false,
            LineEnding::Crlf => true,
            LineEnding::Auto => self.file_crlf,
        };
        match file_io::save(
            &self.file_path,
            &self.lines,
            crlf,
            self.settings.insert_final_newline,
        ) {
            Ok(()) => {
                self.modified = false;
                self.confirm_quit = false;
                self.status_message = format!("Saved {}", self.file_path.display());
                self.status_is_error = false;
                true
            }
            Err(e) => {
                self.status_message = format!("Save failed: {e}");
                self.status_is_error = true;
                false
            }
        }
    }

    /// Strips trailing spaces and tabs from every line, keeping the cursor
    /// and selection on valid columns.
    fn trim_trailing_whitespace(&mut self) {
        for line in &mut self.lines {
            let kept = line.trim_end_matches([' ', '\t']).len();
            line.truncate(kept);
        }
        self.clamp_cursor();
        let lines = &self.lines;
        if let Some((row, col)) = self.selection_anchor.as_mut() {
            *row = (*row).min(lines.len() - 1);
            *col = (*col).min(lines[*row].chars().count());
        }
    }

    // ---- diagnostics (Ctrl+S check) ----

    pub fn diagnostics_for_line(&self, row: usize) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(move |d| d.line == row)
    }

    pub fn set_diagnostics(&mut self, diagnostics: Vec<Diagnostic>) {
        self.diagnostics = diagnostics;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_editor() -> Editor {
        Editor::open(PathBuf::from("__test_buffer__.cs")).unwrap()
    }

    fn new_python_editor() -> Editor {
        Editor::open(PathBuf::from("__test_buffer__.py")).unwrap()
    }

    fn new_editor_with_ext(ext: &str) -> Editor {
        Editor::open(PathBuf::from(format!("__test_buffer__.{ext}"))).unwrap()
    }

    fn type_str(ed: &mut Editor, s: &str) {
        for c in s.chars() {
            ed.insert_char(c);
        }
    }

    #[test]
    fn curly_brace_autocloses() {
        let mut ed = new_editor();
        ed.insert_char('{');
        assert_eq!(ed.lines[0], "{}");
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn paren_and_bracket_autoclose() {
        let mut ed = new_editor();
        ed.insert_char('(');
        assert_eq!(ed.lines[0], "()");
        assert_eq!(ed.cursor_col, 1);

        let mut ed = new_editor();
        ed.insert_char('[');
        assert_eq!(ed.lines[0], "[]");
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn typing_closer_types_over_instead_of_duplicating() {
        let mut ed = new_editor();
        ed.insert_char('(');
        // cursor is between ( and ), typing ')' should just move over it
        ed.insert_char(')');
        assert_eq!(ed.lines[0], "()");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn double_and_single_quotes_autoclose() {
        let mut ed = new_editor();
        ed.insert_char('"');
        assert_eq!(ed.lines[0], "\"\"");
        assert_eq!(ed.cursor_col, 1);

        let mut ed = new_editor();
        ed.insert_char('\'');
        assert_eq!(ed.lines[0], "''");
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn typing_quote_over_existing_close_types_over() {
        let mut ed = new_editor();
        ed.insert_char('"');
        ed.insert_char('"');
        assert_eq!(ed.lines[0], "\"\"");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn typing_quote_to_close_an_open_string_does_not_autoclose_again() {
        let mut ed = new_editor();
        type_str(&mut ed, "\"hello");
        // cursor is now after "hello, i.e. not adjacent to a matching
        // closer — closing the string should just insert one quote.
        ed.insert_char('"');
        assert_eq!(ed.lines[0], "\"hello\"");
        assert_eq!(ed.cursor_col, 7);
    }

    #[test]
    fn backspace_deletes_empty_quote_pair_as_unit() {
        let mut ed = new_editor();
        ed.insert_char('"');
        ed.backspace();
        assert_eq!(ed.lines[0], "");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn ctrl_backspace_deletes_previous_word() {
        let mut ed = new_editor();
        type_str(&mut ed, "int foo = bar();");
        ed.delete_word_backward(); // removes trailing "();"
        assert_eq!(ed.lines[0], "int foo = bar");
        ed.delete_word_backward(); // removes "bar"
        assert_eq!(ed.lines[0], "int foo = ");
        // trailing whitespace and the punctuation run before it ("= ") go
        // together in one press, same as most editors' Ctrl+Backspace
        ed.delete_word_backward();
        assert_eq!(ed.lines[0], "int foo ");
    }

    #[test]
    fn ctrl_backspace_at_line_start_merges_with_previous_line() {
        let mut ed = new_editor();
        type_str(&mut ed, "abc");
        ed.newline();
        type_str(&mut ed, "def");
        ed.move_home();
        ed.delete_word_backward();
        assert_eq!(ed.lines.len(), 1);
        assert_eq!(ed.lines[0], "abcdef");
    }

    #[test]
    fn ctrl_delete_deletes_next_word() {
        let mut ed = new_editor();
        type_str(&mut ed, "foo bar");
        ed.move_home();
        ed.delete_word_forward(); // removes "foo"
        assert_eq!(ed.lines[0], " bar");
    }

    #[test]
    fn ctrl_left_right_jump_by_word() {
        let mut ed = new_editor();
        type_str(&mut ed, "foo.bar(baz)");
        ed.move_home();
        ed.move_word_right(); // -> end of "foo"
        assert_eq!(ed.cursor_col, 3);
        ed.move_word_right(); // -> end of "."
        assert_eq!(ed.cursor_col, 4);
        ed.move_word_right(); // -> end of "bar"
        assert_eq!(ed.cursor_col, 7);
        ed.move_word_left(); // -> start of "bar"
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn ctrl_shift_left_selects_by_word() {
        let mut ed = new_editor();
        type_str(&mut ed, "hello world");
        ed.move_end();
        ed.move_word_left_select();
        assert_eq!(ed.selection_bounds(), Some(((0, 6), (0, 11))));
    }

    #[test]
    fn smart_enter_splits_curly_block_allman_style() {
        let mut ed = new_editor();
        type_str(&mut ed, "void Foo() {");
        // cursor sits between the auto-inserted { and }
        assert_eq!(ed.lines[0], "void Foo() {}");
        ed.newline();

        assert_eq!(ed.lines.len(), 4);
        assert_eq!(ed.lines[0], "void Foo()");
        assert_eq!(ed.lines[1], "{");
        assert_eq!(ed.lines[2], "    ");
        assert_eq!(ed.lines[3], "}");
        assert_eq!(ed.cursor_row, 2);
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn smart_enter_respects_existing_indent() {
        let mut ed = new_editor();
        type_str(&mut ed, "class C ");
        ed.insert_char('{');
        ed.newline(); // -> class C\n{\n    \n}
                      // cursor is already on the indented empty line; type the method there
        type_str(&mut ed, "void Foo() ");
        ed.insert_char('{');
        ed.newline();

        assert_eq!(
            ed.lines,
            vec![
                "class C".to_string(),
                "{".to_string(),
                "    void Foo()".to_string(),
                "    {".to_string(),
                "        ".to_string(),
                "    }".to_string(),
                "}".to_string(),
            ]
        );
        assert_eq!(ed.cursor_row, 4);
        assert_eq!(ed.cursor_col, 8);
    }

    #[test]
    fn plain_enter_autoindents_without_splitting() {
        let mut ed = new_editor();
        type_str(&mut ed, "    int x = 1;");
        ed.newline();
        assert_eq!(ed.lines[0], "    int x = 1;");
        assert_eq!(ed.lines[1], "    ");
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn enter_after_open_paren_without_matching_close_just_deepens_indent() {
        let mut ed = new_editor();
        type_str(&mut ed, "foo(");
        ed.move_right(); // step past the auto-inserted ')'
        ed.backspace(); // remove it, simulating no matching closer present
        ed.newline();
        assert_eq!(ed.lines[0], "foo(");
        assert_eq!(ed.lines[1], "    ");
    }

    #[test]
    fn backspace_deletes_empty_pair_as_unit() {
        let mut ed = new_editor();
        ed.insert_char('{');
        ed.backspace();
        assert_eq!(ed.lines[0], "");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn backspace_merges_with_previous_line() {
        let mut ed = new_editor();
        type_str(&mut ed, "abc");
        ed.newline();
        type_str(&mut ed, "def");
        ed.move_home();
        ed.backspace();
        assert_eq!(ed.lines.len(), 1);
        assert_eq!(ed.lines[0], "abcdef");
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn undo_redo_roundtrip_for_typing() {
        let mut ed = new_editor();
        type_str(&mut ed, "hello");
        assert_eq!(ed.lines[0], "hello");
        ed.undo();
        assert_eq!(ed.lines[0], "");
        ed.redo();
        assert_eq!(ed.lines[0], "hello");
    }

    #[test]
    fn undo_restores_pre_smart_enter_state() {
        let mut ed = new_editor();
        type_str(&mut ed, "void Foo() {");
        ed.newline();
        assert_eq!(ed.lines.len(), 4);
        ed.undo();
        assert_eq!(ed.lines.len(), 1);
        assert_eq!(ed.lines[0], "void Foo() {}");
    }

    #[test]
    fn tab_inserts_four_spaces() {
        let mut ed = new_editor();
        ed.insert_tab();
        assert_eq!(ed.lines[0], "    ");
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn char_count_sums_all_lines_excluding_line_terminators() {
        let mut ed = new_editor();
        assert_eq!(ed.char_count(), 0);
        type_str(&mut ed, "abc");
        ed.newline();
        type_str(&mut ed, "de");
        assert_eq!(ed.char_count(), 5); // "abc" + "de", newline not counted
    }

    #[test]
    fn delete_forward_merges_next_line() {
        let mut ed = new_editor();
        type_str(&mut ed, "abc");
        ed.newline();
        type_str(&mut ed, "def");
        ed.move_up();
        ed.move_end();
        ed.delete_forward();
        assert_eq!(ed.lines.len(), 1);
        assert_eq!(ed.lines[0], "abcdef");
    }

    #[test]
    fn shift_arrows_build_a_selection_not_including_gutter() {
        let mut ed = new_editor();
        type_str(&mut ed, "hello");
        ed.move_home();
        ed.move_right_select();
        ed.move_right_select();
        ed.move_right_select();
        // selection only ever indexes into ed.lines char content, never the
        // rendered line-number gutter, so it can only ever cover "hel".
        assert_eq!(ed.selection_col_range(0), Some(0..3));
        assert_eq!(ed.selection_bounds(), Some(((0, 0), (0, 3))));
    }

    #[test]
    fn plain_arrow_key_clears_selection() {
        let mut ed = new_editor();
        type_str(&mut ed, "hello");
        ed.move_home();
        ed.move_right_select();
        assert!(ed.has_selection());
        ed.move_right();
        assert!(!ed.has_selection());
    }

    #[test]
    fn select_all_selects_entire_buffer() {
        let mut ed = new_editor();
        type_str(&mut ed, "abc");
        ed.newline();
        type_str(&mut ed, "de");
        ed.select_all();
        assert_eq!(ed.selection_bounds(), Some(((0, 0), (1, 2))));
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut ed = new_editor();
        type_str(&mut ed, "hello world");
        ed.move_home();
        for _ in 0..5 {
            ed.move_right_select();
        }
        ed.insert_char('X');
        assert_eq!(ed.lines[0], "X world");
        assert!(!ed.has_selection());
    }

    #[test]
    fn backspace_with_selection_deletes_whole_selection() {
        let mut ed = new_editor();
        type_str(&mut ed, "hello world");
        ed.move_home();
        for _ in 0..5 {
            ed.move_right_select();
        }
        ed.backspace();
        assert_eq!(ed.lines[0], " world");
    }

    #[test]
    fn copy_then_paste_roundtrips_selection() {
        let mut ed = new_editor();
        type_str(&mut ed, "hello world");
        ed.move_home();
        for _ in 0..5 {
            ed.move_right_select();
        }
        ed.copy();
        assert!(
            ed.has_selection(),
            "copy should not clear the active selection"
        );
        ed.move_end();
        ed.paste();
        assert_eq!(ed.lines[0], "hello worldhello");
    }

    #[test]
    fn cut_removes_selection_and_fills_clipboard() {
        let mut ed = new_editor();
        type_str(&mut ed, "hello world");
        ed.move_home();
        for _ in 0..5 {
            ed.move_right_select();
        }
        ed.cut();
        assert_eq!(ed.lines[0], " world");
        ed.move_end();
        ed.paste();
        assert_eq!(ed.lines[0], " worldhello");
    }

    #[test]
    fn paste_multiline_splits_into_lines() {
        let mut ed = new_editor();
        ed.clipboard = "foo\nbar".to_string();
        type_str(&mut ed, "()");
        ed.move_home();
        ed.paste();
        assert_eq!(ed.lines[0], "foo");
        assert_eq!(ed.lines[1], "bar()");
    }

    // ---- Python language support ----

    #[test]
    fn python_enter_deepens_after_colon() {
        let mut ed = new_python_editor();
        type_str(&mut ed, "def foo():");
        ed.newline();
        assert_eq!(ed.lines, vec!["def foo():".to_string(), "    ".to_string()]);
        assert_eq!(ed.cursor_row, 1);
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn python_curly_braces_do_not_allman_split() {
        // {} in Python is a dict/set literal, not a block — Enter between
        // an autoclosed pair should just deepen-indent, not explode into
        // the C#-style four-line Allman split.
        let mut ed = new_python_editor();
        type_str(&mut ed, "d = {");
        assert_eq!(ed.lines[0], "d = {}");
        ed.newline();
        assert_eq!(ed.lines, vec!["d = {".to_string(), "    }".to_string()]);
        assert_eq!(ed.cursor_row, 1);
        assert_eq!(ed.cursor_col, 4);
    }

    // ---- smart Tab / Shift+Tab block indent ----

    #[test]
    fn tab_indents_full_selected_lines() {
        let mut ed = new_editor();
        type_str(&mut ed, "abc");
        ed.newline();
        type_str(&mut ed, "def");
        ed.select_all();
        ed.insert_tab();
        assert_eq!(ed.lines, vec!["    abc".to_string(), "    def".to_string()]);
        assert_eq!(ed.cursor_col, 7);
        assert!(ed.has_selection());
    }

    #[test]
    fn shift_tab_dedents_selected_lines() {
        let mut ed = new_editor();
        type_str(&mut ed, "    abc");
        ed.newline();
        type_str(&mut ed, "def");
        ed.select_all();
        ed.dedent();
        assert_eq!(ed.lines, vec!["abc".to_string(), "def".to_string()]);
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn dedent_line_without_selection_removes_up_to_one_indent_level() {
        let mut ed = new_editor();
        type_str(&mut ed, "      abc"); // 6 leading spaces
        ed.move_home();
        ed.dedent();
        assert_eq!(ed.lines[0], "  abc"); // only one 4-space level removed
        assert_eq!(ed.cursor_col, 0);
    }

    // ---- JavaScript / Rust: Allman brace split, same as C# ----

    #[test]
    fn javascript_enter_uses_allman_split() {
        let mut ed = new_editor_with_ext("js");
        type_str(&mut ed, "function foo() {");
        assert_eq!(ed.lines[0], "function foo() {}");
        ed.newline();
        assert_eq!(
            ed.lines,
            vec![
                "function foo()".to_string(),
                "{".to_string(),
                "    ".to_string(),
                "}".to_string(),
            ]
        );
        assert_eq!(ed.cursor_row, 2);
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn rust_enter_uses_allman_split() {
        let mut ed = new_editor_with_ext("rs");
        type_str(&mut ed, "fn foo() {");
        assert_eq!(ed.lines[0], "fn foo() {}");
        ed.newline();
        assert_eq!(
            ed.lines,
            vec![
                "fn foo()".to_string(),
                "{".to_string(),
                "    ".to_string(),
                "}".to_string()
            ]
        );
        assert_eq!(ed.cursor_row, 2);
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn rust_single_quote_does_not_autoclose() {
        // Rust uses `'` constantly for lifetimes (`&'a str`); autoclosing
        // it into `''` would get in the way, so it should insert plainly.
        let mut ed = new_editor_with_ext("rs");
        type_str(&mut ed, "&'a str");
        assert_eq!(ed.lines[0], "&'a str");
        assert_eq!(ed.cursor_col, 7);
    }

    #[test]
    fn other_languages_still_autoclose_single_quote() {
        let mut ed = new_editor_with_ext("py");
        ed.insert_char('\'');
        assert_eq!(ed.lines[0], "''");
        assert_eq!(ed.cursor_col, 1);
    }

    // ---- backtick (template literal) pairing, any language ----

    #[test]
    fn backtick_autocloses_and_types_over() {
        let mut ed = new_editor_with_ext("js");
        ed.insert_char('`');
        assert_eq!(ed.lines[0], "``");
        assert_eq!(ed.cursor_col, 1);
        ed.insert_char('`');
        assert_eq!(ed.lines[0], "``");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn backspace_deletes_empty_backtick_pair_as_unit() {
        let mut ed = new_editor_with_ext("js");
        ed.insert_char('`');
        ed.backspace();
        assert_eq!(ed.lines[0], "");
        assert_eq!(ed.cursor_col, 0);
    }

    // ---- GDScript: Python-style colon blocks, no brace split ----

    #[test]
    fn gdscript_enter_deepens_after_colon_with_a_tab() {
        // New .gd files are tab-indented (Godot's own style, and mixing
        // tabs with spaces is a parse error there).
        let mut ed = new_editor_with_ext("gd");
        type_str(&mut ed, "func _ready():");
        ed.newline();
        assert_eq!(
            ed.lines,
            vec!["func _ready():".to_string(), "\t".to_string()]
        );
        assert_eq!(ed.cursor_row, 1);
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn gdscript_curly_braces_do_not_split() {
        let mut ed = new_editor_with_ext("gd");
        type_str(&mut ed, "var d = {");
        assert_eq!(ed.lines[0], "var d = {}");
        ed.newline();
        assert_eq!(ed.lines, vec!["var d = {".to_string(), "\t}".to_string()]);
        assert_eq!(ed.cursor_row, 1);
        assert_eq!(ed.cursor_col, 1);
    }

    // ---- tabs: indent style detection, display columns ----

    /// An editor for `__test_buffer__.<ext>` holding `lines`, with the
    /// indent style detected from them as `Editor::open` would.
    fn editor_with_lines(ext: &str, lines: &[&str]) -> Editor {
        let mut ed = new_editor_with_ext(ext);
        ed.lines = lines.iter().map(|l| l.to_string()).collect();
        ed.indent = choose_indent(&ed.lines, ed.language, &ed.settings);
        ed
    }

    #[test]
    fn indent_style_follows_the_file_when_it_has_one() {
        assert_eq!(
            editor_with_lines("cs", &["class A", "{", "\tint x;"]).indent,
            "\t"
        );
        assert_eq!(
            editor_with_lines("gd", &["func f():", "    pass"]).indent,
            "    "
        );
        // A lone leading space is a block comment's ` * `, not indentation.
        assert_eq!(
            editor_with_lines("cs", &["/**", " * doc", " */"]).indent,
            "    "
        );
    }

    #[test]
    fn indent_style_defaults_by_language_for_empty_files() {
        assert_eq!(new_editor_with_ext("gd").indent, "\t");
        assert_eq!(new_editor_with_ext("cs").indent, "    ");
        assert_eq!(new_editor_with_ext("py").indent, "    ");
        assert_eq!(new_editor_with_ext("rs").indent, "    ");
    }

    #[test]
    fn enter_in_a_tab_indented_file_never_mixes_in_spaces() {
        let mut ed = editor_with_lines("gd", &["func f():", "\tif x:"]);
        ed.cursor_row = 1;
        ed.cursor_col = 6; // end of "\tif x:"
        ed.newline();
        assert_eq!(ed.lines[2], "\t\t");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn tab_key_inserts_the_files_indent_unit() {
        // Tab-indented file: Tab inserts one real tab character.
        let mut ed = editor_with_lines("gd", &["\tfirst", "second"]);
        ed.cursor_row = 1;
        ed.insert_tab();
        assert_eq!(ed.lines[1], "\tsecond");
        assert_eq!(ed.cursor_col, 1);

        // Space-indented file: Tab inserts four spaces.
        let mut spaces = editor_with_lines("py", &["    first", "second"]);
        spaces.cursor_row = 1;
        spaces.insert_tab();
        assert_eq!(spaces.lines[1], "    second");
        assert_eq!(spaces.cursor_col, 4);
    }

    #[test]
    fn block_indent_and_dedent_use_tabs_in_a_tab_file() {
        let mut ed = editor_with_lines("gd", &["\ta", "\tb"]);
        ed.select_all();
        ed.insert_tab();
        assert_eq!(ed.lines, vec!["\t\ta".to_string(), "\t\tb".to_string()]);
        ed.dedent();
        ed.dedent();
        assert_eq!(ed.lines, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn display_columns_expand_tabs_to_the_next_tab_stop() {
        // '\t' at col 0 -> 4 wide, 'a','b' at 4,5, '\t' at 6 -> 2 wide,
        // 'c' at 8, end at 9.
        assert_eq!(display_columns("\tab\tc", 4), vec![0, 4, 5, 6, 8, 9]);
        assert_eq!(display_columns("", 4), vec![0]);
        assert_eq!(display_columns("abc", 4), vec![0, 1, 2, 3]);
    }

    #[test]
    fn cursor_display_col_accounts_for_tabs_before_it() {
        let mut ed = editor_with_lines("gd", &["\t\tprint(1)"]);
        ed.cursor_col = 2; // right after the two tabs
        assert_eq!(ed.cursor_display_col(), 8);
        ed.cursor_col = 3; // after the 'p'
        assert_eq!(ed.cursor_display_col(), 9);
    }

    #[test]
    fn horizontal_scroll_is_measured_in_screen_cells() {
        // 10 tabs = 40 cells wide; cursor at the end must scroll to show
        // cell 40 even though the line is only 10 characters long.
        let mut ed = editor_with_lines("gd", &["\t\t\t\t\t\t\t\t\t\t"]);
        ed.cursor_col = 10;
        ed.ensure_visible(20, 5);
        assert_eq!(ed.col_scroll, 40 + 1 - 20);
    }

    // ---- HTML / XML: smart tags ----

    #[test]
    fn html_typing_gt_autocloses_the_tag_and_enter_opens_it_up() {
        let mut ed = new_editor_with_ext("html");
        type_str(&mut ed, "<div>");
        assert_eq!(ed.lines[0], "<div></div>");
        assert_eq!(ed.cursor_col, 5, "cursor sits between the tags");

        ed.newline();
        assert_eq!(
            ed.lines,
            vec![
                "<div>".to_string(),
                "    ".to_string(),
                "</div>".to_string()
            ]
        );
        assert_eq!((ed.cursor_row, ed.cursor_col), (1, 4));
    }

    #[test]
    fn html_nested_tags_each_autoclose() {
        let mut ed = new_editor_with_ext("html");
        type_str(&mut ed, "<ul>");
        type_str(&mut ed, "<li class=\"a\">");
        assert_eq!(ed.lines[0], "<ul><li class=\"a\"></li></ul>");
        type_str(&mut ed, "x");
        assert_eq!(ed.lines[0], "<ul><li class=\"a\">x</li></ul>");
    }

    #[test]
    fn html_does_not_autoclose_void_closing_doctype_or_comments() {
        for typed in [
            "<br>",
            "<img src=\"a.png\">",
            "<br/>",
            "</div>",
            "<!DOCTYPE html>",
            "<!-- note -->",
            "1 < 2 > 0",
        ] {
            let mut ed = new_editor_with_ext("html");
            type_str(&mut ed, typed);
            assert_eq!(ed.lines[0], typed, "typing {typed:?}");
        }
    }

    #[test]
    fn xml_autocloses_every_opening_tag_even_html_void_names() {
        let mut ed = new_editor_with_ext("xml");
        type_str(&mut ed, "<br>");
        assert_eq!(ed.lines[0], "<br></br>");

        let mut proj = new_editor_with_ext("csproj");
        type_str(&mut proj, "<ItemGroup>");
        assert_eq!(proj.lines[0], "<ItemGroup></ItemGroup>");
    }

    #[test]
    fn enter_in_html_without_a_tag_pair_is_plain_autoindent() {
        let mut ed = new_editor_with_ext("html");
        type_str(&mut ed, "<p>text");
        assert_eq!(ed.lines[0], "<p>text</p>");

        // Mid-line, Enter carries the rest of the line down...
        ed.newline();
        assert_eq!(ed.lines, vec!["<p>text".to_string(), "</p>".to_string()]);

        // ...and at the end of a line with no tag pair around the cursor
        // it's plain autoindent, not a tag split.
        let mut ed = editor_with_lines("html", &["  <p>text</p>"]);
        ed.cursor_col = 13;
        ed.newline();
        assert_eq!(ed.lines[1], "  ");
    }

    #[test]
    fn tags_are_not_autoclosed_outside_html_and_xml() {
        let mut ed = new_editor_with_ext("cs");
        type_str(&mut ed, "List<int>");
        assert_eq!(ed.lines[0], "List<int>");
    }

    // ---- JSON / CSS brace styles ----

    #[test]
    fn json_enter_between_braces_and_brackets_is_a_three_line_split() {
        let mut obj = new_editor_with_ext("json");
        type_str(&mut obj, "{");
        obj.newline();
        assert_eq!(
            obj.lines,
            vec!["{".to_string(), "    ".to_string(), "}".to_string()]
        );
        assert_eq!((obj.cursor_row, obj.cursor_col), (1, 4));

        let mut arr = new_editor_with_ext("json");
        type_str(&mut arr, "[");
        arr.newline();
        assert_eq!(
            arr.lines,
            vec!["[".to_string(), "    ".to_string(), "]".to_string()]
        );
    }

    #[test]
    fn brackets_only_split_for_json_not_for_allman_languages() {
        let mut ed = new_editor_with_ext("cs");
        type_str(&mut ed, "var a = [");
        ed.newline();
        // Generic deepen: `[` stays, the `]` lands on the new indented line.
        assert_eq!(ed.lines[0], "var a = [");
        assert_eq!(ed.lines[1], "    ]");
    }

    #[test]
    fn css_enter_between_braces_is_allman_like_the_other_brace_languages() {
        let mut ed = new_editor_with_ext("css");
        type_str(&mut ed, ".box {");
        ed.newline();
        assert_eq!(
            ed.lines,
            vec![
                ".box".to_string(),
                "{".to_string(),
                "    ".to_string(),
                "}".to_string()
            ]
        );
    }

    // ---- YAML / Markdown ----

    #[test]
    fn yaml_indents_by_two_and_deepens_after_a_colon() {
        let mut ed = new_editor_with_ext("yaml");
        assert_eq!(ed.indent, "  ");
        type_str(&mut ed, "services:");
        ed.newline();
        assert_eq!(ed.lines[1], "  ");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn yaml_never_indents_with_tabs_even_if_the_file_does() {
        let ed = editor_with_lines("yml", &["a:", "\tb: 1"]);
        assert_eq!(ed.indent, "  ");
    }

    #[test]
    fn markdown_apostrophes_are_not_autoclosed() {
        let mut ed = new_editor_with_ext("md");
        type_str(&mut ed, "don't stop");
        assert_eq!(ed.lines[0], "don't stop");
    }

    // ---- indent width follows the file ----

    #[test]
    fn indent_width_is_the_narrowest_indentation_in_the_file() {
        let two = editor_with_lines(
            "html",
            &["<div>", "  <p>", "    <b>x</b>", "  </p>", "</div>"],
        );
        assert_eq!(two.indent, "  ");
        let four = editor_with_lines(
            "cs",
            &[
                "class A",
                "{",
                "    void F()",
                "    {",
                "        x();",
                "    }",
                "}",
            ],
        );
        assert_eq!(four.indent, "    ");
        let eight = editor_with_lines("cs", &["a", "        b", "                c"]);
        assert_eq!(eight.indent, "        ");
        // An odd width (alignment, not indentation) falls back to the default.
        let odd = editor_with_lines("cs", &["a", "     b"]);
        assert_eq!(odd.indent, "    ");
    }

    #[test]
    fn enter_and_dedent_use_the_files_two_space_width() {
        // Enter after an open bracket deepens by the file's own width.
        let mut json = editor_with_lines("json", &["{", "  \"k\": 1", "}"]);
        json.cursor_row = 0;
        json.cursor_col = 1; // right after the `{`
        json.newline();
        assert_eq!(json.lines[1], "  ", "two spaces, not four");

        // Shift+Tab removes one two-space level.
        let mut yaml = editor_with_lines("yaml", &["a:", "  b:", "    c: 1"]);
        yaml.cursor_row = 2;
        yaml.cursor_col = 0;
        yaml.dedent();
        assert_eq!(yaml.lines[2], "  c: 1", "removes 2 spaces, not 4");
    }
}

/// Every editing setting, checked against the behavior it controls.
#[cfg(test)]
mod settings_tests {
    use super::*;
    use crate::config::{BraceStyle, Config, LanguageOverride};

    fn temp(name: &str) -> PathBuf {
        // The extension must stay last: it decides the language.
        std::env::temp_dir().join(format!("pc_settings_{}_{name}", std::process::id()))
    }

    /// An empty buffer for `file_name` with `tweak` applied to the config.
    fn editor(file_name: &str, tweak: impl FnOnce(&mut Config)) -> Editor {
        let mut config = Config::default();
        tweak(&mut config);
        let path = PathBuf::from(format!("__nonexistent__/{file_name}"));
        let language = config.language_of(&path);
        let settings = Settings::resolve(&config, language);
        Editor::open_with(path, language, settings).unwrap()
    }

    /// A buffer loaded from real content.
    fn editor_with_content(
        file_name: &str,
        content: &str,
        tweak: impl FnOnce(&mut Config),
    ) -> Editor {
        let path = temp(file_name);
        std::fs::write(&path, content).unwrap();
        let mut config = Config::default();
        tweak(&mut config);
        let language = config.language_of(&path);
        let settings = Settings::resolve(&config, language);
        let ed = Editor::open_with(path.clone(), language, settings).unwrap();
        let _ = std::fs::remove_file(path);
        ed
    }

    fn type_str(ed: &mut Editor, s: &str) {
        for c in s.chars() {
            ed.insert_char(c);
        }
    }

    // ---- display ----

    #[test]
    fn tab_width_sets_how_far_a_tab_reaches() {
        assert_eq!(display_columns("\tx", 2), vec![0, 2, 3]);
        assert_eq!(display_columns("\tx", 8), vec![0, 8, 9]);
        assert_eq!(
            display_columns("ab\tx", 4),
            vec![0, 1, 2, 4, 5],
            "next stop, not a fixed width"
        );

        let mut ed = editor("a.rs", |c| c.editor.tab_width = 8);
        ed.lines[0] = "\tab".to_string();
        ed.cursor_col = 1;
        assert_eq!(ed.cursor_display_col(), 8);
        let mut ed = editor("a.rs", |c| c.editor.tab_width = 2);
        ed.lines[0] = "\tab".to_string();
        ed.cursor_col = 1;
        assert_eq!(ed.cursor_display_col(), 2);
    }

    #[test]
    fn a_language_can_have_its_own_tab_width() {
        let ed = editor("a.py", |c| {
            c.languages.insert(
                "python".into(),
                LanguageOverride {
                    tab_width: Some(2),
                    ..Default::default()
                },
            );
        });
        assert_eq!(ed.tab_width(), 2);
        assert_eq!(editor("a.rs", |_| {}).tab_width(), 4);
    }

    // ---- indentation ----

    #[test]
    fn indent_style_and_width_decide_what_tab_inserts() {
        let mut ed = editor("a.rs", |c| c.editor.indent_style = IndentStyle::Tabs);
        ed.insert_tab();
        assert_eq!(ed.lines[0], "\t");

        let mut ed = editor("a.rs", |c| {
            c.editor.indent_style = IndentStyle::Spaces;
            c.editor.indent_width = Some(2);
        });
        ed.insert_tab();
        assert_eq!(ed.lines[0], "  ");

        // Auto with nothing in the file to learn from: indent_width alone.
        let mut ed = editor("a.rs", |c| c.editor.indent_width = Some(3));
        ed.insert_tab();
        assert_eq!(ed.lines[0], "   ");

        // Spaces is spaces even for GDScript, whose own default is a tab.
        let mut ed = editor("a.gd", |c| c.editor.indent_style = IndentStyle::Spaces);
        ed.insert_tab();
        assert_eq!(ed.lines[0], "    ");
    }

    #[test]
    fn detect_indentation_lets_the_file_win_unless_switched_off() {
        let content = "fn a() {\n\tx;\n}\n";
        let mut learned = editor_with_content("detect_on.rs", content, |_| {});
        learned.insert_tab();
        assert_eq!(learned.lines[0], "\tfn a() {", "followed the file's tab");

        let mut ignored = editor_with_content("detect_off.rs", content, |c| {
            c.editor.detect_indentation = false
        });
        ignored.insert_tab();
        assert_eq!(
            ignored.lines[0], "    fn a() {",
            "used the language default"
        );

        // Forcing the style beats detection too.
        let mut forced = editor_with_content("detect_forced.rs", content, |c| {
            c.editor.indent_style = IndentStyle::Spaces;
        });
        forced.insert_tab();
        assert_eq!(forced.lines[0], "    fn a() {");
    }

    #[test]
    fn auto_indent_off_starts_every_line_at_column_zero() {
        let mut on = editor("a.rs", |_| {});
        type_str(&mut on, "    foo");
        on.newline();
        assert_eq!(on.lines[1], "    ");

        let mut off = editor("a.rs", |c| c.editor.auto_indent = false);
        type_str(&mut off, "    foo");
        off.newline();
        assert_eq!(off.lines[1], "");
        assert_eq!(off.cursor_col, 0);

        // ...including after an opening bracket.
        let mut off = editor("a.py", |c| c.editor.auto_indent = false);
        type_str(&mut off, "if x:");
        off.newline();
        assert_eq!(off.lines[1], "");
    }

    #[test]
    fn auto_indent_off_also_drops_the_indent_of_the_smart_brace_split() {
        let mut ed = editor("a.cs", |c| c.editor.auto_indent = false);
        type_str(&mut ed, "void F() {");
        ed.newline();
        assert_eq!(ed.lines, vec!["void F()", "{", "", "}"]);
    }

    #[test]
    fn smart_enter_off_gives_a_plain_newline_between_braces() {
        let mut ed = editor("a.cs", |c| c.editor.smart_enter = false);
        type_str(&mut ed, "void F() {");
        ed.newline();
        // No Allman split: the line is just broken in two.
        assert_eq!(ed.lines, vec!["void F() {", "    }"]);

        let mut html = editor("a.html", |c| c.editor.smart_enter = false);
        type_str(&mut html, "<div>");
        html.cursor_col = 5;
        html.newline();
        assert_eq!(html.lines.len(), 2, "no tag split either: {:?}", html.lines);
    }

    #[test]
    fn brace_style_changes_how_enter_splits_braces() {
        let mut allman = editor("a.cs", |_| {});
        type_str(&mut allman, "void F() {");
        allman.newline();
        assert_eq!(allman.lines, vec!["void F()", "{", "    ", "}"]);

        let mut kr = editor("a.cs", |c| c.editor.brace_style = BraceStyle::KAndR);
        type_str(&mut kr, "void F() {");
        kr.newline();
        assert_eq!(kr.lines, vec!["void F() {", "    ", "}"]);

        let mut off = editor("a.cs", |c| c.editor.brace_style = BraceStyle::Off);
        type_str(&mut off, "void F() {");
        off.newline();
        assert_eq!(off.lines, vec!["void F() {", "    }"]);

        let mut forced = editor("a.py", |c| c.editor.brace_style = BraceStyle::Allman);
        type_str(&mut forced, "d = {");
        forced.newline();
        assert_eq!(
            forced.lines,
            vec!["d =", "{", "    ", "}"],
            "Python normally never splits"
        );
    }

    #[test]
    fn a_lone_brace_pair_does_not_leave_a_blank_line_above_it() {
        let mut ed = editor("a.cs", |_| {});
        type_str(&mut ed, "{");
        ed.newline();
        assert_eq!(ed.lines, vec!["{", "    ", "}"], "{:?}", ed.lines);
    }

    // ---- auto-closing ----

    #[test]
    fn auto_close_brackets_off_types_only_what_you_type() {
        let mut ed = editor("a.rs", |c| c.editor.auto_close_brackets = false);
        type_str(&mut ed, "f(");
        assert_eq!(ed.lines[0], "f(");
        type_str(&mut ed, ")");
        assert_eq!(ed.lines[0], "f()", "no type-over either");
        let mut ed = editor("a.rs", |c| c.editor.auto_close_brackets = false);
        type_str(&mut ed, "{[");
        assert_eq!(ed.lines[0], "{[");
        // Quotes are a separate switch.
        let mut ed = editor("a.rs", |c| c.editor.auto_close_brackets = false);
        type_str(&mut ed, "\"");
        assert_eq!(ed.lines[0], "\"\"");
    }

    #[test]
    fn auto_close_quotes_off_disables_every_kind_of_quote() {
        let mut ed = editor("a.cs", |c| c.editor.auto_close_quotes = false);
        type_str(&mut ed, "\"'`");
        assert_eq!(ed.lines[0], "\"'`");
        let mut ed = editor("a.cs", |c| c.editor.auto_close_quotes = false);
        type_str(&mut ed, "(");
        assert_eq!(ed.lines[0], "()", "brackets are a separate switch");
    }

    #[test]
    fn single_quote_autoclose_follows_the_language_unless_set() {
        // Rust: `'a` is a lifetime, so off by default...
        let mut rust = editor("a.rs", |_| {});
        type_str(&mut rust, "'");
        assert_eq!(rust.lines[0], "'");
        // ...C# is the other way round...
        let mut cs = editor("a.cs", |_| {});
        type_str(&mut cs, "'");
        assert_eq!(cs.lines[0], "''");
        // ...and the setting overrides either default, globally or per language.
        let mut rust = editor("a.rs", |c| c.editor.auto_close_single_quote = Some(true));
        type_str(&mut rust, "'");
        assert_eq!(rust.lines[0], "''");
        let mut cs = editor("a.cs", |c| {
            c.languages.insert(
                "csharp".into(),
                LanguageOverride {
                    auto_close_single_quote: Some(false),
                    ..Default::default()
                },
            );
        });
        type_str(&mut cs, "'");
        assert_eq!(cs.lines[0], "'");
    }

    #[test]
    fn auto_close_tags_can_be_switched_off() {
        let mut on = editor("a.html", |_| {});
        type_str(&mut on, "<div>");
        assert_eq!(on.lines[0], "<div></div>");
        let mut off = editor("a.html", |c| c.editor.auto_close_tags = false);
        type_str(&mut off, "<div>");
        assert_eq!(off.lines[0], "<div>");
    }

    #[test]
    fn backspace_only_removes_pairs_that_would_have_been_auto_inserted() {
        // With auto-closing on, backspace deletes the empty pair as a unit.
        let mut on = editor("a.rs", |_| {});
        type_str(&mut on, "(");
        on.backspace();
        assert_eq!(on.lines[0], "");
        // With it off, the user typed `()` by hand: one backspace, one char.
        let mut off = editor("a.rs", |c| c.editor.auto_close_brackets = false);
        type_str(&mut off, "()");
        off.cursor_col = 1;
        off.backspace();
        assert_eq!(off.lines[0], ")");
    }

    // ---- movement ----

    fn tall(n: usize, tweak: impl FnOnce(&mut Config)) -> Editor {
        let mut ed = editor("a.rs", tweak);
        ed.lines = (0..n).map(|i| format!("line {i}")).collect();
        ed
    }

    #[test]
    fn scroll_off_keeps_context_around_the_cursor() {
        let mut ed = tall(100, |c| c.editor.scroll_off = 3);
        ed.cursor_row = 9;
        ed.ensure_visible(80, 10);
        assert_eq!(ed.scroll, 3, "cursor 3 lines from the bottom edge");

        ed.cursor_row = 4;
        ed.ensure_visible(80, 10);
        assert_eq!(ed.scroll, 1, "cursor 3 lines from the top edge");

        let mut plain = tall(100, |_| {});
        plain.cursor_row = 9;
        plain.ensure_visible(80, 10);
        assert_eq!(
            plain.scroll, 0,
            "default: scroll only when the cursor leaves the screen"
        );
        plain.cursor_row = 10;
        plain.ensure_visible(80, 10);
        assert_eq!(plain.scroll, 1);
    }

    #[test]
    fn scroll_off_never_exceeds_what_fits_on_screen() {
        // A huge margin on a tiny window must not make the view jump around.
        let mut ed = tall(100, |c| c.editor.scroll_off = 50);
        ed.cursor_row = 20;
        ed.ensure_visible(80, 5);
        assert!(
            ed.scroll <= 20 && 20 < ed.scroll + 5,
            "cursor stays visible: scroll {}",
            ed.scroll
        );
        let before = ed.scroll;
        ed.ensure_visible(80, 5);
        assert_eq!(ed.scroll, before, "stable when nothing changed");
    }

    #[test]
    fn page_size_sets_how_far_page_keys_move() {
        let mut screenful = tall(100, |_| {});
        screenful.ensure_visible(80, 10);
        screenful.page_down();
        assert_eq!(screenful.cursor_row, 9, "a screenful less one line");
        screenful.page_up();
        assert_eq!(screenful.cursor_row, 0);

        let mut fixed = tall(100, |c| c.editor.page_size = 5);
        fixed.ensure_visible(80, 10);
        fixed.page_down();
        fixed.page_down();
        assert_eq!(fixed.cursor_row, 10);
        fixed.page_up();
        assert_eq!(fixed.cursor_row, 5);
    }

    #[test]
    fn word_chars_decide_where_words_end() {
        let walk = |word_chars: &str| {
            let word_chars = word_chars.to_string();
            let mut ed = editor("a.txt", |c| c.editor.word_chars = word_chars);
            ed.lines[0] = "foo-bar baz".to_string();
            ed.cursor_col = 7; // right after "bar"
            ed.move_word_left();
            ed.cursor_col
        };
        assert_eq!(walk("_"), 4, "hyphen separates words by default");
        assert_eq!(
            walk("_-"),
            0,
            "with '-' as a word character, foo-bar is one word"
        );

        let mut ed = editor("a.txt", |c| c.editor.word_chars = "_-".into());
        ed.lines[0] = "foo-bar baz".to_string();
        ed.cursor_col = 0;
        ed.move_word_right();
        assert_eq!(ed.cursor_col, 7);
        ed.cursor_col = 7;
        ed.delete_word_backward();
        assert_eq!(ed.lines[0], " baz", "word deletion uses it too");
    }

    #[test]
    fn a_language_can_have_its_own_word_chars() {
        let mut ed = editor("a.css", |c| {
            c.languages.insert(
                "css".into(),
                LanguageOverride {
                    word_chars: Some("_-".into()),
                    ..Default::default()
                },
            );
        });
        ed.lines[0] = "font-size".to_string();
        ed.cursor_col = 9;
        ed.move_word_left();
        assert_eq!(ed.cursor_col, 0);
        let mut other = editor("a.rs", |_| {});
        other.lines[0] = "font-size".to_string();
        other.cursor_col = 9;
        other.move_word_left();
        assert_eq!(other.cursor_col, 5);
    }

    // ---- undo ----

    #[test]
    fn undo_limit_caps_how_many_steps_are_remembered() {
        // Each of these breaks coalescing, so each is its own undo step.
        let mut ed = editor("a.txt", |c| c.editor.undo_limit = 3);
        for _ in 0..6 {
            type_str(&mut ed, "x");
            ed.move_left();
            ed.move_right();
        }
        assert_eq!(ed.lines[0], "xxxxxx");
        let mut undone = 0;
        loop {
            let before = ed.lines[0].clone();
            ed.undo();
            if ed.lines[0] == before {
                break;
            }
            undone += 1;
        }
        assert_eq!(undone, 3, "only the last three steps can be undone");
        assert_eq!(ed.lines[0], "xxx");

        let mut roomy = editor("a.txt", |_| {});
        for _ in 0..6 {
            type_str(&mut roomy, "x");
            roomy.move_left();
            roomy.move_right();
        }
        for _ in 0..10 {
            roomy.undo();
        }
        assert_eq!(roomy.lines[0], "");
    }

    // ---- the C family ----

    #[test]
    fn braces_split_allman_style_in_the_c_family() {
        for file in [
            "a.c", "a.cpp", "A.java", "a.php", "a.ts", "a.kt", "a.swift", "a.dart", "a.m",
            "A.scala",
        ] {
            let mut ed = editor(file, |_| {});
            type_str(&mut ed, "void f() {");
            ed.newline();
            assert_eq!(
                ed.lines,
                vec![
                    "void f()".to_string(),
                    "{".into(),
                    ed.indent.clone(),
                    "}".into()
                ],
                "{file}"
            );
        }
    }

    #[test]
    fn go_keeps_the_brace_on_the_line_and_indents_with_a_tab() {
        let mut ed = editor("main.go", |_| {});
        type_str(&mut ed, "func main() {");
        ed.newline();
        assert_eq!(ed.lines, vec!["func main() {", "\t", "}"]);
        assert_eq!(ed.cursor_row, 1);
        ed.insert_tab();
        assert_eq!(ed.lines[1], "\t\t");

        // `[]` is a type here, not an array literal: Enter doesn't split it.
        let mut ed = editor("main.go", |_| {});
        type_str(&mut ed, "x := [");
        ed.newline();
        assert_eq!(ed.lines.len(), 2, "{:?}", ed.lines);
    }

    #[test]
    fn default_indent_follows_each_languages_convention() {
        let indent_of = |file: &str| {
            let mut ed = editor(file, |_| {});
            ed.insert_tab();
            ed.lines[0].clone()
        };
        assert_eq!(indent_of("a.go"), "\t");
        assert_eq!(indent_of("a.dart"), "  ");
        assert_eq!(indent_of("A.java"), "    ");
        assert_eq!(indent_of("a.c"), "    ");
    }

    #[test]
    fn a_c_file_keeps_its_tabs_when_it_is_indented_with_them() {
        let mut ed = editor_with_content("tabs.c", "int main(void)\n{\n\treturn 0;\n}\n", |_| {});
        ed.cursor_row = 2;
        ed.cursor_col = ed.lines[2].chars().count();
        ed.newline();
        assert_eq!(ed.lines[3], "\t", "continues with the file's own tab");
        ed.insert_tab();
        assert_eq!(ed.lines[3], "\t\t", "and Tab adds another one, not spaces");
    }

    #[test]
    fn char_literals_autoclose_in_c_but_swift_has_none() {
        let mut c = editor("a.c", |_| {});
        type_str(&mut c, "char c = '");
        assert_eq!(c.lines[0], "char c = ''");
        let mut swift = editor("a.swift", |_| {});
        type_str(&mut swift, "let c = '");
        assert_eq!(swift.lines[0], "let c = '");
        // Double quotes still pair up in both.
        let mut swift = editor("a.swift", |_| {});
        type_str(&mut swift, "let s = \"");
        assert_eq!(swift.lines[0], "let s = \"\"");
    }

    #[test]
    fn an_association_can_make_a_header_cpp_or_borrow_a_language() {
        let ed = editor("a.h", |_| {});
        assert!(ed.language == Language::C, ".h is C by default");
        let ed = editor("a.h", |c| {
            c.associations.insert("h".into(), "cpp".into());
        });
        assert!(ed.language == Language::Cpp);
        let ed = editor("shader.glsl", |c| {
            c.associations.insert("glsl".into(), "c".into());
        });
        assert!(ed.language == Language::C);
        let mut ed = editor("shader.glsl", |c| {
            c.associations.insert("glsl".into(), "c".into());
        });
        type_str(&mut ed, "void main() {");
        ed.newline();
        assert_eq!(ed.lines.len(), 4, "gets the smart braces too");
    }

    // ---- saving ----

    fn save_to_disk(
        name: &str,
        content: &str,
        tweak: impl FnOnce(&mut Config),
        edit: impl FnOnce(&mut Editor),
    ) -> Vec<u8> {
        let path = temp(name);
        std::fs::write(&path, content).unwrap();
        let mut config = Config::default();
        tweak(&mut config);
        let language = config.language_of(&path);
        let settings = Settings::resolve(&config, language);
        let mut ed = Editor::open_with(path.clone(), language, settings).unwrap();
        edit(&mut ed);
        assert!(ed.save());
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(path);
        bytes
    }

    #[test]
    fn line_ending_auto_keeps_what_the_file_had() {
        let touch = |ed: &mut Editor| ed.insert_char('!');
        assert_eq!(
            save_to_disk("le_auto_crlf.txt", "a\r\nb\r\n", |_| {}, touch),
            b"!a\r\nb\r\n"
        );
        assert_eq!(
            save_to_disk("le_auto_lf.txt", "a\nb\n", |_| {}, touch),
            b"!a\nb\n"
        );
    }

    #[test]
    fn line_ending_can_be_forced() {
        let touch = |ed: &mut Editor| ed.insert_char('!');
        let to_lf = |c: &mut Config| c.files.line_ending = LineEnding::Lf;
        let to_crlf = |c: &mut Config| c.files.line_ending = LineEnding::Crlf;
        assert_eq!(
            save_to_disk("le_lf.txt", "a\r\nb\r\n", to_lf, touch),
            b"!a\nb\n"
        );
        assert_eq!(
            save_to_disk("le_crlf.txt", "a\nb\n", to_crlf, touch),
            b"!a\r\nb\r\n"
        );
    }

    #[test]
    fn insert_final_newline_controls_the_last_line_break() {
        let touch = |ed: &mut Editor| ed.insert_char('!');
        assert_eq!(save_to_disk("nl_on.txt", "a", |_| {}, touch), b"!a\n");
        assert_eq!(
            save_to_disk(
                "nl_off.txt",
                "a",
                |c| c.files.insert_final_newline = false,
                touch
            ),
            b"!a"
        );
    }

    #[test]
    fn trim_trailing_whitespace_cleans_lines_on_save_only_when_asked() {
        let messy = "a  \n\tb\t \n   \nc\n";
        let touch = |ed: &mut Editor| ed.insert_char('!');
        assert_eq!(
            save_to_disk("trim_off.txt", messy, |_| {}, touch),
            b"!a  \n\tb\t \n   \nc\n"
        );
        assert_eq!(
            save_to_disk(
                "trim_on.txt",
                messy,
                |c| c.files.trim_trailing_whitespace = true,
                touch
            ),
            b"!a\n\tb\n\nc\n"
        );
    }

    #[test]
    fn trimming_keeps_the_cursor_on_a_valid_column() {
        let path = temp("trim_cursor.txt");
        let mut config = Config::default();
        config.files.trim_trailing_whitespace = true;
        let language = config.language_of(&path);
        let settings = Settings::resolve(&config, language);
        let mut ed = Editor::open_with(path.clone(), language, settings).unwrap();
        ed.lines = vec!["abc     ".to_string(), "x".to_string()];
        ed.cursor_row = 0;
        ed.cursor_col = 8;
        ed.selection_anchor = Some((0, 7));
        assert!(ed.save());
        assert_eq!(ed.lines[0], "abc");
        assert_eq!(ed.cursor_col, 3);
        assert_eq!(ed.selection_anchor, Some((0, 3)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn per_language_save_settings_apply_only_to_that_language() {
        let touch = |ed: &mut Editor| ed.insert_char('!');
        let tweak = |c: &mut Config| {
            c.languages.insert(
                "markdown".into(),
                LanguageOverride {
                    trim_trailing_whitespace: Some(false),
                    ..Default::default()
                },
            );
            c.files.trim_trailing_whitespace = true;
        };
        // Markdown keeps its two-space hard line breaks; everything else is trimmed.
        assert_eq!(
            save_to_disk("lang_trim.md", "a  \nb\n", tweak, touch),
            b"!a  \nb\n"
        );
        assert_eq!(
            save_to_disk("lang_trim.txt", "a  \nb\n", tweak, touch),
            b"!a\nb\n"
        );
    }
}
