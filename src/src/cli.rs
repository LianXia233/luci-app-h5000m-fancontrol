//! Command line interface.
//!
//! The positional commands and their exit codes are the ones the 2.x shell
//! controller had, because `/etc/init.d/h5000m-fancontrol`, the LuCI ACL and any
//! operator script all call into this binary. The new subcommands are additive.

use crate::config::Settings;
use crate::daemon::{self, ApplyOutcome};
use crate::events;
use crate::json::Value;
use crate::paths::Paths;
use crate::platform;
use crate::report::{self, Report, SampleMode};
use crate::sensor::IwprivProbe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Apply,
    Status { mode: SampleMode, format: Format },
    Daemon,
    Release,
    Events { limit: usize },
    Validate,
    Schema,
    Version,
    Help,
}

pub const USAGE: &str = "\
Usage: h5000m-fancontrol [command]

  apply              run one control cycle now (default)
  status             print the current state as key=value pairs
  status json        print the machine readable status contract
  status live        sample every sensor now, then print key=value pairs
  daemon             run the control loop in the foreground (used by procd)
  release            restore a thermal governor left in user_space by 2.0.x
  events [n]         print the last n journal entries as JSON (default 40)
  validate           check the configuration and the active fan curve
  schema             list the status contract field paths
  --version          print the controller version
  --help             show this text

Environment overrides for testing (never needed on a device):
  H5000M_FAN_SENSOR_SYSFS_ROOT, H5000M_FAN_RADIO_SYSFS_ROOT,
  H5000M_FAN_DT_ROOT, H5000M_FAN_UCI_CONFIG, H5000M_FAN_RUN_DIR,
  H5000M_FAN_LOCK_DIR, H5000M_FAN_PROC_ROOT,
  H5000M_FAN_MT5700M_TEMP_CACHE, H5000M_FAN_MT5700M_TEMP_MAX_AGE

Exit codes: 0 success, 1 a control cycle or status could not be produced,
2 the command line was not understood.";

pub fn parse(args: &[String]) -> std::result::Result<Command, String> {
    let first = args.first().map(String::as_str).unwrap_or("apply");
    match first {
        "apply" | "" => {
            if args.len() > 1 {
                return Err(String::from("apply takes no extra arguments"));
            }
            Ok(Command::Apply)
        }
        "status" => {
            let mut mode = SampleMode::Cached;
            let mut format = Format::Plain;
            for argument in &args[1..] {
                match argument.as_str() {
                    "json" => format = Format::Json,
                    "plain" => format = Format::Plain,
                    "live" => mode = SampleMode::Live,
                    "cached" => mode = SampleMode::Cached,
                    other => return Err(format!("unknown status option {other:?}")),
                }
            }
            Ok(Command::Status { mode, format })
        }
        "daemon" | "run" => {
            if args.len() > 1 {
                return Err(String::from("daemon takes no extra arguments"));
            }
            Ok(Command::Daemon)
        }
        "release" => Ok(Command::Release),
        "events" => {
            let limit = args
                .get(1)
                .map(|value| value.parse::<usize>())
                .transpose()
                .map_err(|err| format!("bad event limit: {err}"))?
                .unwrap_or(report::EVENT_LIMIT);
            if args.len() > 2 {
                return Err(String::from("events takes at most one argument"));
            }
            Ok(Command::Events { limit })
        }
        "validate" => Ok(Command::Validate),
        "schema" => Ok(Command::Schema),
        "--version" | "version" => Ok(Command::Version),
        "--help" | "help" | "-h" => Ok(Command::Help),
        other => Err(format!("unknown command {other:?}")),
    }
}

fn report_json(report: &Report, paths: &Paths) -> String {
    let mut text = report.to_value(paths).to_json();
    text.push('\n');
    text
}

fn report_plain(report: &Report) -> String {
    report.to_plain()
}

