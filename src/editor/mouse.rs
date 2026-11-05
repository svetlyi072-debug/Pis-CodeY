//! Mouse support: click to place the cursor, drag to select, double-click for
//! a word, triple-click for a line, the wheel to scroll.
//!
//! Selecting with the editor's own mouse handling (rather than the
//! terminal's) is what keeps the line numbers out of a selection: only the
//! text area responds, so only text is ever selected or copied.

use super::{display_columns, CharClass, Editor};
use std::time::Instant;

/// Where the last draw put things, in screen cells; mouse positions are
/// turned into buffer positions with it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct View {
    /// Top-left of the text area, which starts with the line-number gutter.
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// Width of the gutter at the left of the text area (0 without numbers).
    pub gutter: u16,
}

#[derive(Debug, Clone, Copy)]
struct Click {
    at: Instant,
    col: u16,
    row: u16,
    count: u8,
}

#[derive(Debug, Default)]
pub struct MouseState {
    last_click: Option<Click>,
    /// Where the button went down, while it is held: the fixed end of a
    /// drag selection.
    drag_anchor: Option<(usize, usize)>,
}

/// What is under a screen cell.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// A line number (the line's index).
    Gutter(usize),
    /// A position in the text.
    Text(usize, usize),
    Outside,
}

impl Editor {
    /// The character index on `row` whose cell is nearest `cell` (a cell
    /// counted from the start of the line, tabs expanded).
    fn char_at_cell(&self, row: usize, cell: usize) -> usize {
        let cols = display_columns(&self.lines[row], self.settings.tab_width);
        let len = cols.len() - 1; // the last entry is the line's width
        if cell >= cols[len] {
            return len;
        }
        // The last character starting at or before the cell...
        let i = cols.partition_point(|&c| c <= cell) - 1;
        // ...or, past the middle of a wide one (a tab), the next boundary.
        let width = cols[i + 1] - cols[i];
        if cell - cols[i] >= width.div_ceil(2) && width > 1 {
            i + 1
        } else {
            i
        }
    }

    fn target_at(&self, col: u16, row: u16) -> Target {
        let v = self.view;
        if col < v.x
            || row < v.y
            || row >= v.y.saturating_add(v.height)
            || col >= v.x.saturating_add(v.width)
        {
            return Target::Outside;
        }
        let file_row = (self.scroll + (row - v.y) as usize).min(self.lines.len() - 1);
        let across = col - v.x;
        if across < v.gutter {
            return Target::Gutter(file_row);
        }
        let cell = (across - v.gutter) as usize + self.col_scroll;
        Target::Text(file_row, self.char_at_cell(file_row, cell))
    }

    /// Left button pressed at screen cell (`col`, `row`).
    pub fn mouse_down(&mut self, col: u16, row: u16, now: Instant) {
        self.confirm_quit = false;
        self.reset_edit_group();
        self.mouse.drag_anchor = None;
        match self.target_at(col, row) {
            Target::Outside => {}
            // A line number is not text: clicking it only goes to the line.
            Target::Gutter(line) => {
                self.selection_anchor = None;
                self.cursor_row = line;
                self.cursor_col = 0;
                self.desired_col = 0;
                self.mouse.last_click = None;
            }
            Target::Text(line, c) => {
                let count = self.count_click(col, row, now);
                match count {
                    2 => {
                        let (start, end) = self.word_bounds(line, c);
                        self.select_span(line, start, end);
                        self.mouse.drag_anchor = Some((line, start));
                    }
                    3 => {
                        let end = self.lines[line].chars().count();
                        self.select_span(line, 0, end);
                        self.mouse.drag_anchor = Some((line, 0));
                    }
                    _ => {
                        self.selection_anchor = None;
                        self.cursor_row = line;
                        self.cursor_col = c;
                        self.desired_col = c;
                        self.mouse.drag_anchor = Some((line, c));
                    }
                }
            }
        }
    }

