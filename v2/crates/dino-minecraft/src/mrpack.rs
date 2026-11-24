//! Instalar un `.mrpack` en una instancia.
//!
//! Reglas heredadas de 1.x (`src-tauri/src/pack.rs`): los archivos *seed* son del jugador
//! y solo se escriben la primera vez, y ninguna ruta del zip puede salirse de la instancia.
//! Lo nuevo: los mods se bajan **en paralelo**, en streaming y con reintentos, y el `.mrpack`
//! no se carga entero en memoria.

use crate::install::Loader;
use crate::{io_err, Error, Report, Result};
use dino_download::{Downloader, Item, Stats};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Index {
    pub name: String,
    #[serde(rename = "versionId")]
    pub version_id: String,
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub files: Vec<IndexFile>,
}

#[derive(Debug, Deserialize)]
pub struct IndexFile {
    pub path: String,
    pub hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub downloads: Vec<String>,
    #[serde(rename = "fileSize", default)]
    pub file_size: u64,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
}

impl Index {
    pub fn minecraft(&self) -> Result<&str> {
        self.dependencies
            .get("minecraft")
            .map(String::as_str)
            .ok_or_else(|| Error::Other("el pack no dice su versión de Minecraft".into()))
    }

    pub fn loader(&self) -> Result<Loader> {
        let d = &self.dependencies;
        if let Some(v) = d.get("neoforge") {
            Ok(Loader::NeoForge { version: v.clone() })
        } else if let Some(v) = d.get("fabric-loader") {
            Ok(Loader::Fabric { version: v.clone() })
        } else if d.contains_key("forge") || d.contains_key("quilt-loader") {
            Err(Error::Other("Forge y Quilt están fuera del alcance de 2.0 (ningún pack los usa)".into()))
        } else {
            Ok(Loader::Vanilla)
        }
    }
}

pub fn read_index(mrpack: &Path) -> Result<Index> {
    let f = std::fs::File::open(mrpack).map_err(io_err(mrpack))?;
    let mut z = zip::ZipArchive::new(f)?;
    let mut s = String::new();
    z.by_name("modrinth.index.json")?.read_to_string(&mut s).map_err(io_err(mrpack))?;
    Ok(serde_json::from_str(&s)?)
}

pub fn is_seed(rel: &str) -> bool {
    const FILES: [&str; 4] = ["options.txt", "servers.dat", "optionsof.txt", "optionsshaders.txt"];
    const DIRS: [&str; 4] = ["saves/", "screenshots/", "logs/", "crash-reports/"];
    FILES.contains(&rel) || DIRS.iter().any(|d| rel.starts_with(d))
}

pub fn safe_relative(rel: &str) -> Result<String> {
    let p = rel.replace('\\', "/");
    let bad = p.is_empty()
        || p.starts_with('/')
        || p.split('/').any(|s| s == ".." || s == ".")
        || p.contains('\0')
        || p.chars().nth(1) == Some(':');
    if bad {
        return Err(Error::Other(format!("ruta insegura en el pack: {rel}")));
    }
    Ok(p)
}

/// Resultado de instalar un `.mrpack`.
pub struct Installed {
    pub index: Index,
    pub stats: Stats,
    /// Lo que puso esta versión, relativo a `game_dir`: va a `instance.json` como
    /// `managed_files` para saber qué sobra en la próxima actualización.
    pub managed: Vec<String>,
    /// Lo que se quitó por sobrar de la versión anterior.
    pub removed: Vec<String>,
}

