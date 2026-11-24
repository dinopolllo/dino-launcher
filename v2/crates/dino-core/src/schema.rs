//! Lo que Dino 2.0 guarda en disco.
//!
//! ```text
//! <datos>/                      %APPDATA%\gg.dinolab.launcher en Windows (el de siempre)
//!   config.json                 preferencias
//!   library.json                biblioteca (sucesora de packs.json)
//!   accounts.json               cuentas SIN secretos
//!   cache/{libraries,assets,versions}/   compartido entre instancias, por hash
//!   java/<componente>/
//!   instances/<id>/instance.json + minecraft/
//!   packs/<id>.mrpack, packs/<id>.png
//!   logs/
//! ```
//!
//! Reglas comunes a todos los archivos:
//! - llevan `schema` (entero). Un `schema` **mayor** que el que entiende esta versión se
//!   lee igual con los campos conocidos y **no se sobrescribe**: un downgrade no puede
//!   destruir datos de una versión más nueva;
//! - se escriben con [`write_atomic`]: temporal + renombrado, nunca a medias;
//! - un archivo ilegible es un error, no un archivo vacío: confundirlos borraría la
//!   biblioteca buena en el siguiente guardado (regla heredada de 1.x).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CONFIG_SCHEMA: u32 = 1;
pub const LIBRARY_SCHEMA: u32 = 1;
pub const INSTANCE_SCHEMA: u32 = 1;
pub const ACCOUNTS_SCHEMA: u32 = 1;

// ---------------------------------------------------------------- rutas

#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn config(&self) -> PathBuf {
        self.root.join("config.json")
    }
    pub fn library(&self) -> PathBuf {
        self.root.join("library.json")
    }
    pub fn accounts(&self) -> PathBuf {
        self.root.join("accounts.json")
    }
    pub fn instance_dir(&self, id: &str) -> PathBuf {
        self.root.join("instances").join(id)
    }
    pub fn instance_manifest(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("instance.json")
    }
    pub fn game_dir(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("minecraft")
    }
    pub fn pack_file(&self, id: &str) -> PathBuf {
        self.root.join("packs").join(format!("{id}.mrpack"))
    }
    pub fn pack_icon(&self, id: &str) -> PathBuf {
        self.root.join("packs").join(format!("{id}.png"))
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    /// Librerías en disposición Maven, compartidas entre instancias.
    pub fn libraries(&self) -> PathBuf {
        self.root.join("cache").join("libraries")
    }
    /// Assets por hash (`objects/`) e índices (`indexes/`), compartidos entre instancias.
    pub fn assets(&self) -> PathBuf {
        self.root.join("cache").join("assets")
    }
    /// Versiones instaladas: JSON fusionado y jar del cliente.
    pub fn versions(&self) -> PathBuf {
        self.root.join("cache").join("versions")
    }
    pub fn version_dir(&self, version_id: &str) -> PathBuf {
        self.versions().join(version_id)
    }
    /// Caché por hash de archivos compartidos entre packs (mods): `<aa>/<sha1>`.
    pub fn shared_files(&self) -> PathBuf {
        self.root.join("cache").join("files")
    }
    /// Runtimes de Java descargados, uno por componente de Mojang.
    pub fn java(&self) -> PathBuf {
        self.root.join("java")
    }
}

// ---------------------------------------------------------------- config.json

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub schema: u32,
    /// Cerrar el WebView mientras se juega (decisión de la etapa 2).
    #[serde(default = "yes")]
    pub close_ui_while_playing: bool,
    /// Descargas simultáneas.
    #[serde(default = "default_concurrency")]
    pub download_concurrency: u32,
    /// Límite de descarga en bytes por segundo. `None`: sin límite.
    #[serde(default)]
    pub download_limit: Option<u64>,
    /// Canal del updater del launcher.
    #[serde(default)]
    pub update_channel: UpdateChannel,
    /// Marca del panel de admin desbloqueado (antes, el archivo `admin`).
    #[serde(default)]
    pub admin_unlocked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Beta,
}

fn yes() -> bool {
    true
}
fn default_concurrency() -> u32 {
    8
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: CONFIG_SCHEMA,
            close_ui_while_playing: true,
            download_concurrency: default_concurrency(),
            download_limit: None,
            update_channel: UpdateChannel::Stable,
            admin_unlocked: false,
        }
    }
}

// ---------------------------------------------------------------- library.json

