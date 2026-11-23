//! Lo que la interfaz pide y lo que recibe: las formas de datos de los comandos.
//!
//! Cada comando devuelve `Result<T, DinoError>`. La lista de comandos, con su equivalente
//! en 1.x, está en `v2/docs/contratos.md`.

use crate::schema::{JavaChoice, LoaderSpec, Memory, PackSource};
use serde::{Deserialize, Serialize};

/// Una ficha de la biblioteca, lista para pintar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceView {
    pub id: String,
    pub name: String,
    pub source: PackSource,
    pub pack_version: String,
    pub minecraft: Option<String>,
    pub loader: Option<LoaderSpec>,
    pub server: Option<String>,
    /// Ruta del icono (se sirve con el protocolo de assets de Tauri), si hay.
    pub icon: Option<String>,
    pub selected: bool,
    /// Qué hace el botón principal si se pulsa ahora.
    pub action: PrimaryAction,
}

/// Lo que dice el botón grande. Sustituye a `pack_state` de 1.x.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrimaryAction {
    Play,
    /// Falta instalar; `bytes` es lo que se va a bajar (0 si no se sabe).
    Install { bytes: u64 },
    /// Hay una versión nueva del pack en el catálogo.
    Update { to_version: String, bytes: u64 },
    /// Ya hay una partida de esta instancia abierta.
    Playing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayRequest {
    pub instance: String,
    /// Si es `false`, solo instala/repara y no abre el juego (la vía de Importar).
    #[serde(default = "yes")]
    pub launch: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlayOutcome {
    /// El juego está arrancando; los eventos `Session` dirán cuándo se abre.
    Launched,
    /// Instalado sin abrir el juego (`launch: false`).
    Installed,
    /// No hay cuenta: la interfaz abre el setup.
    NeedsAccount,
}

/// Ajustes de una instancia (pantalla Ajustes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceSettings {
    pub memory: Memory,
    pub jvm_args: Vec<String>,
    pub java: JavaChoice,
    /// RAM física de la máquina y la recomendada, en MB, para el slider.
    pub machine_ram_mb: u32,
    pub recommended_max_mb: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountView {
    pub kind: crate::schema::AccountKind,
    pub name: String,
    pub uuid: String,
    pub active: bool,
    /// Solo Microsoft: minutos que le quedan a la sesión, si se sabe.
    pub session_minutes_left: Option<u64>,
}

/// Lo que se lee de un `.mrpack` antes de importarlo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackPreview {
    pub name: String,
    pub version: String,
    pub minecraft: String,
    pub loader: LoaderSpec,
    pub mods: u32,
    pub bytes: u64,
    /// `false` si el loader está fuera de alcance (Forge, Quilt): la interfaz lo explica.
    pub supported: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerStatus {
    pub online: bool,
    pub motd: String,
    pub players_online: u32,
    pub players_max: u32,
    pub version: Option<String>,
    pub ping_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub version: String,
    pub data_dir: String,
    /// Si el vidrio nativo de la ventana está disponible (antes `window_glass`).
    pub glass: bool,
    pub platform: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_boton_principal_en_json() {
        let a = PrimaryAction::Update { to_version: "3.16.0".into(), bytes: 1024 };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["kind"], "update");
        assert_eq!(v["to_version"], "3.16.0");
        let r: PlayRequest = serde_json::from_str(r#"{"instance":"voidsafio"}"#).unwrap();
        assert!(r.launch, "por defecto Jugar abre el juego");
    }
}
