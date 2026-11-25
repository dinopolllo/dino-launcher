//! Catálogo remoto de modpacks: resolver un código y traerse el `.mrpack`.
//!
//! Hasta ahora `pack.json` guardaba una **ruta del disco de quien lo importó**, así que
//! el launcher solo servía en esa máquina. El catálogo es lo que lo vuelve repartible:
//! un amigo escribe un código, se resuelve contra un archivo público y el `.mrpack`
//! acaba en su propia carpeta de datos, donde sí existe.
//!
//! Vive en el repo público de descargas y se lee de ahí (ver [`fuentes`]), no como asset de una
//! release: publicar un pack no debe obligar a publicar una versión del launcher, y
//! actualizar el catálogo tiene que ser un commit y nada más.

use serde::{Deserialize, Serialize};

/// De dónde se lee el catálogo, por orden.
///
/// **Primero la API de GitHub y luego el CDN.** El CDN (`raw.githubusercontent.com`)
/// guarda cada archivo cinco minutos, y un parámetro en la URL no se lo salta —medido—:
/// un pack recién publicado daba «ese código no existe» durante todo ese rato. La API
/// sirve lo que hay, pero sin autenticar da 60 peticiones por hora por IP, y unos amigos
/// jugando en la misma red se las reparten. Por eso el CDN queda detrás: un catálogo de
/// hace cinco minutos es mejor que ninguno.
///
/// Al compilar se puede apuntar a un archivo propio con `DINO_CATALOG_URL`, y entonces
/// es la única fuente: así se prueba sin tocar el de verdad.
pub fn fuentes() -> Vec<&'static str> {
    match option_env!("DINO_CATALOG_URL") {
        Some(u) => vec![u],
        None => vec![
            "https://api.github.com/repos/dinopolllo/dino-launcher-releases/contents/catalog.json?ref=main",
            "https://raw.githubusercontent.com/dinopolllo/dino-launcher-releases/main/catalog.json",
        ],
    }
}

/// Cliente con `User-Agent`.
///
/// La API de GitHub responde 403 a lo que llega sin él, y `reqwest` no manda ninguno
/// por defecto.
fn cliente() -> Result<reqwest::Client, CatalogError> {
    Ok(reqwest::Client::builder().user_agent(concat!("dino-launcher/", env!("CARGO_PKG_VERSION"))).build()?)
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("error de red: {0}")]
    Http(#[from] reqwest::Error),
    #[error("error de disco: {0}")]
    Io(#[from] std::io::Error),
    #[error("el catálogo no es JSON válido: {0}")]
    Json(#[from] serde_json::Error),
    #[error("formato de catálogo {0} no soportado (esperaba 1)")]
    Formato(u32),
    #[error("no encontré ningún modpack con ese código")]
    NoExiste,
    #[error("la descarga vino mal: el hash no coincide")]
    HashMalo,
    #[error("descarga fallida ({0})")]
    Estado(u16),
    #[error("lo descargado no es una imagen")]
    NoEsImagen,
    #[error("cancelado")]
    Cancelado,
}

/// Un modpack publicado.
///
/// Casi todo es opcional salvo lo que hace falta para instalarlo: quien publica no
/// debería tener que rellenar metadatos para que el pack funcione.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PackRemoto {
    pub code: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub mrpack: String,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub sha512: Option<String>,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub minecraft: Option<String>,
    #[serde(default)]
    pub loader: Option<String>,
    #[serde(default)]
    pub mods: u32,
    #[serde(default)]
    pub icon: Option<String>,
    /// Lo opcional: actividad de Discord, notas de parche y tema. Un pack que no lo trae
    /// se comporta como siempre. Va aplanado: `presence`, `theme` y `notes` cuelgan del
    /// pack en el JSON del catálogo.
    #[serde(flatten)]
    pub extras: dino_core::pack_meta::PackExtras,
}

#[derive(Debug, Deserialize)]
pub struct Catalogo {
    #[serde(rename = "formatVersion")]
    pub format_version: u32,
    #[serde(default)]
    pub packs: Vec<PackRemoto>,
}

/// Deja un código en su forma canónica.
///
/// Se normaliza para que **nadie falle por cómo lo escribió**: el código se comparte
/// por chat, se copia a mano y se teclea mal. `dino smp`, `DINO-SMP` y `Dino_Smp` son
/// el mismo pack, y que no lo fueran sería una frustración gratuita.
pub fn normalizar_codigo(bruto: &str) -> String {
    bruto.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// Saca el código de lo que sea que hayan pegado.
///
/// Se comparte por código **y por link**, así que la misma casilla tiene que aceptar
/// las dos cosas: pegar la URL entera y que funcione es lo que la gente va a hacer.
pub fn codigo_de_texto(texto: &str) -> String {
    let t = texto.trim();
    // De un link interesa el último tramo: `.../p/DINO-SMP`, `...#DINO-SMP`.
    let ultimo = t.rsplit(['/', '#', '?', '=']).find(|s| !s.trim().is_empty()).unwrap_or(t);
    normalizar_codigo(ultimo)
}

/// Saca el código de un link de invitación: `dinolauncher://join/CODIGO`.
///
/// Solo del protocolo del launcher: cualquier otra cosa que llegue como argumento al
/// arrancar no es una invitación. Y un link sin código —`dinolauncher://join/`— tampoco,
/// en vez de acabar buscando un pack llamado «JOIN» que diría que no existe.
pub fn codigo_de_invitacion(url: &str) -> Option<String> {
    let resto = url.trim().strip_prefix("dinolauncher://")?;
    let codigo = codigo_de_texto(resto);
    (!codigo.is_empty() && codigo != "JOIN").then_some(codigo)
}

pub fn parse_catalog(json: &str) -> Result<Catalogo, CatalogError> {
    let c: Catalogo = serde_json::from_str(json)?;
    if c.format_version != 1 {
        return Err(CatalogError::Formato(c.format_version));
    }
    Ok(c)
}

/// Busca un pack por su código, ya normalizado a los dos lados.
pub fn buscar<'a>(cat: &'a Catalogo, codigo: &str) -> Option<&'a PackRemoto> {
    let buscado = normalizar_codigo(codigo);
    cat.packs.iter().find(|p| normalizar_codigo(&p.code) == buscado)
}

