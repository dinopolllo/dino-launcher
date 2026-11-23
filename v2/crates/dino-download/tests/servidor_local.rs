//! Criterio de aceptación de la etapa 5, sin red: un servidor HTTP local, dentro del test.
//!
//! - un lote de 200 archivos llega entero y verificado por SHA-1;
//! - un archivo grande cuyo envío se corta a mitad se **reanuda** con `Range` y no se
//!   vuelve a bajar entero;
//! - un mod repetido en dos packs sale de la caché compartida sin tocar la red;
//! - el límite de velocidad se respeta.

use dino_download::{Downloader, Item};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Contenido determinista de cada archivo.
fn contenido(n: u64, size: usize) -> Vec<u8> {
    (0..size).map(|i| ((i as u64 * 31 + n) % 251) as u8).collect()
}

fn sha1(b: &[u8]) -> String {
    Sha1::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Default)]
struct Registro {
    /// Peticiones por ruta, con el `Range` que trajeron.
    peticiones: HashMap<String, Vec<Option<u64>>>,
    /// Rutas cuyo primer envío se corta a la mitad.
    cortar: Vec<String>,
    /// Bytes de cuerpo enviados en total.
    enviados: u64,
}

/// Servidor HTTP/1.1 mínimo: `GET /f/<n>/<tamaño>`, con `Range: bytes=N-`.
async fn servidor(reg: Arc<Mutex<Registro>>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            let reg = reg.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let mut req = Vec::new();
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let req = String::from_utf8_lossy(&req).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let range: Option<u64> = req.lines().find_map(|l| {
                    let l = l.to_ascii_lowercase();
                    l.strip_prefix("range: bytes=").and_then(|r| r.trim_end_matches('-').parse().ok())
                });
                let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
                let (Some(n), Some(size)) = (
                    parts.get(1).and_then(|x| x.parse::<u64>().ok()),
                    parts.get(2).and_then(|x| x.parse::<usize>().ok()),
                ) else {
                    let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n").await;
                    return;
                };
                let body = contenido(n, size);
                let (cortar, desde) = {
                    let mut r = reg.lock().unwrap();
                    let primera = !r.peticiones.contains_key(&path);
                    r.peticiones.entry(path.clone()).or_default().push(range);
                    (primera && r.cortar.contains(&path), range.unwrap_or(0) as usize)
                };
                let resto = &body[desde.min(body.len())..];
                let head = if range.is_some() {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\ncontent-range: bytes {}-{}/{}\r\n\r\n",
                        resto.len(),
                        desde,
                        body.len() - 1,
                        body.len()
                    )
                } else {
                    format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len())
                };
                let _ = s.write_all(head.as_bytes()).await;
                // Un corte de red: se promete el archivo entero y se manda la mitad.
                let manda = if cortar { &resto[..resto.len() / 2] } else { resto };
                let _ = s.write_all(manda).await;
                reg.lock().unwrap().enviados += manda.len() as u64;
                let _ = s.shutdown().await;
            });
        }
    });
    port
}

fn tmp(nombre: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dino-dl-it-{}-{nombre}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn item(port: u16, dir: &std::path::Path, n: u64, size: usize) -> Item {
    Item {
        url: format!("http://127.0.0.1:{port}/f/{n}/{size}"),
        dest: dir.join(format!("{n}.bin")),
        sha1: Some(sha1(&contenido(n, size))),
        size: Some(size as u64),
        shared: false,
    }
}

#[tokio::test]
async fn doscientos_archivos_verificados_y_la_segunda_vez_nada() {
    let reg = Arc::new(Mutex::new(Registro::default()));
    let port = servidor(reg.clone()).await;
    let dir = tmp("200");
    let items: Vec<Item> = (0..200).map(|n| item(port, &dir, n, 1_000 + (n as usize * 137) % 40_000)).collect();
    let total: u64 = items.iter().filter_map(|i| i.size).sum();

    let dl = Downloader::new(16);
    let st = dl.fetch_all(items.clone(), &|_| {}).await.unwrap();
    assert_eq!((st.files, st.skipped), (200, 0));
    assert_eq!(st.downloaded_bytes, total);
    for it in &items {
        let b = std::fs::read(&it.dest).unwrap();
        assert_eq!(Some(sha1(&b)), it.sha1, "{}", it.dest.display());
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 200, "no quedan temporales");

    let st = dl.fetch_all(items, &|_| {}).await.unwrap();
    assert_eq!((st.skipped, st.downloaded_bytes), (200, 0), "lo que está bien no se vuelve a bajar");
}

#[tokio::test]
async fn un_corte_a_mitad_se_reanuda_sin_volver_a_empezar() {
    let reg = Arc::new(Mutex::new(Registro::default()));
    let port = servidor(reg.clone()).await;
    let dir = tmp("corte");
    let size = 2 * 1024 * 1024;
    let it = item(port, &dir, 7, size);
    let ruta = format!("/f/7/{size}");
    reg.lock().unwrap().cortar.push(ruta.clone());

    let st = Downloader::new(4).fetch_all(vec![it.clone()], &|_| {}).await.unwrap();
    assert_eq!(st.resumed, 1, "el reintento tiene que reanudar");
    assert_eq!(Some(sha1(&std::fs::read(&it.dest).unwrap())), it.sha1);

    let r = reg.lock().unwrap();
    let pet = &r.peticiones[&ruta];
    assert_eq!(pet.len(), 2);
    assert_eq!(pet[0], None);
    assert!(pet[1].is_some_and(|desde| desde > 0), "la segunda petición pide desde donde quedó: {pet:?}");
    // Se mandó la mitad y luego el resto: ni un archivo entero de más.
    assert_eq!(r.enviados, size as u64);
}

#[tokio::test]
async fn un_mod_repetido_sale_de_la_cache_sin_tocar_la_red() {
    let reg = Arc::new(Mutex::new(Registro::default()));
    let port = servidor(reg.clone()).await;
    let base = tmp("cache");
    let dl = Downloader::new(4).with_cache(base.join("cache"));
    let mut a = item(port, &base.join("pack-a/mods"), 3, 50_000);
    a.shared = true;
    let mut b = item(port, &base.join("pack-b/mods"), 3, 50_000);
    b.shared = true;

    dl.fetch_all(vec![a.clone()], &|_| {}).await.unwrap();
    let st = dl.fetch_all(vec![b.clone()], &|_| {}).await.unwrap();
    assert_eq!((st.from_cache, st.downloaded_bytes), (1, 0));
    assert_eq!(std::fs::read(&b.dest).unwrap(), std::fs::read(&a.dest).unwrap());
    assert_eq!(reg.lock().unwrap().peticiones.values().map(Vec::len).sum::<usize>(), 1);
}

#[tokio::test]
async fn el_limite_de_velocidad_se_respeta() {
    let reg = Arc::new(Mutex::new(Registro::default()));
    let port = servidor(reg).await;
    let dir = tmp("limite");
    let size = 600 * 1024;
    let t0 = std::time::Instant::now();
    Downloader::new(2)
        .with_rate_limit(Some(1024 * 1024))
        .fetch_all(vec![item(port, &dir, 1, size)], &|_| {})
        .await
        .unwrap();
    // 600 KB a 1 MB/s: al menos ~0,55 s (con margen para el primer trozo).
    assert!(t0.elapsed().as_secs_f64() >= 0.45, "fue demasiado rápido: {:?}", t0.elapsed());
}