    /// The mouse moved with the left button held.
    pub fn mouse_drag(&mut self, col: u16, row: u16) {
        let Some(anchor) = self.mouse.drag_anchor else {
            return;
        };
        let v = self.view;
        if v.height == 0 {
            return;
        }
        // Dragging past the top or bottom edge scrolls one line per event.
        let last_line = self.lines.len() - 1;
        let clamped_row = if row < v.y {
            self.scroll = self.scroll.saturating_sub(1);
            v.y
        } else if row >= v.y + v.height {
            let max_scroll = last_line.saturating_sub(v.height as usize - 1);
            self.scroll = (self.scroll + 1).min(max_scroll);
            v.y + v.height - 1
        } else {
            row
        };
        // Left of the text (over the numbers) counts as its first column.
        let clamped_col = col.max(v.x + v.gutter);
        let (line, c) = match self.target_at(clamped_col, clamped_row) {
            Target::Text(line, c) => (line, c),
            Target::Gutter(line) => (line, 0),
            Target::Outside => {
                // Right of the text area: the end of that line.
                let line = (self.scroll + (clamped_row - v.y) as usize).min(last_line);
                (line, self.lines[line].chars().count())
            }
        };
        self.cursor_row = line;
        self.cursor_col = c;
        self.desired_col = c;
        self.selection_anchor = ((line, c) != anchor).then_some(anchor);
    }

    /// The left button was released.
    pub fn mouse_up(&mut self) {
        self.mouse.drag_anchor = None;
    }

    /// The wheel: scrolls the view by `lines` (negative is up), dragging the
    /// cursor along when it would otherwise be left off screen.
    pub fn scroll_view(&mut self, lines: isize) {
        let height = self.view.height as usize;
        if height == 0 {
            return;
        }
        let max_scroll = self.lines.len().saturating_sub(height);
        self.scroll = self.scroll.saturating_add_signed(lines).min(max_scroll);
        // The view follows the cursor on every draw, so the cursor has to
        // stay inside the view or the scroll would be undone.
        let row = self.cursor_row.clamp(self.scroll, self.scroll + height - 1);
        if row != self.cursor_row {
            self.cursor_row = row;
            self.cursor_col = self.desired_col.min(self.current_line_char_len());
        }
    }

    /// 1 for a lone click; 2 or 3 when this one follows the last on the
    /// same cell quickly enough.
    fn count_click(&mut self, col: u16, row: u16, now: Instant) -> u8 {
        let window = self.settings.mouse.double_click;
        let count = match self.mouse.last_click {
            Some(prev)
                if !window.is_zero()
                    && prev.col == col
                    && prev.row == row
                    && now.saturating_duration_since(prev.at) <= window =>
            {
                prev.count % 3 + 1
            }
            _ => 1,
        };
        self.mouse.last_click = Some(Click {
            at: now,
            col,
            row,
            count,
        });
        count
    }

    fn select_span(&mut self, line: usize, start: usize, end: usize) {
        self.selection_anchor = Some((line, start));
        self.cursor_row = line;
        self.cursor_col = end;
        self.desired_col = end;
    }