/// Trae el catálogo de la red, probando las fuentes en orden.
///
/// Solo se pasa a la siguiente si la anterior **falla**. Si la API responde y el código
/// no está, no está: preguntarle al CDN daría una respuesta más vieja, no una mejor.
pub async fn fetch_catalog() -> Result<Catalogo, CatalogError> {
    let c = cliente()?;
    let mut ultimo = None;
    for url in fuentes() {
        match leer_catalogo(&c, url).await {
            Ok(cat) => return Ok(cat),
            Err(e) => {
                tracing::warn!("{url}: {e}");
                ultimo = Some(e);
            }
        }
    }
    Err(ultimo.unwrap_or(CatalogError::NoExiste))
}

async fn leer_catalogo(c: &reqwest::Client, url: &str) -> Result<Catalogo, CatalogError> {
    let r = c
        .get(url)
        // Con esto la API devuelve el archivo tal cual; sin él, un JSON con el contenido
        // en base64. Al CDN le da igual.
        .header(reqwest::header::ACCEPT, "application/vnd.github.raw")
        .send()
        .await?;
    if !r.status().is_success() {
        return Err(CatalogError::Estado(r.status().as_u16()));
    }
    parse_catalog(&r.text().await?)
}

/// Resuelve un código contra el catálogo.
pub async fn resolver(texto: &str) -> Result<PackRemoto, CatalogError> {
    let cat = fetch_catalog().await?;
    buscar(&cat, &codigo_de_texto(texto)).cloned().ok_or(CatalogError::NoExiste)
}

/// Descarga el `.mrpack` al disco, informando del avance.
///
/// Se verifica el hash cuando el catálogo lo trae: un `.mrpack` a medias no da error
/// al abrirse, da un pack al que le faltan mods, y eso se manifiesta mucho después
/// como un crash del juego que nadie relaciona con la descarga.
pub async fn descargar_pack(
    pack: &PackRemoto,
    destino: &std::path::Path,
    progreso: &(dyn Fn(u64, u64) + Send + Sync),
    cancelar: &std::sync::atomic::AtomicBool,
) -> Result<(), CatalogError> {
    if let Some(p) = destino.parent() {
        std::fs::create_dir_all(p)?;
    }
    let parcial = ruta_parcial(pack, destino);
    let mut ya = std::fs::metadata(&parcial).map(|m| m.len()).unwrap_or(0);

    // Un resto más grande que el archivo entero no es el principio de nada: sobra.
    if pack.bytes > 0 && ya >= pack.bytes {
        let _ = std::fs::remove_file(&parcial);
        ya = 0;
    }

    // Dos intentos como mucho: si el servidor rechaza el tramo que pedimos —porque lo
    // que había sobra o ya no cuadra—, se tira ese resto y se baja entero.
    for intento in 0..2 {
        let desde = if intento == 0 { ya } else { 0 };
        match bajar(pack, &parcial, desde, progreso, cancelar).await {
            Err(CatalogError::Estado(416)) if intento == 0 => {
                tracing::info!("lo que había a medias ya no sirve: lo bajo entero");
                let _ = std::fs::remove_file(&parcial);
            }
            Err(e) => return Err(e),
            Ok(hex) => {
                if let Some(esperado) = pack.sha512.as_deref() {
                    if hex != esperado.to_lowercase() {
                        // Un resto dañado no se reaprovecha: se quita para que el
                        // siguiente intento empiece limpio en vez de repetir el fallo.
                        let _ = std::fs::remove_file(&parcial);
                        return Err(CatalogError::HashMalo);
                    }
                }
                std::fs::rename(&parcial, destino)?;
                return Ok(());
            }
        }
    }
    Err(CatalogError::Estado(416))
}

