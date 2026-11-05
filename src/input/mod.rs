use crate::config::{Action, Keymap};
use crate::editor::Editor;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::Instant;

/// What the main loop should do after a key was handled.
pub enum Outcome {
    Continue,
    Quit,
    /// A save just succeeded and a background check should start.
    TriggerCheck,
}

/// Handles one key press. `keymap` says which action (if any) the key is
/// bound to; `check_on_save` is the `check.on_save` setting.
pub fn handle_key(
    editor: &mut Editor,
    key: KeyEvent,
    keymap: &Keymap,
    check_on_save: bool,
) -> Outcome {
    let resolved = keymap.resolve(&key);

    // The find bar takes the keyboard while it is open; any key that is
    // not for the bar closes it and is then handled as usual.
    if editor.search.active {
        if let Some(outcome) = handle_search_key(editor, &key, resolved) {
            return outcome;
        }
    }

    // While a "quit with unsaved changes?" prompt is showing, only quit,
    // save and Esc mean anything.
    if editor.confirm_quit {
        match resolved {
            Some((Action::Quit, _)) => return Outcome::Quit,
            Some((Action::Save, _)) => {
                editor.save();
                if !editor.modified {
                    return Outcome::Quit;
                }
            }
            _ if key.code == KeyCode::Esc => {
                editor.confirm_quit = false;
                editor.status_message = "Cancelled".to_string();
                editor.status_is_error = false;
            }
            _ => {}
        }
        return Outcome::Continue;
    }

    let Some((action, select)) = resolved else {
        // Not a binding: ordinary typing.
        if let KeyCode::Char(c) = key.code {
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                editor.insert_char(c);
            }
        }
        return Outcome::Continue;
    };

    match action {
        Action::Save => {
            if editor.save() && check_on_save && editor.settings.check {
                return Outcome::TriggerCheck;
            }
        }
        Action::SaveAndCheck => {
            if editor.save() {
                if editor.settings.check {
                    return Outcome::TriggerCheck;
                }
                editor.status_message = if editor.language.has_checker() {
                    "Saved (checks are turned off in the config)".to_string()
                } else {
                    "Saved (no checker for this file type)".to_string()
                };
                editor.status_is_error = false;
            }
        }
        Action::Quit => {
            if editor.modified {
                editor.confirm_quit = true;
                editor.status_message = unsaved_prompt(keymap);
                editor.status_is_error = true;
            } else {
                return Outcome::Quit;
            }
        }
        Action::Undo => editor.undo(),
        Action::Redo => editor.redo(),
        Action::SelectAll => editor.select_all(),
        Action::Copy => editor.copy(),
        Action::Cut => editor.cut(),
        Action::Paste => editor.paste(),
        Action::Newline => editor.newline(),
        Action::Backspace => editor.backspace(),
        Action::Delete => editor.delete_forward(),
        Action::DeleteWordBackward => editor.delete_word_backward(),
        Action::DeleteWordForward => editor.delete_word_forward(),
        Action::Indent => editor.insert_tab(),
        Action::Dedent => editor.dedent(),
        Action::MoveLeft if select => editor.move_left_select(),
        Action::MoveLeft => editor.move_left(),
        Action::MoveRight if select => editor.move_right_select(),
        Action::MoveRight => editor.move_right(),
        Action::MoveUp if select => editor.move_up_select(),
        Action::MoveUp => editor.move_up(),
        Action::MoveDown if select => editor.move_down_select(),
        Action::MoveDown => editor.move_down(),
        Action::MoveHome if select => editor.move_home_select(),
        Action::MoveHome => editor.move_home(),
        Action::MoveEnd if select => editor.move_end_select(),
        Action::MoveEnd => editor.move_end(),
        Action::PageUp if select => editor.page_up_select(),
        Action::PageUp => editor.page_up(),
        Action::PageDown if select => editor.page_down_select(),
        Action::PageDown => editor.page_down(),
        Action::WordLeft if select => editor.move_word_left_select(),
        Action::WordLeft => editor.move_word_left(),
        Action::WordRight if select => editor.move_word_right_select(),
        Action::WordRight => editor.move_word_right(),
        Action::Find => editor.open_search(),
        Action::FindNext => editor.search_next(),
        Action::FindPrevious => editor.search_prev(),
        // These only mean something inside the bar.
        Action::FindToggleCase | Action::FindToggleWholeWord | Action::FindToggleRegex => {}
    }

    Outcome::Continue
}

