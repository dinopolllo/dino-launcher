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
use std::collections::BTreeMap;
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
            Ok(Loader::NeoForge(v.clone()))
        } else if let Some(v) = d.get("fabric-loader") {
            Ok(Loader::Fabric(v.clone()))
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

/// Copia los overrides y baja los archivos del índice a `game_dir`.
pub async fn install(dl: &Downloader, mrpack: &Path, game_dir: &Path, report: Report<'_>) -> Result<(Index, Stats)> {
    let index = read_index(mrpack)?;
    std::fs::create_dir_all(game_dir).map_err(io_err(game_dir))?;

    report("Copiando configuración del pack", None);
    {
        let f = std::fs::File::open(mrpack).map_err(io_err(mrpack))?;
        let mut z = zip::ZipArchive::new(f)?;
        for i in 0..z.len() {
            let mut e = z.by_index(i)?;
            if e.is_dir() {
                continue;
            }
            let name = e.name().to_string();
            let Some(rel) = name.strip_prefix("overrides/").or_else(|| name.strip_prefix("client-overrides/")) else {
                continue;
            };
            let rel = safe_relative(rel)?;
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
        let Some(url) = f.downloads.first() else { continue };
        items.push(Item {
            url: url.clone(),
            dest: game_dir.join(rel),
            sha1: f.hashes.get("sha1").cloned(),
            size: Some(f.file_size).filter(|s| *s > 0),
        });
    }
    let stats = dl.fetch_all(items, &|p| report("Mods", Some(p))).await?;
    Ok((index, stats))
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

    #[test]
    fn los_archivos_del_jugador_son_seed() {
        assert!(is_seed("options.txt"));
        assert!(is_seed("saves/mundo/level.dat"));
        assert!(!is_seed("config/sodium.json"));
    }
}