/// Copia los overrides, baja los archivos del índice a `game_dir` y quita lo que la
/// versión anterior puso y esta ya no trae.
///
/// `previous`: los `managed_files` de la instalación anterior (vacío si es la primera o
/// si viene de antes de anotarlos).
pub async fn install(
    dl: &Downloader,
    mrpack: &Path,
    game_dir: &Path,
    previous: &[String],
    report: Report<'_>,
) -> Result<Installed> {
    let index = read_index(mrpack)?;
    let mut nuevos: HashSet<String> = HashSet::new();
    std::fs::create_dir_all(game_dir).map_err(io_err(game_dir))?;

    report("Copiando configuración del pack", None);
    {
        let f = std::fs::File::open(mrpack).map_err(io_err(mrpack))?;
        let mut z = zip::ZipArchive::new(f)?;
        for i in 0..z.len() {
            // Copiar los overrides de un pack grande lleva segundos: también se corta al cancelar.
            if dl.cancelled() {
                return Err(Error::Download(dino_download::DownloadError::Cancelled));
            }
            let mut e = z.by_index(i)?;
            if e.is_dir() {
                continue;
            }
            let name = e.name().to_string();
            let Some(rel) = name.strip_prefix("overrides/").or_else(|| name.strip_prefix("client-overrides/")) else {
                continue;
            };
            let rel = safe_relative(rel)?;
            nuevos.insert(rel.clone());
            let out = game_dir.join(&rel);
            if is_seed(&rel) && out.exists() {
                continue;
            }
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).map_err(io_err(p))?;
            }
            let mut w = std::fs::File::create(&out).map_err(io_err(&out))?;
            std::io::copy(&mut e, &mut w).map_err(io_err(&out))?;
        }
    }

    let mut items = Vec::new();
    for f in &index.files {
        if f.env.as_ref().and_then(|e| e.get("client")).map(String::as_str) == Some("unsupported") {
            continue;
        }
        let rel = safe_relative(&f.path)?;
        nuevos.insert(rel.clone());
        let Some(url) = f.downloads.first() else { continue };
        items.push(Item {
            url: url.clone(),
            dest: game_dir.join(rel),
            sha1: f.hashes.get("sha1").cloned(),
            size: Some(f.file_size).filter(|s| *s > 0),
            // Un mismo mod en dos packs se baja una vez: caché compartida por hash.
            shared: true,
        });
    }
    let stats = dl.fetch_all(items, &|p| report("Mods", Some(p))).await?;

    // Lo que sobra: solo se borra después de tener lo nuevo en su sitio.
    let previos: Option<HashSet<String>> = (!previous.is_empty()).then(|| previous.iter().cloned().collect());
    let mods: Vec<String> = std::fs::read_dir(game_dir.join("mods"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let mut removed = Vec::new();
    for rel in leftovers(previos.as_ref(), &nuevos, &mods) {
        let Ok(rel) = safe_relative(&rel) else { continue };
        match std::fs::remove_file(game_dir.join(&rel)) {
            Ok(()) => removed.push(rel),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(game_dir.join(&rel), e)),
        }
    }
    let mut managed: Vec<String> = nuevos.into_iter().collect();
    managed.sort();
    Ok(Installed { index, stats, managed, removed })
}

/// Lo que sobra al instalar una versión: lo que puso la anterior y esta ya no trae.
///
/// Además, siempre, en `mods/`: las **versiones viejas de un mod que el pack sigue
/// trayendo** (mismo nombre antes del número). Así se limpian también las instalaciones
/// de antes de anotar nada. Nunca se toca lo que el pack no conoce (es del jugador), ni
/// el gemelo activado/desactivado (`.jar` / `.jar.disabled`) de un archivo del pack, ni
/// los archivos *seed*. Portado de `pack::sobrantes` de 1.x, con sus mismos casos.
pub fn leftovers(previous: Option<&HashSet<String>>, new: &HashSet<String>, mods: &[String]) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    if let Some(prev) = previous {
        for p in prev {
            if !new.contains(p) && !is_twin(p, new) && !is_seed(p) {
                out.insert(p.clone());
            }
        }
    }
    let families: HashSet<String> =
        new.iter().filter_map(|p| p.strip_prefix("mods/")).filter(|n| !n.contains('/')).filter_map(family).collect();
    for n in mods {
        let p = format!("mods/{n}");
        if new.contains(&p) || is_twin(&p, new) {
            continue;
        }
        if family(n).is_some_and(|f| families.contains(&f)) {
            out.insert(p);
        }
    }
    out.into_iter().collect()
}

