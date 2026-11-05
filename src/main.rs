mod check;
mod cli;
mod config;
mod editor;
mod file_io;
mod input;
mod language;
mod markup;
mod syntax;
mod ui;

use anyhow::{Context, Result};
use check::{CheckMessage, CheckOptions, Checker};
use config::{CursorStyle, HintSetting, Keymap, LoadOptions, Settings, UiSettings};
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use editor::Editor;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use syntax::Highlighter;

/// Tracks whether we pushed the Kitty keyboard protocol flags, so the
/// panic hook knows whether it needs to pop them too.
static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

/// Same, for mouse reporting.
static MOUSE_CAPTURED: AtomicBool = AtomicBool::new(false);

/// Same, for a cursor shape set by `display.cursor_style`.
static CURSOR_SHAPE_CHANGED: AtomicBool = AtomicBool::new(false);

/// Everything the running editor needs from the configuration.
struct Session {
    keymap: Keymap,
    ui: UiSettings,
    highlighter: Highlighter,
    check_on_save: bool,
}

fn main() -> Result<()> {
    let cli = match cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("PC: {message}");
            std::process::exit(2);
        }
    };

    if cli.help {
        print!("{}", cli::USAGE);
        return Ok(());
    }
    if cli.version {
        println!("PC {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let load_options = LoadOptions {
        explicit: cli.config.clone(),
        disabled: cli.no_config,
    };

    if cli.init_config {
        return init_config(&cli);
    }
    if cli.config_path {
        return show_config_paths(&cli);
    }

    let loaded = match config::load(&load_options, cli.file.as_deref()) {
        Ok(loaded) => loaded,
        Err(message) => {
            eprintln!("PC: {message}");
            std::process::exit(1);
        }
    };

    if cli.print_config {
        for warning in &loaded.warnings {
            eprintln!("warning: {warning}");
        }
        println!(
            "# Effective configuration{}",
            match &cli.file {
                Some(file) => format!(" for {}", file.display()),
                None => String::new(),
            }
        );
        if loaded.sources.is_empty() {
            println!("# Read from: built-in defaults only");
        } else {
            for source in &loaded.sources {
                println!("# Read from: {}", source.display());
            }
        }
        println!();
        print!("{}", toml::to_string_pretty(&loaded.config)?);
        return Ok(());
    }

    let Some(path) = cli.file else {
        eprint!("{}", cli::USAGE);
        std::process::exit(1);
    };

    let config = loaded.config;
    let mut warnings = loaded.warnings;
    let language = config.language_of(&path);
    let settings = Settings::resolve(&config, language);
    let (highlighter, theme_warning) =
        Highlighter::new(language, settings.highlight, &config.syntax.theme);
    warnings.extend(theme_warning);

    let (ui, _) = UiSettings::from_config(&config);
    let (keymap, _) = Keymap::from_config(&config.keys);
    let session = Session {
        keymap,
        ui,
        highlighter,
        check_on_save: config.check.on_save,
    };

    let mut editor = Editor::open_with(path, language, settings).context("failed to open file")?;
    editor.status_message = match &session.ui.hint {
        HintSetting::Auto => session.keymap.hint(),
        HintSetting::Off => String::new(),
        HintSetting::Text(text) => text.clone(),
    };
    if let Some(first) = warnings.first() {
        let more = match warnings.len() {
            1 => String::new(),
            n => format!(" (+{} more; see `PC --print-config`)", n - 1),
        };
        editor.status_message = format!("config: {first}{more}");
        editor.status_is_error = true;
    }

    install_panic_restore_hook();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    if let Some(shape) = cursor_command(session.ui.cursor_style) {
        execute!(stdout, shape)?;
        CURSOR_SHAPE_CHANGED.store(true, Ordering::Relaxed);
    }

    // With mouse reporting on, clicks and drags reach the editor instead of
    // the terminal's own text selection — which is what keeps the line
    // numbers out of a selection.
    if editor.settings.mouse.enabled {
        execute!(stdout, EnableMouseCapture)?;
        MOUSE_CAPTURED.store(true, Ordering::Relaxed);
    }

    // The base terminal protocol can't distinguish e.g. Ctrl+Backspace
    // from plain Backspace at all — both send the same byte. Terminals
    // that support the Kitty keyboard protocol (Ghostty, Kitty, WezTerm,
    // foot, ...) can report modifiers on such keys if we ask for it.
    // Best-effort: older/unsupported terminals just ignore the query.
    let keyboard_enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if keyboard_enhanced {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        KEYBOARD_ENHANCED.store(true, Ordering::Relaxed);
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut checker = Checker::new(CheckOptions::from_config(&config));
    let result = run_app(&mut terminal, &mut editor, &session, &mut checker);

    if keyboard_enhanced {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    if CURSOR_SHAPE_CHANGED.load(Ordering::Relaxed) {
        let _ = execute!(terminal.backend_mut(), SetCursorStyle::DefaultUserShape);
    }
    if MOUSE_CAPTURED.load(Ordering::Relaxed) {
        let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
    }
    restore_terminal(&mut terminal)?;

    result
}

/// `--init-config`: writes the documented default configuration.
fn init_config(cli: &cli::Cli) -> Result<()> {
    let Some(target) = cli.config.clone().or_else(config::user_config_path) else {
        eprintln!("PC: can't tell where the config file should go; use --config FILE");
        std::process::exit(1);
    };
    if target.exists() && !cli.force {
        eprintln!(
            "PC: {} already exists (use --force to overwrite it)",
            target.display()
        );
        std::process::exit(1);
    }
    if let Some(dir) = target.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    std::fs::write(&target, config::DEFAULT_CONFIG_TOML)
        .with_context(|| format!("can't write {}", target.display()))?;
    println!("Wrote {}", target.display());
    Ok(())
}

/// `--config-path`: which files would be read for the given file.
fn show_config_paths(cli: &cli::Cli) -> Result<()> {
    let describe = |path: &Path| {
        let state = if path.exists() { "found" } else { "not there" };
        format!("{} ({state})", path.display())
    };
    if cli.no_config {
        println!("--no-config: no config files are read");
        return Ok(());
    }
    match cli
        .config
        .as_deref()
        .map(Path::to_path_buf)
        .or_else(config::user_config_path)
    {
        Some(user) => println!("user config:    {}", describe(&user)),
        None => println!("user config:    (no config directory on this system)"),
    }
    match cli.file.as_deref().and_then(config::find_project_config) {
        Some(project) => println!("project config: {}", describe(&project)),
        None => println!(
            "project config: none ({} in the file's folder or above)",
            config::PROJECT_CONFIG_NAME
        ),
    }
    Ok(())
}

/// The terminal command that selects `style`; `None` leaves the cursor
/// as the terminal has it.
fn cursor_command(style: CursorStyle) -> Option<SetCursorStyle> {
    Some(match style {
        CursorStyle::Default => return None,
        CursorStyle::BlinkingBlock => SetCursorStyle::BlinkingBlock,
        CursorStyle::Block => SetCursorStyle::SteadyBlock,
        CursorStyle::BlinkingUnderline => SetCursorStyle::BlinkingUnderScore,
        CursorStyle::Underline => SetCursorStyle::SteadyUnderScore,
        CursorStyle::BlinkingBar => SetCursorStyle::BlinkingBar,
        CursorStyle::Bar => SetCursorStyle::SteadyBar,
    })
}

/// Redraws only when something actually changed, and only wakes up
/// periodically (instead of redrawing on that tick) to check whether a
/// background Ctrl+S diagnostics check has finished — an idle editor
/// should burn ~0% CPU, not repaint several times a second forever.
fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    editor: &mut Editor,
    session: &Session,
    checker: &mut Checker,
) -> Result<()> {
    let draw = |terminal: &mut Terminal<B>, editor: &mut Editor| {
        terminal
            .draw(|f| ui::draw(f, editor, &session.highlighter, &session.ui))
            .map(|_| ())
    };
    draw(terminal, editor)?;

    loop {
        let mut needs_redraw = false;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                        match input::handle_key(editor, key, &session.keymap, session.check_on_save)
                        {
                            input::Outcome::Quit => return Ok(()),
                            input::Outcome::Continue => {}
                            input::Outcome::TriggerCheck => {
                                if checker.start(editor.file_path.clone(), editor.language) {
                                    editor.status_message = "Checking…".to_string();
                                    editor.status_is_error = false;
                                }
                            }
                        }
                        needs_redraw = true;
                    }
                }
                Event::Mouse(mouse) => {
                    needs_redraw = input::handle_mouse(editor, mouse, std::time::Instant::now());
                }
                Event::Resize(_, _) => needs_redraw = true,
                _ => {}
            }
        } else if let Some(msg) = checker.poll() {
            apply_check_message(editor, msg);
            needs_redraw = true;
        }

        if needs_redraw {
            draw(terminal, editor)?;
        }
    }
}

