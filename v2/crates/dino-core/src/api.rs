//! Lo que la interfaz pide y lo que recibe: las formas de datos de los comandos.
//!
//! Cada comando devuelve `Result<T, DinoError>`. La lista de comandos, con su equivalente
//! en 1.x, está en `v2/docs/contratos.md`. Revisado con la sesión de diseño el 2026-11-23
//! cruzando estos tipos con lo que lee de verdad `src/index.html`.
//!
//! Convenciones para la interfaz:
//! - en los datos de los comandos, un tamaño que **no se sabe es `None`** (nunca 0). En los
//!   eventos `progress`, un total de 0 significa lo mismo;
//! - `loader` llega como objeto (`{kind, version}`), no como texto: la interfaz lo redacta;
//! - `icon` es una URL lista para `<img src>`, servida por un protocolo propio de `dino-app`
//!   y construida con [`icon_url`] según la plataforma.
//!   La interfaz no llama a `convertFileSrc` ni depende del `scope` del protocolo de assets
//!   (con un scope mal puesto la imagen fallaba sin error y salía el dino genérico).

use crate::pack_meta::{PatchNotes, Theme};
use crate::profile::{LauncherPrefs, ResolvedPrefs, Source, WindowPref};
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
    pub icon: Option<String>,
    /// Mods del pack (la línea de datos: «… · 44 mods · …»).
    pub mods: u32,
    /// Lo que ocupa la instancia en disco. `None` si no está instalada o no se ha medido.
    pub disk_bytes: Option<u64>,
    pub selected: bool,
    /// Qué hace el botón principal si se pulsa ahora.
    pub action: PrimaryAction,
    /// El tema que publicó el pack (ya saneado), si lo activó. La interfaz decide si lo
    /// aplica según la preferencia del jugador.
    #[serde(default)]
    pub theme: Option<Theme>,
    /// Notas de parche: las de la versión nueva si hay actualización (`action.kind ==
    /// "update"`) y, si no, las de la instalada. `None` si el pack no publicó ninguna.
    #[serde(default)]
    pub notes: Option<PatchNotes>,
    /// A qué versión del pack pertenecen esas notas.
    #[serde(default)]
    pub notes_version: Option<String>,
}

/// Lo que dice el botón grande. Sustituye a `pack_state` de 1.x.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrimaryAction {
    Play,
    /// Falta instalar; `bytes` es lo que se va a **descargar** (`None` si no se sabe).
    Install {
        bytes: Option<u64>,
    },
    /// Hay una versión nueva del pack en el catálogo; `bytes` es lo que se va a descargar.
    Update {
        to_version: String,
        bytes: Option<u64>,
    },
    /// Ya hay una partida de esta instancia abierta.
    Playing,
}

/// Detalle de una instancia instalada: Ajustes («En disco: …») y el diálogo de quitar,
/// que dice qué se pierde antes de un borrado que no se deshace. Sustituye a `instance_info`.
///
/// Con una instancia que no está instalada, `instance_details` devuelve el error
/// `not_installed`. La interfaz no necesita llamarlo en ese caso: `InstanceView.disk_bytes`
/// es `None` y no hay archivos que borrar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceDetails {
    pub id: String,
    /// Carpeta del juego (`instances/<id>/minecraft`).
    pub path: String,
    pub minecraft: String,
    pub loader: LoaderSpec,
    pub mods: u32,
    /// Mundos en `saves/`.
    pub worlds: u32,
    pub disk_bytes: u64,
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
    /// Lo que vale ahora para el jugador activo: lo suyo si lo eligió y, si no, lo del pack.
    pub memory: Memory,
    pub jvm_args: Vec<String>,
    pub java: JavaChoice,
    /// Ventana del juego (`{mode: "default"}` si nadie pidió otra cosa).
    pub window: WindowPref,
    /// De quién es cada valor: del perfil del jugador o del pack.
    pub memory_from: Source,
    pub jvm_args_from: Source,
    pub java_from: Source,
    pub window_from: Source,
    /// Nombre del jugador cuyo perfil se está editando; `None` si no hay cuenta (entonces
    /// se edita el pack mismo, como en 1.x).
    pub profile: Option<String>,
    /// Javas que el selector puede ofrecer además de «Automático» (sucesor de `java_runtimes`).
    pub available_java: Vec<JavaOption>,
    /// RAM física de la máquina y la recomendada, en MB, para el slider.
    pub machine_ram_mb: u32,
    pub recommended_max_mb: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JavaOption {
    pub path: String,
    /// Versión mayor (21, 17…), si se pudo leer.
    pub major: Option<u32>,
    pub origin: JavaOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JavaOrigin {
    /// Runtime de Mojang descargado por Dino.
    Mojang,
    /// Java instalado en la máquina.
    System,
}

/// Qué cambiar de una instancia para el jugador activo. Cada campo es un parche: `None` deja
/// como está lo que había.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InstanceSettingsPatch {
    #[serde(default)]
    pub memory: Option<Memory>,
    #[serde(default)]
    pub jvm_args: Option<Vec<String>>,
    #[serde(default)]
    pub java: Option<JavaChoice>,
    #[serde(default)]
    pub window: Option<WindowPref>,
}

