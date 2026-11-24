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
//! - **reanudar**: si se corta a mitad, el reintento (o la próxima vez) sigue donde quedó con
//!   una petición `Range`, en vez de volver a empezar;
//! - **reintentos** con espera creciente para los fallos que pueden ser pasajeros;
//! - **saltar lo que ya está bien**: un archivo presente con el hash esperado no se baja;
//! - **caché compartida por hash** para mods: el mismo `.jar` en dos packs se baja una vez;
//! - **límite de velocidad** opcional, para no ahogar la conexión de la casa.

use futures_util::{stream, StreamExt};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    #[error("cancelado")]
    Cancelled,
}

impl DownloadError {
    /// ¿Merece la pena volver a intentarlo? Un 404 no va a cambiar; un corte de red, un 5xx
    /// o un hash malo (corte silencioso del CDN) sí pueden. Un 416 es un resto a medias que
    /// ya no sirve: se tira y el reintento empieza de cero.
    fn pasajero(&self) -> bool {
        match self {
            DownloadError::Http(_) => true,
            DownloadError::Status { status, .. } => *status == 416 || *status == 429 || *status >= 500,
            DownloadError::Hash { .. } => true,
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
    /// SHA-1 en hexadecimal. Sin él, solo se comprueba que el archivo exista y no se reanuda.
    pub sha1: Option<String>,
    /// Tamaño esperado, para el progreso. No es obligatorio.
    pub size: Option<u64>,
    /// Pasa por la caché compartida por hash (mods). Lo demás ya vive en carpetas
    /// compartidas (librerías, assets) y no la necesita.
    pub shared: bool,
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
    /// Ya estaban en su sitio con el hash bueno.
    pub skipped: u64,
    /// Salieron de la caché compartida sin tocar la red.
    pub from_cache: u64,
    /// Reanudados desde un resto a medias.
    pub resumed: u64,
    pub downloaded_bytes: u64,
}

#[derive(Default)]
struct Counters {
    files_done: AtomicU64,
    bytes_done: AtomicU64,
    net: AtomicU64,
    skipped: AtomicU64,
    from_cache: AtomicU64,
    resumed: AtomicU64,
}

/// Límite de velocidad compartido por todas las descargas en curso.
struct Limiter {
    bytes_per_sec: u64,
    state: tokio::sync::Mutex<(Instant, u64)>,
}

impl Limiter {
    async fn take(&self, n: u64) {
        let wait = {
            let mut s = self.state.lock().await;
            s.1 += n;
            let due = Duration::from_secs_f64(s.1 as f64 / self.bytes_per_sec as f64);
            due.saturating_sub(s.0.elapsed())
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

#[derive(Clone)]
pub struct Downloader {
    client: reqwest::Client,
    concurrency: usize,
    retries: u32,
    cache: Option<PathBuf>,
    limiter: Option<Arc<Limiter>>,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
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
        Self { client, concurrency: concurrency.max(1), retries: 3, cache: None, limiter: None, cancel: None }
    }

    /// Carpeta de la caché compartida por hash (`<dir>/<aa>/<sha1>`).
    pub fn with_cache(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache = Some(dir.into());
        self
    }

    /// Límite global en bytes por segundo. `None` o 0: sin límite.
    pub fn with_rate_limit(mut self, bytes_per_sec: Option<u64>) -> Self {
        self.limiter = bytes_per_sec
            .filter(|b| *b > 0)
            .map(|b| Arc::new(Limiter { bytes_per_sec: b, state: tokio::sync::Mutex::new((Instant::now(), 0)) }));
        self
    }

    /// Señal para cancelar: se mira entre archivos y en cada trozo. Lo ya escrito se
    /// queda en su `.part` y se reanuda la próxima vez.
    pub fn with_cancel(mut self, flag: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst))
    }

    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
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
        // Un mismo destino puede venir dos veces (NeoForge y Mojang listan alguna librería
        // por duplicado). Bajarlo dos veces a la vez hacía chocar las dos escrituras.
        let mut vistos = std::collections::HashSet::new();
        let items: Vec<Item> = items.into_iter().filter(|i| vistos.insert(i.dest.clone())).collect();
        let total = Progress {
            files_total: items.len() as u64,
            bytes_total: items.iter().filter_map(|i| i.size).sum(),
            ..Default::default()
        };
        let c = Arc::new(Counters::default());
        // El total se anuncia antes de empezar: quien pinta una barra de varias fases lo
        // necesita ya, no tras el primer archivo.
        on_progress(total);

        let mut s = stream::iter(items.into_iter().map(|item| {
            let c = c.clone();
            async move {
                let before = c.net.load(Ordering::Relaxed);
                self.fetch_one(&item, &c).await?;
                let bajado = c.net.load(Ordering::Relaxed) - before;
                c.bytes_done.fetch_add(item.size.unwrap_or(bajado), Ordering::Relaxed);
                c.files_done.fetch_add(1, Ordering::Relaxed);
                Ok::<(), DownloadError>(())
            }
        }))
        .buffer_unordered(self.concurrency);

        while let Some(r) = s.next().await {
            r?;
            on_progress(Progress {
                files_done: c.files_done.load(Ordering::Relaxed),
                bytes_done: c.bytes_done.load(Ordering::Relaxed),
                bytes_downloaded: c.net.load(Ordering::Relaxed),
                ..total
            });
        }
        Ok(Stats {
            files: total.files_total,
            skipped: c.skipped.load(Ordering::Relaxed),
            from_cache: c.from_cache.load(Ordering::Relaxed),
            resumed: c.resumed.load(Ordering::Relaxed),
            downloaded_bytes: c.net.load(Ordering::Relaxed),
        })
    }

    async fn fetch_one(&self, item: &Item, c: &Counters) -> Result<(), DownloadError> {
        if self.cancelled() {
            return Err(DownloadError::Cancelled);
        }
        if ya_esta(item, self.cancel.clone()).await {
            c.skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        // Comprobar el hash de un archivo grande también se corta al cancelar.
        if self.cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let cached = self.cache_path(item);
        if let Some(cp) = &cached {
            if cache_ok(cp, item, self.cancel.clone()).await {
                link_or_copy(cp, &item.dest).await?;
                c.from_cache.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }
        let mut intento = 0;
        loop {
            match self.stream_to_disk(item, c).await {
                Err(e) if e.pasajero() && intento < self.retries => {
                    intento += 1;
                    tokio::time::sleep(espera(intento)).await;
                }
                Err(e) => return Err(e),
                Ok(()) => break,
            }
        }
        if let Some(cp) = &cached {
            // Mejor esfuerzo: si no se puede guardar en la caché, el archivo ya está en su sitio.
            let _ = link_or_copy(&item.dest, cp).await;
        }
        Ok(())
    }

    fn cache_path(&self, item: &Item) -> Option<PathBuf> {
        let (dir, sha) = (self.cache.as_ref()?, item.sha1.as_ref()?);
        if !item.shared || sha.len() < 3 {
            return None;
        }
        let sha = sha.to_ascii_lowercase();
        Some(dir.join(&sha[..2]).join(sha))
    }

    async fn stream_to_disk(&self, item: &Item, c: &Counters) -> Result<(), DownloadError> {
        if let Some(dir) = item.dest.parent() {
            tokio::fs::create_dir_all(dir).await.map_err(io(dir))?;
        }
        let part = part_path(item);
        let resumable = item.sha1.is_some();
        let mut have = if resumable { tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0) } else { 0 };
        // Un resto tan grande como el archivo entero no es el principio de nada.
        if item.size.is_some_and(|s| have >= s) {
            let _ = tokio::fs::remove_file(&part).await;
            have = 0;
        }

        let mut req = self.client.get(&item.url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let r = req.send().await?;
        let status = r.status();
        if status.as_u16() == 416 {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(DownloadError::Status { url: item.url.clone(), status: 416 });
        }
        if !status.is_success() {
            return Err(DownloadError::Status { url: item.url.clone(), status: status.as_u16() });
        }
        let continues = have > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;

        let mut h = Sha1::new();
        let mut f = if continues {
            // El hash se siembra con lo que ya había en disco: leer local en vez de bajarlo otra vez.
            let mut prev = tokio::fs::File::open(&part).await.map_err(io(&part))?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = prev.read(&mut buf).await.map_err(io(&part))?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            c.resumed.fetch_add(1, Ordering::Relaxed);
            tokio::fs::OpenOptions::new().append(true).open(&part).await.map_err(io(&part))?
        } else {
            // Sin reanudar (o el servidor mandó el archivo entero): se empieza de cero.
            tokio::fs::File::create(&part).await.map_err(io(&part))?
        };

        let mut body = r.bytes_stream();
        while let Some(chunk) = body.next().await {
            // Lo ya escrito se queda en `part`: es lo que permite reanudar si esto falla.
            let chunk = match chunk {
                Ok(ch) => ch,
                Err(e) => {
                    let _ = f.flush().await;
                    return Err(e.into());
                }
            };
            h.update(&chunk);
            f.write_all(&chunk).await.map_err(io(&part))?;
            c.net.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            if self.cancelled() {
                let _ = f.flush().await;
                return Err(DownloadError::Cancelled);
            }
            if let Some(l) = &self.limiter {
                l.take(chunk.len() as u64).await;
            }
        }
        f.flush().await.map_err(io(&part))?;
        drop(f);
        if let Some(want) = &item.sha1 {
            let got = hex(&h.finalize());
            if !got.eq_ignore_ascii_case(want) {
                // Un resto dañado no se reaprovecha: el reintento empieza limpio.
                let _ = tokio::fs::remove_file(&part).await;
                return Err(DownloadError::Hash { path: item.dest.clone(), expected: want.clone(), got });
            }
        }
        tokio::fs::rename(&part, &item.dest).await.map_err(io(&item.dest))?;
        Ok(())
    }
}

fn espera(intento: u32) -> Duration {
    Duration::from_millis(500 * 2u64.pow(intento.saturating_sub(1)))
}

/// Dónde se va escribiendo.
///
/// Con hash, el nombre es **estable** y lleva el principio del hash: así un corte se puede
/// reanudar después, y un resto de otra versión del archivo nunca se mezcla con esta. Dentro
/// de un lote no hay dos escritores para el mismo destino (se quitan duplicados), y entre
/// procesos lo impide la instancia única de la app.
///
/// Sin hash no se puede comprobar un resto, así que no se reanuda: nombre único por descarga.
fn part_path(item: &Item) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let mut s = item.dest.as_os_str().to_owned();
    match item.sha1.as_deref().and_then(|h| h.get(..8)) {
        Some(h) => s.push(format!(".{}.part", h.to_ascii_lowercase())),
        None => s.push(format!(".{}-{}.part", std::process::id(), N.fetch_add(1, Ordering::Relaxed))),
    }
    PathBuf::from(s)
}

/// ¿El archivo ya está y es el correcto?
async fn ya_esta(item: &Item, cancel: Option<Arc<std::sync::atomic::AtomicBool>>) -> bool {
    let Ok(meta) = tokio::fs::metadata(&item.dest).await else { return false };
    if let Some(size) = item.size {
        if meta.len() != size {
            return false;
        }
    }
    match &item.sha1 {
        None => true,
        Some(want) => match sha1_file_cancellable(&item.dest, cancel).await {
            Ok(got) => got.eq_ignore_ascii_case(want),
            Err(_) => false,
        },
    }
}

/// La caché solo guarda archivos ya verificados, pero se comprueba el hash igualmente: un
/// archivo de la caché que alguien tocó no debe repartirse a todos los packs.
async fn cache_ok(path: &Path, item: &Item, cancel: Option<Arc<std::sync::atomic::AtomicBool>>) -> bool {
    let probe = Item { dest: path.to_path_buf(), ..item.clone() };
    ya_esta(&probe, cancel).await
}

/// Enlace duro si se puede (mismo disco: 0 bytes extra), copia si no.
async fn link_or_copy(from: &Path, to: &Path) -> Result<(), DownloadError> {
    if let Some(dir) = to.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(io(dir))?;
    }
    let _ = tokio::fs::remove_file(to).await;
    if tokio::fs::hard_link(from, to).await.is_ok() {
        return Ok(());
    }
    tokio::fs::copy(from, to).await.map_err(io(to))?;
    Ok(())
}

/// SHA-1 de un archivo, leyéndolo por trozos.
pub async fn sha1_file(path: &Path) -> std::io::Result<String> {
    sha1_file_cancellable(path, None).await
}

/// Como [`sha1_file`], pero mira la señal de cancelar en cada trozo.
async fn sha1_file_cancellable(
    path: &Path,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> std::io::Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut f = std::fs::File::open(&path)?;
        let mut h = Sha1::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelado"));
            }
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

