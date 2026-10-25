//! CLI entry point. Commands and semantics match the shell controller:
//!
//! * `apply` (default) - run exactly one control cycle under the lock
//! * `status` - print current status (`--json` for the structured interface,
//!   default keeps the legacy `key=value` output for external scripts)
//! * `daemon` - foreground control loop for procd
//! * `release` - restore a legacy thermal policy takeover
//! * `version`, `help` - diagnostics

use h5000m_fancontrol::config::Config;
use h5000m_fancontrol::controller::Controller;
use h5000m_fancontrol::paths::Paths;
use h5000m_fancontrol::sensor::IwprivVendor;
use h5000m_fancontrol::service::{self, Lock};
use h5000m_fancontrol::status;
use h5000m_fancontrol::thermal;
use h5000m_fancontrol::uci::UciShowSource;

const USAGE: &str = "Usage: h5000m-fancontrol {apply|status [--json]|daemon|release|version}";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("apply");
    let json_output = args.iter().any(|a| a == "--json" || a == "json");
    let paths = Paths::from_env();

    match command {
        "apply" => {
            let Some(mut lock) = Lock::acquire(&paths) else {
                eprintln!("fan control is busy");
                std::process::exit(1);
            };
            let mut uci_source = UciShowSource::new();
            let cfg = Config::load(&mut uci_source);
            let mut vendor = IwprivVendor::new(paths.radio_root.clone());
            let controller = Controller::new(&paths);
            match controller.apply_once(&cfg, &mut vendor, service::now_epoch(), &|secs| {
                std::thread::sleep(std::time::Duration::from_secs(secs));
            }) {
                Ok(()) => {}
                Err(message) => {
                    eprintln!("{message}");
                    lock.release();
                    std::process::exit(1);
                }
            }
        }
        "status" => {
            let mut uci_source = UciShowSource::new();
            let cfg = Config::load(&mut uci_source);
            let mut vendor = IwprivVendor::new(paths.radio_root.clone());
            let now = service::now_epoch();
            if json_output {
                println!("{}", status::json_status(&paths, &cfg, &mut vendor, now));
            } else {
                let model = status::build_live(&paths, &cfg, &mut vendor, now);
                print!("{}", model.to_legacy_text());
            }
        }
        "daemon" => {
            std::process::exit(service::run_daemon(&paths));
        }
        "release" => {
            thermal::release_thermal_control(&paths);
        }
        "version" => {
            println!("h5000m-fancontrol {}", h5000m_fancontrol::VERSION);
        }
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(1);
        }
    }
}