/// El perfil del jugador activo: sus preferencias del launcher, tal como las eligió y ya
/// resueltas con los valores de fábrica.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileView {
    /// `None` si no hay ninguna cuenta: entonces todo son valores de fábrica.
    pub uuid: Option<String>,
    pub name: Option<String>,
    /// Lo que eligió (`null` = no ha tocado ese ajuste).
    pub raw: LauncherPrefs,
    /// Lo que vale de verdad.
    pub prefs: ResolvedPrefs,
    /// Ventana del juego para todas sus instancias.
    pub window: Option<WindowPref>,
}

/// Esquema del protocolo propio que sirve los iconos.
pub const ICON_SCHEME: &str = "dino-icon";

/// URL de un icono, lista para `<img src>`.
///
/// Tauri 2 no sirve un protocolo propio igual en todas partes: en WebView2 (Windows, y
/// también Android) es `http://<esquema>.localhost/…`; en macOS y Linux,
/// `<esquema>://localhost/…`. Con la forma equivocada el `<img>` falla sin error y sale el
/// icono genérico. Comprobado en esta máquina: el IPC de Tauri va a `http://ipc.localhost/…`.
pub fn icon_url(instance_id: &str) -> String {
    icon_url_for(cfg!(any(windows, target_os = "android")), instance_id)
}

/// URL de la skin de un nombre, servida por el mismo protocolo que los iconos.
pub fn skin_url(name: &str) -> String {
    icon_url(&format!("skin/{name}"))
}

fn icon_url_for(webview2_style: bool, id: &str) -> String {
    if webview2_style {
        format!("http://{ICON_SCHEME}.localhost/{id}")
    } else {
        format!("{ICON_SCHEME}://localhost/{id}")
    }
}

/// Respuesta de `account_microsoft_add`: qué se le está enseñando al jugador.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicrosoftLoginView {
    /// Solo la ventana «Add Microsoft Account» del motor. `false`: el motor entero (otra
    /// plataforma, o no se pudo llegar a esa ventana), y el jugador va a Cuentas a mano.
    pub only_microsoft_window: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountView {
    pub kind: crate::schema::AccountKind,
    pub name: String,
    pub uuid: String,
    pub active: bool,
    /// Solo Microsoft: minutos que le quedan a la sesión, si se sabe.
    pub session_minutes_left: Option<u64>,
    /// URL del PNG de la skin de ese nombre (ver [`skin_url`]). Responde 404 si el nombre
    /// no tiene skin propia: la interfaz pinta entonces su cabeza genérica.
    pub skin: String,
}

