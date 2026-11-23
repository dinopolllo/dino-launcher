//! Formatos de Mojang y de los perfiles de loaders.
//!
//! Solo los campos que se usan. Los perfiles de NeoForge siguen el formato de
//! launcher-meta de Modrinth (`data`, `processors`, `downloadable`, `include_in_classpath`),
//! que es el mismo que modela `minecraft-meta-manifest` en `modrinth/code` (GPL-3.0).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
pub struct VersionManifest {
    pub versions: Vec<ManifestEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ManifestEntry {
    pub id: String,
    pub url: String,
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    pub main_class: String,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    pub downloads: Option<VersionDownloads>,
    pub asset_index: Option<AssetIndexRef>,
    pub assets: Option<String>,
    pub java_version: Option<JavaVersion>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Lo que añade un loader (NeoForge): variables de los processors.
    #[serde(default)]
    pub data: Option<BTreeMap<String, SidedValue>>,
    #[serde(default)]
    pub processors: Option<Vec<Processor>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<Arg>,
    #[serde(default)]
    pub jvm: Vec<Arg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Arg {
    Plain(String),
    Ruled { rules: Vec<Rule>, value: OneOrMany },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn iter(&self) -> Box<dyn Iterator<Item = &String> + '_> {
        match self {
            OneOrMany::One(s) => Box::new(std::iter::once(s)),
            OneOrMany::Many(v) => Box::new(v.iter()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub action: String,
    #[serde(default)]
    pub os: Option<OsRule>,
    #[serde(default)]
    pub features: Option<BTreeMap<String, bool>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsRule {
    pub name: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<LibraryDownloads>,
    /// Base de Maven, si la librería no trae `downloads` (perfiles de Fabric).
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub rules: Option<Vec<Rule>>,
    #[serde(default = "yes")]
    pub downloadable: bool,
    #[serde(default = "yes")]
    pub include_in_classpath: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryDownloads {
    pub artifact: Option<Artifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub path: Option<String>,
    pub url: String,
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionDownloads {
    pub client: Artifact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexRef {
    pub id: String,
    pub url: String,
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    pub component: String,
    pub major_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidedValue {
    pub client: String,
    pub server: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Processor {
    pub jar: String,
    #[serde(default)]
    pub classpath: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub sides: Option<Vec<String>>,
}

/// Perfil parcial de un loader, que hereda de una versión de Mojang.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoaderProfile {
    pub id: String,
    pub main_class: Option<String>,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub data: Option<BTreeMap<String, SidedValue>>,
    #[serde(default)]
    pub processors: Option<Vec<Processor>>,
}

#[derive(Debug, Deserialize)]
pub struct AssetIndex {
    pub objects: BTreeMap<String, AssetObject>,
}

#[derive(Debug, Deserialize)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

// ---------------------------------------------------------------- reglas

/// Lo que se sabe del sistema y de la partida al evaluar reglas.
#[derive(Debug, Clone, Default)]
pub struct Features {
    pub quick_play_multiplayer: bool,
}

pub fn os_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

/// Reglas de Mojang: sin reglas se permite; con reglas, manda la última que encaja.
pub fn allowed(rules: Option<&[Rule]>, f: &Features) -> bool {
    let Some(rules) = rules else { return true };
    if rules.is_empty() {
        return true;
    }
    let mut ok = false;
    for r in rules {
        if encaja(r, f) {
            ok = r.action == "allow";
        }
    }
    ok
}

fn encaja(r: &Rule, f: &Features) -> bool {
    if let Some(os) = &r.os {
        if let Some(n) = &os.name {
            if n != os_name() {
                return false;
            }
        }
        if let Some(a) = &os.arch {
            // «x86» en las reglas de Mojang es 32 bits.
            let aqui = if cfg!(target_arch = "x86") {
                "x86"
            } else if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "x86_64"
            };
            if a != aqui {
                return false;
            }
        }
    }
    if let Some(feats) = &r.features {
        for (k, want) in feats {
            let have = match k.as_str() {
                "is_quick_play_multiplayer" => f.quick_play_multiplayer,
                _ => false, // demo, resolución propia, otros quick play: no los usamos
            };
            if have != *want {
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------- Maven

/// `grupo:artefacto:versión[:clasificador][@ext]` → ruta relativa en un repositorio Maven.
pub fn maven_path(coord: &str) -> Option<String> {
    let (coord, ext) = match coord.split_once('@') {
        Some((c, e)) => (c, e),
        None => (coord, "jar"),
    };
    let mut parts = coord.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next()?;
    let classifier = parts.next();
    let file = match classifier {
        Some(c) => format!("{artifact}-{version}-{c}.{ext}"),
        None => format!("{artifact}-{version}.{ext}"),
    };
    Some(format!("{}/{artifact}/{version}/{file}", group.replace('.', "/")))
}

impl Library {
    /// Ruta relativa dentro de `libraries/`.
    pub fn path(&self) -> Option<String> {
        self.downloads
            .as_ref()
            .and_then(|d| d.artifact.as_ref())
            .and_then(|a| a.path.clone())
            .or_else(|| maven_path(&self.name))
    }

    /// De dónde bajarla, si se baja.
    pub fn url(&self) -> Option<String> {
        if let Some(a) = self.downloads.as_ref().and_then(|d| d.artifact.as_ref()) {
            if !a.url.is_empty() {
                return Some(a.url.clone());
            }
        }
        let base = self.url.as_deref()?;
        let base = base.trim_end_matches('/');
        Some(format!("{base}/{}", maven_path(&self.name)?))
    }

    pub fn sha1(&self) -> Option<String> {
        self.downloads
            .as_ref()
            .and_then(|d| d.artifact.as_ref())
            .and_then(|a| a.sha1.clone())
            .or_else(|| self.sha1.clone())
    }

    pub fn size(&self) -> Option<u64> {
        self.downloads.as_ref().and_then(|d| d.artifact.as_ref()).and_then(|a| a.size).or(self.size)
    }

    /// `grupo:artefacto[:clasificador]`, sin versión: lo que identifica a una librería
    /// cuando el loader trae otra versión de algo que también trae Mojang.
    pub fn key(&self) -> String {
        let p: Vec<&str> = self.name.split('@').next().unwrap_or("").split(':').collect();
        match p.as_slice() {
            [g, a, _v, c, ..] => format!("{g}:{a}:{c}"),
            [g, a, ..] => format!("{g}:{a}"),
            _ => self.name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rutas_maven() {
        assert_eq!(
            maven_path("net.neoforged:neoforge:21.1.251:client").unwrap(),
            "net/neoforged/neoforge/21.1.251/neoforge-21.1.251-client.jar"
        );
        assert_eq!(
            maven_path("net.neoforged:neoform:1.21.1-20240808.144430:mappings@txt").unwrap(),
            "net/neoforged/neoform/1.21.1-20240808.144430/neoform-1.21.1-20240808.144430-mappings.txt"
        );
        assert_eq!(maven_path("org.ow2.asm:asm:9.7").unwrap(), "org/ow2/asm/asm/9.7/asm-9.7.jar");
    }

    #[test]
    fn la_ultima_regla_que_encaja_manda() {
        let r = |action: &str, os: Option<&str>| Rule {
            action: action.into(),
            os: os.map(|n| OsRule { name: Some(n.into()), arch: None }),
            features: None,
        };
        let f = Features::default();
        assert!(allowed(None, &f));
        assert!(allowed(Some(&[r("allow", None)]), &f));
        assert!(!allowed(Some(&[r("allow", None), r("disallow", Some(os_name()))]), &f));
        assert!(!allowed(Some(&[r("allow", Some("nadie"))]), &f));
    }

    #[test]
    fn quick_play_solo_si_hay_servidor() {
        let rule = Rule {
            action: "allow".into(),
            os: None,
            features: Some([("is_quick_play_multiplayer".to_string(), true)].into()),
        };
        assert!(!allowed(Some(std::slice::from_ref(&rule)), &Features::default()));
        assert!(allowed(Some(&[rule]), &Features { quick_play_multiplayer: true }));
    }

    #[test]
    fn la_clave_ignora_la_version_pero_no_el_clasificador() {
        let l = |n: &str| Library {
            name: n.into(),
            downloads: None,
            url: None,
            sha1: None,
            size: None,
            rules: None,
            downloadable: true,
            include_in_classpath: true,
        };
        assert_eq!(l("org.ow2.asm:asm:9.7").key(), l("org.ow2.asm:asm:9.10.1").key());
        assert_ne!(l("org.lwjgl:lwjgl:3.3.3").key(), l("org.lwjgl:lwjgl:3.3.3:natives-windows").key());
    }
}
