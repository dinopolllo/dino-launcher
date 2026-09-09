//! Instalación de modpacks `.mrpack`.
//!
//! El motor **no** puede importar un `.mrpack` sin interfaz gráfica: su bandera `-I`
//! solo enruta *recursos* (mods sueltos, resourcepacks, mundos) y el manejo de packs
//! vive en `ImportPage.cpp`, que es un diálogo. Así que la instancia la construimos
//! nosotros y el motor solo la lanza.
//!
//! Nos conviene: controlar la instalación es lo que permite respetar los archivos
//! del jugador entre actualizaciones.

use serde::Deserialize;
use std::collections::BTreeMap;

/// `modrinth.index.json` dentro del `.mrpack`.
#[derive(Debug, Deserialize)]
pub struct MrpackIndex {
    #[serde(rename = "formatVersion")]
    pub format_version: u32,
    pub name: String,
    #[serde(rename = "versionId")]
    pub version_id: String,
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub files: Vec<MrpackFile>,
}

#[derive(Debug, Deserialize)]
pub struct MrpackFile {
    pub path: String,
    pub hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub downloads: Vec<String>,
    #[serde(rename = "fileSize", default)]
    pub file_size: u64,
}

impl MrpackFile {
    pub fn sha512(&self) -> Option<&str> {
        self.hashes.get("sha512").map(String::as_str)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("el índice no es JSON válido: {0}")]
    Json(#[from] serde_json::Error),
    #[error("formatVersion {0} no soportado (esperaba 1)")]
    Format(u32),
    #[error("el pack no declara versión de Minecraft")]
    NoMinecraft,
    #[error("mod loader desconocido: {0}")]
    UnknownLoader(String),
    #[error("la ruta '{0}' se sale de la instancia")]
    UnsafePath(String),
}

pub fn parse_index(json: &str) -> Result<MrpackIndex, PackError> {
    let idx: MrpackIndex = serde_json::from_str(json)?;
    if idx.format_version != 1 {
        return Err(PackError::Format(idx.format_version));
    }
    Ok(idx)
}

/// Las dependencias del `.mrpack` traducidas a los `uid` del motor.
/// Mapeo tomado de `ModrinthInstanceCreationTask.cpp`.
fn component_uid(dep: &str) -> Option<&'static str> {
    Some(match dep {
        "fabric-loader" => "net.fabricmc.fabric-loader",
        "quilt-loader" => "org.quiltmc.quilt-loader",
        "forge" => "net.minecraftforge",
        "neoforge" => "net.neoforged",
        _ => return None,
    })
}

