use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::config::{Config, ToolsConfig};
use crate::language::Language;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Clone)]
pub struct Diagnostic {
    /// 0-indexed, to match the editor's own coordinates.
    pub line: usize,
    pub col: usize,
    pub severity: Severity,
    pub code: String,
    pub message: String,
}

pub enum CheckMessage {
    Finished(Vec<Diagnostic>),
    /// The executable that couldn't be started, as configured.
    ToolMissing(String),
    Failed(String),
}

/// The parts of the configuration the checkers use.
#[derive(Clone, Debug)]
pub struct CheckOptions {
    pub tools: ToolsConfig,
    /// How long any one external tool may run before it is killed.
    pub timeout: Duration,
    /// `auto` (match the installed SDK) or a target framework like `net8.0`.
    pub dotnet_target_framework: String,
    pub rust_edition: String,
    /// Extra arguments for the C / C++ compiler.
    pub c_flags: Vec<String>,
    pub cpp_flags: Vec<String>,
}

impl CheckOptions {
    pub fn from_config(config: &Config) -> Self {
        Self {
            tools: config.tools.clone(),
            timeout: Duration::from_secs(config.check.timeout_seconds.max(1)),
            dotnet_target_framework: config.check.dotnet_target_framework.trim().to_string(),
            rust_edition: config.check.rust_edition.trim().to_string(),
            c_flags: config.check.c_flags.clone(),
            cpp_flags: config.check.cpp_flags.clone(),
        }
    }
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

/// Runs the appropriate checker for the file's language on a background
/// thread (so the editor never blocks on it) and reports parsed
/// diagnostics back over a channel. Only one check runs at a time.
pub struct Checker {
    options: CheckOptions,
    receiver: Option<Receiver<CheckMessage>>,
    running: bool,
}

impl Checker {
    pub fn new(options: CheckOptions) -> Self {
        Self {
            options,
            receiver: None,
            running: false,
        }
    }

    /// Starts a check for `file_path` unless one is already in flight.
    /// Returns `true` if a new check was started.
    pub fn start(&mut self, file_path: PathBuf, language: Language) -> bool {
        if self.running {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.running = true;
        let options = self.options.clone();
        thread::spawn(move || {
            let _ = tx.send(run_check(&file_path, language, &options));
        });
        true
    }

    /// Non-blocking: returns a message if the running check has finished.
    pub fn poll(&mut self) -> Option<CheckMessage> {
        let rx = self.receiver.as_ref()?;
        match rx.try_recv() {
            Ok(msg) => {
                self.running = false;
                Some(msg)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.running = false;
                self.receiver = None;
                None
            }
        }
    }
}

fn run_check(file_path: &Path, language: Language, options: &CheckOptions) -> CheckMessage {
    match language {
        Language::CSharp => run_check_csharp(file_path, options),
        Language::C => run_check_c_family(file_path, options, false),
        Language::Cpp => run_check_c_family(file_path, options, true),
        Language::Java => run_check_java(file_path, options),
        Language::Go => run_check_go(file_path, options),
        Language::Php => run_check_php(file_path, options),
        Language::TypeScript => run_check_typescript(file_path, options),
        Language::Kotlin => run_check_kotlin(file_path, options),
        Language::Swift => run_check_swift(file_path, options),
        Language::Rust => run_check_rust(file_path, options),
        Language::JavaScript => run_check_javascript(file_path, options),
        Language::Python => run_check_python(file_path, options),
        Language::GdScript => run_check_gdscript(file_path, options),
        Language::Html => run_check_markup(file_path, true),
        Language::Xml => run_check_markup(file_path, false),
        Language::Json => run_check_json(file_path),
        Language::Yaml => run_check_yaml(file_path),
        Language::Css
        | Language::Markdown
        | Language::ObjectiveC
        | Language::Dart
        | Language::Scala
        | Language::Other => {
            CheckMessage::Failed("No checker available for this file type".to_string())
        }
    }
}

// ==================== running external tools ====================

enum RunError {
    /// The executable doesn't exist (or isn't executable).
    NotFound,
    TimedOut,
    Other(String),
}

/// Collects a child's output stream on its own thread so a full pipe can
/// never stall the child. The buffer is shared, not returned from the
/// thread, so output read so far is still available if the stream is held
/// open by a grandchild process (e.g. a lingering build server).
struct Drain {
    buffer: Arc<Mutex<Vec<u8>>>,
    thread: thread::JoinHandle<()>,
}

impl Drain {
    fn start(mut stream: impl Read + Send + 'static) -> Self {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&buffer);
        let thread = thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                shared.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        Self { buffer, thread }
    }

    /// Everything read so far, after giving the stream a moment to finish.
    fn finish(self) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_millis(500);
        while !self.thread.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        std::mem::take(&mut *self.buffer.lock().unwrap())
    }
}

/// `ETXTBSY` ("text file busy"): the executable is still open for writing
/// somewhere, e.g. a tool that was only just installed or generated.
#[cfg(unix)]
const ETXTBSY: i32 = 26;

/// Spawns `command`, retrying a few times if the executable is momentarily
/// busy (it clears within milliseconds).
fn spawn_retrying(command: &mut Command) -> std::io::Result<std::process::Child> {
    let mut attempts_left = 20;
    loop {
        match command.spawn() {
            #[cfg(unix)]
            Err(e) if e.raw_os_error() == Some(ETXTBSY) && attempts_left > 0 => {
                attempts_left -= 1;
                thread::sleep(Duration::from_millis(25));
            }
            result => return result,
        }
    }
}

/// `Command::output()` with a deadline: the child is killed if it runs
/// longer than `timeout`.
fn run(command: &mut Command, timeout: Duration) -> Result<Output, RunError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_retrying(command).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => RunError::NotFound,
        _ => RunError::Other(e.to_string()),
    })?;
    let stdout = child.stdout.take().map(Drain::start);
    let stderr = child.stderr.take().map(Drain::start);

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::TimedOut);
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(RunError::Other(e.to_string())),
        }
    };
    Ok(Output {
        status,
        stdout: stdout.map(Drain::finish).unwrap_or_default(),
        stderr: stderr.map(Drain::finish).unwrap_or_default(),
    })
}

/// Runs `command`, turning every way it can fail into the message the
/// editor shows. `tool` is the executable as configured.
fn run_tool(
    tool: &str,
    command: &mut Command,
    options: &CheckOptions,
) -> Result<Output, CheckMessage> {
    run(command, options.timeout).map_err(|e| match e {
        RunError::NotFound => CheckMessage::ToolMissing(tool.to_string()),
        RunError::TimedOut => CheckMessage::Failed(format!(
            "{tool} timed out after {}s (check.timeout_seconds)",
            options.timeout.as_secs()
        )),
        RunError::Other(e) => CheckMessage::Failed(e),
    })
}

/// The first of `candidates` that starts at all with `--version`.
fn first_available(candidates: &[&str], options: &CheckOptions) -> Option<String> {
    candidates
        .iter()
        .find(|c| run(Command::new(c).arg("--version"), options.timeout).is_ok())
        .map(|c| c.to_string())
}

/// Compares `diag_file` (as printed by a compiler/interpreter, which may
/// be relative, absolute, or from a different working directory) against
/// the file we're actually checking, canonicalizing both sides so
/// differently-formatted-but-equal paths still match.
fn matches_target_file(diag_file: &Path, target: &Path) -> bool {
    match (
        std::fs::canonicalize(target).ok(),
        std::fs::canonicalize(diag_file).ok(),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => diag_file.file_name() == target.file_name(),
    }
}

// ==================== C# (dotnet build) ====================

fn run_check_csharp(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let dotnet = options.tools.dotnet.as_str();
    let version_output = match run_tool(dotnet, Command::new(dotnet).arg("--version"), options) {
        Ok(o) if o.status.success() => o,
        Ok(_) => return CheckMessage::ToolMissing(dotnet.to_string()),
        Err(msg) => return msg,
    };
    let target_framework = if options.dotnet_target_framework.is_empty()
        || options.dotnet_target_framework.eq_ignore_ascii_case("auto")
    {
        let version = String::from_utf8_lossy(&version_output.stdout);
        let major = version.split('.').next().unwrap_or("8").trim().to_string();
        format!("net{major}.0")
    } else {
        options.dotnet_target_framework.clone()
    };

    let project = match find_ancestor_project(file_path, "csproj") {
        Some(csproj) => csproj,
        None => match write_synthetic_csproj(file_path, &target_framework) {
            Ok(csproj) => csproj,
            Err(e) => return CheckMessage::Failed(e.to_string()),
        },
    };

    let output = match run_tool(
        dotnet,
        Command::new(dotnet)
            .args(["build", "--nologo", "-v", "q"])
            .arg(&project),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };

    CheckMessage::Finished(parse_csharp_diagnostics(
        &combined_output(&output),
        file_path,
    ))
}

