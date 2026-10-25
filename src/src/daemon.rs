//! Service lifecycle: the procd foreground loop and the one-shot `apply`.
//!
//! The daemon holds the exclusive lock for its whole lifetime, so `apply` can
//! detect a running controller and ask it to sample immediately instead of
//! performing a second, racing PWM write. Configuration edits are picked up by
//! watching the UCI file, which is why saving no longer restarts the service.

use crate::config::Settings;
use crate::controller;
use crate::error::{Error, Result};
use crate::events::{self, Event, Level, Throttle};
use crate::lock::{Acquisition, Lock};
use crate::logging;
use crate::paths::Paths;
use crate::platform::{self, Signals};
use crate::safety;
use crate::sensor::IwprivProbe;
use crate::state::{ControlResult, Snapshot};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// How often the daemon looks for a wake marker or a configuration edit.
const POLL_SLICE: Duration = Duration::from_millis(200);
/// Repeated identical log lines are collapsed to one per window.
const LOG_WINDOW_SECONDS: u64 = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The cycle ran here. Boxed: a snapshot is far larger than a pid.
    Applied(Box<Snapshot>),
    /// A live daemon owns the lock and was told to run a cycle immediately.
    Queued { pid: u32 },
}

#[derive(Default)]
struct FileStamp {
    path: Option<PathBuf>,
    stamp: Option<SystemTime>,
}

impl FileStamp {
    fn observe(&mut self, path: &PathBuf) -> bool {
        let stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let changed = self.path.as_ref() != Some(path) || self.stamp != stamp;
        self.path = Some(path.clone());
        self.stamp = stamp;
        changed
    }

    fn is_unknown(&self) -> bool {
        self.stamp.is_none()
    }
}

/// Ask a running daemon to apply the policy right now.
pub fn request_wake(paths: &Paths) -> Result<()> {
    if let Some(parent) = paths.wake_file().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(paths.wake_file(), format!("{}\n", platform::wall_now()))
        .map_err(|err| Error::io(paths.wake_file().display().to_string(), &err))
}

fn wake_is_pending(paths: &Paths, handled_at: &mut Option<SystemTime>) -> bool {
    let Ok(metadata) = std::fs::metadata(paths.wake_file()) else {
        *handled_at = None;
        return false;
    };
    let Ok(stamp) = metadata.modified() else {
        return false;
    };
    if handled_at
        .map(|previous| previous == stamp)
        .unwrap_or(false)
    {
        return false;
    }
    *handled_at = Some(stamp);
    let _ = std::fs::remove_file(paths.wake_file());
    true
}

/// `h5000m-fancontrol apply`: one cycle, or a nudge when a daemon is running.
pub fn apply(paths: &Paths) -> Result<ApplyOutcome> {
    let probe = IwprivProbe::default();
    match Lock::try_acquire(paths) {
        Acquisition::Held { pid } => {
            request_wake(paths)?;
            Ok(ApplyOutcome::Queued { pid })
        }
        Acquisition::Unavailable { message } => Err(Error::io(
            paths.lock_dir.display().to_string(),
            &std::io::Error::other(message),
        )),
        Acquisition::Acquired(_lock) => {
            // The lock is released when `_lock` leaves this scope, including on
            // an early `?` return.
            if let Some(note) = safety::restore_legacy_policy(paths) {
                log(paths, Level::Info, "policy-restore", note.clone());
            }
            let previous = Snapshot::read(paths);
            let now = platform::wall_now();
            let mut snapshot =
                controller::run_cycle(paths, &probe, previous.as_ref(), now, &mut |duration| {
                    std::thread::sleep(duration);
                });
            snapshot.pid = platform::process_id();
            // A one-shot apply is an operator action, so it always leaves an
            // audit line; the periodic loop only logs state transitions.
            log(
                paths,
                match snapshot.result {
                    ControlResult::Error => Level::Error,
                    ControlResult::Failsafe => Level::Warn,
                    ControlResult::Ok => Level::Info,
                },
                "applied",
                format!(
                    "target={} applied={} reason={}",
                    snapshot.target_pwm, snapshot.applied_pwm, snapshot.reason
                ),
            );
            let mut tracker = CycleTracker::from_snapshot(previous.as_ref());
            tracker.record(paths, &snapshot, now);
            let outcome = controller::publish(paths, &snapshot);
            if let Err(err) = outcome {
                log(paths, Level::Error, "state-write", err.to_string());
            }
            Ok(ApplyOutcome::Applied(Box::new(snapshot)))
        }
    }
}

