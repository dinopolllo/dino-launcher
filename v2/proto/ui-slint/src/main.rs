// Ventana de medición: misma estructura que la pantalla Jugar de Dino 0.6.2.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let app = App::new()?;
    app.set_pack_name("VoidSafio".into());
    app.set_pack_meta("NeoForge 21.1.251 · Minecraft 1.21.1 · 132 mods".into());
    app.set_server("mc.dinolab.gg · 12/40 jugadores".into());
    let weak = app.as_weak();
    app.on_play(move || {
        if let Some(a) = weak.upgrade() {
            a.set_progress(0.35);
            a.set_status("Descargando assets · 1.384/3.890".into());
        }
    });
    app.run()
}
