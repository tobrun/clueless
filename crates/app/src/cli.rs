//! Hand-rolled argument parsing: five flags, so a parser crate would be
//! more ceremony than code. Any parse error makes the binary print the
//! usage text to stderr and exit 2.

use std::collections::VecDeque;
use std::path::PathBuf;

/// One `--replay` run: one or two WAV files (Me first, then Them), a
/// playback speed multiplier and whether to ask the LLM afterwards.
#[derive(Debug, Clone, PartialEq)]
pub struct Replay {
    pub files: Vec<PathBuf>,
    pub speed: f64,
    pub ask: bool,
}

/// The parsed command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Cli {
    /// `None` means `~/.config/clueless/config.toml`.
    pub config: Option<PathBuf>,
    /// `None` means `./.env`, then `~/.config/clueless/.env`.
    pub env_file: Option<PathBuf>,
    pub log_file: PathBuf,
    /// `None` means GUI mode.
    pub replay: Option<Replay>,
}

/// What [`parse`] produced.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Cli(Cli),
    /// `-h` or `--help`: print [`usage`] to stdout and exit 0.
    Help,
}

pub fn usage() -> String {
    "\
usage: clueless [OPTIONS]

Options:
  --config PATH      config file (default ~/.config/clueless/config.toml)
  --env-file PATH    .env file with LLM_* and ASR_* settings
                     (default ./.env, then ~/.config/clueless/.env)
  --log-file PATH    log file (default ~/Library/Logs/clueless/clueless.log)
  --replay ME [THEM] replay WAV file(s) through the engine instead of the GUI
  --speed N          replay speed multiplier (default 1)
  --ask              after a replay, ask the LLM and print the suggestion
  -h, --help         print this help and exit
"
    .to_string()
}

/// `~/Library/Logs/clueless/clueless.log`.
pub fn default_log_file() -> PathBuf {
    home().join("Library/Logs/clueless/clueless.log")
}

/// The home directory of the current user.
pub fn home() -> PathBuf {
    PathBuf::from(
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .expect("clueless runs as a user process, so HOME is set"),
    )
}

/// Parse arguments (already with the program name removed).
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args: VecDeque<String> = args.into_iter().collect();
    let mut config: Option<PathBuf> = None;
    let mut env_file: Option<PathBuf> = None;
    let mut log_file: Option<PathBuf> = None;
    let mut replay_files: Option<Vec<PathBuf>> = None;
    let mut speed: Option<f64> = None;
    let mut ask = false;

    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--config" => {
                config = Some(PathBuf::from(take_value(&mut args, &arg)?));
            }
            "--env-file" => {
                env_file = Some(PathBuf::from(take_value(&mut args, &arg)?));
            }
            "--log-file" => {
                log_file = Some(PathBuf::from(take_value(&mut args, &arg)?));
            }
            "--replay" => {
                if replay_files.is_some() {
                    return Err("--replay given twice".to_string());
                }
                let mut files = Vec::new();
                while files.len() < 2 {
                    match args.front() {
                        Some(next) if next.starts_with('-') => break,
                        Some(_) => files.push(PathBuf::from(args.pop_front().expect("checked"))),
                        None => break,
                    }
                }
                if files.is_empty() {
                    return Err("--replay needs at least a Me WAV file".to_string());
                }
                replay_files = Some(files);
            }
            "--speed" => {
                let value = take_value(&mut args, &arg)?;
                let parsed: f64 = value
                    .parse()
                    .map_err(|_| format!("--speed needs a number, got {value}"))?;
                if !parsed.is_finite() || parsed <= 0.0 {
                    return Err(format!("--speed must be a positive number, got {value}"));
                }
                speed = Some(parsed);
            }
            "--ask" => ask = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }

    let replay = replay_files.map(|files| Replay {
        files,
        speed: speed.unwrap_or(1.0),
        ask,
    });
    Ok(Parsed::Cli(Cli {
        config,
        env_file,
        log_file: log_file.unwrap_or_else(default_log_file),
        replay,
    }))
}

fn take_value(args: &mut VecDeque<String>, flag: &str) -> Result<String, String> {
    args.pop_front()
        .ok_or_else(|| format!("{flag} needs a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_line(line: &str) -> Result<Parsed, String> {
        parse(line.split_whitespace().map(str::to_string))
    }

    #[test]
    fn defaults_apply_when_no_flags_are_given() {
        let Parsed::Cli(cli) = parse_line("").unwrap() else {
            panic!("expected a parsed cli");
        };
        assert_eq!(cli.config, None);
        assert_eq!(cli.env_file, None);
        assert_eq!(cli.replay, None);
        assert!(cli.log_file.ends_with("Library/Logs/clueless/clueless.log"));
    }

    #[test]
    fn env_file_is_a_path_flag() {
        let Parsed::Cli(cli) = parse_line("--env-file /tmp/clueless.env").unwrap() else {
            panic!("expected a parsed cli");
        };
        assert_eq!(cli.env_file, Some(PathBuf::from("/tmp/clueless.env")));
        assert!(parse_line("--env-file").is_err());
    }

    #[test]
    fn replay_takes_one_or_two_files_and_options() {
        let Parsed::Cli(cli) = parse_line("--replay a.wav --speed 3.5 --ask").unwrap() else {
            panic!("expected a parsed cli");
        };
        let replay = cli.replay.expect("replay mode");
        assert_eq!(replay.files, vec![PathBuf::from("a.wav")]);
        assert_eq!(replay.speed, 3.5);
        assert!(replay.ask);
    }

    #[test]
    fn every_kind_of_parse_error_is_a_message() {
        assert!(parse_line("--bogus").is_err());
        assert!(parse_line("--speed").is_err());
        assert!(parse_line("--speed fast").is_err());
        assert!(parse_line("--speed 0").is_err());
        assert!(parse_line("--replay").is_err());
    }

    #[test]
    fn help_short_circuits() {
        assert_eq!(parse_line("--help").unwrap(), Parsed::Help);
    }
}
