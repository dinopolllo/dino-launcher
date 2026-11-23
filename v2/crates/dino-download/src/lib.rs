//! Descargas concurrentes y verificadas.
//!
//! Sustituye a las dos formas de bajar archivos de 1.x: `reqwest::get` con un cliente nuevo
//! por archivo, en serie y entero en memoria (mods), y la descarga ciega que hacía el motor
//! (Minecraft, librerías, assets). Aquí todo pasa por un único cliente HTTP compartido:
//!
//! - **concurrencia acotada** (`buffer_unordered`), para no abrir mil conexiones;
//! - **streaming a disco** con el hash calculado por el camino, sin el archivo entero en RAM;
//! - **escritura atómica**: se escribe a `*.part` y solo se renombra si el hash cuadra, así
//!   que un corte nunca deja un archivo a medias con el nombre bueno;
//! - **reintentos** con espera creciente para los fallos que pueden ser pasajeros;
//! - **saltar lo que ya está bien**: un archivo presente con el hash esperado no se baja.

use futures_util::{stream, StreamExt};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("red: {0}")]
    Http(#[from] reqwest::Error),
    #[error("disco ({path}): {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{url} respondió {status}")]
    Status { url: String, status: u16 },
    #[error("{path}: el hash no coincide (esperaba {expected}, llegó {got})")]
    Hash { path: PathBuf, expected: String, got: String },
    #[error("JSON inválido en {url}: {source}")]
    Json { url: String, source: serde_json::Error },
}

impl DownloadError {
    /// ¿Merece la pena volver a intentarlo? Un 404 o un hash malo de un archivo que
    /// llegó entero no van a cambiar; un corte de red o un 5xx sí pueden.
    fn pasajero(&self) -> bool {
        match self {
            DownloadError::Http(_) => true,
            DownloadError::Status { status, .. } => *status == 429 || *status >= 500,
            DownloadError::Hash { .. } => true, // puede ser un corte silencioso del CDN
            _ => false,
        }
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> DownloadError + '_ {
    move |source| DownloadError::Io { path: path.to_path_buf(), source }
}

/// Un archivo a traer.
#[derive(Debug, Clone)]
pub struct Item {
    pub url: String,
    pub dest: PathBuf,
    /// SHA-1 en hexadecimal. Sin él, solo se comprueba que el archivo exista.
    pub sha1: Option<String>,
    /// Tamaño esperado, para el progreso. No es obligatorio.
    pub size: Option<u64>,
}

/// Avance de un lote: lo que la interfaz necesita para una barra real.
#[derive(Debug, Clone, Copy, Default)]
pub struct Progress {
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Bytes que de verdad cruzaron la red (lo que ya estaba no cuenta).
    pub bytes_downloaded: u64,
}

/// Resumen de un lote terminado.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub files: u64,
    pub skipped: u64,
    pub downloaded_bytes: u64,
}

#[derive(Clone)]
pub struct Downloader {
    client: reqwest::Client,
    concurrency: usize,
    retries: u32,
}