/// Run `argv` and return the process exit code.
pub fn execute(command: Command, paths: &Paths) -> i32 {
    let probe = IwprivProbe::default();
    match command {
        Command::Help => {
            print!("{USAGE}");
            0
        }
        Command::Version => {
            println!("{}", crate::VERSION);
            0
        }
        Command::Schema => {
            let value = Value::array(
                report::contract()
                    .into_iter()
                    .map(Value::str)
                    .collect::<Vec<_>>(),
            );
            println!("{}", value.to_json());
            0
        }
        Command::Events { limit } => {
            let value = Value::array(events::to_values(&events::read_recent(paths, limit)));
            println!("{}", value.to_json());
            0
        }
        Command::Validate => {
            let settings = match Settings::load(paths) {
                Ok(settings) => settings,
                Err(err) => {
                    eprintln!("configuration cannot be read: {err}");
                    return 1;
                }
            };
            let curve_error = settings.curve_failure();
            let value = Value::object([
                ("ok", Value::Bool(curve_error.is_none())),
                (
                    "curve_valid",
                    Value::Bool(settings.curve_failure().is_none()),
                ),
                ("curve_error", Value::opt_str(curve_error.clone())),
                (
                    "issues",
                    Value::array(
                        settings
                            .issues
                            .iter()
                            .map(|issue| {
                                Value::object([
                                    ("option", Value::str(issue.option.clone())),
                                    ("value", Value::str(issue.value.clone())),
                                    ("problem", Value::str(issue.problem.clone())),
                                    ("applied", Value::str(issue.applied.clone())),
                                ])
                            })
                            .collect::<Vec<_>>(),
                    ),
                ),
                (
                    "pwm_node",
                    Value::opt_str(
                        crate::fan::discover(paths)
                            .map(|device| device.pwm_path.display().to_string()),
                    ),
                ),
                (
                    "sensors_found",
                    Value::Int(
                        crate::sensor::sample_all(paths, &probe, platform::wall_now())
                            .readings
                            .iter()
                            .filter(|reading| reading.is_valid())
                            .count() as i64,
                    ),
                ),
            ]);
            println!("{}", value.to_json());
            if let Some(problem) = curve_error {
                eprintln!("active fan curve is not usable: {problem}");
                return 1;
            }
            if !settings.issues.is_empty() {
                eprintln!(
                    "{} configuration problem(s) corrected; see the JSON output",
                    settings.issues.len()
                );
            }
            0
        }
        Command::Status { mode, format } => {
            let built = report::build(paths, &probe, mode, platform::wall_now());
            match built {
                Ok(built) => {
                    match format {
                        Format::Json => print!("{}", report_json(&built, paths)),
                        Format::Plain => print!("{}", report_plain(&built)),
                    }
                    0
                }
                Err(err) => {
                    let value = Value::object([
                        ("schema", Value::Int(report::REPORT_SCHEMA as i64)),
                        ("ok", Value::Bool(false)),
                        ("generated_at", Value::Int(platform::wall_now() as i64)),
                        ("error", Value::str(err.to_string())),
                    ]);
                    match format {
                        Format::Json => println!("{}", value.to_json()),
                        Format::Plain => println!("error={err}"),
                    }
                    eprintln!("cannot read the controller status: {err}");
                    1
                }
            }
        }
        Command::Apply => match daemon::apply(paths) {
            Ok(ApplyOutcome::Queued { pid }) => {
                println!("queued: the running controller (pid {pid}) will apply immediately");
                0
            }
            Ok(ApplyOutcome::Applied(snapshot)) => {
                if snapshot.result == crate::state::ControlResult::Error {
                    eprintln!(
                        "control cycle failed: reason={} error={:?}",
                        snapshot.reason, snapshot.last_error
                    );
                    return 1;
                }
                println!(
                    "applied: target={} applied={} reason={}",
                    snapshot.target_pwm, snapshot.applied_pwm, snapshot.reason
                );
                0
            }
            Err(err) => {
                if matches!(err, crate::Error::LockBusy { .. }) {
                    // Keep the 2.x message so a script grepping for it still works.
                    eprintln!("fan control is busy");
                } else {
                    eprintln!("{err}");
                }
                1
            }
        },
        Command::Release => match daemon::release(paths) {
            Ok(Some(note)) => {
                println!("{note}");
                0
            }
            Ok(None) => {
                println!("no legacy thermal policy record to restore");
                0
            }
            Err(err) => {
                eprintln!("{err}");
                1
            }
        },
        Command::Daemon => {
            let signals = platform::Signals::new();
            platform::install_handlers(&signals);
            let result = daemon::run(paths, &signals);
            match result {
                crate::state::ControlResult::Error => 1,
                _ => 0,
            }
        }
    }
}