/// Looks for the nearest file in `file_path`'s directory or any ancestor
/// whose name satisfies `wanted`, so files that already live in a real
/// project get built/checked with their actual references instead of a
/// bare synthetic one.
fn find_ancestor_file(file_path: &Path, wanted: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut dir = file_path.parent()?;
    loop {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && wanted(&path) {
                    return Some(path);
                }
            }
        }
        dir = dir.parent()?;
    }
}

fn find_ancestor_project(file_path: &Path, ext: &str) -> Option<PathBuf> {
    find_ancestor_file(file_path, |p| {
        p.extension().and_then(|e| e.to_str()) == Some(ext)
    })
}

/// The nearest `Cargo.toml`. (Not just any `.toml`: `rustfmt.toml` or a
/// `.pis-code.toml` next to the file must not hide the real manifest.)
fn find_cargo_manifest(file_path: &Path) -> Option<PathBuf> {
    find_ancestor_file(file_path, |p| {
        p.file_name().and_then(|n| n.to_str()) == Some("Cargo.toml")
    })
}

fn combined_output(output: &std::process::Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// For a loose file with no project, builds a minimal throwaway project
/// under a stable temp directory (keyed by the file's own path, so repeat
/// checks reuse the same `obj`/`bin` and stay fast).
fn write_synthetic_csproj(file_path: &Path, target_framework: &str) -> anyhow::Result<PathBuf> {
    let absolute = std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.to_path_buf());
    let project_dir = stable_temp_dir(&absolute);

    let csproj = format!(
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>{target_framework}</TargetFramework>
    <Nullable>disable</Nullable>
    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>
    <GenerateAssemblyInfo>false</GenerateAssemblyInfo>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="{}" />
  </ItemGroup>
</Project>
"#,
        absolute.display()
    );

    let project_path = project_dir.join("check.csproj");
    std::fs::write(&project_path, csproj)?;
    Ok(project_path)
}

/// A stable per-file temp directory (keyed by the file's own absolute
/// path), reused across repeated checks so build caches stay warm.
fn stable_temp_dir(absolute_file: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    absolute_file.hash(&mut hasher);
    let key = hasher.finish();
    let dir = std::env::temp_dir()
        .join("pis-code-check")
        .join(format!("{key:x}"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn parse_csharp_diagnostics(output: &str, file_path: &Path) -> Vec<Diagnostic> {
    parse_paren_diagnostics(output, file_path, true)
}

/// MSBuild and `tsc` both print `path(line,col): error CODE: message`.
/// MSBuild appends ` [project.csproj]`, which `strip_project` removes.
fn parse_paren_diagnostics(output: &str, file_path: &Path, strip_project: bool) -> Vec<Diagnostic> {
    let re = Regex::new(r"^(?P<file>.+)\((?P<line>\d+),(?P<col>\d+)\): (?P<sev>error|warning) (?P<code>[A-Za-z0-9]+): (?P<msg>.*)$")
        .expect("static regex is valid");

    let mut diagnostics = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        let Some(caps) = re.captures(line) else {
            continue;
        };
        if !matches_target_file(Path::new(&caps["file"]), file_path) {
            continue;
        }

        let line_no: usize = caps["line"].parse().unwrap_or(1);
        let col_no: usize = caps["col"].parse().unwrap_or(1);
        let severity = if &caps["sev"] == "error" {
            Severity::Error
        } else {
            Severity::Warning
        };

        let mut message = caps["msg"].to_string();
        if strip_project {
            if let Some(idx) = message.rfind(" [") {
                if message.ends_with(']') {
                    message.truncate(idx);
                }
            }
        }

        diagnostics.push(Diagnostic {
            line: line_no.saturating_sub(1),
            col: col_no.saturating_sub(1),
            severity,
            code: caps["code"].to_string(),
            message,
        });
    }
    diagnostics
}

// ==================== C, C++ (compiler, syntax-only) ====================

/// A diagnostic's file as the compiler printed it, resolved against the
/// directory the compiler ran in.
fn printed_path(base_dir: &Path, printed: &str) -> PathBuf {
    base_dir.join(printed.strip_prefix("vet: ").unwrap_or(printed))
}

fn absolute_path(file_path: &Path) -> PathBuf {
    std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.to_path_buf())
}

/// Asks tools that translate their messages (gcc, javac, ...) for English,
/// since the parsers below look for the words `error` and `warning`.
fn in_english(command: &mut Command) -> &mut Command {
    command.env("LC_ALL", "C").env("LANGUAGE", "C")
}

fn run_check_c_family(file_path: &Path, options: &CheckOptions, cpp: bool) -> CheckMessage {
    let (configured, candidates, flags, language) = if cpp {
        (
            &options.tools.cxx,
            ["c++", "g++", "clang++"],
            &options.cpp_flags,
            "c++",
        )
    } else {
        (
            &options.tools.cc,
            ["cc", "gcc", "clang"],
            &options.c_flags,
            "c",
        )
    };
    let compiler = if configured.is_empty() {
        match first_available(&candidates, options) {
            Some(found) => found,
            None => return CheckMessage::ToolMissing(candidates[0].to_string()),
        }
    } else {
        configured.clone()
    };

    // `-x` pins the language, so a `.h` associated with C++ is checked as C++.
    let absolute = absolute_path(file_path);
    let dir = absolute.parent().unwrap_or(Path::new(".")).to_path_buf();
    let output = match run_tool(
        &compiler,
        in_english(Command::new(&compiler).current_dir(&dir))
            .args(["-fsyntax-only", "-Wall", "-fdiagnostics-color=never"])
            .args(flags)
            .args(["-x", language])
            .arg(&absolute),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };
    CheckMessage::Finished(parse_colon_diagnostics(
        &combined_output(&output),
        &dir,
        file_path,
    ))
}

/// The `path:line:col: severity: message` shape shared by gcc, clang,
/// kotlinc, swiftc and Go. Go prints no severity (every line is an error),
/// gcc/clang add `note:` lines that only elaborate on the previous one.
fn parse_colon_diagnostics(output: &str, base_dir: &Path, file_path: &Path) -> Vec<Diagnostic> {
    let re = Regex::new(
        r"^(?P<file>.+?):(?P<line>\d+):(?:(?P<col>\d+):)?\s*(?:(?P<sev>fatal error|error|warning|note|remark)\s*:\s*)?(?P<msg>.+)$",
    )
    .expect("static regex is valid");
    let option_re = Regex::new(r"^(?P<msg>.*\S)\s+\[(?P<code>-W[\w+=-]+)\]$").expect("valid regex");

    let mut diagnostics = Vec::new();
    for line in output.lines() {
        let Some(caps) = re.captures(line.trim_end()) else {
            continue;
        };
        let severity = match caps.name("sev").map(|m| m.as_str()) {
            Some("note" | "remark") => continue,
            Some("warning") => Severity::Warning,
            _ => Severity::Error,
        };
        if !matches_target_file(&printed_path(base_dir, &caps["file"]), file_path) {
            continue;
        }

        let mut message = caps["msg"].trim().to_string();
        // gcc/clang tag warnings with the option that produced them.
        let mut code = String::new();
        if let Some(c) = option_re.captures(&message.clone()) {
            code = c["code"].to_string();
            message = c["msg"].to_string();
        }
        // A header checked on its own always trips this one (worded
        // differently, and given an option name, by newer compilers).
        if code == "-Wpragma-once-outside-header"
            || (message.contains("#pragma once") && message.contains("in main file"))
        {
            continue;
        }

        diagnostics.push(Diagnostic {
            line: caps["line"].parse::<usize>().unwrap_or(1).saturating_sub(1),
            col: caps
                .name("col")
                .and_then(|c| c.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1),
            severity,
            code,
            message,
        });
    }
    diagnostics
}

// ==================== Java (javac) ====================

fn run_check_java(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let javac = options.tools.javac.as_str();
    let absolute = absolute_path(file_path);
    let dir = absolute.parent().unwrap_or(Path::new(".")).to_path_buf();
    // Compiled classes go to a scratch folder instead of next to the source.
    let out_dir = stable_temp_dir(&absolute);
    let output = match run_tool(
        javac,
        in_english(Command::new(javac).current_dir(&dir))
            .arg("-d")
            .arg(&out_dir)
            .arg(&absolute),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };
    CheckMessage::Finished(parse_javac_diagnostics(
        &combined_output(&output),
        &dir,
        file_path,
    ))
}

/// javac prints `path:line: error: message`, the source line, then a line
/// with a caret under the problem — the caret gives the column.
fn parse_javac_diagnostics(output: &str, base_dir: &Path, file_path: &Path) -> Vec<Diagnostic> {
    let re = Regex::new(r"^(?P<file>.+?):(?P<line>\d+): (?P<sev>error|warning): (?P<msg>.*)$")
        .expect("static regex is valid");
    let lines: Vec<&str> = output.lines().collect();

    let mut diagnostics = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(caps) = re.captures(line.trim_end()) else {
            continue;
        };
        if !matches_target_file(&printed_path(base_dir, &caps["file"]), file_path) {
            continue;
        }
        let col = lines[i + 1..(i + 4).min(lines.len())]
            .iter()
            .find(|l| l.trim() == "^")
            .and_then(|l| l.find('^'))
            .unwrap_or(0);

        // Lint warnings are tagged `[unchecked]`, `[deprecation]`, ...
        let mut message = caps["msg"].to_string();
        let mut code = String::new();
        if let Some(rest) = message.strip_prefix('[') {
            if let Some((tag, text)) = rest.split_once("] ") {
                code = tag.to_string();
                message = text.to_string();
            }
        }
        diagnostics.push(Diagnostic {
            line: caps["line"].parse::<usize>().unwrap_or(1).saturating_sub(1),
            col,
            severity: if &caps["sev"] == "error" {
                Severity::Error
            } else {
                Severity::Warning
            },
            code,
            message,
        });
    }
    diagnostics
}