/// Genera el `mmc-pack.json` de la instancia.
///
/// `formatVersion` es 1, verificado en `PackProfile.cpp`.
pub fn mmc_pack(deps: &BTreeMap<String, String>) -> Result<String, PackError> {
    let mc = deps.get("minecraft").ok_or(PackError::NoMinecraft)?;

    let mut components = vec![serde_json::json!({
        "uid": "net.minecraft",
        "version": mc,
        "important": true,
    })];

    for (k, v) in deps {
        if k == "minecraft" {
            continue;
        }
        let uid = component_uid(k).ok_or_else(|| PackError::UnknownLoader(k.clone()))?;
        components.push(serde_json::json!({ "uid": uid, "version": v }));
    }

    let doc = serde_json::json!({ "formatVersion": 1, "components": components });
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// Archivos que pertenecen al jugador: se escriben en la primera instalación
/// y **nunca** se vuelven a tocar.
///
/// Todo lo demás que trae el pack es del admin y se reescribe en cada actualización.
pub fn is_seed(rel: &str) -> bool {
    let p = rel.replace('\\', "/");
    const SEED_FILES: [&str; 4] = ["options.txt", "servers.dat", "optionsof.txt", "optionsshaders.txt"];
    const SEED_DIRS: [&str; 4] = ["saves/", "screenshots/", "logs/", "crash-reports/"];
    SEED_FILES.contains(&p.as_str()) || SEED_DIRS.iter().any(|d| p.starts_with(d))
}

/// Rechaza rutas que se escapen del directorio de la instancia.
///
/// Un `.mrpack` es un zip de origen remoto: sus rutas son entrada no confiable.
pub fn safe_relative(rel: &str) -> Result<String, PackError> {
    let p = rel.replace('\\', "/");
    let bad = p.starts_with('/')
        || p.split('/').any(|seg| seg == ".." || seg == ".")
        || p.contains('\0')
        || p.chars().nth(1) == Some(':');
    if bad || p.is_empty() {
        return Err(PackError::UnsafePath(rel.into()));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recortado del pack real de prueba (Fabulously Optimized 1.0.0).
    const INDEX: &str = r#"{
      "formatVersion": 1,
      "game": "minecraft",
      "versionId": "1.0.0",
      "name": "Fabulously Optimized",
      "files": [
        { "path": "mods/sodium-extra-fabric-0.9.3+mc26.2.jar",
          "hashes": { "sha1": "aaa", "sha512": "bbb" },
          "downloads": ["https://cdn.modrinth.com/data/x/versions/y/sodium.jar"],
          "fileSize": 482304 }
      ],
      "dependencies": { "minecraft": "26.2", "fabric-loader": "0.19.3" }
    }"#;

    #[test]
    fn lee_el_indice_del_pack_real() {
        let i = parse_index(INDEX).unwrap();
        assert_eq!(i.name, "Fabulously Optimized");
        assert_eq!(i.version_id, "1.0.0");
        assert_eq!(i.dependencies["minecraft"], "26.2");
        assert_eq!(i.dependencies["fabric-loader"], "0.19.3");
        assert_eq!(i.files[0].sha512(), Some("bbb"));
    }

    #[test]
    fn rechaza_formatos_futuros_en_vez_de_adivinar() {
        let j = INDEX.replace("\"formatVersion\": 1", "\"formatVersion\": 2");
        assert!(matches!(parse_index(&j), Err(PackError::Format(2))));
    }

    #[test]
    fn traduce_fabric_al_uid_del_motor() {
        let i = parse_index(INDEX).unwrap();
        let m = mmc_pack(&i.dependencies).unwrap();
        assert!(m.contains("\"uid\": \"net.minecraft\""));
        assert!(m.contains("\"version\": \"26.2\""));
        assert!(m.contains("\"uid\": \"net.fabricmc.fabric-loader\""));
        assert!(m.contains("\"version\": \"0.19.3\""));
        assert!(m.contains("\"formatVersion\": 1"));
    }

    #[test]
    fn traduce_los_demas_loaders() {
        for (dep, uid) in [
            ("neoforge", "net.neoforged"),
            ("forge", "net.minecraftforge"),
            ("quilt-loader", "org.quiltmc.quilt-loader"),
        ] {
            let mut d = BTreeMap::new();
            d.insert("minecraft".into(), "26.2".into());
            d.insert(dep.into(), "1.0".into());
            assert!(mmc_pack(&d).unwrap().contains(uid), "faltó {uid}");
        }
    }

    #[test]
    fn un_loader_desconocido_falla_en_vez_de_instalar_algo_roto() {
        let mut d = BTreeMap::new();
        d.insert("minecraft".into(), "26.2".into());
        d.insert("liteloader".into(), "1.0".into());
        assert!(matches!(mmc_pack(&d), Err(PackError::UnknownLoader(_))));
    }

    #[test]
    fn los_archivos_del_jugador_son_seed() {
        for p in ["options.txt", "servers.dat", "saves/mundo/level.dat", "screenshots/a.png"] {
            assert!(is_seed(p), "{p} debería respetarse");
        }
    }

    #[test]
    fn los_mods_y_configs_del_admin_son_managed() {
        for p in [
            "mods/sodium.jar",
            "config/sodium-options.json",
            "config/modpack_defaults/options.txt", // defaults del pack, no los del jugador
            "resourcepacks/Mod Menu Helper.zip",
        ] {
            assert!(!is_seed(p), "{p} debería reescribirse en cada update");
        }
    }

    #[test]
    fn rechaza_rutas_que_escapan_de_la_instancia() {
        for p in ["../fuera.txt", "/etc/passwd", "mods/../../x", "C:/Windows/x", ""] {
            assert!(safe_relative(p).is_err(), "{p:?} debería rechazarse");
        }
        assert_eq!(safe_relative("mods/a.jar").unwrap(), "mods/a.jar");
        assert_eq!(safe_relative("config\\x\\y.json").unwrap(), "config/x/y.json");
    }
}