/// Lo que se sabe de un pack antes de instalarlo. Lo devuelven `catalog_lookup` (por código
/// o link) y `mrpack_inspect` (archivo local).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackPreview {
    /// Código normalizado del catálogo: lo que luego recibe `catalog_add`, aunque se haya
    /// buscado con un link. `None` para un archivo local.
    pub code: Option<String>,
    pub name: String,
    pub version: String,
    pub minecraft: String,
    pub loader: LoaderSpec,
    pub mods: u32,
    pub icon: Option<String>,
    /// Servidor al que se entra directo («entras directo a X»).
    pub server: Option<String>,
    /// Peso del archivo `.mrpack`. **No** es lo que se va a descargar: los mods se bajan
    /// aparte (en septiembre esto confundió un pack de KB con uno de cientos de MB).
    pub pack_bytes: u64,
    /// Total a descargar (pack + mods), si se conoce. `None` = no se sabe todavía.
    pub download_bytes: Option<u64>,
    /// `false` si el loader está fuera de alcance (Forge, Quilt): la interfaz lo explica.
    pub supported: bool,
}

/// Estado de un servidor. Los nombres de los jugadores son los que ya lee la interfaz de 1.x.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerStatus {
    pub online: bool,
    /// `None` cuando no se sabe (servidor apagado o que no lo dice).
    pub motd: Option<String>,
    pub players: Option<u32>,
    pub max_players: Option<u32>,
    pub version: Option<String>,
    pub ping_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub version: String,
    pub data_dir: String,
    /// Si el vidrio nativo de la ventana está disponible (antes `window_glass`).
    pub glass: bool,
    pub platform: String,
    /// El estado de la partida al cargar: si la página se recarga a mitad de una
    /// instalación, sabe que está `preparing` sin tener que sondear.
    pub session: crate::events::Session,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_boton_principal_en_json() {
        let a = PrimaryAction::Update { to_version: "3.16.0".into(), bytes: Some(1024) };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["kind"], "update");
        assert_eq!(v["to_version"], "3.16.0");
        let r: PlayRequest = serde_json::from_str(r#"{"instance":"voidsafio"}"#).unwrap();
        assert!(r.launch, "por defecto Jugar abre el juego");
    }

    #[test]
    fn no_se_sabe_es_null_en_todas_partes() {
        let v = serde_json::to_value(PrimaryAction::Install { bytes: None }).unwrap();
        assert!(v["bytes"].is_null());
    }

    #[test]
    fn la_url_del_icono_depende_de_la_plataforma() {
        assert_eq!(icon_url_for(true, "voidsafio"), "http://dino-icon.localhost/voidsafio");
        assert_eq!(icon_url_for(false, "voidsafio"), "dino-icon://localhost/voidsafio");
        let aqui = icon_url("x");
        if cfg!(windows) {
            assert!(aqui.starts_with("http://dino-icon.localhost/"));
        } else {
            assert!(aqui.starts_with("dino-icon://localhost/"));
        }
    }

    #[test]
    fn el_origen_de_java_es_un_conjunto_cerrado() {
        assert_eq!(serde_json::to_value(JavaOrigin::Mojang).unwrap(), "mojang");
        assert!(serde_json::from_str::<JavaOrigin>("\"Mojang\"").is_err());
    }

    #[test]
    fn el_estado_del_servidor_usa_los_nombres_que_lee_la_interfaz() {
        let s = ServerStatus {
            online: true,
            motd: Some("x".into()),
            players: Some(3),
            max_players: Some(20),
            version: None,
            ping_ms: Some(40),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!((v["players"].as_u64(), v["max_players"].as_u64()), (Some(3), Some(20)));
    }

    #[test]
    fn la_vista_previa_separa_el_archivo_de_la_descarga() {
        let p = PackPreview {
            code: Some("VOIDSAFIO".into()),
            name: "VoidSafio".into(),
            version: "3.15.0".into(),
            minecraft: "1.21.1".into(),
            loader: LoaderSpec::NeoForge { version: "21.1.251".into() },
            mods: 132,
            icon: None,
            server: Some("voidsafio.minecra.fr".into()),
            pack_bytes: 296_512_611,
            download_bytes: None,
            supported: true,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("bytes").is_none(), "nada de un `bytes` ambiguo");
        assert_eq!(v["loader"]["kind"], "neo_forge");
        assert!(v["download_bytes"].is_null());
    }
}