// ==================== Go (go vet) ====================

fn run_check_go(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let go = options.tools.go.as_str();
    let absolute = absolute_path(file_path);
    let dir = absolute.parent().unwrap_or(Path::new(".")).to_path_buf();
    // Inside a module the whole package is checked, so the file can use
    // its neighbors; a loose file is checked alone.
    let in_module = find_ancestor_file(&absolute, |p| {
        p.file_name().and_then(|n| n.to_str()) == Some("go.mod")
    })
    .is_some();
    let target = if in_module {
        ".".to_string()
    } else {
        absolute
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let output = match run_tool(
        go,
        Command::new(go).current_dir(&dir).arg("vet").arg(target),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };
    CheckMessage::Finished(parse_colon_diagnostics(
        &combined_output(&output),
        &dir,
        file_path,
    ))
}

// ==================== PHP (php -l) ====================

fn run_check_php(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let php = options.tools.php.as_str();
    let output = match run_tool(
        php,
        Command::new(php).arg("-l").arg(absolute_path(file_path)),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };
    CheckMessage::Finished(parse_php_diagnostics(&combined_output(&output), file_path))
}

/// `php -l` stops at the first error and reports it twice (once through the
/// log, once on stdout): `Parse error: message in /path/file.php on line 3`.
fn parse_php_diagnostics(output: &str, file_path: &Path) -> Vec<Diagnostic> {
    let re = Regex::new(
        r"(?m)^(?:PHP )?(?:Parse|Fatal) error:\s+(?P<msg>.*?) in (?P<file>.+?) on line (?P<line>\d+)\s*$",
    )
    .expect("static regex is valid");
    for caps in re.captures_iter(output) {
        if !matches_target_file(Path::new(&caps["file"]), file_path) {
            continue;
        }
        return vec![Diagnostic {
            line: caps["line"].parse::<usize>().unwrap_or(1).saturating_sub(1),
            col: 0,
            severity: Severity::Error,
            code: String::new(),
            message: caps["msg"].to_string(),
        }];
    }
    Vec::new()
}

// ==================== TypeScript (tsc) ====================

fn run_check_typescript(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let tsc = options.tools.tsc.as_str();
    let output = match run_tool(
        tsc,
        Command::new(tsc)
            .args(["--noEmit", "--pretty", "false"])
            .arg(absolute_path(file_path)),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };
    CheckMessage::Finished(parse_paren_diagnostics(
        &combined_output(&output),
        file_path,
        false,
    ))
}

// ==================== Kotlin (kotlinc), Swift (swiftc) ====================

fn run_check_kotlin(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let kotlinc = options.tools.kotlinc.as_str();
    let absolute = absolute_path(file_path);
    let dir = absolute.parent().unwrap_or(Path::new(".")).to_path_buf();
    let out_dir = stable_temp_dir(&absolute);
    let output = match run_tool(
        kotlinc,
        in_english(Command::new(kotlinc).current_dir(&dir))
            .arg("-d")
            .arg(&out_dir)
            .arg(&absolute),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };
    CheckMessage::Finished(parse_colon_diagnostics(
        &combined_output(&output),
        &dir,
        file_path,
    ))
}

fn run_check_swift(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let swiftc = options.tools.swiftc.as_str();
    let absolute = absolute_path(file_path);
    let dir = absolute.parent().unwrap_or(Path::new(".")).to_path_buf();
    let output = match run_tool(
        swiftc,
        in_english(Command::new(swiftc).current_dir(&dir))
            .arg("-typecheck")
            .arg(&absolute),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };
    CheckMessage::Finished(parse_colon_diagnostics(
        &combined_output(&output),
        &dir,
        file_path,
    ))
}

// ==================== Rust (cargo check / rustc) ====================

fn run_check_rust(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let rustc = options.tools.rustc.as_str();
    let cargo = options.tools.cargo.as_str();
    if let Err(msg) = run_tool(rustc, Command::new(rustc).arg("--version"), options) {
        return msg;
    }

    let output = match find_cargo_manifest(file_path) {
        Some(manifest) => run_tool(
            cargo,
            Command::new(cargo)
                .args(["check", "--message-format=human", "--manifest-path"])
                .arg(&manifest),
            options,
        ),
        None => {
            let absolute =
                std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.to_path_buf());
            let out_dir = stable_temp_dir(&absolute);
            run_tool(
                rustc,
                Command::new(rustc)
                    .args(["--edition", &options.rust_edition])
                    .args(["--crate-type", "lib", "--emit=metadata"])
                    .arg("-o")
                    .arg(out_dir.join("check.rmeta"))
                    .arg(&absolute),
                options,
            )
        }
    };

    let output = match output {
        Ok(o) => o,
        Err(msg) => return msg,
    };

    CheckMessage::Finished(parse_rustc_diagnostics(
        &combined_output(&output),
        file_path,
    ))
}

/// rustc/cargo print each diagnostic as a header line (`error[E0425]: msg`
/// or `warning: msg`) followed shortly by a `--> file:line:col` location
/// line — unlike C#'s MSBuild format, these are two separate lines.
fn parse_rustc_diagnostics(output: &str, file_path: &Path) -> Vec<Diagnostic> {
    let header_re = Regex::new(r"^(error|warning)(?:\[(\w+)\])?: (.*)$").expect("valid regex");
    let loc_re = Regex::new(r"^\s*-->\s*(.+):(\d+):(\d+)$").expect("valid regex");

    let lines: Vec<&str> = output.lines().collect();
    let mut diagnostics = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(header) = header_re.captures(line) else {
            continue;
        };
        let Some(loc) = lines[i + 1..(i + 4).min(lines.len())]
            .iter()
            .find_map(|l| loc_re.captures(l))
        else {
            continue;
        };
        if !matches_target_file(Path::new(&loc[1]), file_path) {
            continue;
        }

        let severity = if &header[1] == "error" {
            Severity::Error
        } else {
            Severity::Warning
        };
        diagnostics.push(Diagnostic {
            line: loc[2].parse::<usize>().unwrap_or(1).saturating_sub(1),
            col: loc[3].parse::<usize>().unwrap_or(1).saturating_sub(1),
            severity,
            code: header
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
            message: header[3].to_string(),
        });
    }
    diagnostics
}

// ==================== JavaScript (node --check) ====================

fn run_check_javascript(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let node = options.tools.node.as_str();
    let output = match run_tool(
        node,
        Command::new(node).arg("--check").arg(file_path),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };
    CheckMessage::Finished(parse_node_diagnostics(&combined_output(&output), file_path))
}

