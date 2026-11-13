//! The binary entry: thin, so the logic stays testable in the lib.
//! Hides the extra console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    uva_desktop::run()
}