fn apply_check_message(editor: &mut Editor, msg: CheckMessage) {
    match msg {
        CheckMessage::Finished(diagnostics) => {
            let errors = diagnostics
                .iter()
                .filter(|d| d.severity == check::Severity::Error)
                .count();
            let warnings = diagnostics.len() - errors;
            editor.status_message = if diagnostics.is_empty() {
                "No errors".to_string()
            } else {
                // Errors take priority over warnings for the headline
                // diagnostic shown alongside the count.
                let headline = diagnostics
                    .iter()
                    .filter(|d| d.severity == check::Severity::Error)
                    .chain(diagnostics.iter())
                    .next()
                    .expect("diagnostics is non-empty");
                let code_prefix = if headline.code.is_empty() {
                    String::new()
                } else {
                    format!("{} ", headline.code)
                };
                format!(
                    "{errors} error(s), {warnings} warning(s) — Ln {}: {code_prefix}{}",
                    headline.line + 1,
                    headline.message
                )
            };
            editor.status_is_error = errors > 0;
            editor.set_diagnostics(diagnostics);
        }
        CheckMessage::ToolMissing(tool) => {
            editor.status_message =
                format!("{tool} not found — install it, or point [tools] in the config at it");
            editor.status_is_error = true;
        }
        CheckMessage::Failed(e) => {
            editor.status_message = format!("Check failed: {e}");
            editor.status_is_error = true;
        }
    }
}