/// Entry point used by `main`.
pub fn run<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let collected: Vec<String> = args.into_iter().map(Into::into).collect();
    let paths = Paths::from_env();
    match parse(&collected) {
        Ok(command) => execute(command, &paths),
        Err(message) => {
            eprintln!("{message}");
            eprint!("{USAGE}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn default_command_is_apply_for_procd_compatibility() {
        assert_eq!(parse(&[]).unwrap(), Command::Apply);
        assert_eq!(parse(&args("apply")).unwrap(), Command::Apply);
    }

    #[test]
    fn status_formats_and_modes_are_independent() {
        assert_eq!(
            parse(&args("status")).unwrap(),
            Command::Status {
                mode: SampleMode::Cached,
                format: Format::Plain
            }
        );
        assert_eq!(
            parse(&args("status json")).unwrap(),
            Command::Status {
                mode: SampleMode::Cached,
                format: Format::Json
            }
        );
        assert_eq!(
            parse(&args("status live")).unwrap(),
            Command::Status {
                mode: SampleMode::Live,
                format: Format::Plain
            }
        );
        assert_eq!(
            parse(&args("status live json")).unwrap(),
            Command::Status {
                mode: SampleMode::Live,
                format: Format::Json
            }
        );
        assert_eq!(
            parse(&args("status json live")).unwrap(),
            Command::Status {
                mode: SampleMode::Live,
                format: Format::Json
            }
        );
    }

    #[test]
    fn unknown_arguments_are_rejected_with_usage_exit() {
        assert!(parse(&args("status turbo")).is_err());
        assert!(parse(&args("teleport")).is_err());
        assert!(parse(&args("apply now")).is_err());
        assert_eq!(run(["teleport"]), 2);
    }

    #[test]
    fn version_and_help_always_succeed() {
        let paths = Paths::sandbox(std::env::temp_dir().join("h5fan-cli-absent"));
        assert_eq!(execute(Command::Version, &paths), 0);
        assert_eq!(execute(Command::Help, &paths), 0);
    }

    #[test]
    fn status_reports_an_error_object_instead_of_an_empty_one() {
        let root = std::env::temp_dir().join(format!("h5fan-cli-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("etc/config")).unwrap();
        // A directory where the UCI file should be makes reading fail for real.
        std::fs::create_dir_all(root.join("etc/config/h5000m_fancontrol")).unwrap();
        let paths = Paths::sandbox(&root);
        let code = execute(
            Command::Status {
                mode: SampleMode::Cached,
                format: Format::Json,
            },
            &paths,
        );
        assert_eq!(code, 1, "a broken status must be visible to the caller");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn validate_passes_for_the_default_shipped_configuration() {
        let root = std::env::temp_dir().join(format!("h5fan-cli-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("etc/config")).unwrap();
        std::fs::write(
            root.join("etc/config/h5000m_fancontrol"),
            std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../root/etc/config/h5000m_fancontrol"
            ))
            .unwrap(),
        )
        .unwrap();
        let paths = Paths::sandbox(&root);
        assert_eq!(execute(Command::Validate, &paths), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn events_survive_a_missing_journal() {
        let paths = Paths::sandbox(std::env::temp_dir().join("h5fan-cli-events-absent"));
        assert_eq!(
            execute(Command::Events { limit: 10 }, &paths),
            0,
            "an absent journal is simply empty"
        );
    }
}