/// `node --check` prints the offending `path:line`, then the source line,
/// then a caret line, then (after a blank line) `SyntaxError: message`.
/// It stops at the first error, so there's at most one diagnostic.
fn parse_node_diagnostics(output: &str, file_path: &Path) -> Vec<Diagnostic> {
    let path_line_re = Regex::new(r"^(.+):(\d+)$").expect("valid regex");
    let lines: Vec<&str> = output.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let Some(caps) = path_line_re.captures(line) else {
            continue;
        };
        if !matches_target_file(Path::new(&caps[1]), file_path) {
            continue;
        }
        let line_no: usize = caps[2].parse().unwrap_or(1);

        let mut col = 0usize;
        for l in &lines[i + 1..(i + 6).min(lines.len())] {
            if let Some(pos) = l.find('^') {
                col = pos;
            }
            if let Some(msg) = l.trim_start().strip_prefix_error_message() {
                return vec![Diagnostic {
                    line: line_no.saturating_sub(1),
                    col,
                    severity: Severity::Error,
                    code: String::new(),
                    message: msg,
                }];
            }
        }
    }
    Vec::new()
}

// ==================== Python (py_compile) ====================

fn run_check_python(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let configured = options.tools.python.as_str();
    let python = if configured.is_empty() {
        match first_available(&["python3", "python"], options) {
            Some(found) => found,
            None => return CheckMessage::ToolMissing("python3".to_string()),
        }
    } else {
        configured.to_string()
    };

    let output = match run_tool(
        &python,
        Command::new(&python)
            .args(["-m", "py_compile"])
            .arg(file_path),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };
    CheckMessage::Finished(parse_python_diagnostics(
        &combined_output(&output),
        file_path,
    ))
}

/// `py_compile` prints `  File "path", line N`, then the source line,
/// then a caret line, then `SyntaxError: message` (or IndentationError,
/// TabError, ...). Like node, it stops at the first error.
fn parse_python_diagnostics(output: &str, file_path: &Path) -> Vec<Diagnostic> {
    let file_line_re = Regex::new(r#"^\s*File "(.+)", line (\d+)$"#).expect("valid regex");
    let lines: Vec<&str> = output.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let Some(caps) = file_line_re.captures(line) else {
            continue;
        };
        if !matches_target_file(Path::new(&caps[1]), file_path) {
            continue;
        }
        let line_no: usize = caps[2].parse().unwrap_or(1);

        let mut col = 0usize;
        for l in &lines[i + 1..(i + 5).min(lines.len())] {
            if let Some(pos) = l.find('^') {
                col = pos;
            }
            if let Some(msg) = l.trim_start().strip_prefix_error_message() {
                return vec![Diagnostic {
                    line: line_no.saturating_sub(1),
                    col,
                    severity: Severity::Error,
                    code: String::new(),
                    message: msg,
                }];
            }
        }
    }
    Vec::new()
}

/// Both Node's and Python's checkers end their message on a line shaped
/// like `SomeKindOfError: the actual message`.
trait StripErrorMessage {
    fn strip_prefix_error_message(&self) -> Option<String>;
}

impl StripErrorMessage for str {
    fn strip_prefix_error_message(&self) -> Option<String> {
        let idx = self.find("Error: ")?;
        // Require the part before "Error: " to look like an identifier
        // (e.g. "Syntax", "Indentation"), not an unrelated line that
        // happens to contain that substring.
        let prefix = &self[..idx];
        if prefix.chars().all(|c| c.is_ascii_alphabetic()) {
            Some(self[idx + "Error: ".len()..].to_string())
        } else {
            None
        }
    }
}

// ==================== HTML / XML (built-in tag check) ====================

fn run_check_markup(file_path: &Path, html: bool) -> CheckMessage {
    let source = match std::fs::read_to_string(file_path) {
        Ok(s) => s,
        Err(e) => return CheckMessage::Failed(e.to_string()),
    };
    CheckMessage::Finished(
        crate::markup::check(&source, html)
            .into_iter()
            .map(|e| Diagnostic {
                line: e.line,
                col: e.col,
                severity: Severity::Error,
                code: String::new(),
                message: e.message,
            })
            .collect(),
    )
}

// ==================== JSON / YAML (parsed in-process) ====================

/// serde_json and yaml-rust both append ` at line N column M` to their
/// messages; the location is shown through the diagnostic itself.
fn strip_location(message: &str) -> String {
    match message.rfind(" at line ") {
        Some(i) => message[..i].to_string(),
        None => message.to_string(),
    }
}

fn run_check_json(file_path: &Path) -> CheckMessage {
    let source = match std::fs::read_to_string(file_path) {
        Ok(s) => s,
        Err(e) => return CheckMessage::Failed(e.to_string()),
    };
    match serde_json::from_str::<serde_json::Value>(&source) {
        Ok(_) => CheckMessage::Finished(Vec::new()),
        Err(e) => CheckMessage::Finished(vec![Diagnostic {
            line: e.line().saturating_sub(1),
            col: e.column().saturating_sub(1),
            severity: Severity::Error,
            code: String::new(),
            message: strip_location(&e.to_string()),
        }]),
    }
}

fn run_check_yaml(file_path: &Path) -> CheckMessage {
    let source = match std::fs::read_to_string(file_path) {
        Ok(s) => s,
        Err(e) => return CheckMessage::Failed(e.to_string()),
    };
    match yaml_rust::YamlLoader::load_from_str(&source) {
        Ok(_) => CheckMessage::Finished(Vec::new()),
        Err(e) => {
            let mark = e.marker();
            CheckMessage::Finished(vec![Diagnostic {
                line: mark.line().saturating_sub(1),
                col: mark.col(),
                severity: Severity::Error,
                code: String::new(),
                message: strip_location(&e.to_string()),
            }])
        }
    }
}

// ==================== GDScript (Godot CLI) ====================
//
// Best-effort: Godot's `--check-only` output format is implemented from
// its documented CLI flags, not verified against a real Godot binary (not
// available in this environment). If it doesn't parse into a clean
// per-line diagnostic, we still surface the raw message rather than
// silently failing.

fn run_check_gdscript(file_path: &Path, options: &CheckOptions) -> CheckMessage {
    let configured = options.tools.godot.as_str();
    let godot_bin = if configured.is_empty() {
        match first_available(&["godot4", "godot"], options) {
            Some(found) => found,
            None => return CheckMessage::ToolMissing("godot".to_string()),
        }
    } else {
        configured.to_string()
    };

    let output = match run_tool(
        &godot_bin,
        Command::new(&godot_bin)
            .args(["--headless", "--check-only", "--script"])
            .arg(file_path),
        options,
    ) {
        Ok(o) => o,
        Err(msg) => return msg,
    };

    if output.status.success() {
        return CheckMessage::Finished(Vec::new());
    }

    let combined = combined_output(&output);
    let diagnostics = parse_gdscript_diagnostics(&combined, file_path);
    if diagnostics.is_empty() {
        // Couldn't parse a structured location out of Godot's output;
        // surface whatever it said instead of pretending nothing's wrong.
        let summary: String = combined
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ");
        CheckMessage::Failed(if summary.is_empty() {
            "Godot check failed with no output".to_string()
        } else {
            summary
        })
    } else {
        CheckMessage::Finished(diagnostics)
    }
}