fn restore_terminal<B: Backend + io::Write>(terminal: &mut Terminal<B>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Make sure the terminal isn't left in raw/alternate-screen mode (or with
/// the keyboard protocol still pushed) if we panic while the TUI is
/// active.
fn install_panic_restore_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if KEYBOARD_ENHANCED.load(Ordering::Relaxed) {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        if CURSOR_SHAPE_CHANGED.load(Ordering::Relaxed) {
            let _ = execute!(io::stdout(), SetCursorStyle::DefaultUserShape);
        }
        if MOUSE_CAPTURED.load(Ordering::Relaxed) {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cursor_style_maps_to_its_terminal_command() {
        assert!(
            cursor_command(CursorStyle::Default).is_none(),
            "default leaves the terminal alone"
        );
        assert!(matches!(
            cursor_command(CursorStyle::Block),
            Some(SetCursorStyle::SteadyBlock)
        ));
        assert!(matches!(
            cursor_command(CursorStyle::BlinkingBlock),
            Some(SetCursorStyle::BlinkingBlock)
        ));
        assert!(matches!(
            cursor_command(CursorStyle::Bar),
            Some(SetCursorStyle::SteadyBar)
        ));
        assert!(matches!(
            cursor_command(CursorStyle::BlinkingBar),
            Some(SetCursorStyle::BlinkingBar)
        ));
        assert!(matches!(
            cursor_command(CursorStyle::Underline),
            Some(SetCursorStyle::SteadyUnderScore)
        ));
        assert!(matches!(
            cursor_command(CursorStyle::BlinkingUnderline),
            Some(SetCursorStyle::BlinkingUnderScore)
        ));
    }
}