/// Dónde se guarda lo que se va bajando.
///
/// El nombre lleva el principio del hash esperado, así que **solo se reanuda lo que es
/// de esta versión**: un resto de la anterior no se puede mezclar con esta. Sin hash en
/// el catálogo no hay forma de comprobar nada, y entonces tampoco se reanuda.
fn ruta_parcial(pack: &PackRemoto, destino: &std::path::Path) -> std::path::PathBuf {
    match pack.sha512.as_deref().and_then(|h| h.get(..8)) {
        Some(h) => destino.with_extension(format!("{h}.parcial")),
        None => destino.with_extension("parcial"),
    }
}

/// Baja el archivo a `parcial`, continuando desde `desde`, y devuelve su hash completo.
///
/// Se escribe a disco según llega y se va hasheando por el camino: un modpack pesa
/// cientos de megas y tenerlo entero en RAM solo para comprobarlo es un pico que no
/// hace falta en la máquina de nadie.
async fn bajar(
    pack: &PackRemoto,
    parcial: &std::path::Path,
    desde: u64,
    progreso: &(dyn Fn(u64, u64) + Send + Sync),
    cancelar: &std::sync::atomic::AtomicBool,
) -> Result<String, CatalogError> {
    use futures_util::StreamExt;
    use sha2::{Digest, Sha512};
    use std::io::{Read, Write};

    let mut req = cliente()?.get(&pack.mrpack);
    // Reanudar es lo que separa «se cortó internet y vuelvo a empezar los 386 MB» de
    // «sigo donde iba». El servidor puede no admitirlo: entonces responde 200 con el
    // archivo entero y aquí se empieza de cero sin más.
    if desde > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={desde}-"));
    }
    let r = req.send().await?;
    if !r.status().is_success() {
        return Err(CatalogError::Estado(r.status().as_u16()));
    }
    let continua = desde > 0 && r.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let total =
        if continua { desde + r.content_length().unwrap_or(0) } else { r.content_length().unwrap_or(pack.bytes) };

    let mut h = Sha512::new();
    let mut bajado = 0u64;
    let mut f = if continua {
        // El hash se siembra con lo que ya había en disco: una lectura local contra
        // volver a bajarlo todo.
        let mut previo = std::fs::File::open(parcial)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = previo.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        bajado = desde;
        tracing::info!("reanudo la descarga en {desde} B");
        std::fs::OpenOptions::new().append(true).open(parcial)?
    } else {
        std::fs::File::create(parcial)?
    };

    progreso(bajado, total);
    let mut stream = r.bytes_stream();
    while let Some(trozo) = stream.next().await {
        // Lo ya escrito se queda en disco: es justo lo que permite reanudar en vez de
        // volver a empezar si esto falla a la mitad.
        let trozo = trozo?;
        h.update(&trozo);
        f.write_all(&trozo)?;
        bajado += trozo.len() as u64;
        progreso(bajado, total);
        // Lo bajado se queda en el `.parcial`: la próxima vez se reanuda desde ahí.
        if cancelar.load(std::sync::atomic::Ordering::SeqCst) {
            f.flush()?;
            return Err(CatalogError::Cancelado);
        }
    }
    f.flush()?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Descarga el icono publicado del pack.
///
/// No sale del `.mrpack`: al publicar se puede cambiar, y el catálogo lo sirve aparte.
/// Sin traerlo por su lado, el icono que eligió quien publica solo se vería en la ficha
/// del código y nunca en la biblioteca de quien lo instala.
///
/// Devuelve `false` si el pack no tiene icono publicado.
pub async fn descargar_icono(pack: &PackRemoto, destino: &std::path::Path) -> Result<bool, CatalogError> {
    let Some(url) = pack.icon.as_deref().filter(|u| !u.trim().is_empty()) else {
        return Ok(false);
    };
    let r = cliente()?.get(url).send().await?;
    if !r.status().is_success() {
        return Err(CatalogError::Estado(r.status().as_u16()));
    }
    let datos = r.bytes().await?;
    // Un 200 no garantiza una imagen: un proxy o una página de error también lo dan, y
    // guardar eso dejaría la tarjeta con un icono roto en vez del dino de siempre.
    if !es_imagen(&datos) {
        return Err(CatalogError::NoEsImagen);
    }
    if let Some(p) = destino.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(destino, &datos)?;
    Ok(true)
}

