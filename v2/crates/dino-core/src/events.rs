//! Lo que el núcleo le cuenta a la interfaz sin que ella pregunte.
//!
//! En 1.x la interfaz preguntaba `session_state` cada segundo y el progreso llegaba como
//! `{paso, frac}` con texto ya redactado. En 2.0 llegan eventos tipados por un único
//! canal (`dino://event`), y el texto lo pone la interfaz.

use serde::{Deserialize, Serialize};

/// Nombre del canal de eventos de Tauri.
pub const CHANNEL: &str = "dino://event";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Avance de una tarea larga (instalar, actualizar, verificar).
    Progress(Progress),
    /// Cambio de estado de la partida. Sustituye al sondeo de `session_state`.
    Session(Session),
    /// La biblioteca cambió (pack añadido, quitado, actualizado, seleccionado).
    LibraryChanged,
    /// Cambiaron las cuentas o la activa.
    AccountsChanged,
    /// Llegó un link de invitación (`dinolauncher://join/CODIGO`).
    Invite { code: String },
    /// Hay una versión nueva del launcher.
    UpdateAvailable { version: String, notes: Option<String> },
}

/// Avance de una tarea.
///
/// Dos niveles, para que **la barra no retroceda nunca**:
/// - `task_bytes_done / task_bytes_total`: el total de la tarea entera, monótono. Es lo
///   que pinta la barra, aunque varias fases corran a la vez;
/// - `phase` + `files_*` / `bytes_*`: la fase que acaba de avanzar, para el texto
///   («Descargando mods · 3 de 120»).
///
/// Un total que vale **0 significa «no se sabe»**: barra indeterminada. `launching` y
/// `renewing_session` nunca traen cifras.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    /// Identifica la tarea, para que dos barras no se mezclen.
    pub task: String,
    pub instance: Option<String>,
    pub task_bytes_done: u64,
    pub task_bytes_total: u64,
    pub phase: Phase,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// `true` al terminar la fase. Una tarea termina con un `Progress` en `Done`.
    pub finished: bool,
}

/// Fases en el orden en que suelen ocurrir. Varias pueden ir a la vez (assets, librerías
/// y Java se descargan en paralelo): la interfaz puede sumar o enseñar la más lenta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    CheckingUpdates,
    DownloadingPack,
    ReadingVersions,
    DownloadingLibraries,
    DownloadingAssets,
    InstallingJava,
    PreparingLoader,
    DownloadingMods,
    /// Comprobar hashes de lo instalado (`instance_verify`). No es descargar.
    VerifyingFiles,
    RenewingSession,
    Launching,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub state: SessionState,
    pub instance: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Idle,
    Preparing,
    /// La ventana del juego ya está abierta.
    Playing,
    /// Cerrando: se espera hasta 30 s a que guarde el mundo antes de forzar.
    Stopping,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forma_en_json() {
        let e = Event::Session(Session { state: SessionState::Playing, instance: Some("voidsafio".into()) });
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "session");
        assert_eq!(v["state"], "playing");
        let v = serde_json::to_value(Event::Invite { code: "VOIDSAFIO".into() }).unwrap();
        assert_eq!((v["type"].as_str(), v["code"].as_str()), (Some("invite"), Some("VOIDSAFIO")));
        assert_eq!(serde_json::to_value(Event::LibraryChanged).unwrap()["type"], "library_changed");
    }

    #[test]
    fn la_barra_lee_el_total_de_la_tarea_y_la_fase_queda_para_el_texto() {
        let p = Progress {
            task: "t".into(),
            instance: None,
            task_bytes_done: 500,
            task_bytes_total: 1000,
            phase: Phase::VerifyingFiles,
            files_done: 3,
            files_total: 120,
            bytes_done: 0,
            bytes_total: 0,
            finished: false,
        };
        let v = serde_json::to_value(Event::Progress(p)).unwrap();
        assert_eq!(v["phase"], "verifying_files");
        assert_eq!((v["task_bytes_done"].as_u64(), v["task_bytes_total"].as_u64()), (Some(500), Some(1000)));
    }
}