    /// The run of same-kind characters (word, spaces or punctuation)
    /// around `col`.
    fn word_bounds(&self, line: usize, col: usize) -> (usize, usize) {
        let chars: Vec<char> = self.lines[line].chars().collect();
        if chars.is_empty() {
            return (0, 0);
        }
        let at = col.min(chars.len() - 1);
        let class = |c: char| Self::char_class(c, &self.settings.word_chars);
        let kind: CharClass = class(chars[at]);
        let mut start = at;
        while start > 0 && class(chars[start - 1]) == kind {
            start -= 1;
        }
        let mut end = at + 1;
        while end < chars.len() && class(chars[end]) == kind {
            end += 1;
        }
        (start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Settings};
    use crate::language::Language;
    use std::path::PathBuf;
    use std::time::Duration;

    /// An editor over `lines`, drawn (as far as the mouse is concerned) in
    /// a 70x10 text area at the screen's origin with a 4-cell gutter.
    fn editor(lines: &[&str]) -> Editor {
        editor_with(lines, |_| {})
    }

    fn editor_with(lines: &[&str], tweak: impl FnOnce(&mut Config)) -> Editor {
        let mut config = Config::default();
        tweak(&mut config);
        let settings = Settings::resolve(&config, Language::Other);
        let mut ed = Editor::open_with(
            PathBuf::from("__nonexistent__/m.txt"),
            Language::Other,
            settings,
        )
        .unwrap();
        ed.lines = lines.iter().map(|l| l.to_string()).collect();
        ed.view = View {
            x: 0,
            y: 0,
            width: 70,
            height: 10,
            gutter: 4,
        };
        ed
    }

    fn selected(ed: &Editor) -> Option<((usize, usize), (usize, usize))> {
        ed.selection_bounds()
    }

    fn click(ed: &mut Editor, col: u16, row: u16) {
        ed.mouse_down(col, row, Instant::now());
        ed.mouse_up();
    }

    #[test]
    fn a_click_puts_the_cursor_under_the_pointer() {
        let mut ed = editor(&["hello world", "second"]);
        click(&mut ed, 4 + 6, 0); // gutter is 4 wide
        assert_eq!((ed.cursor_row, ed.cursor_col), (0, 6));
        click(&mut ed, 4 + 2, 1);
        assert_eq!((ed.cursor_row, ed.cursor_col), (1, 2));
        assert!(!ed.has_selection());
    }

    #[test]
    fn clicking_past_the_end_or_below_the_text_goes_to_the_nearest_end() {
        let mut ed = editor(&["abc", "de"]);
        click(&mut ed, 40, 0);
        assert_eq!(
            (ed.cursor_row, ed.cursor_col),
            (0, 3),
            "past the end of the line"
        );
        click(&mut ed, 5, 8);
        assert_eq!(
            (ed.cursor_row, ed.cursor_col),
            (1, 1),
            "below the last line: that line"
        );
    }

    #[test]
    fn scrolling_and_the_view_offset_are_taken_into_account() {
        let lines: Vec<String> = (0..50).map(|i| format!("line number {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut ed = editor(&refs);
        ed.scroll = 20;
        ed.col_scroll = 5;
        click(&mut ed, 4, 3);
        assert_eq!(
            (ed.cursor_row, ed.cursor_col),
            (23, 5),
            "row 3 of the view, scrolled 5 columns"
        );
        // The text area need not start at the screen's corner.
        ed.view = View {
            x: 2,
            y: 1,
            width: 60,
            height: 10,
            gutter: 4,
        };
        ed.col_scroll = 0;
        click(&mut ed, 2 + 4 + 3, 1);
        assert_eq!((ed.cursor_row, ed.cursor_col), (20, 3));
        click(&mut ed, 0, 0);
        assert_eq!(
            (ed.cursor_row, ed.cursor_col),
            (20, 3),
            "outside the text area: ignored"
        );
    }

    #[test]
    fn tabs_and_the_halfway_rule() {
        let mut ed = editor(&["\tx"]);
        // Tab is 4 cells wide: nearer its start -> before it, nearer its end -> after it.
        click(&mut ed, 4, 0);
        assert_eq!(ed.cursor_col, 0);
        click(&mut ed, 4 + 1, 0);
        assert_eq!(ed.cursor_col, 0);
        click(&mut ed, 4 + 2, 0);
        assert_eq!(ed.cursor_col, 1);
        click(&mut ed, 4 + 4, 0);
        assert_eq!(ed.cursor_col, 1, "on the x");
        click(&mut ed, 4 + 5, 0);
        assert_eq!(ed.cursor_col, 2, "after it");
    }

    #[test]
    fn multi_byte_characters_count_as_one_cell() {
        let mut ed = editor(&["привет"]);
        click(&mut ed, 4 + 3, 0);
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn clicking_a_line_number_goes_to_the_line_without_selecting_anything() {
        let mut ed = editor(&["one", "two", "three"]);
        ed.selection_anchor = Some((0, 0));
        click(&mut ed, 1, 2);
        assert_eq!((ed.cursor_row, ed.cursor_col), (2, 0));
        assert!(!ed.has_selection(), "the number is not text");
    }

    #[test]
    fn dragging_selects_text_in_either_direction() {
        let mut ed = editor(&["hello world", "second line"]);
        ed.mouse_down(4 + 6, 0, Instant::now());
        ed.mouse_drag(4 + 3, 1);
        assert_eq!(selected(&ed), Some(((0, 6), (1, 3))));
        assert_eq!((ed.cursor_row, ed.cursor_col), (1, 3));
        ed.mouse_drag(4 + 1, 0); // back up and to the left of the start
        assert_eq!(selected(&ed), Some(((0, 1), (0, 6))));
        ed.mouse_drag(4 + 6, 0); // back to where it started: nothing selected
        assert!(!ed.has_selection());
        ed.mouse_up();
        ed.mouse_drag(4 + 9, 1);
        assert!(!ed.has_selection(), "a drag needs a press first");
    }

    #[test]
    fn a_drag_over_the_line_numbers_never_selects_them() {
        let mut ed = editor(&["hello world", "second line"]);
        ed.mouse_down(4 + 5, 1, Instant::now());
        ed.mouse_drag(0, 0); // far left: over the gutter, a row up
        assert_eq!(
            selected(&ed),
            Some(((0, 0), (1, 5))),
            "stops at the first column of the text"
        );
        // Whatever the selection covers is text only.
        assert_eq!(ed.extract_range((0, 0), (1, 5)), "hello world\nsecon");
    }

    #[test]
    fn dragging_right_of_the_text_selects_to_the_end_of_the_line() {
        let mut ed = editor(&["abc", "defgh"]);
        ed.mouse_down(4, 0, Instant::now());
        ed.mouse_drag(68, 1);
        assert_eq!(selected(&ed), Some(((0, 0), (1, 5))));
    }

    #[test]
    fn dragging_past_the_edges_scrolls() {
        let lines: Vec<String> = (0..100).map(|i| format!("l{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut ed = editor(&refs);
        ed.scroll = 30;
        ed.mouse_down(5, 5, Instant::now());
        ed.mouse_drag(5, 12); // below the 10-row view
        assert_eq!(ed.scroll, 31);
        assert_eq!(ed.cursor_row, 31 + 9, "the last visible row");
        ed.mouse_drag(5, 12);
        assert_eq!(ed.scroll, 32);
    }

    #[test]
    fn dragging_above_the_text_scrolls_up() {
        // With the status bar on top, the pointer can leave the text upward.
        let lines: Vec<String> = (0..100).map(|i| format!("l{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut ed = editor(&refs);
        ed.view = View {
            x: 0,
            y: 2,
            width: 70,
            height: 10,
            gutter: 4,
        };
        ed.scroll = 30;
        ed.mouse_down(5, 6, Instant::now());
        ed.mouse_drag(5, 0);
        assert_eq!(ed.scroll, 29);
        ed.mouse_drag(5, 1);
        assert_eq!(ed.scroll, 28);
        assert_eq!(ed.cursor_row, 28, "the first visible row");
        assert_eq!(selected(&ed), Some(((28, 1), (34, 1))));
        // At the top of the file it stops.
        ed.scroll = 0;
        ed.mouse_drag(5, 0);
        assert_eq!(ed.scroll, 0);
    }

    #[test]
    fn double_click_selects_a_word_triple_click_the_line() {
        let mut ed = editor(&["let foo_bar = 42;"]);
        let t = Instant::now();
        ed.mouse_down(4 + 6, 0, t);
        ed.mouse_up();
        ed.mouse_down(4 + 6, 0, t + Duration::from_millis(100));
        assert_eq!(
            selected(&ed),
            Some(((0, 4), (0, 11))),
            "foo_bar (underscore is a word character)"
        );
        ed.mouse_up();
        ed.mouse_down(4 + 6, 0, t + Duration::from_millis(200));
        assert_eq!(selected(&ed), Some(((0, 0), (0, 17))), "the whole line");
        ed.mouse_up();
        ed.mouse_down(4 + 6, 0, t + Duration::from_millis(300));
        assert!(!ed.has_selection(), "a fourth click starts over");
    }

    #[test]
    fn a_double_click_on_spaces_or_punctuation_selects_that_run() {
        let mut ed = editor(&["a   (b)"]);
        let t = Instant::now();
        for (cell, want) in [(2, (1, 4)), (4, (4, 5))] {
            ed.mouse_down(4 + cell, 0, t);
            ed.mouse_down(4 + cell, 0, t + Duration::from_millis(50));
            assert_eq!(
                selected(&ed),
                Some(((0, want.0), (0, want.1))),
                "cell {cell}"
            );
            ed.mouse_up();
            ed.mouse_down(0, 20, t + Duration::from_secs(10)); // reset
        }
    }

    #[test]
    fn slow_or_distant_clicks_are_not_double_clicks() {
        let mut ed = editor(&["hello world"]);
        let t = Instant::now();
        ed.mouse_down(4 + 2, 0, t);
        ed.mouse_down(4 + 2, 0, t + Duration::from_millis(900));
        assert!(!ed.has_selection(), "too slow");
        ed.mouse_down(4 + 2, 0, t + Duration::from_millis(1000));
        ed.mouse_down(4 + 8, 0, t + Duration::from_millis(1100));
        assert!(!ed.has_selection(), "a different spot");
    }

    #[test]
    fn multi_click_can_be_turned_off() {
        let mut ed = editor_with(&["hello world"], |c| c.mouse.double_click_ms = 0);
        assert_eq!(ed.settings.mouse.double_click, Duration::ZERO);
        let t = Instant::now();
        ed.mouse_down(4 + 2, 0, t);
        ed.mouse_down(4 + 2, 0, t);
        assert!(!ed.has_selection());
    }

    #[test]
    fn the_wheel_scrolls_the_view_and_takes_the_cursor_along() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut ed = editor(&refs);
        ed.cursor_row = 5;
        ed.scroll_view(3);
        assert_eq!(ed.scroll, 3);
        assert_eq!(ed.cursor_row, 5, "still in view: stays");
        ed.scroll_view(30);
        assert_eq!(ed.scroll, 33);
        assert_eq!(ed.cursor_row, 33, "dragged along to the top of the view");
        ed.scroll_view(1000);
        assert_eq!(ed.scroll, 90, "stops when the last line is at the bottom");
        assert_eq!(ed.cursor_row, 90);
        ed.scroll_view(-5);
        assert_eq!(ed.scroll, 85);
        assert_eq!(ed.cursor_row, 90, "inside the view: unmoved");
        ed.scroll_view(-1000);
        assert_eq!(ed.scroll, 0);
        assert_eq!(ed.cursor_row, 9, "pulled to the bottom of the view");
    }

    #[test]
    fn a_short_file_does_not_scroll() {
        let mut ed = editor(&["a", "b"]);
        ed.scroll_view(5);
        assert_eq!(ed.scroll, 0);
    }

    #[test]
    fn a_click_cancels_a_pending_quit_prompt_like_any_other_key() {
        let mut ed = editor(&["a"]);
        ed.confirm_quit = true;
        click(&mut ed, 5, 0);
        assert!(!ed.confirm_quit);
    }
}