/// A mouse event. Returns whether anything changed that needs a redraw —
/// the pointer merely moving (which some terminals report constantly) does
/// not.
pub fn handle_mouse(editor: &mut Editor, event: MouseEvent, now: Instant) -> bool {
    if !editor.settings.mouse.enabled {
        return false;
    }
    let lines = editor.settings.mouse.scroll_lines as isize;
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Like any other key outside the find bar, a click closes it.
            editor.close_search();
            editor.mouse_down(event.column, event.row, now);
        }
        MouseEventKind::Drag(MouseButton::Left) => editor.mouse_drag(event.column, event.row),
        MouseEventKind::Up(MouseButton::Left) => {
            editor.mouse_up();
            return false;
        }
        MouseEventKind::ScrollUp => editor.scroll_view(-lines),
        MouseEventKind::ScrollDown => editor.scroll_view(lines),
        _ => return false,
    }
    true
}

/// A key pressed while the find bar is open. Returns `None` when the key
/// isn't for the bar: the bar has then been closed (leaving the current
/// match selected) and the caller carries on handling the key.
fn handle_search_key(
    editor: &mut Editor,
    key: &KeyEvent,
    resolved: Option<(Action, bool)>,
) -> Option<Outcome> {
    match resolved {
        // Enter goes to the next match, Shift+Enter to the previous one.
        Some((Action::Newline, _)) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            editor.search_prev()
        }
        Some((Action::Newline | Action::MoveDown | Action::FindNext, false)) => {
            editor.search_next()
        }
        Some((Action::MoveUp | Action::FindPrevious, false)) => editor.search_prev(),
        Some((Action::Backspace, _)) => editor.search_backspace(),
        Some((Action::DeleteWordBackward, _)) => editor.search_delete_word(),
        Some((Action::Paste, _)) => editor.search_paste(),
        Some((Action::Find, _)) => editor.open_search(),
        Some((Action::FindToggleCase, _)) => editor.search_toggle_case(),
        Some((Action::FindToggleWholeWord, _)) => editor.search_toggle_whole_word(),
        Some((Action::FindToggleRegex, _)) => editor.search_toggle_regex(),
        Some(_) => {
            editor.close_search();
            return None;
        }
        None => match key.code {
            KeyCode::Esc => editor.close_search(),
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                editor.search_insert(c)
            }
            // Anything else the bar doesn't know is ignored.
            _ => {}
        },
    }
    Some(Outcome::Continue)
}