/// `a.jar` frente a `a.jar.disabled` (y al revés): el mismo mod que el jugador activó o
/// desactivó. Es suyo.
fn is_twin(p: &str, new: &HashSet<String>) -> bool {
    match p.strip_suffix(".disabled") {
        Some(base) => new.contains(base),
        None => new.contains(&format!("{p}.disabled")),
    }
}

/// El nombre de un jar sin su versión: `voidcore-0.25.1.jar` → `voidcore`. `None` si no
/// parece llevar versión (no se arriesga a juntar dos mods distintos) o no es un jar.
fn family(name: &str) -> Option<String> {
    let n = name.strip_suffix(".disabled").unwrap_or(name);
    let n = n.strip_suffix(".jar")?.to_lowercase();
    let b = n.as_bytes();
    for i in 1..b.len() {
        let sep = matches!(b[i - 1], b'-' | b'_' | b'+' | b' ');
        let num = b[i].is_ascii_digit() || (b[i] == b'v' && b.get(i + 1).is_some_and(|c| c.is_ascii_digit()));
        if sep && num {
            let f = n[..i - 1].trim_end_matches(['-', '_', '+', ' ']).to_string();
            return if f.is_empty() { None } else { Some(f) };
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nada_se_sale_de_la_instancia() {
        assert!(safe_relative("../x").is_err());
        assert!(safe_relative("C:/Windows/x").is_err());
        assert!(safe_relative("/etc/x").is_err());
        assert_eq!(safe_relative("mods\\a.jar").unwrap(), "mods/a.jar");
    }

    fn set(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lo_que_quita_el_pack_se_va_y_lo_del_jugador_se_queda() {
        let antes = set(&["mods/a-1.0.jar", "mods/quitado-2.0.jar", "config/x.toml", "options.txt"]);
        let ahora = set(&["mods/a-1.1.jar", "config/x.toml"]);
        let mods = vec!["a-1.0.jar".into(), "a-1.1.jar".into(), "quitado-2.0.jar".into(), "mio-3.0.jar".into()];
        assert_eq!(leftovers(Some(&antes), &ahora, &mods), vec!["mods/a-1.0.jar", "mods/quitado-2.0.jar"]);
    }

    #[test]
    fn sin_registro_se_limpian_las_versiones_viejas_de_lo_que_sigue() {
        let ahora = set(&[
            "mods/voidcore-0.30.0.jar",
            "mods/Axiom-6.0.5-for-MC1.21.1.jar.disabled",
            "mods/sodium-neoforge-0.6.13+mc1.21.1.jar",
        ]);
        let mods: Vec<String> = [
            "voidcore-0.25.1.jar",
            "voidcore-0.29.0.jar",
            "voidcore-0.30.0.jar",
            "Axiom-6.0.5-for-MC1.21.1.jar",
            "sodium-neoforge-0.6.12+mc1.21.1.jar",
            "jei-1.21.1-19.0.jar",
            "simpleschematics-neoforge-1.21.1-0.7.0.jar",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            leftovers(None, &ahora, &mods),
            vec!["mods/sodium-neoforge-0.6.12+mc1.21.1.jar", "mods/voidcore-0.25.1.jar", "mods/voidcore-0.29.0.jar"],
        );
    }

    #[test]
    fn la_familia_de_un_jar() {
        assert_eq!(family("voidcore-0.25.1.jar").as_deref(), Some("voidcore"));
        assert_eq!(family("Axiom-6.0.5-for-MC1.21.1.jar.disabled").as_deref(), Some("axiom"));
        assert_eq!(family("rinku_neoforge_3.0.4_MC_1.21.1.jar").as_deref(), Some("rinku_neoforge"));
        assert_eq!(family("sinversion.jar"), None);
        assert_eq!(family("notas-1.txt"), None);
    }

    #[test]
    fn los_archivos_del_jugador_son_seed() {
        assert!(is_seed("options.txt"));
        assert!(is_seed("saves/mundo/level.dat"));
        assert!(!is_seed("config/sodium.json"));
    }
}