fn log(paths: &Paths, level: Level, code: &str, message: String) {
    let line = format!("[{code}] {message}");
    logging::emit(paths, level, platform::process_id(), &line);
    let _ = events::append(
        paths,
        &Event::new(level, code, message, platform::wall_now()),
    );
}

/// Track what changed between cycles so the log stays readable.
struct CycleTracker {
    last_result: Option<ControlResult>,
    last_reason: Option<String>,
    last_signature: Option<String>,
    throttle: Throttle,
}

impl CycleTracker {
    fn new() -> Self {
        CycleTracker {
            last_result: None,
            last_reason: None,
            last_signature: None,
            throttle: Throttle::new(LOG_WINDOW_SECONDS),
        }
    }

    /// Seed from the published snapshot so that a one-shot `apply` still
    /// recognises a transition against what the previous cycle did.
    fn from_snapshot(previous: Option<&Snapshot>) -> Self {
        let mut tracker = CycleTracker::new();

        if let Some(previous) = previous {
            tracker.last_result = Some(previous.result);
            tracker.last_reason = Some(previous.reason.clone());
        }

        tracker
    }

    fn record(&mut self, paths: &Paths, snapshot: &Snapshot, now: u64) {
        self.throttle.prune(now);
        let changed = self.last_result != Some(snapshot.result)
            || self.last_reason.as_deref() != Some(snapshot.reason.as_str());
        if changed {
            let level = match (self.last_result, snapshot.result) {
                (_, ControlResult::Error) => Level::Error,
                (_, ControlResult::Failsafe) => Level::Warn,
                (Some(ControlResult::Error) | Some(ControlResult::Failsafe), ControlResult::Ok) => {
                    Level::Info
                }
                _ => Level::Info,
            };
            // The very first cycle is not a transition worth a log line; only
            // report it when something is already wrong.
            if self.last_result.is_some() || level != Level::Info {
                log(
                    paths,
                    level,
                    "control",
                    format!(
                        "reason={} target={} applied={} temp={:?}",
                        snapshot.reason,
                        snapshot.target_pwm,
                        snapshot.applied_pwm,
                        snapshot.control_temp
                    ),
                );
            }
            self.last_result = Some(snapshot.result);
            self.last_reason = Some(snapshot.reason.clone());
        }
        if let Some(error) = snapshot.last_error.as_deref() {
            if self.throttle.allow(error, now) {
                log(paths, Level::Error, "cycle", error.to_string());
            }
        }
        if snapshot.boost_applied && self.throttle.allow("boost", now) {
            log(
                paths,
                Level::Info,
                "boost",
                format!("startup boost applied, target {}", snapshot.target_pwm),
            );
        }
    }

