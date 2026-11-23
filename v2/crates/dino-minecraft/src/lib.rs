//! Instalar y lanzar Minecraft sin motor intermedio.
//!
//! Partes adaptadas del núcleo de lanzamiento de Modrinth App (`packages/app-lib/src/launcher`
//! en <https://github.com/modrinth/code>, GPL-3.0-only): la fusión de perfiles de loader con
//! la versión de Mojang y la ejecución de los *processors* de NeoForge.

pub mod install;
pub mod java;
pub mod launch;
pub mod model;
pub mod mrpack;

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Download(#[from] dino_download::DownloadError),
    #[error("disco ({0}): {1}")]
    Io(PathBuf, std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io_err(p: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |e| Error::Io(p.to_path_buf(), e)
}

/// Rutas de Dino 2.0: las de `dino-core`, que es su única fuente.
pub use dino_core::schema::Paths;

/// Lo que se va haciendo, para quien muestre progreso.
pub type Report<'a> = &'a (dyn Fn(&str, Option<dino_download::Progress>) + Send + Sync);
