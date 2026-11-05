use anyhow::Result;
use std::fs;
use std::path::Path;

pub struct LoadedFile {
    pub lines: Vec<String>,
    /// The file used CRLF line endings. A file that mixes them counts as
    /// CRLF if its first line break is one.
    pub crlf: bool,
}

/// Load a file's contents as lines, or return a single empty line if the
/// file does not exist yet (a brand new buffer, like `nano` would give you).
pub fn load_or_create(path: &Path) -> Result<LoadedFile> {
    if !path.exists() {
        return Ok(LoadedFile {
            lines: vec![String::new()],
            crlf: false,
        });
    }
    let raw = fs::read_to_string(path)?;
    let crlf = raw
        .find('\n')
        .is_some_and(|i| i > 0 && raw.as_bytes()[i - 1] == b'\r');
    // `lines()` drops the `\r` of a `\r\n`, so the buffer never contains
    // carriage returns; the line ending is put back on save.
    let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    Ok(LoadedFile { lines, crlf })
}

/// Save the buffer to disk, joining lines with `\r\n` if `crlf` and `\n`
/// otherwise, and ending the file with a line break if `final_newline`.
pub fn save(path: &Path, lines: &[String], crlf: bool, final_newline: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let eol = if crlf { "\r\n" } else { "\n" };
    let mut contents = lines.join(eol);
    if final_newline {
        contents.push_str(eol);
    }
    fs::write(path, contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("pc_fileio_{name}_{}", std::process::id()))
    }

    #[test]
    fn a_missing_file_is_one_empty_lf_line() {
        let f = load_or_create(&temp("missing")).unwrap();
        assert_eq!(f.lines, vec![String::new()]);
        assert!(!f.crlf);
    }

    #[test]
    fn crlf_is_detected_and_stripped_from_the_buffer() {
        let p = temp("crlf");
        fs::write(&p, "a\r\nb\r\n").unwrap();
        let f = load_or_create(&p).unwrap();
        assert_eq!(f.lines, vec!["a", "b"]);
        assert!(f.crlf);

        fs::write(&p, "a\nb\r\n").unwrap();
        assert!(
            !load_or_create(&p).unwrap().crlf,
            "first line break decides"
        );
        fs::write(&p, "no line breaks").unwrap();
        assert!(!load_or_create(&p).unwrap().crlf);
        let _ = fs::remove_file(p);
    }

    #[test]
    fn save_uses_the_requested_line_ending_and_final_newline() {
        let p = temp("save");
        let lines = vec!["a".to_string(), "b".to_string()];

        save(&p, &lines, false, true).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"a\nb\n");
        save(&p, &lines, true, true).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"a\r\nb\r\n");
        save(&p, &lines, false, false).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"a\nb");
        save(&p, &lines, true, false).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"a\r\nb");
        let _ = fs::remove_file(p);
    }

    #[test]
    fn a_crlf_file_round_trips_unchanged() {
        let p = temp("roundtrip");
        let original = "fn main()\r\n{\r\n}\r\n";
        fs::write(&p, original).unwrap();
        let f = load_or_create(&p).unwrap();
        save(&p, &f.lines, f.crlf, true).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), original);
        let _ = fs::remove_file(p);
    }
}