/// Godot's parse-error banner looks like:
/// `SCRIPT ERROR: Parse Error: <message>` followed by
/// `          at: GDScript::reload (res://path.gd:LINE)`.
fn parse_gdscript_diagnostics(output: &str, file_path: &Path) -> Vec<Diagnostic> {
    let err_re =
        Regex::new(r"(?i)^\s*SCRIPT ERROR:\s*(?:Parse Error:\s*)?(.*)$").expect("valid regex");
    let at_re = Regex::new(r"\(([^():]+):(\d+)\)\s*$").expect("valid regex");

    let lines: Vec<&str> = output.lines().collect();
    let mut diagnostics = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(err) = err_re.captures(line) else {
            continue;
        };
        let Some(at) = lines[i + 1..(i + 3).min(lines.len())]
            .iter()
            .find_map(|l| at_re.captures(l))
        else {
            continue;
        };
        if !matches_target_file(Path::new(&at[1]), file_path) {
            continue;
        }
        diagnostics.push(Diagnostic {
            line: at[2].parse::<usize>().unwrap_or(1).saturating_sub(1),
            col: 0,
            severity: Severity::Error,
            code: String::new(),
            message: err[1].trim().to_string(),
        });
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // Real files, so `std::fs::canonicalize` in the matcher succeeds —
    // the target-file filter depends on the path actually existing.
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    fn temp_file(name_hint: &str, ext: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pc_check_test_{name_hint}_{n}.{ext}"));
        std::fs::write(&path, "// test\n").unwrap();
        path
    }

    #[test]
    fn parses_csharp_error_and_warning_for_target_file() {
        let file = temp_file("target", "cs");
        let output = format!(
            "{}(4,32): error CS1002: ; expected [/tmp/proj.csproj]\n{}(2,5): warning CS0168: variable declared but never used [/tmp/proj.csproj]",
            file.display(),
            file.display()
        );

        let diags = parse_csharp_diagnostics(&output, &file);
        assert_eq!(diags.len(), 2);

        assert_eq!(diags[0].line, 3); // 1-indexed 4 -> 0-indexed 3
        assert_eq!(diags[0].col, 31);
        assert!(matches!(diags[0].severity, Severity::Error));
        assert_eq!(diags[0].code, "CS1002");
        assert_eq!(diags[0].message, "; expected");

        assert_eq!(diags[1].line, 1);
        assert!(matches!(diags[1].severity, Severity::Warning));
        assert_eq!(diags[1].code, "CS0168");

        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn csharp_ignores_diagnostics_for_other_files() {
        let file = temp_file("target2", "cs");
        let other = temp_file("other", "cs");
        let output = format!(
            "{}(1,1): error CS0000: unrelated file's problem",
            other.display()
        );

        let diags = parse_csharp_diagnostics(&output, &file);
        assert!(diags.is_empty());

        std::fs::remove_file(&file).ok();
        std::fs::remove_file(&other).ok();
    }

    #[test]
    fn csharp_ignores_unrelated_output_lines() {
        let file = temp_file("target3", "cs");
        let output =
            "Determining projects to restore...\n  Restored /tmp/proj.csproj\nBuild succeeded.";

        let diags = parse_csharp_diagnostics(output, &file);
        assert!(diags.is_empty());

        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn severity_ord_ranks_error_above_warning() {
        assert!(Severity::Error > Severity::Warning);
    }

    #[test]
    fn parses_rustc_error_with_code() {
        let file = temp_file("target", "rs");
        let output = format!(
            "error[E0425]: cannot find value `y` in this scope\n --> {}:3:20\n  |\n",
            file.display()
        );
        let diags = parse_rustc_diagnostics(&output, &file);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 2);
        assert_eq!(diags[0].col, 19);
        assert!(matches!(diags[0].severity, Severity::Error));
        assert_eq!(diags[0].code, "E0425");
        assert_eq!(diags[0].message, "cannot find value `y` in this scope");
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn parses_rustc_warning_without_code() {
        let file = temp_file("target", "rs");
        let output = format!(
            "warning: unused variable: `x`\n --> {}:2:9\n  |\n",
            file.display()
        );
        let diags = parse_rustc_diagnostics(&output, &file);
        assert_eq!(diags.len(), 1);
        assert!(matches!(diags[0].severity, Severity::Warning));
        assert_eq!(diags[0].code, "");
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn parses_node_syntax_error() {
        let file = temp_file("target", "js");
        let output = format!(
            "{}:3\n    let x = ;\n            ^\n\nSyntaxError: Unexpected token ';'\n    at wrapSafe (node:internal/modules/cjs/loader:1804:18)\n",
            file.display()
        );
        let diags = parse_node_diagnostics(&output, &file);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 2);
        assert_eq!(diags[0].col, 12);
        assert_eq!(diags[0].message, "Unexpected token ';'");
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn parses_python_syntax_error() {
        let file = temp_file("target", "py");
        let output = format!(
            "  File \"{}\", line 3\n    if x == 1\n             ^\nSyntaxError: expected ':'\n",
            file.display()
        );
        let diags = parse_python_diagnostics(&output, &file);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 2);
        assert_eq!(diags[0].col, 13);
        assert_eq!(diags[0].message, "expected ':'");
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn parses_gdscript_parse_error() {
        let file = temp_file("target", "gd");
        let output = format!(
            "SCRIPT ERROR: Parse Error: Expected end of statement, found \":\" instead.\n          at: GDScript::reload ({}:3)\n",
            file.display()
        );
        let diags = parse_gdscript_diagnostics(&output, &file);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 2);
        assert_eq!(
            diags[0].message,
            "Expected end of statement, found \":\" instead."
        );
        std::fs::remove_file(&file).ok();
    }

    fn temp_file_with(name_hint: &str, ext: &str, content: &str) -> PathBuf {
        let path = temp_file(name_hint, ext);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn diagnostics_of(msg: CheckMessage) -> Vec<Diagnostic> {
        match msg {
            CheckMessage::Finished(d) => d,
            CheckMessage::Failed(e) => panic!("check failed: {e}"),
            CheckMessage::ToolMissing(t) => panic!("{t} missing"),
        }
    }

    #[test]
    fn json_syntax_errors_are_located() {
        let file = temp_file_with("ok", "json", "{\"a\": [1, 2, {\"b\": null}]}");
        assert!(diagnostics_of(run_check_json(&file)).is_empty());

        let bad = temp_file_with("bad", "json", "{\n  \"a\": 1,\n  \"b\": ,\n}");
        let diags = diagnostics_of(run_check_json(&bad));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 2, "error is on the third line");
        assert!(matches!(diags[0].severity, Severity::Error));
        assert!(
            !diags[0].message.contains(" at line "),
            "location is shown by the diagnostic, not repeated in the text: {}",
            diags[0].message
        );
        std::fs::remove_file(&file).ok();
        std::fs::remove_file(&bad).ok();
    }

    #[test]
    fn yaml_syntax_errors_are_located() {
        let file = temp_file_with("ok", "yaml", "a: 1\nb:\n  - x\n  - y\n");
        assert!(diagnostics_of(run_check_yaml(&file)).is_empty());

        let bad = temp_file_with("bad", "yaml", "a: 1\nb: [unclosed\nc: 2\n");
        let diags = diagnostics_of(run_check_yaml(&bad));
        assert_eq!(diags.len(), 1);
        assert!(matches!(diags[0].severity, Severity::Error));
        assert!(!diags[0].message.contains(" at line "));
        std::fs::remove_file(&file).ok();
        std::fs::remove_file(&bad).ok();
    }

    #[test]
    fn html_and_xml_tag_checks_report_diagnostics() {
        let bad = temp_file_with("bad", "html", "<div>\n  <span>hi\n</div>\n");
        let diags = diagnostics_of(run_check_markup(&bad, true));
        assert_eq!(diags.len(), 1);
        assert_eq!((diags[0].line, diags[0].col), (1, 2));
        assert_eq!(diags[0].message, "unclosed <span>");

        let ok = temp_file_with("ok", "xml", "<a><b/></a>");
        assert!(diagnostics_of(run_check_markup(&ok, false)).is_empty());
        std::fs::remove_file(&bad).ok();
        std::fs::remove_file(&ok).ok();
    }

    #[test]
    fn the_cargo_manifest_is_found_even_beside_other_toml_files() {
        let dir = std::env::temp_dir().join(format!("pc_manifest_{}", std::process::id()));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("aaa.toml"), "").unwrap();
        std::fs::write(dir.join(".pis-code.toml"), "").unwrap();
        std::fs::write(src.join("rustfmt.toml"), "").unwrap();
        let found = find_cargo_manifest(&src.join("main.rs")).expect("manifest");
        assert_eq!(found, dir.join("Cargo.toml"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn options_come_from_the_config() {
        let mut c = Config::default();
        c.check.timeout_seconds = 7;
        c.check.rust_edition = " 2018 ".into();
        c.tools.node = "/opt/node".into();
        let o = CheckOptions::from_config(&c);
        assert_eq!(o.timeout, Duration::from_secs(7));
        assert_eq!(o.rust_edition, "2018");
        assert_eq!(o.tools.node, "/opt/node");
        c.check.timeout_seconds = 0;
        assert_eq!(
            CheckOptions::from_config(&c).timeout,
            Duration::from_secs(1),
            "never zero"
        );
    }

    /// Tests that run stand-in "tools" (shell scripts) to prove the
    /// configured executables, timeout, framework and edition are really
    /// what the checkers use.
    #[cfg(unix)]
    mod with_fake_tools {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn fresh_dir(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("pc_fake_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn script(dir: &Path, name: &str, body: &str) -> String {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.to_str().unwrap().to_string()
        }

        fn quick() -> CheckOptions {
            CheckOptions {
                timeout: Duration::from_secs(20),
                ..CheckOptions::default()
            }
        }

        #[test]
        fn run_collects_both_streams_even_when_the_output_is_huge() {
            let out = run(
                Command::new("sh")
                    .args(["-c", "echo out; echo err >&2; head -c 3000000 /dev/zero"]),
                Duration::from_secs(20),
            )
            .ok()
            .expect("runs");
            assert!(out.status.success());
            assert!(out.stdout.len() > 3_000_000, "{}", out.stdout.len());
            assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
        }

        #[test]
        fn run_kills_a_command_that_outlives_the_timeout() {
            let started = Instant::now();
            let result = run(
                Command::new("sh").args(["-c", "sleep 30"]),
                Duration::from_millis(300),
            );
            assert!(matches!(result, Err(RunError::TimedOut)));
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{:?}",
                started.elapsed()
            );
        }

        #[test]
        fn run_reports_a_missing_executable() {
            let result = run(
                &mut Command::new("/definitely/not/here"),
                Duration::from_secs(5),
            );
            assert!(matches!(result, Err(RunError::NotFound)));
        }

        #[test]
        fn a_grandchild_holding_the_pipe_open_does_not_hang_the_check() {
            // Like a build server that outlives `dotnet build`.
            let started = Instant::now();
            let out = run(
                Command::new("sh").args(["-c", "echo hi; sleep 5 &"]),
                Duration::from_secs(20),
            )
            .ok()
            .expect("runs");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
            assert!(
                started.elapsed() < Duration::from_secs(4),
                "{:?}",
                started.elapsed()
            );
        }

        #[test]
        fn the_configured_node_is_the_one_that_runs() {
            let dir = fresh_dir("node");
            let file = dir.join("a.js");
            std::fs::write(&file, "let x = ;").unwrap();
            let mut options = quick();
            options.tools.node = script(
                &dir,
                "fake-node",
                r#"printf '%s:3\n    let x = ;\n            ^\n\nSyntaxError: from the fake node\n' "$2" >&2; exit 1"#,
            );
            let diags = diagnostics_of(run_check_javascript(&file, &options));
            assert_eq!(diags.len(), 1);
            assert_eq!(
                (diags[0].line, diags[0].message.as_str()),
                (2, "from the fake node")
            );
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn the_configured_python_is_the_one_that_runs() {
            let dir = fresh_dir("python");
            let file = dir.join("a.py");
            std::fs::write(&file, "x =").unwrap();
            let mut options = quick();
            options.tools.python = script(
                &dir,
                "fake-python",
                r#"printf '  File "%s", line 1\n    x =\n      ^\nSyntaxError: from the fake python\n' "$3" >&2; exit 1"#,
            );
            let diags = diagnostics_of(run_check_python(&file, &options));
            assert_eq!(diags[0].message, "from the fake python");
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn the_configured_godot_is_the_one_that_runs() {
            let dir = fresh_dir("godot");
            let file = dir.join("a.gd");
            std::fs::write(&file, "x").unwrap();
            let mut options = quick();
            options.tools.godot = script(
                &dir,
                "fake-godot",
                r#"printf 'SCRIPT ERROR: Parse Error: from the fake godot\n          at: GDScript::reload (%s:4)\n' "$4"; exit 1"#,
            );
            let diags = diagnostics_of(run_check_gdscript(&file, &options));
            assert_eq!(
                (diags[0].line, diags[0].message.as_str()),
                (3, "from the fake godot")
            );
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn a_missing_configured_tool_is_reported_by_the_name_the_user_gave() {
            let dir = fresh_dir("missing");
            let file = dir.join("a.js");
            std::fs::write(&file, "").unwrap();
            let mut options = quick();
            options.tools.node = "/no/such/node".into();
            match run_check_javascript(&file, &options) {
                CheckMessage::ToolMissing(name) => assert_eq!(name, "/no/such/node"),
                _ => panic!("expected ToolMissing"),
            }
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn a_check_that_takes_too_long_is_stopped_after_the_configured_timeout() {
            let dir = fresh_dir("timeout");
            let file = dir.join("a.js");
            std::fs::write(&file, "").unwrap();
            let mut options = quick();
            options.timeout = Duration::from_secs(1);
            options.tools.node = script(&dir, "slow-node", "sleep 30");
            let started = Instant::now();
            match run_check_javascript(&file, &options) {
                CheckMessage::Failed(message) => {
                    assert!(message.contains("timed out after 1s"), "{message}");
                }
                _ => panic!("expected a timeout"),
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            let _ = std::fs::remove_dir_all(dir);
        }

        fn dotnet_that_logs_the_project(dir: &Path) -> (String, PathBuf) {
            let log = dir.join("csproj.log");
            let body = format!(
                r#"case "$1" in --version) echo 9.0.100;; build) cp "$5" '{}';; esac"#,
                log.display()
            );
            (script(dir, "fake-dotnet", &body), log)
        }

        #[test]
        fn dotnet_target_framework_auto_follows_the_installed_sdk() {
            let dir = fresh_dir("tfm_auto");
            let file = dir.join("a.cs");
            std::fs::write(&file, "class A {}").unwrap();
            let (dotnet, log) = dotnet_that_logs_the_project(&dir);
            let mut options = quick();
            options.tools.dotnet = dotnet;
            diagnostics_of(run_check_csharp(&file, &options));
            let project = std::fs::read_to_string(&log).unwrap();
            assert!(
                project.contains("<TargetFramework>net9.0</TargetFramework>"),
                "{project}"
            );
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn dotnet_target_framework_can_be_pinned() {
            let dir = fresh_dir("tfm_pinned");
            let file = dir.join("a.cs");
            std::fs::write(&file, "class A {}").unwrap();
            let (dotnet, log) = dotnet_that_logs_the_project(&dir);
            let mut options = quick();
            options.tools.dotnet = dotnet;
            options.dotnet_target_framework = "net6.0".into();
            diagnostics_of(run_check_csharp(&file, &options));
            let project = std::fs::read_to_string(&log).unwrap();
            assert!(
                project.contains("<TargetFramework>net6.0</TargetFramework>"),
                "{project}"
            );
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn rust_edition_and_tools_are_used_for_a_loose_file() {
            let dir = fresh_dir("edition");
            let file = dir.join("a.rs");
            std::fs::write(&file, "fn f() {}").unwrap();
            let log = dir.join("rustc.log");
            let mut options = quick();
            options.rust_edition = "2018".into();
            options.tools.rustc = script(
                &dir,
                "fake-rustc",
                &format!(
                    r#"if [ "$1" != "--version" ]; then echo "$@" > '{}'; fi"#,
                    log.display()
                ),
            );
            diagnostics_of(run_check_rust(&file, &options));
            let args = std::fs::read_to_string(&log).unwrap();
            assert!(args.contains("--edition 2018"), "{args}");
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn the_configured_cargo_checks_a_file_inside_a_cargo_project() {
            let dir = fresh_dir("cargo");
            std::fs::write(dir.join("Cargo.toml"), "").unwrap();
            std::fs::write(dir.join("rustfmt.toml"), "").unwrap();
            let file = dir.join("main.rs");
            std::fs::write(&file, "fn main() {}").unwrap();
            let log = dir.join("cargo.log");
            let mut options = quick();
            options.tools.rustc = script(&dir, "fake-rustc", "true");
            options.tools.cargo = script(
                &dir,
                "fake-cargo",
                &format!(r#"echo "$@" > '{}'"#, log.display()),
            );
            diagnostics_of(run_check_rust(&file, &options));
            let args = std::fs::read_to_string(&log).unwrap();
            assert!(
                args.starts_with("check ") && args.contains("Cargo.toml"),
                "{args}"
            );
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    // ---- C-family compilers and friends ----

    fn files_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pc_cfam_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_gcc_and_clang_output() {
        let dir = files_dir("gccparse");
        let file = dir.join("a.c");
        let other = dir.join("other.h");
        std::fs::write(&file, "").unwrap();
        std::fs::write(&other, "").unwrap();
        let (f, o) = (file.display(), other.display());
        let output = format!(
            "{f}: In function 'main':\n\
             {f}:3:5: warning: unused variable 'x' [-Wunused-variable]\n\
             \x20   3 |     int x;\n\
             \x20     |     ^\n\
             {f}:3:5: note: declared here\n\
             {f}:5:1: error: expected ';' before '}}' token\n\
             {f}:2:10: fatal error: nope.h: No such file or directory\n\
             {o}:1:1: error: belongs to another file\n\
             {f}:1:1: warning: #pragma once in main file\n\
             {f}:1:9: warning: '#pragma once' in main file [-Wpragma-once-outside-header]\n"
        );
        let diags = parse_colon_diagnostics(&output, &dir, &file);
        assert_eq!(
            diags.len(),
            3,
            "{:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        assert_eq!((diags[0].line, diags[0].col), (2, 4));
        assert!(matches!(diags[0].severity, Severity::Warning));
        assert_eq!(diags[0].code, "-Wunused-variable");
        assert_eq!(diags[0].message, "unused variable 'x'");

        assert_eq!((diags[1].line, diags[1].col), (4, 0));
        assert!(matches!(diags[1].severity, Severity::Error));
        assert_eq!(diags[1].message, "expected ';' before '}' token");

        assert!(
            matches!(diags[2].severity, Severity::Error),
            "fatal error counts as an error"
        );
        assert_eq!(diags[2].message, "nope.h: No such file or directory");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_go_output_which_has_no_severity() {
        let dir = files_dir("goparse");
        let file = dir.join("main.go");
        std::fs::write(&file, "").unwrap();
        let output = "# command-line-arguments\n\
                      ./main.go:5:2: declared and not used: y\n\
                      vet: ./main.go:7:3: expected operand, found '}'\n\
                      ./util.go:1:1: belongs to another file\n";
        let diags = parse_colon_diagnostics(output, &dir, &file);
        assert_eq!(diags.len(), 2);
        assert_eq!((diags[0].line, diags[0].col), (4, 1));
        assert!(matches!(diags[0].severity, Severity::Error));
        assert_eq!(diags[0].message, "declared and not used: y");
        assert_eq!(
            diags[1].message, "expected operand, found '}'",
            "the `vet:` prefix is dropped"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_relative_path_is_resolved_against_the_compilers_folder_not_ours() {
        // Two different files named a.c: only the one in the compiler's
        // folder is the target, whatever our own working directory holds.
        let dir = files_dir("relpath");
        let elsewhere = files_dir("relpath_other");
        let target = dir.join("a.c");
        std::fs::write(&target, "").unwrap();
        std::fs::write(elsewhere.join("a.c"), "").unwrap();
        let output = "a.c:1:1: error: in the target\n";
        assert_eq!(parse_colon_diagnostics(output, &dir, &target).len(), 1);
        assert!(parse_colon_diagnostics(output, &elsewhere, &target).is_empty());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(elsewhere);
    }

    #[test]
    fn parses_javac_output_and_finds_the_column_from_the_caret() {
        let dir = files_dir("javacparse");
        let file = dir.join("Main.java");
        std::fs::write(&file, "").unwrap();
        let output = "Main.java:3: error: ';' expected\n\
                      \x20       int x = 1\n\
                      \x20                ^\n\
                      Main.java:7: warning: [deprecation] stop() in Thread has been deprecated\n\
                      \x20       t.stop();\n\
                      \x20        ^\n\
                      Other.java:1: error: elsewhere\n\
                      \x20^\n\
                      2 errors\n";
        let diags = parse_javac_diagnostics(output, &dir, &file);
        assert_eq!(diags.len(), 2);
        assert_eq!((diags[0].line, diags[0].col), (2, 17));
        assert_eq!(diags[0].message, "';' expected");
        assert!(matches!(diags[0].severity, Severity::Error));
        assert_eq!((diags[1].line, diags[1].col), (6, 9));
        assert_eq!(diags[1].code, "deprecation");
        assert_eq!(diags[1].message, "stop() in Thread has been deprecated");
        assert!(matches!(diags[1].severity, Severity::Warning));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_php_lint_output_reported_twice() {
        let dir = files_dir("phpparse");
        let file = dir.join("a.php");
        std::fs::write(&file, "").unwrap();
        let output = format!(
            "PHP Parse error:  syntax error, unexpected token \";\" in {0} on line 3\n\
             Parse error: syntax error, unexpected token \";\" in {0} on line 3\n\
             Errors parsing {0}\n",
            file.display()
        );
        let diags = parse_php_diagnostics(&output, &file);
        assert_eq!(diags.len(), 1, "reported once");
        assert_eq!(diags[0].line, 2);
        assert_eq!(diags[0].message, "syntax error, unexpected token \";\"");
        assert!(parse_php_diagnostics(
            &format!("No syntax errors detected in {}\n", file.display()),
            &file
        )
        .is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_tsc_output_without_trimming_brackets_from_messages() {
        let dir = files_dir("tscparse");
        let file = dir.join("a.ts");
        std::fs::write(&file, "").unwrap();
        let output = format!(
            "{0}(3,5): error TS1005: ';' expected.\n\
             {0}(4,1): error TS2322: Type 'string' is not assignable to type 'number [] '.\n",
            file.display()
        );
        let diags = parse_paren_diagnostics(&output, &file, false);
        assert_eq!(diags.len(), 2);
        assert_eq!(
            (diags[0].line, diags[0].col, diags[0].code.as_str()),
            (2, 4, "TS1005")
        );
        assert!(diags[1].message.ends_with("'number [] '."));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn installed(tool: &str, flag: &str) -> bool {
        Command::new(tool).arg(flag).output().is_ok()
    }

    /// Real compilers, when this machine has them.
    #[cfg(unix)]
    mod real_compilers {
        use super::*;

        fn write(dir: &Path, name: &str, source: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, source).unwrap();
            path
        }

        fn options() -> CheckOptions {
            CheckOptions {
                timeout: Duration::from_secs(60),
                ..CheckOptions::default()
            }
        }

        #[test]
        fn gcc_finds_errors_and_warnings_in_a_c_file() {
            if !installed("gcc", "--version") {
                eprintln!("skipped: no gcc");
                return;
            }
            let dir = files_dir("realc");
            let ok = write(
                &dir,
                "ok.c",
                "#include <stdio.h>\nint main(void) { printf(\"hi\\n\"); return 0; }\n",
            );
            assert!(diagnostics_of(run_check_c_family(&ok, &options(), false)).is_empty());

            let bad = write(
                &dir,
                "bad.c",
                "int main(void) {\n    int x = 1\n    return 0;\n}\n",
            );
            let diags = diagnostics_of(run_check_c_family(&bad, &options(), false));
            let error = diags
                .iter()
                .find(|d| matches!(d.severity, Severity::Error))
                .expect("an error");
            assert!(
                error.line <= 2,
                "points at the missing semicolon: line {}",
                error.line + 1
            );
            assert!(error.message.contains("expected"), "{}", error.message);

            let warn = write(
                &dir,
                "warn.c",
                "int main(void) {\n    int unused;\n    return 0;\n}\n",
            );
            let diags = diagnostics_of(run_check_c_family(&warn, &options(), false));
            assert_eq!(
                diags.len(),
                1,
                "{:?}",
                diags.iter().map(|d| &d.message).collect::<Vec<_>>()
            );
            assert!(matches!(diags[0].severity, Severity::Warning));
            assert_eq!(
                (diags[0].line, diags[0].code.as_str()),
                (1, "-Wunused-variable")
            );
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn messages_stay_english_whatever_the_users_locale() {
            if !installed("gcc", "--version") {
                eprintln!("skipped: no gcc");
                return;
            }
            let dir = files_dir("locale");
            let bad = write(&dir, "bad.c", "int main(void) { return x; }\n");
            // Even if the environment asks for another language.
            std::env::set_var("LANGUAGE", "ru");
            let diags = diagnostics_of(run_check_c_family(&bad, &options(), false));
            assert_eq!(diags.len(), 1);
            assert!(
                diags[0].message.contains("undeclared"),
                "{}",
                diags[0].message
            );
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn extra_flags_reach_the_compiler() {
            if !installed("gcc", "--version") {
                eprintln!("skipped: no gcc");
                return;
            }
            let dir = files_dir("flags");
            std::fs::create_dir_all(dir.join("inc")).unwrap();
            write(&dir.join("inc"), "thing.h", "#define THING 1\n");
            let src = write(
                &dir,
                "a.c",
                "#include <thing.h>\nint f(void) { return THING; }\n",
            );
            // Without the include path the header is missing...
            let diags = diagnostics_of(run_check_c_family(&src, &options(), false));
            assert!(
                diags.iter().any(|d| d.message.contains("thing.h")),
                "{:?}",
                diags.iter().map(|d| &d.message).collect::<Vec<_>>()
            );
            // ...a relative -I is relative to the file's folder.
            let mut with_flag = options();
            with_flag.c_flags = vec!["-Iinc".to_string()];
            assert!(diagnostics_of(run_check_c_family(&src, &with_flag, false)).is_empty());
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn gpp_checks_cpp_and_pins_the_language_for_headers() {
            if !installed("g++", "--version") {
                eprintln!("skipped: no g++");
                return;
            }
            let dir = files_dir("realcpp");
            let bad = write(
                &dir,
                "bad.cpp",
                "#include <vector>\nint main() { std::vector<int> v; v.push_back(\"s\"); }\n",
            );
            let diags = diagnostics_of(run_check_c_family(&bad, &options(), true));
            assert!(
                diags
                    .iter()
                    .any(|d| matches!(d.severity, Severity::Error) && d.line == 1),
                "{:?}",
                diags
                    .iter()
                    .map(|d| (d.line, &d.message))
                    .collect::<Vec<_>>()
            );

            // A .h that is really C++, and a header's #pragma once.
            let header = write(
                &dir,
                "shape.h",
                "#pragma once\nclass Shape { public: virtual ~Shape() {} };\n",
            );
            assert!(diagnostics_of(run_check_c_family(&header, &options(), true)).is_empty());
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn go_vet_finds_syntax_and_type_errors() {
            if !installed("go", "version") {
                eprintln!("skipped: no go");
                return;
            }
            let dir = files_dir("realgo");
            let ok = write(&dir, "ok.go", "package main\n\nfunc main() {}\n");
            assert!(diagnostics_of(run_check_go(&ok, &options())).is_empty());

            let typed = write(
                &dir,
                "typed.go",
                "package main\n\nfunc main() {\n\ty := 1\n}\n",
            );
            let diags = diagnostics_of(run_check_go(&typed, &options()));
            assert_eq!(
                diags.len(),
                1,
                "{:?}",
                diags.iter().map(|d| &d.message).collect::<Vec<_>>()
            );
            assert_eq!(diags[0].line, 3, "points at `y := 1`");
            assert!(diags[0].message.contains("y"), "{}", diags[0].message);

            let syntax = write(
                &dir,
                "syntax.go",
                "package main\n\nfunc main() {\n\tx := \n}\n",
            );
            let diags = diagnostics_of(run_check_go(&syntax, &options()));
            assert!(!diags.is_empty(), "a syntax error is reported");
            assert!(matches!(diags[0].severity, Severity::Error));
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn go_checks_the_whole_package_inside_a_module() {
            if !installed("go", "version") {
                eprintln!("skipped: no go");
                return;
            }
            let dir = files_dir("gomod");
            write(&dir, "go.mod", "module example.com/demo\n\ngo 1.21\n");
            write(
                &dir,
                "helper.go",
                "package main\n\nfunc helper() int { return 1 }\n",
            );
            // Uses helper() from the other file — only works if the package is checked as a whole.
            let main = write(
                &dir,
                "main.go",
                "package main\n\nfunc main() { _ = helper() }\n",
            );
            assert!(diagnostics_of(run_check_go(&main, &options())).is_empty());
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Tools this machine may not have, stood in for by scripts that print
    /// what the real ones print.
    #[cfg(unix)]
    mod stand_in_tools {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn script(dir: &Path, name: &str, body: &str) -> String {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.to_str().unwrap().to_string()
        }

        fn options() -> CheckOptions {
            CheckOptions {
                timeout: Duration::from_secs(20),
                ..CheckOptions::default()
            }
        }

        fn source(dir: &Path, name: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, "x").unwrap();
            path
        }

        /// A stand-in that records its arguments and prints a canned
        /// diagnostic naming its last argument.
        fn recorder(dir: &Path, name: &str, printed: &str) -> (String, PathBuf) {
            let log = dir.join(format!("{name}.args"));
            let body = format!(
                "echo \"$@\" > '{}'\nfor last; do :; done\nprintf '{}\\n' \"$last\"",
                log.display(),
                printed
            );
            (script(dir, name, &body), log)
        }

        #[test]
        fn c_and_cpp_pass_the_language_the_flags_and_the_file() {
            let dir = files_dir("fakecc");
            let file = source(&dir, "a.h");
            let (cc, log) = recorder(&dir, "fake-cc", "%s:3:5: error: boom");
            let mut o = options();
            o.tools.cc = cc;
            o.c_flags = vec!["-Iinc".into(), "-std=c11".into()];
            let diags = diagnostics_of(run_check_c_family(&file, &o, false));
            assert_eq!((diags.len(), diags[0].line, diags[0].col), (1, 2, 4));
            let args = std::fs::read_to_string(&log).unwrap();
            assert!(
                args.contains("-fsyntax-only") && args.contains("-Wall"),
                "{args}"
            );
            assert!(
                args.contains("-Iinc -std=c11 -x c "),
                "flags, then the language, then the file: {args}"
            );

            let (cxx, log) = recorder(&dir, "fake-cxx", "%s:4:1: warning: hmm [-Wfoo]");
            let mut o = options();
            o.tools.cxx = cxx;
            o.cpp_flags = vec!["-std=c++20".into()];
            let diags = diagnostics_of(run_check_c_family(&file, &o, true));
            assert!(matches!(diags[0].severity, Severity::Warning));
            assert_eq!(diags[0].code, "-Wfoo");
            let args = std::fs::read_to_string(&log).unwrap();
            assert!(args.contains("-std=c++20 -x c++ "), "{args}");
            assert!(
                !args.contains("-std=c11"),
                "C flags don't leak into C++: {args}"
            );
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn a_missing_c_compiler_is_reported_by_name() {
            let dir = files_dir("nocc");
            let file = source(&dir, "a.c");
            let mut o = options();
            o.tools.cc = "/no/such/cc".into();
            match run_check_c_family(&file, &o, false) {
                CheckMessage::ToolMissing(name) => assert_eq!(name, "/no/such/cc"),
                _ => panic!("expected ToolMissing"),
            }
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn javac_is_run_with_an_output_folder_and_the_caret_column_is_used() {
            let dir = files_dir("fakejavac");
            let file = source(&dir, "Main.java");
            let log = dir.join("javac.args");
            let mut o = options();
            o.tools.javac = script(
                &dir,
                "fake-javac",
                &format!(
                    "echo \"$@\" > '{}'\nfor last; do :; done\nprintf 'Main.java:3: error: boom\\n  int x\\n     ^\\n1 error\\n' >&2",
                    log.display()
                ),
            );
            let diags = diagnostics_of(run_check_java(&file, &o));
            assert_eq!(
                (diags[0].line, diags[0].col, diags[0].message.as_str()),
                (2, 5, "boom")
            );
            let args = std::fs::read_to_string(&log).unwrap();
            assert!(
                args.starts_with("-d ") && args.trim_end().ends_with("Main.java"),
                "{args}"
            );
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn go_php_tsc_kotlinc_and_swiftc_use_their_configured_executables() {
            let dir = files_dir("fakemisc");

            let go_file = source(&dir, "main.go");
            let (go, log) = recorder(&dir, "fake-go", "./main.go:3:7: undefined: nope");
            let mut o = options();
            o.tools.go = go;
            let diags = diagnostics_of(run_check_go(&go_file, &o));
            assert_eq!((diags[0].line, diags[0].col), (2, 6));
            assert!(
                std::fs::read_to_string(&log)
                    .unwrap()
                    .starts_with("vet main.go"),
                "a loose file is vetted alone"
            );

            let php_file = source(&dir, "a.php");
            let mut o = options();
            o.tools.php = script(&dir, "fake-php", "printf 'PHP Parse error:  syntax error, unexpected end of file in %s on line 4\\n' \"$2\" >&2");
            let diags = diagnostics_of(run_check_php(&php_file, &o));
            assert_eq!(
                (diags[0].line, diags[0].message.as_str()),
                (3, "syntax error, unexpected end of file")
            );

            let ts_file = source(&dir, "a.ts");
            let (tsc, log) = recorder(
                &dir,
                "fake-tsc",
                "%s(2,9): error TS2304: Cannot find name 'nope'.",
            );
            let mut o = options();
            o.tools.tsc = tsc;
            let diags = diagnostics_of(run_check_typescript(&ts_file, &o));
            assert_eq!(
                (diags[0].line, diags[0].col, diags[0].code.as_str()),
                (1, 8, "TS2304")
            );
            assert!(std::fs::read_to_string(&log)
                .unwrap()
                .starts_with("--noEmit --pretty false "));

            let kt_file = source(&dir, "a.kt");
            let (kotlinc, _) = recorder(
                &dir,
                "fake-kotlinc",
                "%s:5:3: error: unresolved reference: nope",
            );
            let mut o = options();
            o.tools.kotlinc = kotlinc;
            let diags = diagnostics_of(run_check_kotlin(&kt_file, &o));
            assert_eq!((diags[0].line, diags[0].col), (4, 2));

            let swift_file = source(&dir, "a.swift");
            let (swiftc, log) = recorder(
                &dir,
                "fake-swiftc",
                "%s:1:1: error: cannot find 'nope' in scope",
            );
            let mut o = options();
            o.tools.swiftc = swiftc;
            let diags = diagnostics_of(run_check_swift(&swift_file, &o));
            assert_eq!(diags.len(), 1);
            assert!(std::fs::read_to_string(&log)
                .unwrap()
                .starts_with("-typecheck "));
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