    fn item(dest: &str, sha1: Option<&str>) -> Item {
        Item {
            url: "http://127.0.0.1:9/no".into(),
            dest: dest.into(),
            sha1: sha1.map(String::from),
            size: None,
            shared: false,
        }
    }

    #[test]
    fn con_hash_el_temporal_es_estable_y_sin_hash_es_unico() {
        let a = part_path(&item("a/b/lib-1.0.jar", Some("ABCDEF0123456789")));
        assert_eq!(a, part_path(&item("a/b/lib-1.0.jar", Some("abcdef0123456789"))));
        assert_eq!(a, PathBuf::from("a/b/lib-1.0.jar.abcdef01.part"));
        assert_ne!(part_path(&item("x.jar", None)), part_path(&item("x.jar", None)));
    }

    #[test]
    fn la_espera_crece() {
        assert!(espera(1) < espera(2) && espera(2) < espera(3));
    }

    #[test]
    fn que_se_reintenta_y_que_no() {
        let st = |status| DownloadError::Status { url: "x".into(), status };
        assert!(!st(404).pasajero());
        assert!(st(503).pasajero());
        assert!(st(416).pasajero(), "un resto que ya no sirve se tira y se reintenta");
    }

    #[test]
    fn la_cache_solo_para_lo_compartido_con_hash() {
        let d = Downloader::new(1).with_cache("C");
        let mut i = item("m.jar", Some("abcdef"));
        assert!(d.cache_path(&i).is_none(), "sin `shared` no pasa por la caché");
        i.shared = true;
        assert_eq!(d.cache_path(&i).unwrap(), PathBuf::from("C").join("ab").join("abcdef"));
        i.sha1 = None;
        assert!(d.cache_path(&i).is_none());
    }

    #[tokio::test]
    async fn un_archivo_con_el_hash_bueno_no_se_baja_otra_vez() {
        let dir = std::env::temp_dir().join(format!("dino-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("hola.txt");
        std::fs::write(&p, b"hola").unwrap();
        let sha = sha1_file(&p).await.unwrap();
        let mut it = item(p.to_str().unwrap(), Some(&sha));
        it.size = Some(4);
        assert!(ya_esta(&it, None).await);
        let malo = Item { sha1: Some("00".into()), ..it };
        assert!(!ya_esta(&malo, None).await);
        let _ = std::fs::remove_dir_all(dir);
    }
}