impl Downloader {
    pub fn new(concurrency: usize) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("dino-launcher/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(concurrency)
            .build()
            .expect("cliente HTTP");
        Self { client, concurrency: concurrency.max(1), retries: 3 }
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Baja un JSON pequeño (manifiestos), con reintentos.
    pub async fn json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, DownloadError> {
        let bytes = self.bytes(url).await?;
        serde_json::from_slice(&bytes).map_err(|source| DownloadError::Json { url: url.into(), source })
    }

    /// Baja algo pequeño entero a memoria, con reintentos.
    pub async fn bytes(&self, url: &str) -> Result<Vec<u8>, DownloadError> {
        let mut intento = 0;
        loop {
            let r = async {
                let r = self.client.get(url).send().await?;
                if !r.status().is_success() {
                    return Err(DownloadError::Status { url: url.into(), status: r.status().as_u16() });
                }
                Ok(r.bytes().await?.to_vec())
            }
            .await;
            match r {
                Err(e) if e.pasajero() && intento < self.retries => {
                    intento += 1;
                    tokio::time::sleep(espera(intento)).await;
                }
                r => return r,
            }
        }
    }

    /// Trae un lote entero. Devuelve error en cuanto un archivo falle tras sus reintentos.
    pub async fn fetch_all(
        &self,
        items: Vec<Item>,
        on_progress: &(dyn Fn(Progress) + Send + Sync),
    ) -> Result<Stats, DownloadError> {
        let total = Progress {
            files_total: items.len() as u64,
            bytes_total: items.iter().filter_map(|i| i.size).sum(),
            ..Default::default()
        };
        let files_done = Arc::new(AtomicU64::new(0));
        let bytes_done = Arc::new(AtomicU64::new(0));
        let net = Arc::new(AtomicU64::new(0));
        let skipped = Arc::new(AtomicU64::new(0));

        let mut s = stream::iter(items.into_iter().map(|item| {
            let (files_done, bytes_done, net, skipped) =
                (files_done.clone(), bytes_done.clone(), net.clone(), skipped.clone());
            async move {
                let bajado = self.fetch_one(&item).await?;
                if bajado == 0 {
                    skipped.fetch_add(1, Ordering::Relaxed);
                }
                net.fetch_add(bajado, Ordering::Relaxed);
                bytes_done.fetch_add(item.size.unwrap_or(bajado), Ordering::Relaxed);
                files_done.fetch_add(1, Ordering::Relaxed);
                Ok::<(), DownloadError>(())
            }
        }))
        .buffer_unordered(self.concurrency);

        while let Some(r) = s.next().await {
            r?;
            on_progress(Progress {
                files_done: files_done.load(Ordering::Relaxed),
                bytes_done: bytes_done.load(Ordering::Relaxed),
                bytes_downloaded: net.load(Ordering::Relaxed),
                ..total
            });
        }
        Ok(Stats {
            files: total.files_total,
            skipped: skipped.load(Ordering::Relaxed),
            downloaded_bytes: net.load(Ordering::Relaxed),
        })
    }

    /// Devuelve cuántos bytes bajó (0 si ya estaba bien).
    async fn fetch_one(&self, item: &Item) -> Result<u64, DownloadError> {
        if ya_esta(item).await {
            return Ok(0);
        }
        let mut intento = 0;
        loop {
            match self.stream_to_disk(item).await {
                Err(e) if e.pasajero() && intento < self.retries => {
                    intento += 1;
                    tokio::time::sleep(espera(intento)).await;
                }
                r => return r,
            }
        }
    }

    async fn stream_to_disk(&self, item: &Item) -> Result<u64, DownloadError> {
        if let Some(dir) = item.dest.parent() {
            tokio::fs::create_dir_all(dir).await.map_err(io(dir))?;
        }
        let part = part_path(&item.dest);
        let r = self.client.get(&item.url).send().await?;
        if !r.status().is_success() {
            return Err(DownloadError::Status { url: item.url.clone(), status: r.status().as_u16() });
        }
        let mut f = tokio::fs::File::create(&part).await.map_err(io(&part))?;
        let mut h = Sha1::new();
        let mut n = 0u64;
        let mut body = r.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            h.update(&chunk);
            f.write_all(&chunk).await.map_err(io(&part))?;
            n += chunk.len() as u64;
        }
        f.flush().await.map_err(io(&part))?;
        drop(f);
        if let Some(want) = &item.sha1 {
            let got = hex(&h.finalize());
            if !got.eq_ignore_ascii_case(want) {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(DownloadError::Hash { path: item.dest.clone(), expected: want.clone(), got });
            }
        }
        tokio::fs::rename(&part, &item.dest).await.map_err(io(&item.dest))?;
        Ok(n)
    }
}

fn espera(intento: u32) -> Duration {
    Duration::from_millis(500 * 2u64.pow(intento.saturating_sub(1)))
}

fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// ¿El archivo ya está y es el correcto?
async fn ya_esta(item: &Item) -> bool {
    let Ok(meta) = tokio::fs::metadata(&item.dest).await else { return false };
    if let Some(size) = item.size {
        if meta.len() != size {
            return false;
        }
    }
    match &item.sha1 {
        None => true,
        Some(want) => match sha1_file(&item.dest).await {
            Ok(got) => got.eq_ignore_ascii_case(want),
            Err(_) => false,
        },
    }
}

/// SHA-1 de un archivo, leyéndolo por trozos.
pub async fn sha1_file(path: &Path) -> std::io::Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut f = std::fs::File::open(&path)?;
        let mut h = Sha1::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(hex(&h.finalize()))
    })
    .await
    .expect("hilo de hash")
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_temporal_lleva_el_nombre_entero_mas_part() {
        assert_eq!(part_path(Path::new("a/b/lib-1.0.jar")), PathBuf::from("a/b/lib-1.0.jar.part"));
    }

    #[test]
    fn la_espera_crece() {
        assert!(espera(1) < espera(2) && espera(2) < espera(3));
    }

    #[test]
    fn un_404_no_se_reintenta_y_un_503_si() {
        let e404 = DownloadError::Status { url: "x".into(), status: 404 };
        let e503 = DownloadError::Status { url: "x".into(), status: 503 };
        assert!(!e404.pasajero());
        assert!(e503.pasajero());
    }

    #[tokio::test]
    async fn un_archivo_con_el_hash_bueno_no_se_baja_otra_vez() {
        let dir = std::env::temp_dir().join(format!("dino-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("hola.txt");
        std::fs::write(&p, b"hola").unwrap();
        let sha = sha1_file(&p).await.unwrap();
        let item = Item { url: "http://127.0.0.1:9/no".into(), dest: p.clone(), sha1: Some(sha), size: Some(4) };
        assert!(ya_esta(&item).await);
        let malo = Item { sha1: Some("00".into()), ..item };
        assert!(!ya_esta(&malo).await);
        let _ = std::fs::remove_dir_all(dir);
    }
}
