//! Hand-rolled argument parsing: flags over one binary, so a parser crate
//! would be more ceremony than code. Any parse error makes the binary print
//! the usage text to stderr and exit 2. Exactly one mode flag may be given;
//! without one the binary runs the GUI.

use std::collections::VecDeque;
use std::path::PathBuf;

use clueless_types::profile::AssistProfile;

/// One `--replay` run: one or two WAV files (Me first, then Them), a
/// playback speed multiplier, whether to ask the LLM afterwards and the
/// assist profile to run under (`None` means Manual, whatever the config says).
#[derive(Debug, Clone, PartialEq)]
pub struct Replay {
    pub files: Vec<PathBuf>,
    pub speed: f64,
    pub ask: bool,
    pub profile: Option<AssistProfile>,
}

/// The one thing a launch does (D-cli-surface: flags on the binary, one
/// mode per launch).
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// The overlay GUI with live capture (no mode flag).
    Gui,
    /// Replay WAV files through the engine headlessly.
    Replay(Replay),
    /// Re-run a recorded session's audio through this build.
    ReplaySession { session: String, speed: f64 },
    /// Compare a session (or run) against another trace, judging suggestions
    /// unless `--no-judge`.
    Compare {
        a: String,
        b: Option<String>,
        judge: bool,
    },
    /// List recorded sessions.
    Sessions,
    /// Print one session as transcript with suggestions.
    Show { session: String },
    /// Delete one session or run; only deletes with `yes`.
    Delete { session: String, yes: bool },
}

/// The parsed command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Cli {
    /// `None` means `~/.config/clueless/config.toml`.
    pub config: Option<PathBuf>,
    /// `None` means `./.env`, then `~/.config/clueless/.env`.
    pub env_file: Option<PathBuf>,
    pub log_file: PathBuf,
    pub mode: Mode,
    /// The data directory holding `sessions/` (default `~/.clueless`).
    pub data_dir: PathBuf,
}

/// What [`parse`] produced.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Cli(Cli),
    /// `-h` or `--help`: print [`usage`] to stdout and exit 0.
    Help,
}

/// The error for a launch with two mode flags; the flags are named in the
/// order of the `Mode` enum.
const ONE_MODE: &str =
    "only one of --replay, --replay-session, --compare, --sessions, --show, --delete may be given";

