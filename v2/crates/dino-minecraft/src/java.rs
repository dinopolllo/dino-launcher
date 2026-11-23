//! Java: el runtime que publica Mojang para cada versión (`javaVersion.component`).
//!
//! 1.x dependía de lo que bajara el motor; aquí se usa el mismo índice que el launcher
//! oficial, con cada archivo verificado por SHA-1.

use crate::{io_err, Error, Paths, Report, Result};
use dino_download::{Downloader, Item};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

const ALL_RUNTIMES: &str =
    "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

#[derive(Deserialize)]
struct RuntimeEntry {
    manifest: RuntimeManifestRef,
}

#[derive(Deserialize)]
struct RuntimeManifestRef {
    url: String,
}

#[derive(Deserialize)]
struct RuntimeManifest {
    files: BTreeMap<String, RuntimeFile>,
}

#[derive(Deserialize)]
struct RuntimeFile {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    downloads: Option<RuntimeDownloads>,
}

#[derive(Deserialize)]
struct RuntimeDownloads {
    raw: RuntimeRaw,
}

#[derive(Deserialize)]
struct RuntimeRaw {
    url: String,
    sha1: String,
    size: u64,
}

fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("windows-x64"),
        ("windows", "aarch64") => Some("windows-arm64"),
        ("macos", "aarch64") => Some("mac-os-arm64"),
        ("macos", "x86_64") => Some("mac-os"),
        ("linux", "x86_64") => Some("linux"),
        _ => None,
    }
}

/// Ejecutable de Java para el juego. En Windows es `javaw.exe`: sin consola negra.
fn java_bin(home: &std::path::Path) -> PathBuf {
    if cfg!(windows) {
        home.join("bin").join("javaw.exe")
    } else if cfg!(target_os = "macos") {
        home.join("jre.bundle/Contents/Home/bin/java")
    } else {
        home.join("bin").join("java")
    }
}

/// Dónde queda el ejecutable de un runtime instalado.
pub fn bin_for(dirs: &Paths, component: &str) -> PathBuf {
    java_bin(&dirs.java().join(component))
}

/// Ejecutable para los processors: con consola, para poder leer su salida.
pub fn java_console(javaw: &std::path::Path) -> PathBuf {
    if cfg!(windows) {
        javaw.with_file_name("java.exe")
    } else {
        javaw.to_path_buf()
    }
}

/// Deja instalado el runtime `component` y devuelve la ruta a su ejecutable.
pub async fn ensure(dl: &Downloader, dirs: &Paths, component: &str, report: Report<'_>) -> Result<PathBuf> {
    let home = dirs.java().join(component);
    let bin = java_bin(&home);
    let plat = platform().ok_or_else(|| Error::Other("plataforma sin Java de Mojang".into()))?;

    report(&format!("Comprobando Java ({component})"), None);
    let all: BTreeMap<String, BTreeMap<String, Vec<RuntimeEntry>>> = dl.json(ALL_RUNTIMES).await?;
    let entry = all
        .get(plat)
        .and_then(|m| m.get(component))
        .and_then(|v| v.first())
        .ok_or_else(|| Error::Other(format!("Mojang no publica {component} para {plat}")))?;
    let manifest: RuntimeManifest = dl.json(&entry.manifest.url).await?;

    let mut items = Vec::new();
    for (rel, f) in &manifest.files {
        let dest = home.join(rel);
        match f.kind.as_str() {
            "directory" => std::fs::create_dir_all(&dest).map_err(io_err(&dest))?,
            "file" => {
                if let Some(d) = &f.downloads {
                    items.push(Item {
                        url: d.raw.url.clone(),
                        dest,
                        sha1: Some(d.raw.sha1.clone()),
                        size: Some(d.raw.size),
                        shared: false,
                    });
                }
            }
            _ => {} // enlaces simbólicos: solo en macOS/Linux, el runtime no los necesita para arrancar
        }
    }
    let label = format!("Java ({component})");
    dl.fetch_all(items, &|p| report(&label, Some(p))).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755));
    }
    if !bin.exists() {
        return Err(Error::Other(format!("el runtime no trae {}", bin.display())));
    }
    Ok(bin)
}
