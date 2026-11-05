//! Command-line arguments.

use std::path::PathBuf;

pub const USAGE: &str = "\
Pis Code — a small terminal code editor

USAGE:
    PC [OPTIONS] <FILE>

OPTIONS:
    --config <FILE>    Use this config file instead of the default one
    --no-config        Ignore every config file; use the built-in defaults
    --init-config      Write a fully documented config file and exit
                       (to --config FILE if given, else the default location;
                       refuses to overwrite unless --force is also given)
    --force            Let --init-config overwrite an existing file
    --print-config     Print the effective settings for FILE as TOML and exit
    --config-path      Show which config files apply to FILE and exit
    -h, --help         Show this help
    -V, --version      Show the version

CONFIG FILES (later ones override earlier ones, setting by setting):
    built-in defaults
    the user file         ~/.config/pis-code/config.toml (see --config-path)
                          or the file named by $PIS_CODE_CONFIG
    .pis-code.toml        in FILE's folder or any folder above it
";

#[derive(Debug, Default, PartialEq)]
pub struct Cli {
    pub file: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub no_config: bool,
    pub init_config: bool,
    pub force: bool,
    pub print_config: bool,
    pub config_path: bool,
    pub help: bool,
    pub version: bool,
}

/// Parses the arguments after the program name. Errors are one-line
/// messages for the user.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut args = args.into_iter();
    let mut only_files = false;

    while let Some(arg) = args.next() {
        if only_files || !arg.starts_with('-') || arg == "-" {
            if cli.file.replace(PathBuf::from(&arg)).is_some() {
                return Err(format!(
                    "unexpected extra argument {arg:?}; only one file can be edited"
                ));
            }
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let has_inline = inline.is_some();
        match name.as_str() {
            "--" => only_files = true,
            "--config" => {
                let value = match inline {
                    Some(v) => v,
                    None => args.next().ok_or("--config needs a file name")?,
                };
                if value.is_empty() {
                    return Err("--config needs a file name".to_string());
                }
                cli.config = Some(PathBuf::from(value));
            }
            "--no-config" => cli.no_config = true,
            "--init-config" => cli.init_config = true,
            "--force" => cli.force = true,
            "--print-config" => cli.print_config = true,
            "--config-path" => cli.config_path = true,
            "-h" | "--help" => cli.help = true,
            "-V" | "--version" => cli.version = true,
            _ => return Err(format!("unknown option {arg:?} (see --help)")),
        }
        if has_inline && name != "--config" {
            return Err(format!("{name} doesn't take a value"));
        }
    }

    if cli.config.is_some() && cli.no_config && !cli.init_config {
        return Err("--config and --no-config contradict each other".to_string());
    }
    if cli.force && !cli.init_config {
        return Err("--force only makes sense with --init-config".to_string());
    }
    Ok(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Cli, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn a_plain_file_argument() {
        let cli = parse_args(&["a.rs"]).unwrap();
        assert_eq!(cli.file, Some(PathBuf::from("a.rs")));
        assert_eq!(
            cli,
            Cli {
                file: Some(PathBuf::from("a.rs")),
                ..Cli::default()
            }
        );
    }

    #[test]
    fn options_can_come_before_or_after_the_file() {
        for args in [
            &["--config", "c.toml", "a.rs"][..],
            &["a.rs", "--config", "c.toml"][..],
            &["--config=c.toml", "a.rs"][..],
        ] {
            let cli = parse_args(args).unwrap();
            assert_eq!(cli.config, Some(PathBuf::from("c.toml")), "{args:?}");
            assert_eq!(cli.file, Some(PathBuf::from("a.rs")), "{args:?}");
        }
    }

    #[test]
    fn flags_are_recognised() {
        let cli =
            parse_args(&["--print-config", "--no-config", "-V", "-h", "--config-path"]).unwrap();
        assert!(cli.print_config && cli.no_config && cli.version && cli.help && cli.config_path);
        assert_eq!(cli.file, None);
        let init = parse_args(&["--init-config", "--force"]).unwrap();
        assert!(init.init_config && init.force);
    }

    #[test]
    fn double_dash_allows_file_names_starting_with_a_dash() {
        let cli = parse_args(&["--", "-weird.txt"]).unwrap();
        assert_eq!(cli.file, Some(PathBuf::from("-weird.txt")));
    }

    #[test]
    fn mistakes_are_reported_in_one_line() {
        assert!(parse_args(&["--bogus"]).unwrap_err().contains("--bogus"));
        assert!(parse_args(&["--config"]).unwrap_err().contains("file name"));
        assert!(parse_args(&["--config="])
            .unwrap_err()
            .contains("file name"));
        assert!(parse_args(&["a", "b"])
            .unwrap_err()
            .contains("only one file"));
        assert!(parse_args(&["--no-config=1"])
            .unwrap_err()
            .contains("doesn't take a value"));
        assert!(parse_args(&["--config", "c", "--no-config"])
            .unwrap_err()
            .contains("contradict"));
        assert!(parse_args(&["--force"])
            .unwrap_err()
            .contains("--init-config"));
    }
}