pub fn usage() -> String {
    "\
usage: clueless [OPTIONS]

Options:
  --config PATH      config file (default ~/.config/clueless/config.toml)
  --env-file PATH    .env file with LLM_* and ASR_* settings
                     (default ./.env, then ~/.config/clueless/.env)
  --log-file PATH    log file (default ~/Library/Logs/clueless/clueless.log)
  --data-dir PATH    data directory holding sessions/ (default ~/.clueless)
  --replay ME [THEM] replay WAV file(s) through the engine instead of the GUI
  --speed N          replay speed multiplier (default 1)
  --ask              after a replay, ask the LLM and print the suggestion
  --profile NAME     replay under an assist profile: manual (default),
                     interview or brainstorm; automatic answers are printed
                     between the transcript lines
  --replay-session S re-run the audio of recorded session S through this
                     build, writing a run below it
  --compare A [B]    compare session or run A against run or session B
                     (B absent: A's newest run) and judge the suggestions
  --no-judge         with --compare, skip the LLM judging pass
  --sessions         list recorded sessions, newest first
  --show SESSION     print one session as transcript with suggestions
  --delete SESSION   delete one session or run (prints path and size unless
                     --yes is given)
  --yes              confirm --delete
  -h, --help         print this help and exit
"
    .to_string()
}

/// `~/Library/Logs/clueless/clueless.log`.
pub fn default_log_file() -> PathBuf {
    home().join("Library/Logs/clueless/clueless.log")
}

/// `~/.clueless`, the default data directory (D-trace-switches).
pub fn default_data_dir() -> PathBuf {
    home().join(".clueless")
}

/// The home directory of the current user.
pub fn home() -> PathBuf {
    PathBuf::from(
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .expect("clueless runs as a user process, so HOME is set"),
    )
}

/// The raw mode flags, assembled into one [`Mode`] after the loop.
#[derive(Default)]
struct Modes {
    replay_files: Option<Vec<PathBuf>>,
    replay_session: Option<String>,
    compare_a: Option<String>,
    compare_b: Option<String>,
    sessions: bool,
    show: Option<String>,
    delete: Option<String>,
    /// The number of mode flags seen, for the one-mode error.
    seen: usize,
}

impl Modes {
    /// Record one more mode flag; the second one in a launch is an error.
    fn give(&mut self) -> Result<(), String> {
        self.seen += 1;
        if self.seen > 1 {
            return Err(ONE_MODE.to_string());
        }
        Ok(())
    }
}

/// Parse arguments (already with the program name removed).
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args: VecDeque<String> = args.into_iter().collect();
    let mut config: Option<PathBuf> = None;
    let mut env_file: Option<PathBuf> = None;
    let mut log_file: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut modes = Modes::default();
    let mut speed: Option<f64> = None;
    let mut ask = false;
    let mut profile: Option<AssistProfile> = None;
    let mut yes = false;
    let mut no_judge = false;

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
            "--data-dir" => {
                data_dir = Some(PathBuf::from(take_value(&mut args, &arg)?));
            }
            "--replay" => {
                modes.give()?;
                modes.replay_files = Some(take_replay_files(&mut args)?)
            }
            "--replay-session" => {
                modes.give()?;
                modes.replay_session = Some(take_value(&mut args, &arg)?);
            }
            "--compare" => {
                modes.give()?;
                modes.compare_a = Some(take_value(&mut args, &arg)?);
                modes.compare_b = take_optional_value(&mut args);
            }
            "--sessions" => {
                modes.give()?;
                modes.sessions = true;
            }
            "--show" => {
                modes.give()?;
                modes.show = Some(take_value(&mut args, &arg)?);
            }
            "--delete" => {
                modes.give()?;
                modes.delete = Some(take_value(&mut args, &arg)?);
            }
            "--speed" => speed = Some(parse_speed(&take_value(&mut args, &arg)?)?),
            "--ask" => ask = true,
            "--profile" => profile = Some(take_value(&mut args, &arg)?.parse::<AssistProfile>()?),
            "--yes" => yes = true,
            "--no-judge" => no_judge = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }

    let mode = assemble_mode(modes, speed, ask, profile, yes, no_judge)?;
    Ok(Parsed::Cli(Cli {
        config,
        env_file,
        log_file: log_file.unwrap_or_else(default_log_file),
        mode,
        data_dir: data_dir.unwrap_or_else(default_data_dir),
    }))
}

/// Combine the mode flags into the one [`Mode`]; every cross-flag rule of
/// D-cli-surface lives here.
fn assemble_mode(
    modes: Modes,
    speed: Option<f64>,
    ask: bool,
    profile: Option<AssistProfile>,
    yes: bool,
    no_judge: bool,
) -> Result<Mode, String> {
    let Modes {
        replay_files,
        replay_session,
        compare_a,
        compare_b,
        sessions,
        show,
        delete,
        seen: _,
    } = modes;
    if yes && delete.is_none() {
        return Err("--yes only applies to --delete".to_string());
    }
    if no_judge && compare_a.is_none() {
        return Err("--no-judge only applies to --compare".to_string());
    }
    if let Some(files) = replay_files {
        return Ok(Mode::Replay(Replay {
            files,
            speed: speed.unwrap_or(1.0),
            ask,
            profile,
        }));
    }
    if profile.is_some() {
        return Err("--profile only applies to --replay".to_string());
    }
    if let Some(session) = replay_session {
        if ask {
            return Err("--ask only applies to --replay".to_string());
        }
        return Ok(Mode::ReplaySession {
            session,
            speed: speed.unwrap_or(1.0),
        });
    }
    if let Some(a) = compare_a {
        return Ok(Mode::Compare {
            a,
            b: compare_b,
            judge: !no_judge,
        });
    }
    if sessions {
        return Ok(Mode::Sessions);
    }
    if let Some(session) = show {
        return Ok(Mode::Show { session });
    }
    if let Some(session) = delete {
        return Ok(Mode::Delete { session, yes });
    }
    Ok(Mode::Gui)
}