    fn note_settings(&mut self, paths: &Paths, settings: &Settings) {
        if !settings.issues.is_empty() && self.throttle.allow("config-issues", platform::wall_now())
        {
            let summary = settings
                .issues
                .iter()
                .map(|issue| issue.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            log(paths, Level::Warn, "config", summary);
        }
        let signature = settings.policy_signature();
        if self.last_signature.as_deref() != Some(signature.as_str()) {
            if self.last_signature.is_some() {
                log(
                    paths,
                    Level::Info,
                    "config",
                    format!("policy changed: mode={}", settings.mode.as_str()),
                );
            }
            self.last_signature = Some(signature);
        }
    }
}

/// procd foreground loop.
pub fn run(paths: &Paths, signals: &Signals) -> ControlResult {
    let probe = IwprivProbe::default();
    let mut config_stamp = FileStamp::default();
    let mut wake_stamp: Option<SystemTime> = None;
    let mut tracker = CycleTracker::new();

    if let Err(err) = std::fs::create_dir_all(&paths.run_dir) {
        eprintln!(
            "h5000m-fancontrol: cannot create {}: {err}",
            paths.run_dir.display()
        );
    }

    let _lock = match Lock::try_acquire(paths) {
        Acquisition::Acquired(lock) => lock,
        Acquisition::Held { pid } => {
            logging::emit(
                paths,
                Level::Error,
                platform::process_id(),
                &format!("another controller is running (pid {pid}); not starting"),
            );
            return ControlResult::Error;
        }
        Acquisition::Unavailable { message } => {
            logging::emit(
                paths,
                Level::Error,
                platform::process_id(),
                &format!("cannot take the control lock: {message}"),
            );
            return ControlResult::Error;
        }
    };

    if let Some(note) = safety::restore_legacy_policy(paths) {
        log(paths, Level::Info, "policy-restore", note);
    }
    log(
        paths,
        Level::Info,
        "start",
        format!("controller {} started", crate::VERSION),
    );

    let mut result = ControlResult::Ok;
    loop {
        if signals.stop_requested() {
            break;
        }
        let now = platform::wall_now();
        let previous = Snapshot::read(paths);
        let settings = Settings::load(paths).unwrap_or_default();
        tracker.note_settings(paths, &settings);
        let _ = config_stamp.observe(&paths.uci_config);
        let mut snapshot =
            controller::run_cycle(paths, &probe, previous.as_ref(), now, &mut |duration| {
                platform::sleep_slice(duration, signals, &|| false);
            });
        snapshot.pid = platform::process_id();
        tracker.record(paths, &snapshot, now);
        if let Err(err) = controller::publish(paths, &snapshot) {
            logging::emit(
                paths,
                Level::Error,
                platform::process_id(),
                &format!("cannot publish the state snapshot: {err}"),
            );
        }
        result = snapshot.result;

        // Sleep until the next interval, a configuration edit, a wake request or
        // a signal, whichever comes first.
        let interval = Duration::from_secs(settings.interval.max(2));
        let deadline = std::time::Instant::now() + interval;
        loop {
            if signals.stop_requested() {
                break;
            }
            if signals.take_reload() || wake_is_pending(paths, &mut wake_stamp) {
                break;
            }
            if !config_stamp.is_unknown() && config_stamp.observe(&paths.uci_config) {
                log(
                    paths,
                    Level::Info,
                    "config",
                    "configuration changed".to_string(),
                );
                break;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(remaining.min(POLL_SLICE));
        }
    }

    log(
        paths,
        Level::Info,
        "stop",
        "controller stopped; the fan keeps its last output".to_string(),
    );
    // The lock is dropped here. We never lower the fan on exit: the kernel
    // cooling maps stay in charge, and forcing 0 would be the unsafe choice.
    result
}

/// `h5000m-fancontrol release`: undo a legacy governor takeover and stop.
pub fn release(paths: &Paths) -> Result<Option<String>> {
    Ok(safety::restore_legacy_policy(paths))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;
    use crate::platform;

    fn temp_paths(label: &str) -> Paths {
        let root = std::env::temp_dir().join(format!(
            "h5fan-daemon-{label}-{}-{}",
            std::process::id(),
            platform::wall_now()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Paths::sandbox(root)
    }

    #[test]
    fn wake_marker_is_consumed_once() {
        let paths = temp_paths("wake");
        let mut handled = None;
        assert!(!wake_is_pending(&paths, &mut handled));
        request_wake(&paths).unwrap();
        assert!(wake_is_pending(&paths, &mut handled));
        assert!(
            !wake_is_pending(&paths, &mut handled),
            "a consumed wake must not fire twice"
        );
        std::fs::remove_dir_all(paths.sysfs.parent().unwrap()).ok();
    }

    #[test]
    fn file_stamp_detects_a_rewrite() {
        let paths = temp_paths("stamp");
        std::fs::create_dir_all(paths.uci_config.parent().unwrap()).unwrap();
        std::fs::write(&paths.uci_config, "config settings 'settings'\n").unwrap();
        let mut stamp = FileStamp::default();
        assert!(stamp.observe(&paths.uci_config), "first observation counts");
        assert!(!stamp.observe(&paths.uci_config));
        std::fs::write(
            &paths.uci_config,
            "config settings 'settings'\n\toption mode 'manual'\n",
        )
        .unwrap();
        // The modification time has to differ for the watcher to notice; on file
        // systems with a coarse clock this can legitimately stay equal, which is
        // why the daemon also re-reads the file every cycle.
        let _ = stamp.observe(&paths.uci_config);
        std::fs::remove_dir_all(paths.sysfs.parent().unwrap()).ok();
    }

    #[test]
    fn apply_publishes_a_snapshot_even_without_hardware() {
        let paths = temp_paths("apply");
        std::fs::create_dir_all(&paths.run_dir).unwrap();
        std::fs::create_dir_all(paths.uci_config.parent().unwrap()).unwrap();
        std::fs::write(
            &paths.uci_config,
            "config settings 'settings'\n\toption mode 'auto'\n",
        )
        .unwrap();
        match apply(&paths) {
            Ok(ApplyOutcome::Applied(snapshot)) => {
                assert_eq!(snapshot.result, ControlResult::Error);
                assert_eq!(snapshot.reason, "pwm-missing");
                assert!(snapshot.last_error.is_some());
                let read = Snapshot::read(&paths).expect("the snapshot is on disk");
                assert_eq!(read.reason, "pwm-missing");
                assert_eq!(read.cycle_count, 1);
            }
            other => panic!("apply without a daemon must run the cycle, got {other:?}"),
        }
        assert!(
            !paths.lock_dir.exists(),
            "apply must not leave the lock behind"
        );
    }

    #[test]
    fn apply_defers_to_a_live_daemon() {
        let paths = temp_paths("defer");
        std::fs::create_dir_all(&paths.run_dir).unwrap();
        std::fs::create_dir_all(&paths.lock_dir).unwrap();
        // Pretend another live controller owns the lock: our own pid with a
        // matching cmdline.
        std::fs::write(
            paths.lock_dir.join("pid"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        let proc_dir = paths.proc_root.join(std::process::id().to_string());
        std::fs::create_dir_all(&proc_dir).unwrap();
        std::fs::write(
            proc_dir.join("cmdline"),
            b"/usr/sbin/h5000m-fancontrol\0daemon\0",
        )
        .unwrap();
        match apply(&paths) {
            Ok(ApplyOutcome::Queued { pid }) => assert_eq!(pid, std::process::id()),
            other => panic!("a live holder must be deferred to, got {other:?}"),
        }
        assert!(paths.wake_file().exists(), "the daemon was nudged");
    }

    #[test]
    fn tracker_logs_transitions_only_once() {
        let paths = temp_paths("tracker");
        std::fs::create_dir_all(&paths.run_dir).unwrap();
        let mut tracker = CycleTracker::new();
        let mut snapshot = Snapshot::published_now(1000);
        snapshot.result = ControlResult::Ok;
        snapshot.reason = "auto".to_string();
        tracker.record(&paths, &snapshot, 1000);
        tracker.record(&paths, &snapshot, 1001);
        assert_eq!(tracker.last_reason.as_deref(), Some("auto"));
        snapshot.reason = "auto-down-delay".to_string();
        tracker.record(&paths, &snapshot, 1002);
        assert_eq!(tracker.last_reason.as_deref(), Some("auto-down-delay"));
    }
}