/// The "unsaved changes" prompt, naming the keys as they're really bound.
fn unsaved_prompt(keymap: &Keymap) -> String {
    let quit = keymap.label(Action::Quit).unwrap_or_else(|| "quit".into());
    let mut msg = format!("Unsaved changes! {quit} again to discard");
    if let Some(save) = keymap.label(Action::Save) {
        msg.push_str(&format!(", {save} to save"));
    }
    msg.push_str(", Esc to cancel");
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KeysConfig;
    use std::path::PathBuf;

    fn keymap() -> Keymap {
        Keymap::from_config(&KeysConfig::default()).0
    }

    fn editor_with(text: &str) -> Editor {
        let mut ed = Editor::open(PathBuf::from("__input_test__.txt")).unwrap();
        for c in text.chars() {
            ed.insert_char(c);
        }
        ed
    }

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn handle(ed: &mut Editor, key: KeyEvent) -> Outcome {
        handle_key(ed, key, &keymap(), false)
    }

    #[test]
    fn every_encoding_of_delete_word_backward_works() {
        for (name, key) in [
            (
                "Ctrl+Backspace (kitty protocol)",
                press(KeyCode::Backspace, KeyModifiers::CONTROL),
            ),
            (
                "Ctrl+H (byte 0x08, e.g. xterm.js)",
                press(KeyCode::Char('h'), KeyModifiers::CONTROL),
            ),
            (
                "Ctrl+W (shell convention)",
                press(KeyCode::Char('w'), KeyModifiers::CONTROL),
            ),
            (
                "Alt+Backspace (ESC DEL)",
                press(KeyCode::Backspace, KeyModifiers::ALT),
            ),
        ] {
            let mut ed = editor_with("foo bar");
            handle(&mut ed, key);
            assert_eq!(ed.lines[0], "foo ", "{name}");
        }
    }

    #[test]
    fn plain_backspace_still_deletes_one_character() {
        let mut ed = editor_with("foo bar");
        handle(&mut ed, press(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(ed.lines[0], "foo ba");
    }

    #[test]
    fn unbound_printable_keys_are_typed_and_unbound_chords_are_ignored() {
        let mut ed = editor_with("");
        handle(&mut ed, press(KeyCode::Char('x'), KeyModifiers::NONE));
        handle(&mut ed, press(KeyCode::Char('Y'), KeyModifiers::SHIFT));
        handle(&mut ed, press(KeyCode::Char('k'), KeyModifiers::CONTROL)); // unbound chord
        handle(&mut ed, press(KeyCode::F(9), KeyModifiers::NONE)); // unbound key
        assert_eq!(ed.lines[0], "xY");
    }

    #[test]
    fn shift_with_a_motion_selects() {
        let mut ed = editor_with("hello");
        ed.move_home();
        handle(&mut ed, press(KeyCode::Right, KeyModifiers::SHIFT));
        handle(&mut ed, press(KeyCode::Right, KeyModifiers::SHIFT));
        assert!(ed.has_selection());
        assert_eq!(ed.cursor_col, 2);
        handle(&mut ed, press(KeyCode::Right, KeyModifiers::NONE));
        assert!(!ed.has_selection(), "plain motion drops the selection");
    }

    #[test]
    fn a_remapped_key_works_and_the_old_one_stops() {
        let keys = KeysConfig {
            undo: vec!["f5".into()],
            ..KeysConfig::default()
        };
        let km = Keymap::from_config(&keys).0;
        let mut ed = editor_with("abc");
        handle_key(
            &mut ed,
            press(KeyCode::Char('z'), KeyModifiers::CONTROL),
            &km,
            false,
        );
        assert_eq!(ed.lines[0], "abc", "Ctrl+Z no longer undoes");
        handle_key(
            &mut ed,
            press(KeyCode::F(5), KeyModifiers::NONE),
            &km,
            false,
        );
        assert_eq!(ed.lines[0], "", "F5 does");
    }

    fn ctrl(c: char) -> KeyEvent {
        press(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn save_triggers_a_check_only_when_on_save_is_set() {
        let path = std::env::temp_dir().join(format!("pc_input_save_{}.rs", std::process::id()));
        let mk = || {
            let mut ed = Editor::open(path.clone()).unwrap();
            ed.insert_char('x');
            ed
        };
        let km = keymap();

        let mut ed = mk();
        assert!(matches!(
            handle_key(&mut ed, ctrl('s'), &km, false),
            Outcome::Continue
        ));
        let mut ed = mk();
        assert!(matches!(
            handle_key(&mut ed, ctrl('s'), &km, true),
            Outcome::TriggerCheck
        ));
        // Ctrl+Shift+S always checks (when a checker exists), on_save or not.
        let mut ed = mk();
        let ctrl_shift_s = press(KeyCode::Char('S'), KeyModifiers::CONTROL);
        assert!(matches!(
            handle_key(&mut ed, ctrl_shift_s, &km, false),
            Outcome::TriggerCheck
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn save_and_check_explains_when_there_is_nothing_to_run() {
        let path = std::env::temp_dir().join(format!("pc_input_nochk_{}.txt", std::process::id()));
        let mut ed = Editor::open(path.clone()).unwrap();
        ed.insert_char('x');
        let ctrl_shift_s = press(KeyCode::Char('S'), KeyModifiers::CONTROL);
        let out = handle_key(&mut ed, ctrl_shift_s, &keymap(), false);
        assert!(matches!(out, Outcome::Continue));
        assert!(
            ed.status_message.contains("no checker"),
            "{}",
            ed.status_message
        );

        // A language that has a checker, with checks switched off.
        let rs = std::env::temp_dir().join(format!("pc_input_off_{}.rs", std::process::id()));
        let mut config = crate::config::Config::default();
        config.check.enabled = false;
        let lang = crate::language::Language::Rust;
        let mut ed = Editor::open_with(
            rs.clone(),
            lang,
            crate::config::Settings::resolve(&config, lang),
        )
        .unwrap();
        ed.insert_char('x');
        handle_key(&mut ed, ctrl_shift_s, &keymap(), false);
        assert!(
            ed.status_message.contains("turned off"),
            "{}",
            ed.status_message
        );
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(rs);
    }

    #[test]
    fn quitting_with_unsaved_changes_asks_using_the_real_key_names() {
        let km = keymap();
        let mut ed = editor_with("x");
        assert!(matches!(
            handle_key(&mut ed, ctrl('q'), &km, false),
            Outcome::Continue
        ));
        assert!(ed.confirm_quit);
        assert_eq!(
            ed.status_message,
            "Unsaved changes! ^Q again to discard, ^S to save, Esc to cancel"
        );
        // Esc backs out; the second Ctrl+Q discards.
        handle_key(&mut ed, press(KeyCode::Esc, KeyModifiers::NONE), &km, false);
        assert!(!ed.confirm_quit);
        handle_key(&mut ed, ctrl('q'), &km, false);
        assert!(matches!(
            handle_key(&mut ed, ctrl('q'), &km, false),
            Outcome::Quit
        ));

        // Remapped: the prompt names the new keys, and they work.
        let keys = KeysConfig {
            quit: vec!["f10".into()],
            save: vec!["f2".into()],
            ..KeysConfig::default()
        };
        let km = Keymap::from_config(&keys).0;
        let mut ed = editor_with("x");
        handle_key(
            &mut ed,
            press(KeyCode::F(10), KeyModifiers::NONE),
            &km,
            false,
        );
        assert_eq!(
            ed.status_message,
            "Unsaved changes! F10 again to discard, F2 to save, Esc to cancel"
        );
        assert!(matches!(
            handle_key(
                &mut ed,
                press(KeyCode::F(10), KeyModifiers::NONE),
                &km,
                false
            ),
            Outcome::Quit
        ));
    }

    #[test]
    fn shortcuts_work_on_the_russian_layout_and_russian_text_still_types() {
        let path = std::env::temp_dir().join(format!("pc_input_ru_{}.txt", std::process::id()));
        let mut ed = Editor::open(path.clone()).unwrap();
        for c in "привет".chars() {
            handle(&mut ed, press(KeyCode::Char(c), KeyModifiers::NONE));
        }
        handle(&mut ed, press(KeyCode::Char('П'), KeyModifiers::SHIFT));
        assert_eq!(ed.lines[0], "приветП", "Cyrillic typing is untouched");

        // Ctrl+Я undoes (Ctrl+Z on the US layout), Ctrl+Н redoes (Ctrl+Y).
        handle(&mut ed, ctrl('я'));
        assert_eq!(ed.lines[0], "");
        handle(&mut ed, ctrl('н'));
        assert_eq!(ed.lines[0], "приветП");
        // Ctrl+Ц deletes a word (Ctrl+W), Ctrl+Ы saves (Ctrl+S).
        handle(&mut ed, ctrl('ц'));
        assert_eq!(ed.lines[0], "");
        handle(&mut ed, ctrl('ы'));
        assert!(!ed.modified, "saved");
        // Ctrl+Й quits (Ctrl+Q); an unbound Ctrl+letter types nothing.
        handle(&mut ed, ctrl('ж'));
        assert_eq!(ed.lines[0], "");
        assert!(matches!(handle(&mut ed, ctrl('й')), Outcome::Quit));
        let _ = std::fs::remove_file(path);
    }

    // ---- the find bar ----

    fn type_text(ed: &mut Editor, text: &str) {
        for c in text.chars() {
            handle(ed, press(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn doc(lines: &[&str]) -> Editor {
        let mut ed = Editor::open(PathBuf::from("__input_find__.txt")).unwrap();
        ed.lines = lines.iter().map(|l| l.to_string()).collect();
        ed
    }

    #[test]
    fn ctrl_f_opens_the_bar_and_typing_goes_into_it_not_the_buffer() {
        let mut ed = doc(&["hello world", "world"]);
        handle(&mut ed, ctrl('f'));
        assert!(ed.search.active);
        type_text(&mut ed, "world");
        assert_eq!(ed.search.query, "world");
        assert_eq!(
            ed.lines,
            vec!["hello world", "world"],
            "the buffer is untouched"
        );
        assert_eq!(
            (ed.cursor_row, ed.cursor_col),
            (0, 6),
            "jumped to the first match"
        );
        assert!(!ed.modified);
    }

    #[test]
    fn enter_and_shift_enter_and_the_arrows_step_through_matches() {
        let mut ed = doc(&["x1 x2 x3"]);
        handle(&mut ed, ctrl('f'));
        type_text(&mut ed, "x");
        let col = |ed: &Editor| ed.cursor_col;
        assert_eq!(col(&ed), 0);
        handle(&mut ed, press(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(col(&ed), 3);
        handle(&mut ed, press(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(col(&ed), 6);
        handle(&mut ed, press(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(col(&ed), 3);
        handle(&mut ed, press(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(col(&ed), 0);
        handle(&mut ed, press(KeyCode::F(3), KeyModifiers::NONE));
        assert_eq!(col(&ed), 3);
        handle(&mut ed, press(KeyCode::F(3), KeyModifiers::SHIFT));
        assert_eq!(col(&ed), 0);
        assert!(ed.search.active, "stepping keeps the bar open");
    }

    #[test]
    fn escape_closes_the_bar_and_leaves_the_match_selected_to_type_over() {
        let mut ed = doc(&["say hello there"]);
        handle(&mut ed, ctrl('f'));
        type_text(&mut ed, "hello");
        handle(&mut ed, press(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!ed.search.active);
        assert!(ed.has_selection());
        type_text(&mut ed, "bye");
        assert_eq!(
            ed.lines[0], "say bye there",
            "typing now replaces the match"
        );
    }

    #[test]
    fn keys_that_are_not_for_the_bar_close_it_and_act_normally() {
        // Moving the cursor.
        let mut ed = doc(&["abc abc"]);
        handle(&mut ed, ctrl('f'));
        type_text(&mut ed, "abc");
        handle(&mut ed, press(KeyCode::Right, KeyModifiers::NONE));
        assert!(!ed.search.active);
        // Saving.
        let path = std::env::temp_dir().join(format!("pc_input_find_{}.txt", std::process::id()));
        let mut ed = Editor::open(path.clone()).unwrap();
        ed.lines = vec!["find me".to_string()];
        ed.modified = true;
        handle(&mut ed, ctrl('f'));
        type_text(&mut ed, "me");
        handle(&mut ed, ctrl('s'));
        assert!(!ed.search.active && !ed.modified, "closed, then saved");
        // Quitting asks about unsaved changes as usual.
        ed.insert_char('!');
        handle(&mut ed, ctrl('f'));
        assert!(matches!(handle(&mut ed, ctrl('q')), Outcome::Continue));
        assert!(!ed.search.active && ed.confirm_quit);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn backspace_ctrl_backspace_and_paste_edit_the_search_text() {
        let mut ed = doc(&["bar", "ignored"]);
        ed.select_all();
        ed.copy(); // the clipboard now holds "bar\nignored"
        handle(&mut ed, ctrl('f'));
        type_text(&mut ed, "foo bar");
        handle(&mut ed, press(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(ed.search.query, "foo ba");
        handle(&mut ed, press(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(ed.search.query, "foo ");
        handle(&mut ed, ctrl('v'));
        assert_eq!(ed.search.query, "foo bar", "only the first clipboard line");
        assert_eq!(
            ed.lines,
            vec!["bar", "ignored"],
            "nothing was pasted into the buffer"
        );
    }

    #[test]
    fn alt_keys_toggle_case_whole_word_and_regex() {
        let mut ed = doc(&["Cat cat concat"]);
        handle(&mut ed, ctrl('f'));
        type_text(&mut ed, "cat");
        assert_eq!(
            ed.search.total(),
            4 - 1,
            "smart case: lowercase text ignores case"
        );
        handle(&mut ed, press(KeyCode::Char('c'), KeyModifiers::ALT));
        assert_eq!(ed.search.total(), 2, "Alt+C: now case-sensitive");
        handle(&mut ed, press(KeyCode::Char('w'), KeyModifiers::ALT));
        assert_eq!(ed.search.total(), 1, "Alt+W: whole words only");
        handle(&mut ed, press(KeyCode::Char('r'), KeyModifiers::ALT));
        assert!(ed.search.regex, "Alt+R");
        assert_eq!(
            ed.lines[0], "Cat cat concat",
            "the keys did not type anything"
        );
    }

    #[test]
    fn find_works_on_the_russian_layout_too() {
        let mut ed = doc(&["один два"]);
        handle(&mut ed, ctrl('а')); // Ctrl+А is Ctrl+F
        assert!(ed.search.active);
        type_text(&mut ed, "два");
        assert_eq!(ed.cursor_col, 5);
        handle(&mut ed, press(KeyCode::Char('с'), KeyModifiers::ALT)); // Alt+С is Alt+C
        assert!(ed.search_case_sensitive());
    }

    #[test]
    fn f3_without_the_bar_steps_through_the_last_search() {
        let mut ed = doc(&["a x", "b x"]);
        handle(&mut ed, ctrl('f'));
        type_text(&mut ed, "x");
        handle(&mut ed, press(KeyCode::Esc, KeyModifiers::NONE));
        handle(&mut ed, press(KeyCode::F(3), KeyModifiers::NONE));
        assert!(!ed.search.active);
        assert_eq!(ed.selection_bounds_for_test(), Some(((1, 2), (1, 3))));
    }

    // ---- the mouse ----

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn drawn(lines: &[&str]) -> Editor {
        let mut ed = doc(lines);
        ed.view = crate::editor::View {
            x: 0,
            y: 0,
            width: 70,
            height: 10,
            gutter: 4,
        };
        ed
    }

    #[test]
    fn click_drag_and_release_select_text() {
        let mut ed = drawn(&["hello world"]);
        let now = Instant::now();
        assert!(handle_mouse(
            &mut ed,
            mouse(MouseEventKind::Down(MouseButton::Left), 4 + 2, 0),
            now
        ));
        assert!(handle_mouse(
            &mut ed,
            mouse(MouseEventKind::Drag(MouseButton::Left), 4 + 8, 0),
            now
        ));
        assert_eq!(ed.selection_bounds_for_test(), Some(((0, 2), (0, 8))));
        assert!(
            !handle_mouse(
                &mut ed,
                mouse(MouseEventKind::Up(MouseButton::Left), 4 + 8, 0),
                now
            ),
            "nothing to redraw"
        );
        // After release a stray drag does nothing.
        handle_mouse(
            &mut ed,
            mouse(MouseEventKind::Drag(MouseButton::Left), 4 + 1, 0),
            now,
        );
        assert_eq!(ed.selection_bounds_for_test(), Some(((0, 2), (0, 8))));
    }

    #[test]
    fn the_pointer_moving_or_other_buttons_do_not_trigger_redraws() {
        let mut ed = drawn(&["hello"]);
        let now = Instant::now();
        assert!(!handle_mouse(
            &mut ed,
            mouse(MouseEventKind::Moved, 5, 0),
            now
        ));
        assert!(!handle_mouse(
            &mut ed,
            mouse(MouseEventKind::Down(MouseButton::Right), 5, 0),
            now
        ));
        assert!(!handle_mouse(
            &mut ed,
            mouse(MouseEventKind::Down(MouseButton::Middle), 5, 0),
            now
        ));
        assert!(!handle_mouse(
            &mut ed,
            mouse(MouseEventKind::ScrollLeft, 5, 0),
            now
        ));
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn the_wheel_scrolls_by_the_configured_number_of_lines() {
        let lines: Vec<String> = (0..60).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut ed = drawn(&refs);
        let now = Instant::now();
        handle_mouse(&mut ed, mouse(MouseEventKind::ScrollDown, 10, 3), now);
        assert_eq!(ed.scroll, 3, "three lines by default");
        ed.settings.mouse.scroll_lines = 5;
        handle_mouse(&mut ed, mouse(MouseEventKind::ScrollDown, 10, 3), now);
        assert_eq!(ed.scroll, 8);
        handle_mouse(&mut ed, mouse(MouseEventKind::ScrollUp, 10, 3), now);
        assert_eq!(ed.scroll, 3);
    }

    #[test]
    fn with_the_mouse_switched_off_events_are_ignored() {
        let mut ed = drawn(&["hello world"]);
        ed.settings.mouse.enabled = false;
        let changed = handle_mouse(
            &mut ed,
            mouse(MouseEventKind::Down(MouseButton::Left), 4 + 5, 0),
            Instant::now(),
        );
        assert!(!changed);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn a_click_closes_the_find_bar() {
        let mut ed = drawn(&["foo bar foo"]);
        handle(&mut ed, ctrl('f'));
        type_text(&mut ed, "foo");
        assert!(ed.search.active);
        handle_mouse(
            &mut ed,
            mouse(MouseEventKind::Down(MouseButton::Left), 4 + 5, 0),
            Instant::now(),
        );
        assert!(!ed.search.active);
        assert_eq!((ed.cursor_row, ed.cursor_col), (0, 5));
    }

    #[test]
    fn a_clean_buffer_quits_immediately() {
        let mut ed = editor_with("");
        ed.modified = false;
        assert!(matches!(handle(&mut ed, ctrl('q')), Outcome::Quit));
    }
}
