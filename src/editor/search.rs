//! Find (Ctrl+F): an incremental search bar over the buffer.
//!
//! While the bar is open every keystroke re-runs the search over all lines
//! and jumps to the first match at or after where the search began, so
//! extending the text keeps you on the same hit. Enter / Shift+Enter (or
//! F3 / Shift+F3) step through the matches; Esc closes the bar and leaves
//! the current match selected. F3 keeps working with the bar closed.
//!
//! Matching is per line, with columns counted in characters like the rest
//! of the editor.

use super::Editor;
use crate::config::{CaseMode, SearchSettings};
use regex::{Regex, RegexBuilder};

/// One hit, in character columns of its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

/// Stop collecting beyond this many matches (a one-letter search in a huge
/// file); the bar then shows `100000+`.
const MAX_MATCHES: usize = 100_000;

/// The longest selection that is picked up as the initial search text.
const MAX_SEED_LEN: usize = 200;

/// What the find bar tells the user about the search.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchStatus {
    /// Nothing typed yet.
    Empty,
    NoResults,
    /// There are matches, but none further on and wrap-around is off.
    NoMoreResults,
    /// Match number `index` (1-based) of `total`.
    Found {
        index: usize,
        total: usize,
        truncated: bool,
    },
    /// Matches exist but none is current yet (incremental search is off).
    Pending {
        total: usize,
        truncated: bool,
    },
    /// The text is not a valid regular expression.
    Invalid(String),
}

pub struct Search {
    pub active: bool,
    pub query: String,
    /// The next typed character replaces the query: set when the bar opens
    /// on text left over from earlier, like selecting it all.
    replace_on_type: bool,
    pub regex: bool,
    pub whole_word: bool,
    /// `Some` once the user toggled case sensitivity in the bar.
    case_override: Option<bool>,
    /// Where the search began; the first match at or after it is current.
    origin: (usize, usize),
    matches: Vec<Match>,
    current: Option<usize>,
    truncated: bool,
    /// Stepped past the last match with wrap-around off.
    end_reached: bool,
    error: Option<String>,
}

impl Search {
    pub fn new(settings: &SearchSettings) -> Self {
        Self {
            active: false,
            query: String::new(),
            replace_on_type: false,
            regex: settings.regex,
            whole_word: settings.whole_word,
            case_override: None,
            origin: (0, 0),
            matches: Vec::new(),
            current: None,
            truncated: false,
            end_reached: false,
            error: None,
        }
    }

    /// Whether letter case matters right now: the user's toggle if they
    /// made one, otherwise the `search.case` setting (where `smart` means
    /// "only if the text has a capital letter").
    pub fn case_sensitive(&self, mode: CaseMode) -> bool {
        self.case_override.unwrap_or(match mode {
            CaseMode::Sensitive => true,
            CaseMode::Insensitive => false,
            CaseMode::Smart => self.query.chars().any(char::is_uppercase),
        })
    }

