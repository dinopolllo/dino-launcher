// Sin consola en la versión publicada.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    dino_app_lib::run()
}
