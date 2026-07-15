// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(error) = takefive_desktop_lib::run() {
        eprintln!("TakeFive failed to start: {error}");
        std::process::exit(1);
    }
}