    /// The pattern to search with; `None` for an empty query.
    fn compile(&self, mode: CaseMode) -> Result<Option<Regex>, String> {
        if self.query.is_empty() {
            return Ok(None);
        }
        let mut pattern = if self.regex {
            self.query.clone()
        } else {
            regex::escape(&self.query)
        };
        if self.whole_word {
            // For plain text, only ask for a word boundary on a side that
            // starts or ends with a word character (`\b` next to a symbol
            // would demand a letter beside it).
            let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            let (front, back) = if self.regex {
                (true, true)
            } else {
                (
                    is_word(self.query.chars().next()),
                    is_word(self.query.chars().last()),
                )
            };
            pattern = format!(
                "{}(?:{pattern}){}",
                if front { r"\b" } else { "" },
                if back { r"\b" } else { "" }
            );
        }
        RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive(mode))
            .build()
            .map(Some)
            .map_err(|e| {
                // The regex crate's errors are multi-line and quote the pattern.
                e.to_string()
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("invalid pattern")
                    .trim()
                    .trim_start_matches("error: ")
                    .to_string()
            })
    }

    /// Finds every match in `lines`.
    fn recompute(&mut self, lines: &[String], mode: CaseMode) {
        self.matches.clear();
        self.current = None;
        self.truncated = false;
        self.end_reached = false;
        self.error = None;
        let regex = match self.compile(mode) {
            Ok(Some(regex)) => regex,
            Ok(None) => return,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        'lines: for (row, line) in lines.iter().enumerate() {
            let (mut byte, mut chars) = (0usize, 0usize);
            for m in regex.find_iter(line) {
                if m.start() == m.end() {
                    continue; // an empty match (e.g. `a*`) selects nothing
                }
                chars += line[byte..m.start()].chars().count();
                let len = line[m.start()..m.end()].chars().count();
                byte = m.end();
                self.matches.push(Match {
                    row,
                    start: chars,
                    end: chars + len,
                });
                chars += len;
                if self.matches.len() >= MAX_MATCHES {
                    self.truncated = true;
                    break 'lines;
                }
            }
        }
    }

    /// The matches on `row`, in column order.
    pub fn matches_in_row(&self, row: usize) -> &[Match] {
        let lo = self.matches.partition_point(|m| m.row < row);
        let hi = self.matches.partition_point(|m| m.row <= row);
        &self.matches[lo..hi]
    }

    pub fn current_match(&self) -> Option<Match> {
        self.current.map(|i| self.matches[i])
    }

    #[cfg(test)]
    pub fn total(&self) -> usize {
        self.matches.len()
    }

    pub fn status(&self) -> SearchStatus {
        if let Some(e) = &self.error {
            SearchStatus::Invalid(e.clone())
        } else if self.query.is_empty() {
            SearchStatus::Empty
        } else if self.matches.is_empty() {
            SearchStatus::NoResults
        } else if self.end_reached {
            SearchStatus::NoMoreResults
        } else if let Some(i) = self.current {
            SearchStatus::Found {
                index: i + 1,
                total: self.matches.len(),
                truncated: self.truncated,
            }
        } else {
            SearchStatus::Pending {
                total: self.matches.len(),
                truncated: self.truncated,
            }
        }
    }
}

impl Editor {
    /// Ctrl+F: opens the bar. A selection on one line becomes the search
    /// text; otherwise the previous text is offered again (typing replaces it).
    pub fn open_search(&mut self) {
        self.confirm_quit = false;
        if self.search.active {
            self.search.replace_on_type = !self.search.query.is_empty();
            return;
        }
        self.reset_edit_group();
        let seed = self.selection_bounds().and_then(|(start, end)| {
            let short = end.1.saturating_sub(start.1) <= MAX_SEED_LEN;
            (start.0 == end.0 && end.1 > start.1 && short)
                .then(|| (self.extract_range(start, end), start))
        });
        match seed {
            Some((text, start)) => {
                self.search.query = text;
                self.search.replace_on_type = false;
                self.cursor_row = start.0;
                self.cursor_col = start.1;
            }
            None => self.search.replace_on_type = !self.search.query.is_empty(),
        }
        self.selection_anchor = None;
        self.search.origin = (self.cursor_row, self.cursor_col);
        self.search.active = true;
        self.refresh_search();
    }

    /// Closes the bar, leaving the current match selected (if
    /// `search.select_on_close`) so typing replaces it.
    pub fn close_search(&mut self) {
        if !self.search.active {
            return;
        }
        self.search.active = false;
        if self.settings.search.select_on_close {
            if let Some(i) = self.search.current {
                self.go_to_match(i, true);
            }
        }
        self.search.matches.clear();
        self.search.current = None;
    }

    pub fn search_insert(&mut self, c: char) {
        self.start_typing_into_query();
        self.search.query.push(c);
        self.refresh_search();
    }

    pub fn search_backspace(&mut self) {
        if self.search.replace_on_type {
            self.search.query.clear();
            self.search.replace_on_type = false;
        } else {
            self.search.query.pop();
        }
        self.refresh_search();
    }