/// PNG, JPEG o WebP, por sus primeros bytes.
///
/// Los tres, y no solo PNG, porque son los que acepta el selector de icono del panel de
/// publicar; el archivo se sube como `.png` pero dentro va lo que eligiera el admin.
fn es_imagen(b: &[u8]) -> bool {
    b.starts_with(b"\x89PNG\r\n\x1a\n")
        || b.starts_with(&[0xFF, 0xD8, 0xFF])
        || (b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP")
}

#[allow(dead_code)] // queda para comparar en los tests
fn hex512(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha512};
    let mut h = Sha512::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAT: &str = r#"{
      "formatVersion": 1,
      "packs": [
        { "code": "DINO-SMP", "id": "dino-smp", "name": "Dino SMP", "version": "4.3.1",
          "mrpack": "https://ejemplo/dino-smp.mrpack", "server": "mc.dinolab.gg",
          "sha512": "abc", "bytes": 481920, "minecraft": "1.21.4",
          "loader": "Fabric 0.16.9", "mods": 44 }
      ]
    }"#;

    #[test]
    fn el_codigo_perdona_como_lo_escriban() {
        // Se comparte por chat y se teclea a mano: fallar por un guion o una mayúscula
        // sería una frustración que no aporta nada.
        for variante in ["DINO-SMP", "dino smp", "Dino_Smp", " dinosmp ", "dino-smp"] {
            assert_eq!(normalizar_codigo(variante), "DINOSMP", "falló con {variante:?}");
        }
    }

    #[test]
    fn tambien_se_acepta_pegar_el_link() {
        for link in [
            "https://dinolab.gg/p/DINO-SMP",
            "https://github.com/x/y#dino-smp",
            "dinolauncher://join/Dino-Smp",
            "DINO-SMP",
        ] {
            assert_eq!(codigo_de_texto(link), "DINOSMP", "falló con {link:?}");
        }
    }

    #[test]
    fn el_link_de_invitacion_da_el_codigo() {
        for link in [
            "dinolauncher://join/AMEN",
            "dinolauncher://join/amen/",
            "dinolauncher://AMEN",
            "dinolauncher://join?c=Amen",
        ] {
            assert_eq!(codigo_de_invitacion(link).as_deref(), Some("AMEN"), "falló con {link:?}");
        }
    }

    #[test]
    fn lo_que_no_es_una_invitacion_no_da_codigo() {
        // Otro protocolo, o el nuestro sin nada detrás: ninguno es una invitación.
        for texto in ["https://github.com/x/y#AMEN", "AMEN", "dinolauncher://", "dinolauncher://join/", "--flag"] {
            assert!(codigo_de_invitacion(texto).is_none(), "{texto:?} no es una invitación");
        }
    }

    #[test]
    fn un_link_con_barra_final_no_da_codigo_vacio() {
        // `rsplit` sobre "…/DINO-SMP/" devuelve "" primero; hay que saltarlo o el
        // código quedaría vacío y no encontraría nada, sin decir por qué.
        assert_eq!(codigo_de_texto("https://dinolab.gg/p/DINO-SMP/"), "DINOSMP");
    }

    #[test]
    fn lee_el_catalogo_y_encuentra_el_pack() {
        let c = parse_catalog(CAT).unwrap();
        let p = buscar(&c, "dino smp").expect("debería encontrarlo escrito de cualquier forma");
        assert_eq!(p.id, "dino-smp");
        assert_eq!(p.version, "4.3.1");
        assert_eq!(p.server.as_deref(), Some("mc.dinolab.gg"));
        assert_eq!(p.mods, 44);
    }

    #[test]
    fn un_codigo_que_no_existe_no_devuelve_otro_pack() {
        let c = parse_catalog(CAT).unwrap();
        assert!(buscar(&c, "NO-EXISTE").is_none());
        // Y un prefijo tampoco: coincidir a medias sería instalar algo que nadie pidió.
        assert!(buscar(&c, "DINO").is_none());
    }

    #[test]
    fn rechaza_formatos_futuros_en_vez_de_adivinar() {
        let j = CAT.replace("\"formatVersion\": 1", "\"formatVersion\": 2");
        assert!(matches!(parse_catalog(&j), Err(CatalogError::Formato(2))));
    }

    #[test]
    fn los_metadatos_son_opcionales() {
        // Quien publica no debería tener que rellenar fichas para que el pack funcione.
        let minimo = r#"{ "formatVersion": 1, "packs": [
            { "code": "X1", "id": "x", "name": "X", "version": "1", "mrpack": "https://e/x.mrpack" }
        ] }"#;
        let c = parse_catalog(minimo).unwrap();
        let p = buscar(&c, "x1").unwrap();
        assert!(p.server.is_none() && p.sha512.is_none() && p.minecraft.is_none());
        assert_eq!(p.bytes, 0);
    }

    /// Toca la red y el catálogo de verdad; se corre a mano:
    /// `cargo test -- --ignored resuelve_y_descarga_del_catalogo_real --nocapture`
    #[tokio::test]
    #[ignore]
    async fn resuelve_y_descarga_del_catalogo_real() {
        // Escrito como lo escribiría alguien a quien se lo pasaron por chat.
        let p = resolver("dino smp").await.expect("debería resolver el código");
        println!("  resuelto: {} v{} · {} mods · {} B", p.name, p.version, p.mods, p.bytes);
        assert_eq!(p.id, "dinosmp");
        assert!(p.mods > 0, "el catálogo debería decir cuántos mods trae");

        let destino = std::env::temp_dir().join("dinosmp-prueba.mrpack");
        let _ = std::fs::remove_file(&destino);
        let no = std::sync::atomic::AtomicBool::new(false);
        descargar_pack(
            &p,
            &destino,
            &|bajado, total| {
                if bajado == total {
                    println!("  descargado: {bajado} de {total} B");
                }
            },
            &no,
        )
        .await
        .expect("descarga y verificación de hash");

        // Que sea un zip con el índice dentro es lo que prueba que llegó entero.
        let datos = std::fs::read(&destino).unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(datos)).expect("es un zip");
        assert!(z.by_name("modrinth.index.json").is_ok(), "falta el índice");
        let _ = std::fs::remove_file(&destino);
    }

    #[test]
    fn reconoce_los_formatos_que_acepta_el_selector_de_icono() {
        assert!(es_imagen(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"));
        assert!(es_imagen(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]));
        assert!(es_imagen(b"RIFF\x24\0\0\0WEBPVP8 "));
    }

    #[test]
    fn una_pagina_de_error_no_se_guarda_como_icono() {
        // Un 200 con HTML es justo lo que devuelve un portal cautivo o un proxy.
        assert!(!es_imagen(b"<!DOCTYPE html><html>"));
        assert!(!es_imagen(b"404: Not Found"));
        assert!(!es_imagen(b""));
        assert!(!es_imagen(b"RIFF\x24\0\0\0WAVE"), "un RIFF que no es WebP tampoco");
    }

    #[test]
    fn la_api_va_antes_que_el_cdn() {
        // Leer primero el CDN, que guarda el catálogo cinco minutos, era lo que hacía
        // que un pack recién publicado diera «ese código no existe».
        if option_env!("DINO_CATALOG_URL").is_some() {
            return;
        }
        let f = fuentes();
        assert!(f[0].starts_with("https://api.github.com/"), "primero: {}", f[0]);
        assert!(f.last().unwrap().starts_with("https://raw.githubusercontent.com/"));
    }

    #[test]
    fn lo_que_se_baja_a_medias_lleva_el_hash_en_el_nombre() {
        // Así un resto de la versión anterior no se puede confundir con el principio de
        // la nueva: son archivos distintos.
        let mut p = buscar(&parse_catalog(CAT).unwrap(), "dinosmp").unwrap().clone();
        let destino = std::path::Path::new("/packs/dinosmp.mrpack");
        p.sha512 = Some("abcdef0123456789".into());
        let a = ruta_parcial(&p, destino);
        p.sha512 = Some("0123456789abcdef".into());
        let b = ruta_parcial(&p, destino);
        assert_ne!(a, b);
        assert!(a.to_string_lossy().ends_with(".parcial"), "{a:?}");

        // Sin hash no se reanuda nada, así que basta con un nombre fijo.
        p.sha512 = None;
        assert!(ruta_parcial(&p, destino).to_string_lossy().ends_with(".parcial"));
    }

    #[test]
    fn un_catalogo_sin_packs_no_es_un_error() {
        let c = parse_catalog(r#"{ "formatVersion": 1 }"#).unwrap();
        assert!(c.packs.is_empty());
        assert!(buscar(&c, "loquesea").is_none());
    }
}
