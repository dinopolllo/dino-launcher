// Windows: no console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    dino_launcher_lib::run()
}