/// La biblioteca: qué packs tiene el jugador y con cuál juega.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Library {
    pub schema: u32,
    /// Id de la instancia que abre el botón Jugar. Si apunta a una que ya no existe, se
    /// ignora: nunca hace que la biblioteca parezca vacía.
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default)]
    pub instances: Vec<LibraryEntry>,
}

impl Default for Library {
    fn default() -> Self {
        Self { schema: LIBRARY_SCHEMA, selected: None, instances: Vec::new() }
    }
}

/// Una instancia de la biblioteca: lo que se enseña sin abrir su carpeta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryEntry {
    /// Local: nombra la carpeta. Sin separadores ni `..` (ver [`valid_id`]).
    pub id: String,
    pub name: String,
    /// De dónde salió, para buscarle actualizaciones.
    pub source: PackSource,
    /// Versión del pack instalada (`versionId` del `.mrpack`).
    #[serde(default)]
    pub pack_version: String,
    /// Servidor al que entra al pulsar Jugar. Lo actualiza el catálogo.
    #[serde(default)]
    pub server: Option<String>,
    /// Segundos Unix de la última partida, para ordenar.
    #[serde(default)]
    pub last_played: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackSource {
    /// Del catálogo de Dino, por código.
    Catalog { code: String },
    /// Importado de un `.mrpack` local. Sin actualizaciones automáticas.
    File,
}

/// Un id local válido: nombre de carpeta seguro en Windows, macOS y Linux.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
}

// ---------------------------------------------------------------- instance.json

/// Lo que una instancia necesita para lanzarse sin consultar nada por red.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceManifest {
    pub schema: u32,
    pub id: String,
    pub minecraft: String,
    pub loader: LoaderSpec,
    /// Estado de la instalación. Una instancia `Incomplete` se repara al pulsar Jugar.
    pub state: InstallState,
    #[serde(default)]
    pub memory: Memory,
    /// Argumentos extra de Java elegidos por el jugador. Se aplican de verdad (en 1.x el
    /// campo existía y `play` mandaba siempre una cadena vacía).
    #[serde(default)]
    pub jvm_args: Vec<String>,
    #[serde(default)]
    pub java: JavaChoice,
    /// Archivos que puso el pack, relativos a `minecraft/`: lo que se puede quitar al
    /// actualizar. Sucesor de `dino-archivos.json`.
    #[serde(default)]
    pub managed_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoaderSpec {
    Vanilla,
    Fabric { version: String },
    NeoForge { version: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallState {
    Ready,
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Memory {
    /// La que toque según la RAM de la máquina (se recalcula si la máquina cambia).
    #[default]
    Auto,
    Fixed {
        max_mb: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum JavaChoice {
    /// El runtime de Mojang para esta versión, descargado por Dino.
    #[default]
    Auto,
    /// Un Java elegido por el jugador.
    Path { path: PathBuf },
}

// ---------------------------------------------------------------- accounts.json

/// Cuentas de Dino. **Sin secretos**: los tokens de Microsoft los guarda el motor (puente)
/// y, el día que haya login propio, el Credential Manager / Keychain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountsFile {
    pub schema: u32,
    /// UUID de la cuenta activa.
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// Cuentas Microsoft que el jugador quitó en Dino pero siguen en el motor (Dino no
    /// escribe el `accounts.json` del motor). Así la sincronización no las devuelve; vuelven
    /// si el jugador inicia sesión otra vez con ellas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_bridge: Vec<String>,
}

impl Default for AccountsFile {
    fn default() -> Self {
        Self { schema: ACCOUNTS_SCHEMA, active: None, accounts: Vec::new(), hidden_bridge: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub kind: AccountKind,
    pub name: String,
    /// 32 hex sin guiones. En offline, `UUID.nameUUIDFromBytes("OfflinePlayer:<nombre>")`.
    pub uuid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Offline,
    /// Microsoft a través del motor: la sesión vive en el `accounts.json` del motor.
    MicrosoftBridge,
}

// ---------------------------------------------------------------- lectura y escritura

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("no pude leer {0}: {1}")]
    Read(PathBuf, std::io::Error),
    #[error("{0} está dañado: {1}")]
    Corrupt(PathBuf, serde_json::Error),
    #[error("no pude escribir {0}: {1}")]
    Write(PathBuf, std::io::Error),
    #[error("{0} es de una versión más nueva de Dino (esquema {1}); no lo sobrescribo")]
    Newer(PathBuf, u32),
}

/// Lee un archivo con esquema. `Ok(None)` si no existe.
pub fn load<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, StoreError> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(strip_bom(&b)).map(Some).map_err(|e| StoreError::Corrupt(path.into(), e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::Read(path.into(), e)),
    }
}

/// El Bloc de notas y PowerShell 5 guardan UTF-8 con BOM; un archivo retocado a mano
/// no debe darse por dañado por eso.
fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b)
}

/// Guarda con esquema, salvo que el archivo en disco sea de una versión más nueva.
pub fn save<T: Serialize>(path: &Path, value: &T, schema: u32) -> Result<(), StoreError> {
    if let Ok(b) = std::fs::read(path) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(strip_bom(&b)) {
            if let Some(s) = v.get("schema").and_then(|s| s.as_u64()) {
                if s as u32 > schema {
                    return Err(StoreError::Newer(path.into(), s as u32));
                }
            }
        }
    }
    let bytes = serde_json::to_vec_pretty(value).expect("los esquemas siempre serializan");
    write_atomic(path, &bytes).map_err(|e| StoreError::Write(path.into(), e))
}