    /// Ctrl+Backspace in the bar: deletes the last word of the text.
    pub fn search_delete_word(&mut self) {
        self.search.replace_on_type = false;
        let keep = Self::prev_word_boundary(
            &self.search.query,
            self.search.query.chars().count(),
            &self.settings.word_chars,
        );
        self.search.query = self.search.query.chars().take(keep).collect();
        self.refresh_search();
    }

    /// Pastes the first line of the clipboard into the bar.
    pub fn search_paste(&mut self) {
        let text = super::get_system_clipboard().unwrap_or_else(|_| self.clipboard.clone());
        let line = text.lines().next().unwrap_or("");
        if line.is_empty() {
            return;
        }
        self.start_typing_into_query();
        self.search.query.push_str(line);
        self.refresh_search();
    }

    pub fn search_toggle_case(&mut self) {
        let now = self.search_case_sensitive();
        self.search.case_override = Some(!now);
        self.refresh_search();
    }

    pub fn search_toggle_whole_word(&mut self) {
        self.search.whole_word = !self.search.whole_word;
        self.refresh_search();
    }

    pub fn search_toggle_regex(&mut self) {
        self.search.regex = !self.search.regex;
        self.refresh_search();
    }

    pub fn search_case_sensitive(&self) -> bool {
        self.search.case_sensitive(self.settings.search.case)
    }

    /// Enter / F3: the next match, wrapping past the end if allowed.
    pub fn search_next(&mut self) {
        self.step_search(true);
    }

    /// Shift+Enter / Shift+F3: the previous match.
    pub fn search_prev(&mut self) {
        self.step_search(false);
    }

    fn start_typing_into_query(&mut self) {
        if self.search.replace_on_type {
            self.search.query.clear();
            self.search.replace_on_type = false;
        }
    }

    /// Re-runs the search after the text or a mode changed, and (with
    /// incremental search) lands on the first match at or after where the
    /// search began — or back there when there is none.
    fn refresh_search(&mut self) {
        let mode = self.settings.search.case;
        self.search.recompute(&self.lines, mode);
        if !self.settings.search.incremental {
            return;
        }
        match self.first_match_from(self.search.origin) {
            Some(i) => self.go_to_match(i, false),
            None => {
                let (row, col) = self.search.origin;
                self.cursor_row = row.min(self.lines.len() - 1);
                self.cursor_col = col.min(self.current_line_char_len());
                self.desired_col = self.cursor_col;
            }
        }
    }

    fn step_search(&mut self, forward: bool) {
        if self.search.query.is_empty() {
            self.open_search();
            return;
        }
        let bar_open = self.search.active;
        if !bar_open {
            // F3 with the bar closed: the buffer may have changed since.
            let mode = self.settings.search.case;
            self.search.recompute(&self.lines, mode);
        }
        let total = self.search.matches.len();
        let wrap = self.settings.search.wrap_around;

        let target = if total == 0 {
            None
        } else if let Some(cur) = self.search.current.filter(|_| bar_open) {
            match (forward, cur) {
                (true, c) if c + 1 < total => Some(c + 1),
                (true, _) => wrap.then_some(0),
                (false, c) if c > 0 => Some(c - 1),
                (false, _) => wrap.then_some(total - 1),
            }
        } else if forward {
            let from = if bar_open {
                self.search.origin
            } else {
                self.selection_bounds()
                    .map(|(_, end)| end)
                    .unwrap_or((self.cursor_row, self.cursor_col))
            };
            self.first_match_from(from)
        } else {
            let from = if bar_open {
                self.search.origin
            } else {
                self.selection_bounds()
                    .map(|(start, _)| start)
                    .unwrap_or((self.cursor_row, self.cursor_col))
            };
            self.last_match_before(from)
        };

        match target {
            Some(i) => {
                self.search.end_reached = false;
                self.go_to_match(i, !bar_open);
            }
            // Stay on the current match; the bar just says there is no more.
            None => self.search.end_reached = total > 0,
        }
        if !bar_open {
            self.report_search_result();
        }
    }