/// The one or two WAV paths after `--replay`.
fn take_replay_files(args: &mut VecDeque<String>) -> Result<Vec<PathBuf>, String> {
    let files = take_wav_paths(args);
    if files.is_empty() {
        return Err("--replay needs at least a Me WAV file".to_string());
    }
    Ok(files)
}

/// Up to two leading arguments that are not flags.
fn take_wav_paths(args: &mut VecDeque<String>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    while files.len() < 2 && args.front().is_some_and(|next| !next.starts_with('-')) {
        if let Some(next) = args.pop_front() {
            files.push(PathBuf::from(next));
        }
    }
    files
}

/// The next argument unless it starts with `-` (the optional second
/// `--compare` operand).
fn take_optional_value(args: &mut VecDeque<String>) -> Option<String> {
    match args.front() {
        Some(next) if !next.starts_with('-') => args.pop_front(),
        _ => None,
    }
}

fn parse_speed(value: &str) -> Result<f64, String> {
    let parsed: f64 = value
        .parse()
        .map_err(|_| format!("--speed needs a number, got {value}"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(format!("--speed must be a positive number, got {value}"));
    }
    Ok(parsed)
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

    fn cli_of(line: &str) -> Cli {
        match parse_line(line) {
            Ok(Parsed::Cli(cli)) => cli,
            other => panic!("expected a parsed cli, got {other:?}"),
        }
    }

    #[test]
    fn defaults_apply_when_no_flags_are_given() {
        let cli = cli_of("");
        assert_eq!(cli.config, None);
        assert_eq!(cli.env_file, None);
        assert_eq!(cli.mode, Mode::Gui);
        assert!(cli.log_file.ends_with("Library/Logs/clueless/clueless.log"));
    }

    #[test]
    fn the_data_dir_defaults_to_dot_clueless_in_home() {
        let cli = cli_of("");
        assert_eq!(cli.data_dir, crate::cli::home().join(".clueless"));
    }

    #[test]
    fn data_dir_is_a_path_flag_valid_without_a_mode() {
        assert_eq!(cli_of("--data-dir /x").data_dir, PathBuf::from("/x"));
        let error = parse_line("--data-dir").unwrap_err();
        assert_eq!(error, "--data-dir needs a value");
    }

    #[test]
    fn env_file_is_a_path_flag() {
        let cli = cli_of("--env-file /tmp/clueless.env");
        assert_eq!(cli.env_file, Some(PathBuf::from("/tmp/clueless.env")));
        assert!(parse_line("--env-file").is_err());
    }

    #[test]
    fn replay_takes_one_or_two_files_and_options() {
        let Parsed::Cli(cli) = parse_line("--replay a.wav --speed 3.5 --ask").unwrap() else {
            panic!("expected a parsed cli");
        };
        let Mode::Replay(replay) = cli.mode else {
            panic!("expected replay mode");
        };
        assert_eq!(replay.files, vec![PathBuf::from("a.wav")]);
        assert_eq!(replay.speed, 3.5);
        assert!(replay.ask);
    }

    #[test]
    fn profile_is_a_replay_option_with_three_valid_names() {
        let Parsed::Cli(cli) = parse_line("--replay a.wav --profile brainstorm").unwrap() else {
            panic!("expected a parsed cli");
        };
        let Mode::Replay(replay) = cli.mode else {
            panic!("expected replay mode");
        };
        assert_eq!(replay.profile, Some(AssistProfile::Brainstorm));
        let Parsed::Cli(cli) = parse_line("--replay a.wav").unwrap() else {
            panic!("expected a parsed cli");
        };
        let Mode::Replay(replay) = cli.mode else {
            panic!("expected replay mode");
        };
        assert_eq!(replay.profile, None);
    }

    #[test]
    fn an_unknown_profile_names_the_three_valid_ones() {
        let error = parse_line("--replay a.wav --profile coach").unwrap_err();
        for name in ["manual", "interview", "brainstorm"] {
            assert!(error.contains(name), "error was: {error}");
        }
        assert!(parse_line("--replay a.wav --profile").is_err());
    }

    #[test]
    fn replay_takes_at_most_two_files() {
        let error = parse_line("--replay a.wav b.wav c.wav").unwrap_err();
        assert!(
            error.contains("unknown argument c.wav"),
            "error was: {error}"
        );
    }

    #[test]
    fn profile_without_replay_is_an_error() {
        let error = parse_line("--profile interview").unwrap_err();
        assert!(error.contains("--replay"), "error was: {error}");
    }

    #[test]
    fn replay_session_takes_a_name_and_a_speed() {
        assert_eq!(
            cli_of("--replay-session S --speed 2").mode,
            Mode::ReplaySession {
                session: "S".to_string(),
                speed: 2.0,
            }
        );
        assert_eq!(
            cli_of("--replay-session S").mode,
            Mode::ReplaySession {
                session: "S".to_string(),
                speed: 1.0,
            }
        );
    }

    #[test]
    fn ask_and_profile_are_not_replay_session_options() {
        let error = parse_line("--replay-session S --ask").unwrap_err();
        assert!(error.contains("--ask"), "error was: {error}");
        let error = parse_line("--replay-session S --profile interview").unwrap_err();
        assert!(error.contains("--profile"), "error was: {error}");
    }

    #[test]
    fn compare_takes_one_or_two_operands_and_judges_by_default() {
        assert_eq!(
            cli_of("--compare A").mode,
            Mode::Compare {
                a: "A".to_string(),
                b: None,
                judge: true,
            }
        );
        let Mode::Compare { a, b, judge } = cli_of("--compare A B --no-judge").mode else {
            panic!("expected compare mode");
        };
        assert_eq!(a, "A");
        assert_eq!(b, Some("B".to_string()));
        assert!(!judge);
    }

    #[test]
    fn sessions_show_and_delete_parse_their_operands() {
        assert_eq!(cli_of("--sessions").mode, Mode::Sessions);
        assert_eq!(
            cli_of("--show S").mode,
            Mode::Show {
                session: "S".to_string()
            }
        );
        assert_eq!(
            cli_of("--delete S --yes").mode,
            Mode::Delete {
                session: "S".to_string(),
                yes: true,
            }
        );
        assert_eq!(
            cli_of("--delete S").mode,
            Mode::Delete {
                session: "S".to_string(),
                yes: false,
            }
        );
    }

    #[test]
    fn two_mode_flags_are_the_one_mode_error() {
        for line in [
            "--sessions --show S",
            "--replay a.wav --sessions",
            "--compare A --delete B",
            "--replay-session S --replay a.wav",
        ] {
            let error = parse_line(line).unwrap_err();
            assert_eq!(error, ONE_MODE, "line was: {line}");
        }
    }

    #[test]
    fn yes_and_no_judge_need_their_mode() {
        let error = parse_line("--yes").unwrap_err();
        assert!(error.contains("--delete"), "error was: {error}");
        let error = parse_line("--no-judge").unwrap_err();
        assert!(error.contains("--compare"), "error was: {error}");
    }

    #[test]
    fn every_kind_of_parse_error_is_a_message() {
        assert!(parse_line("--bogus").is_err());
        assert!(parse_line("--speed").is_err());
        assert!(parse_line("--speed fast").is_err());
        assert!(parse_line("--speed 0").is_err());
        assert!(parse_line("--replay").is_err());
        assert!(parse_line("--show").is_err());
        assert!(parse_line("--delete").is_err());
        assert!(parse_line("--replay-session").is_err());
        assert!(parse_line("--compare").is_err());
    }

    #[test]
    fn help_short_circuits() {
        assert_eq!(parse_line("--help").unwrap(), Parsed::Help);
    }
}