/// Escribe a un temporal junto al destino y lo renombra encima: o queda el archivo viejo
/// entero o el nuevo entero, nunca uno truncado (en 1.x `packs.json`, `instance.cfg` y
/// `accounts.json` se escribían directamente).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dino-core-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ids_seguros() {
        assert!(valid_id("voidsafio"));
        assert!(valid_id("fabulously-optimized-14-0-0"));
        assert!(!valid_id("../x"));
        assert!(!valid_id("Void Safio"));
        assert!(!valid_id(""));
    }

    #[test]
    fn un_json_con_bom_no_esta_danado() {
        let d = tmp("bom");
        let p = d.join("config.json");
        std::fs::write(&p, b"\xEF\xBB\xBF{\"schema\": 1}").unwrap();
        let c: Config = load(&p).unwrap().unwrap();
        assert_eq!(c.schema, 1);
    }

    #[test]
    fn guardar_y_leer() {
        let d = tmp("rt");
        let p = d.join("library.json");
        let lib = Library {
            schema: LIBRARY_SCHEMA,
            selected: Some("voidsafio".into()),
            instances: vec![LibraryEntry {
                id: "voidsafio".into(),
                name: "VoidSafio".into(),
                source: PackSource::Catalog { code: "VOIDSAFIO".into() },
                pack_version: "3.15.0".into(),
                server: Some("voidsafio.minecra.fr".into()),
                last_played: None,
            }],
        };
        save(&p, &lib, LIBRARY_SCHEMA).unwrap();
        assert_eq!(load::<Library>(&p).unwrap().unwrap(), lib);
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["instances"][0]["source"]["kind"], "catalog");
    }

    #[test]
    fn no_existe_no_es_lo_mismo_que_roto() {
        let d = tmp("roto");
        assert!(load::<Library>(&d.join("no.json")).unwrap().is_none());
        std::fs::write(d.join("roto.json"), b"{ esto no").unwrap();
        assert!(matches!(load::<Library>(&d.join("roto.json")), Err(StoreError::Corrupt(..))));
    }

    #[test]
    fn un_archivo_de_una_version_mas_nueva_no_se_pisa() {
        let d = tmp("nuevo");
        let p = d.join("config.json");
        std::fs::write(&p, br#"{"schema": 9, "algo_futuro": true}"#).unwrap();
        // se puede leer con lo que se entiende…
        let c: Config = load(&p).unwrap().unwrap();
        assert_eq!(c.schema, 9);
        // …pero no sobrescribir
        assert!(matches!(save(&p, &Config::default(), CONFIG_SCHEMA), Err(StoreError::Newer(_, 9))));
        assert!(std::fs::read_to_string(&p).unwrap().contains("algo_futuro"));
    }

    #[test]
    fn la_escritura_no_deja_temporales() {
        let d = tmp("atom");
        write_atomic(&d.join("a.json"), b"{}").unwrap();
        let n = std::fs::read_dir(&d).unwrap().count();
        assert_eq!(n, 1);
    }

    #[test]
    fn la_memoria_y_el_java_por_defecto_son_automaticos() {
        let m: InstanceManifest = serde_json::from_str(
            r#"{"schema":1,"id":"x","minecraft":"1.21.1","loader":{"kind":"vanilla"},"state":"ready"}"#,
        )
        .unwrap();
        assert_eq!(m.memory, Memory::Auto);
        assert_eq!(m.java, JavaChoice::Auto);
        assert!(m.jvm_args.is_empty());
    }
}