    /// With the bar closed there is nowhere to show the result but the
    /// status line.
    fn report_search_result(&mut self) {
        let (message, is_error) = match self.search.status() {
            SearchStatus::Found {
                index,
                total,
                truncated,
            } => (
                format!(
                    "Match {index} of {total}{}",
                    if truncated { "+" } else { "" }
                ),
                false,
            ),
            SearchStatus::NoMoreResults => ("No more matches".to_string(), false),
            SearchStatus::Invalid(e) => (format!("Invalid pattern: {e}"), true),
            _ => (format!("No results for \"{}\"", self.search.query), true),
        };
        self.status_message = message;
        self.status_is_error = is_error;
    }

    /// Index of the first match at or after `pos`; wraps to the first one
    /// when there is none and wrap-around is on.
    fn first_match_from(&self, pos: (usize, usize)) -> Option<usize> {
        let matches = &self.search.matches;
        let i = matches.partition_point(|m| (m.row, m.start) < pos);
        if i < matches.len() {
            Some(i)
        } else if self.settings.search.wrap_around && !matches.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    /// Index of the last match before `pos`, wrapping likewise.
    fn last_match_before(&self, pos: (usize, usize)) -> Option<usize> {
        let matches = &self.search.matches;
        let before = matches.partition_point(|m| (m.row, m.start) < pos);
        if before > 0 {
            Some(before - 1)
        } else if self.settings.search.wrap_around && !matches.is_empty() {
            Some(matches.len() - 1)
        } else {
            None
        }
    }

    /// Moves to match `index`: the cursor at its start, or (when `select`)
    /// the whole match selected.
    fn go_to_match(&mut self, index: usize, select: bool) {
        let m = self.search.matches[index];
        self.search.current = Some(index);
        self.reset_edit_group();
        if select {
            self.selection_anchor = Some((m.row, m.start));
            self.cursor_row = m.row;
            self.cursor_col = m.end;
        } else {
            self.selection_anchor = None;
            self.cursor_row = m.row;
            self.cursor_col = m.start;
        }
        self.desired_col = self.cursor_col;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Settings};
    use crate::language::Language;
    use std::path::PathBuf;

    fn editor_with(lines: &[&str], tweak: impl FnOnce(&mut Config)) -> Editor {
        let mut config = Config::default();
        tweak(&mut config);
        let path = PathBuf::from("__nonexistent__/search.txt");
        let settings = Settings::resolve(&config, Language::Other);
        let mut ed = Editor::open_with(path, Language::Other, settings).unwrap();
        ed.lines = lines.iter().map(|l| l.to_string()).collect();
        ed
    }

    fn text(ed: &mut Editor, s: &str) {
        for c in s.chars() {
            ed.search_insert(c);
        }
    }

    fn all(ed: &Editor) -> Vec<(usize, usize, usize)> {
        (0..ed.lines.len())
            .flat_map(|r| {
                ed.search
                    .matches_in_row(r)
                    .iter()
                    .map(|m| (m.row, m.start, m.end))
            })
            .collect()
    }

    #[test]
    fn finds_every_occurrence_with_character_columns() {
        let mut ed = editor_with(&["foo bar foo", "no", "xfoo"], |_| {});
        ed.open_search();
        text(&mut ed, "foo");
        assert_eq!(all(&ed), vec![(0, 0, 3), (0, 8, 11), (2, 1, 4)]);
        assert_eq!(
            ed.search.status(),
            SearchStatus::Found {
                index: 1,
                total: 3,
                truncated: false
            }
        );
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        let mut ed = editor_with(&["привет мир, привет"], |_| {});
        ed.open_search();
        text(&mut ed, "мир");
        assert_eq!(all(&ed), vec![(0, 7, 10)]);
        assert_eq!(ed.cursor_col, 7);
        // Case-insensitive for Cyrillic too.
        let mut ed = editor_with(&["Привет ПРИВЕТ"], |_| {});
        ed.open_search();
        text(&mut ed, "привет");
        assert_eq!(all(&ed).len(), 2);
    }

    #[test]
    fn smart_case_matches_any_case_until_you_type_a_capital() {
        let mut ed = editor_with(&["Foo foo FOO"], |_| {});
        ed.open_search();
        text(&mut ed, "foo");
        assert_eq!(all(&ed).len(), 3);
        ed.search_insert('X');
        ed.search_backspace();
        text(&mut ed, "");
        let mut ed = editor_with(&["Foo foo FOO"], |_| {});
        ed.open_search();
        text(&mut ed, "Foo");
        assert_eq!(
            all(&ed),
            vec![(0, 0, 3)],
            "a capital makes it case-sensitive"
        );
    }

    #[test]
    fn case_setting_and_toggle() {
        let mut ed = editor_with(&["Foo foo"], |c| c.search.case = CaseMode::Sensitive);
        ed.open_search();
        text(&mut ed, "foo");
        assert_eq!(all(&ed), vec![(0, 4, 7)]);
        ed.search_toggle_case();
        assert_eq!(all(&ed).len(), 2, "toggled to insensitive");
        ed.search_toggle_case();
        assert_eq!(all(&ed).len(), 1);

        let mut ed = editor_with(&["Foo foo"], |c| c.search.case = CaseMode::Insensitive);
        ed.open_search();
        text(&mut ed, "Foo");
        assert_eq!(all(&ed).len(), 2, "insensitive even with a capital");
    }

    #[test]
    fn plain_text_is_not_a_pattern() {
        let mut ed = editor_with(&["a.b a+b axb (x)"], |_| {});
        ed.open_search();
        text(&mut ed, "a.b");
        assert_eq!(all(&ed), vec![(0, 0, 3)], "the dot is a dot");
        ed.search_backspace();
        ed.search_backspace();
        ed.search_backspace();
        text(&mut ed, "(x)");
        assert_eq!(all(&ed), vec![(0, 12, 15)]);
    }

    #[test]
    fn regex_mode() {
        let mut ed = editor_with(&["a1 b22 c333"], |c| c.search.regex = true);
        ed.open_search();
        text(&mut ed, r"\d+");
        assert_eq!(all(&ed), vec![(0, 1, 2), (0, 4, 6), (0, 8, 11)]);
        ed.search_toggle_regex();
        assert_eq!(ed.search.status(), SearchStatus::NoResults, "now literal");
        ed.search_toggle_regex();
        assert_eq!(all(&ed).len(), 3);
    }

    #[test]
    fn a_bad_regex_is_reported_not_fatal() {
        let mut ed = editor_with(&["abc"], |c| c.search.regex = true);
        ed.open_search();
        text(&mut ed, "(");
        match ed.search.status() {
            SearchStatus::Invalid(why) => {
                assert!(!why.contains('\n') && !why.is_empty(), "{why:?}")
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
        assert!(all(&ed).is_empty());
        text(&mut ed, "a)");
        assert_eq!(all(&ed), vec![(0, 0, 1)], "recovers once it is valid");
    }

    #[test]
    fn empty_matches_are_ignored() {
        let mut ed = editor_with(&["bbb"], |c| c.search.regex = true);
        ed.open_search();
        text(&mut ed, "a*");
        assert!(all(&ed).is_empty());
        assert_eq!(ed.search.status(), SearchStatus::NoResults);
    }

    #[test]
    fn whole_word() {
        let mut ed = editor_with(&["cat concat cat_x cat. (cat)"], |c| {
            c.search.whole_word = true
        });
        ed.open_search();
        text(&mut ed, "cat");
        assert_eq!(all(&ed), vec![(0, 0, 3), (0, 17, 20), (0, 23, 26)]);
        ed.search_toggle_whole_word();
        assert_eq!(all(&ed).len(), 5);

        // A query that starts with a symbol still works as a "word".
        let mut ed = editor_with(&["a +b c+b"], |c| c.search.whole_word = true);
        ed.open_search();
        text(&mut ed, "+b");
        assert_eq!(all(&ed).len(), 2);
    }

    #[test]
    fn typing_jumps_to_the_first_match_at_or_after_where_you_were() {
        let mut ed = editor_with(&["one two", "three two", "four two"], |_| {});
        ed.cursor_row = 1;
        ed.cursor_col = 6;
        ed.open_search();
        text(&mut ed, "t");
        assert_eq!(
            (ed.cursor_row, ed.cursor_col),
            (1, 6),
            "stays on the hit under the cursor"
        );
        text(&mut ed, "wo");
        assert_eq!(
            (ed.cursor_row, ed.cursor_col),
            (1, 6),
            "extending the text keeps the match"
        );
        ed.search_backspace();
        ed.search_backspace();
        ed.search_backspace();
        assert_eq!(
            (ed.cursor_row, ed.cursor_col),
            (1, 6),
            "an empty query puts it back"
        );
        text(&mut ed, "nothing");
        assert_eq!(
            (ed.cursor_row, ed.cursor_col),
            (1, 6),
            "no match: stays where it began"
        );
        assert_eq!(ed.search.status(), SearchStatus::NoResults);
    }

    #[test]
    fn enter_steps_forward_and_wraps() {
        let mut ed = editor_with(&["x a x", "x"], |_| {});
        ed.open_search();
        text(&mut ed, "x");
        let at = |ed: &Editor| (ed.cursor_row, ed.cursor_col);
        assert_eq!(at(&ed), (0, 0));
        ed.search_next();
        assert_eq!(at(&ed), (0, 4));
        ed.search_next();
        assert_eq!(at(&ed), (1, 0));
        ed.search_next();
        assert_eq!(at(&ed), (0, 0), "wraps to the first");
        assert_eq!(
            ed.search.status(),
            SearchStatus::Found {
                index: 1,
                total: 3,
                truncated: false
            }
        );
        ed.search_prev();
        assert_eq!(at(&ed), (1, 0), "and back to the last");
        ed.search_prev();
        assert_eq!(at(&ed), (0, 4));
    }

    #[test]
    fn wrap_around_can_be_turned_off() {
        let mut ed = editor_with(&["x x"], |c| c.search.wrap_around = false);
        ed.open_search();
        text(&mut ed, "x");
        ed.search_next();
        assert_eq!(ed.cursor_col, 2);
        ed.search_next();
        assert_eq!(ed.search.status(), SearchStatus::NoMoreResults);
        assert_eq!(ed.cursor_col, 2, "stays on the last match");
        ed.search_prev();
        assert_eq!(ed.cursor_col, 0, "stepping the other way works from there");
        ed.search_prev();
        assert_eq!(ed.search.status(), SearchStatus::NoMoreResults);
    }

    #[test]
    fn without_incremental_search_nothing_moves_until_enter() {
        let mut ed = editor_with(&["abc", "xyz"], |c| c.search.incremental = false);
        ed.open_search();
        text(&mut ed, "xyz");
        assert_eq!((ed.cursor_row, ed.cursor_col), (0, 0));
        assert_eq!(
            ed.search.status(),
            SearchStatus::Pending {
                total: 1,
                truncated: false
            }
        );
        ed.search_next();
        assert_eq!((ed.cursor_row, ed.cursor_col), (1, 0));
    }

    #[test]
    fn closing_selects_the_current_match_unless_told_not_to() {
        let mut ed = editor_with(&["foo bar"], |_| {});
        ed.open_search();
        text(&mut ed, "bar");
        ed.close_search();
        assert!(!ed.search.active);
        assert_eq!(ed.selection_bounds(), Some(((0, 4), (0, 7))));

        let mut ed = editor_with(&["foo bar"], |c| c.search.select_on_close = false);
        ed.open_search();
        text(&mut ed, "bar");
        ed.close_search();
        assert!(!ed.has_selection());
        assert_eq!(ed.cursor_col, 4);
        assert!(
            ed.search.matches_in_row(0).is_empty(),
            "highlights are gone"
        );
    }

    #[test]
    fn a_one_line_selection_becomes_the_search_text() {
        let mut ed = editor_with(&["let value = value + 1"], |_| {});
        ed.cursor_col = 4;
        ed.selection_anchor = Some((0, 4));
        ed.cursor_col = 9;
        ed.open_search();
        assert_eq!(ed.search.query, "value");
        assert!(!ed.has_selection());
        assert_eq!(all(&ed).len(), 2);
        assert_eq!(ed.cursor_col, 4, "starts at the selected occurrence");
    }

    #[test]
    fn reopening_offers_the_last_text_and_typing_replaces_it() {
        let mut ed = editor_with(&["abc def"], |_| {});
        ed.open_search();
        text(&mut ed, "abc");
        ed.close_search();
        ed.selection_anchor = None;
        ed.open_search();
        assert_eq!(ed.search.query, "abc", "offered again");
        text(&mut ed, "d");
        assert_eq!(ed.search.query, "d", "the first key replaces it");

        // Backspace on offered text clears it, not just its last letter.
        ed.close_search();
        ed.open_search();
        ed.search_backspace();
        assert_eq!(ed.search.query, "");
    }

    #[test]
    fn f3_works_with_the_bar_closed_and_reports_in_the_status_line() {
        let mut ed = editor_with(&["a x", "b x", "c"], |_| {});
        ed.open_search();
        text(&mut ed, "x");
        ed.close_search();
        assert_eq!(ed.selection_bounds(), Some(((0, 2), (0, 3))));
        ed.search_next();
        assert_eq!(
            ed.selection_bounds(),
            Some(((1, 2), (1, 3))),
            "selects the next match"
        );
        assert_eq!(ed.status_message, "Match 2 of 2");
        ed.search_next();
        assert_eq!(ed.selection_bounds(), Some(((0, 2), (0, 3))), "wraps");
        ed.search_prev();
        assert_eq!(ed.selection_bounds(), Some(((1, 2), (1, 3))));

        // Text edited since: the search is redone, not stale.
        ed.lines[2] = "x at the end".to_string();
        ed.selection_anchor = None;
        ed.cursor_row = 1;
        ed.cursor_col = 3;
        ed.search_next();
        assert_eq!(ed.selection_bounds(), Some(((2, 0), (2, 1))));
        assert_eq!(ed.status_message, "Match 3 of 3");

        ed.search.query = "zzz".to_string();
        ed.search_next();
        assert!(
            ed.status_is_error && ed.status_message.contains("No results"),
            "{}",
            ed.status_message
        );
    }

    #[test]
    fn f3_without_any_text_opens_the_bar() {
        let mut ed = editor_with(&["abc"], |_| {});
        ed.search_next();
        assert!(ed.search.active);
    }

    #[test]
    fn deleting_a_word_and_pasting() {
        let mut ed = editor_with(&["x"], |_| {});
        ed.open_search();
        text(&mut ed, "foo bar");
        ed.search_delete_word();
        assert_eq!(ed.search.query, "foo ");
        ed.search_delete_word();
        assert_eq!(ed.search.query, "");

        ed.clipboard = "pasted line\nsecond line".to_string();
        ed.search_paste();
        assert_eq!(ed.search.query, "pasted line", "only the first line");
        ed.search_paste();
        assert_eq!(ed.search.query, "pasted linepasted line");
    }

    #[test]
    fn a_huge_number_of_matches_is_capped() {
        let lines: Vec<String> = (0..MAX_MATCHES + 50).map(|_| "a".to_string()).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut ed = editor_with(&refs, |_| {});
        ed.open_search();
        text(&mut ed, "a");
        assert_eq!(ed.search.total(), MAX_MATCHES);
        assert_eq!(
            ed.search.status(),
            SearchStatus::Found {
                index: 1,
                total: MAX_MATCHES,
                truncated: true
            }
        );
    }

    #[test]
    fn matches_are_looked_up_per_row() {
        let mut ed = editor_with(&["a", "b", "a a", "a"], |_| {});
        ed.open_search();
        text(&mut ed, "a");
        let counts: Vec<usize> = (0..4).map(|r| ed.search.matches_in_row(r).len()).collect();
        assert_eq!(counts, vec![1, 0, 2, 1]);
        assert!(ed.search.matches_in_row(99).is_empty());
        assert_eq!(
            ed.search.current_match(),
            Some(Match {
                row: 0,
                start: 0,
                end: 1
            })
        );
    }
}
