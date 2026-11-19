//! podbar: a Bluetooth tray icon (StatusNotifierItem) for Wayland bars such
//! as waybar, built on BlueZ over D-Bus. Paired devices, pairing, power, and
//! for AirPods battery, noise control, conversational awareness and ear
//! detection through Apple's AAP protocol.

mod aap;
mod agent;
mod app;
mod bluez;
mod ctl;
mod media;
mod notify;
mod text;
mod tray;

use std::process::ExitCode;

const USAGE: &str = "\
usage: podbar                 run the tray (normally started by the compositor)
       podbar status          AirPods state of the running tray
       podbar noise MODE      off | transparency | adaptive | anc
       podbar ca on|off       conversational awareness
       podbar ear on|off      ear detection (pause when a bud is removed)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("podbar: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match args.first().map(String::as_str) {
        None | Some("daemon") => rt.block_on(app::run()),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(_) => rt.block_on(ctl::client(&args)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("podbar: {e}");
            ExitCode::FAILURE
        }
    }
}
