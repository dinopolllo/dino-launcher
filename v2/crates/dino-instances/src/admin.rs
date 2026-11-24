//! Publicar modpacks al catálogo, desde el propio launcher.
//!
//! **Se apoya en el `gh` que quien publica ya tiene autenticado.** Es una decisión
//! deliberada: la alternativa era pedir un token y guardarlo en disco, y un launcher
//! que se reparte a amigos no es sitio para eso. Como efecto secundario el panel se
//! limita solo — sin `gh` autenticado no hay nada que hacer ahí, y un amigo no lo
//! tiene.
//!
//! La pestaña está además escondida tras una clave (ver [`termina_en_la_clave`]). No
//! se solapan: el `gh` decide quién **puede** publicar; la clave, quién ve el panel.
//!
//! El `.mrpack` va como **adjunto de una release** del repo de descargas, no por la API
//! de contenidos: ver [`subir_pack`]. El catálogo y el icono, que son pequeños, sí.
//!
//! Los metadatos (versión, Minecraft, loader, número de mods, tamaño, hash) salen del
//! propio `.mrpack`. Escribirlos a mano se desincroniza a la primera.

use crate::catalog::PackRemoto;
use serde::{Deserialize, Serialize};

/// Repo público donde viven el catálogo y los packs.
pub const RELEASES_REPO: &str = match option_env!("DINO_RELEASES_REPO") {
    Some(r) => r,
    None => "dinopolllo/dino-launcher-releases",
};

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("error de disco: {0}")]
    Io(#[from] std::io::Error),
    #[error("el .mrpack está corrupto: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("no es JSON válido: {0}")]
    Json(#[from] serde_json::Error),
    #[error("el archivo no trae modrinth.index.json: ¿seguro que es un .mrpack?")]
    NoIndex,
    #[error("hace falta el CLI de GitHub autenticado para publicar")]
    SinGh,
    #[error("GitHub respondió: {0}")]
    Gh(String),
    #[error("{0}")]
    Campo(String),
}

/// Lo que se va a publicar: lo leído del archivo más lo que decide quien publica.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Borrador {
    pub code: String,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub server: Option<String>,
    pub minecraft: String,
    pub loader: String,
    pub mods: u32,
    pub bytes: u64,
    pub sha512: String,
    /// Icono del pack en base64, si el `.mrpack` lo traía.
    #[serde(default)]
    pub icon_b64: Option<String>,
}

/// Convierte un nombre en algo que valga como identificador y como nombre de archivo.
pub fn slug(nombre: &str) -> String {
    let s: String =
        nombre.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.trim_matches('-').to_string();
    // Varios separadores seguidos quedan feos en una URL y no aportan nada.
    let mut out = String::with_capacity(s.len());
    let mut guion = false;
    for c in s.chars() {
        if c == '-' {
            if !guion {
                out.push(c);
            }
            guion = true;
        } else {
            out.push(c);
            guion = false;
        }
    }
    if out.is_empty() {
        "pack".into()
    } else {
        out
    }
}

/// Hash del archivo leyéndolo a trozos.
///
/// Un `.mrpack` puede pesar cientos de megas: cargarlo entero en memoria solo para
/// medirlo no hace falta, y ahora que no se manda en base64 tampoco hay ningún otro
/// motivo para tenerlo todo dentro.
fn hex512_de_archivo(ruta: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha512};
    use std::io::Read;

    let mut f = std::fs::File::open(ruta)?;
    let mut h = Sha512::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[allow(dead_code)] // lo usan los tests
fn hex512(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha512};
    let mut h = Sha512::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Lee un `.mrpack` y prepara el borrador con todo lo que se puede deducir.
pub fn leer_mrpack(ruta: &std::path::Path) -> Result<Borrador, AdminError> {
    use std::io::Read;

    let datos = std::fs::read(ruta)?;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(&datos))?;

    let indice: serde_json::Value = {
        let mut f = z.by_name("modrinth.index.json").map_err(|_| AdminError::NoIndex)?;
        let mut t = String::new();
        f.read_to_string(&mut t)?;
        serde_json::from_str(&t)?
    };

    let nombre = indice["name"].as_str().unwrap_or("Modpack").to_string();
    let version = indice["versionId"].as_str().unwrap_or("1.0.0").to_string();
    let deps = &indice["dependencies"];
    let minecraft = deps["minecraft"].as_str().unwrap_or("").to_string();

    // El loader es la dependencia que no es Minecraft; se enseña con nombre humano.
    let loader = deps
        .as_object()
        .and_then(|o| o.iter().find(|(k, _)| k.as_str() != "minecraft"))
        .map(|(k, v)| {
            let bonito = match k.as_str() {
                "fabric-loader" => "Fabric",
                "quilt-loader" => "Quilt",
                "forge" => "Forge",
                "neoforge" => "NeoForge",
                otro => otro,
            };
            format!("{bonito} {}", v.as_str().unwrap_or(""))
        })
        .unwrap_or_default()
        .trim()
        .to_string();

    let mods = indice["files"].as_array().map(|a| a.len()).unwrap_or(0) as u32;

    let icon_b64 = z.by_name("icon.png").ok().and_then(|mut f| {
        use base64::Engine;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        (!buf.is_empty()).then(|| base64::engine::general_purpose::STANDARD.encode(&buf))
    });

    Ok(Borrador {
        code: crate::catalog::normalizar_codigo(&nombre),
        id: slug(&nombre),
        name: nombre,
        version,
        server: None,
        minecraft,
        loader,
        mods,
        bytes: datos.len() as u64,
        sha512: hex512(&datos),
        icon_b64,
    })
}

/// Mete el pack en el catálogo, sustituyendo el que hubiera con ese código.
///
/// Pura para poder probarla: es la que, mal hecha, duplicaría un pack o borraría los
/// demás. Actualizar y publicar por primera vez son la misma operación a propósito —
/// que sean dos caminos distintos es justo cómo se acaba con dos entradas del mismo.
pub fn merge_catalogo(existente: &str, nuevo: &PackRemoto) -> Result<String, AdminError> {
    let mut raiz: serde_json::Value = if existente.trim().is_empty() {
        serde_json::json!({ "formatVersion": 1, "packs": [] })
    } else {
        serde_json::from_str(existente)?
    };
    raiz["formatVersion"] = serde_json::json!(1);
    if !raiz["packs"].is_array() {
        raiz["packs"] = serde_json::json!([]);
    }

    let codigo = crate::catalog::normalizar_codigo(&nuevo.code);
    let lista = raiz["packs"].as_array_mut().expect("acabamos de asegurarlo");
    lista.retain(|p| crate::catalog::normalizar_codigo(p["code"].as_str().unwrap_or("")) != codigo);
    // Dos códigos no pueden compartir id: el id nombra sus archivos en el repo, y
    // compartirlo es subir uno encima del otro. `publicar` ya elige uno libre; esto es
    // la red para cualquier otro camino que llegue aquí.
    if lista.iter().any(|p| p["id"].as_str() == Some(nuevo.id.as_str())) {
        return Err(AdminError::Campo(format!("El id «{}» ya es de otro pack del catálogo.", nuevo.id)));
    }

    // El código se guarda ya normalizado, y se hace **aquí** y no en quien llama: es
    // donde se escribe, así que es donde la invariante tiene que sostenerse pase lo
    // que pase. Guardarlo crudo funcionaría igual al buscar —`buscar` normaliza los
    // dos lados— pero dejaría el catálogo enseñando el código con la forma con que
    // se tecleó ese día.
    let mut entrada = serde_json::to_value(nuevo)?;
    entrada["code"] = serde_json::json!(codigo);
    lista.push(entrada);

    Ok(serde_json::to_string_pretty(&raiz)? + "\n")
}

// ------------------------------------------------------------------ gh

/// Dónde está el `gh`.
///
/// No basta con confiar en el PATH: su instalador de Windows **no lo añade**, así que
/// en la máquina donde se escribió esto `gh` funcionaba en una terminal pero
/// `Command::new("gh")` fallaba, y el panel simplemente no habría aparecido nunca sin
/// decir por qué. Se prueban también los sitios donde se instala.
fn gh_bin() -> &'static str {
    use std::sync::OnceLock;
    static ELEGIDO: OnceLock<String> = OnceLock::new();

    ELEGIDO.get_or_init(|| {
        const CANDIDATOS: &[&str] = &[
            "gh",
            r"C:\Program Files\GitHub CLI\gh.exe",
            r"C:\Program Files (x86)\GitHub CLI\gh.exe",
            "/opt/homebrew/bin/gh",
            "/usr/local/bin/gh",
        ];
        for c in CANDIDATOS {
            if probar(c) {
                return c.to_string();
            }
        }
        // Los instaladores por usuario lo dejan aquí.
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let p = format!(r"{local}\GitHubCLI\gh.exe");
            if probar(&p) {
                return p;
            }
        }
        "gh".to_string()
    })
}

fn probar(bin: &str) -> bool {
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn gh(args: &[&str], entrada: Option<&str>) -> Result<String, AdminError> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(gh_bin());
    cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut hijo = cmd.spawn().map_err(|_| AdminError::SinGh)?;
    if let Some(txt) = entrada {
        hijo.stdin.as_mut().ok_or(AdminError::SinGh)?.write_all(txt.as_bytes())?;
    }
    drop(hijo.stdin.take());
    let salida = hijo.wait_with_output()?;
    if !salida.status.success() {
        return Err(AdminError::Gh(String::from_utf8_lossy(&salida.stderr).trim().chars().take(300).collect()));
    }
    Ok(String::from_utf8_lossy(&salida.stdout).into_owned())
}

/// SHA-256 de la clave que desbloquea la pestaña de admin.
///
/// Se guarda el hash y no la clave: el launcher se reparte, y la clave en claro viajaría
/// dentro del ejecutable a la vista de cualquiera que lo abriera con un editor.
///
/// **Esto esconde el panel; no lo protege.** Una clave tan corta se saca de su hash por
/// fuerza bruta, y da igual: quien decide de verdad quién publica es el `gh` autenticado
/// con permiso sobre el repo. Sin él, desbloquear solo enseña una pestaña que no puede
/// subir nada.
const CLAVE_SHA256: &str = "162392b2b0c217ad359165633a18b15edac7ce4bdfa77da21923a59bde6b53c5";

fn sha256_hex(texto: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(texto.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// ¿Termina lo tecleado con la clave?
///
/// La clave se escribe sin campo ni botón, en cualquier parte de la ventana, así que lo
/// que llega es lo último que se tecleó y puede traer otras teclas delante.
pub fn termina_en_la_clave(tecleado: &str) -> bool {
    let letras: Vec<char> = tecleado.chars().collect();
    // Con tope: esto se llama según se teclea, y mirar cada sufijo de un texto
    // arbitrariamente largo no tiene ningún sentido.
    let desde = letras.len().saturating_sub(64);
    (desde..letras.len()).any(|i| sha256_hex(&letras[i..].iter().collect::<String>()) == CLAVE_SHA256)
}

fn marca(root: &std::path::Path) -> std::path::PathBuf {
    root.join("admin")
}

/// Deja anotado en esta máquina que el panel ya se desbloqueó, para no pedirlo más.
pub fn desbloquear(root: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    std::fs::write(marca(root), CLAVE_SHA256)
}

/// ¿Se desbloqueó ya en esta máquina?
///
/// La marca guarda el hash y no un simple «sí» para que un archivo con ese nombre creado
/// por cualquier otro motivo no enseñe el panel. No es más que eso: el hash va en el
/// propio ejecutable.
pub fn desbloqueado(root: &std::path::Path) -> bool {
    std::fs::read_to_string(marca(root)).map(|t| t.trim() == CLAVE_SHA256).unwrap_or(false)
}

/// ¿Se puede publicar desde esta máquina?
pub fn disponible() -> bool {
    gh(&["auth", "status"], None).is_ok()
}

/// El `sha` del archivo en el repo, que la API exige para sustituirlo.
///
/// Sin él, GitHub rechaza la escritura de un archivo que ya existe. Que falte es lo
/// normal la primera vez y no es un error.
fn sha_remoto(ruta: &str) -> Option<String> {
    let salida = gh(&["api", &format!("repos/{RELEASES_REPO}/contents/{ruta}"), "--jq", ".sha"], None).ok()?;
    let s = salida.trim().to_string();
    (!s.is_empty() && s != "null").then_some(s)
}

/// Sube un archivo y devuelve el commit que lo dejó, para poder enlazarlo fijo.
/// ¿Es un fallo de los que GitHub mismo pide reintentar?
///
/// Su validación de reglas se queda a veces sin tiempo y responde «Timed out validating
/// rule, please try again». Lo que **no** se reintenta es un rechazo con motivo —un
/// archivo demasiado grande, un permiso que falta—: repetirlo daría lo mismo.
fn merece_reintento(e: &AdminError) -> bool {
    let AdminError::Gh(msg) = e else { return false };
    let m = msg.to_lowercase();
    ["timed out", "timeout", "please try again", "502", "503", "504"].iter().any(|p| m.contains(p))
}

/// Repite lo que falló por algo temporal, esperando cada vez un poco más.
fn con_reintentos<T>(intentos: u32, mut f: impl FnMut() -> Result<T, AdminError>) -> Result<T, AdminError> {
    let intentos = intentos.max(1);
    let mut espera = std::time::Duration::from_secs(2);
    for intento in 1..=intentos {
        match f() {
            Err(e) if intento < intentos && merece_reintento(&e) => {
                tracing::warn!("{e} — reintento {intento} de {intentos}");
                std::thread::sleep(espera);
                espera *= 3;
            }
            otro => return otro,
        }
    }
    unreachable!("la última vuelta siempre devuelve")
}

fn tag_de_pack(id: &str) -> String {
    format!("pack-{id}")
}

/// Nombre del archivo adjunto, con el principio del hash dentro.
///
/// Así **cada contenido tiene su propia URL**: publicar una versión nueva no reescribe
/// el archivo que otro está descargando en ese momento, y ninguna caché puede servir
/// algo viejo bajo la misma dirección.
fn nombre_de_asset(id: &str, sha512: &str) -> String {
    format!("{id}-{}.mrpack", sha512.get(..8).unwrap_or("sinhash"))
}

fn url_de_asset(tag: &str, nombre: &str) -> String {
    format!("https://github.com/{RELEASES_REPO}/releases/download/{tag}/{nombre}")
}

/// Sube el `.mrpack` como adjunto de una release del repo de descargas.
///
/// **No por la API de contenidos.** Esa tiene un tope de 100 MB, manda el archivo en
/// base64 —un tercio más de peso— y dejaría cada versión del pack en el historial del
/// repo para siempre. Un modpack de 257 MB la tumbó con «Repository rule violations
/// found: Timed out validating rule» (2026-09-25). Un adjunto de release admite hasta
/// 2 GB y vive fuera del historial.
fn subir_pack(archivo: &std::path::Path, id: &str, sha512: &str, nombre_pack: &str) -> Result<String, AdminError> {
    let tag = tag_de_pack(id);
    let nombre = nombre_de_asset(id, sha512);

    // `gh` sube el archivo con el nombre que tenga en el disco, así que hace falta uno
    // que ya se llame así. Un enlace duro no copia los megas; si el temporal cae en otro
    // disco no queda más remedio que copiar.
    let dir = std::env::temp_dir().join("dino-publicar");
    std::fs::create_dir_all(&dir)?;
    let copia = dir.join(&nombre);
    let _ = std::fs::remove_file(&copia);
    if std::fs::hard_link(archivo, &copia).is_err() {
        std::fs::copy(archivo, &copia)?;
    }
    let ruta = copia.to_string_lossy().into_owned();

    let existe = gh(&["release", "view", &tag, "--repo", RELEASES_REPO, "--json", "tagName"], None).is_ok();
    if !existe {
        // `--latest=false` porque estas releases son packs, no versiones del launcher:
        // sin eso, la página de invitación acabaría ofreciendo un modpack a quien va a
        // descargarse el launcher.
        let titulo = format!("Modpack: {nombre_pack}");
        con_reintentos(3, || {
            gh(
                &[
                    "release",
                    "create",
                    &tag,
                    "--repo",
                    RELEASES_REPO,
                    "--title",
                    &titulo,
                    "--notes",
                    "Archivos de modpack que reparte Dino Launcher.",
                    "--latest=false",
                ],
                None,
            )
            .map(|_| ())
        })?;
    }

    let subida = con_reintentos(3, || {
        gh(&["release", "upload", &tag, &ruta, "--repo", RELEASES_REPO, "--clobber"], None).map(|_| ())
    });
    let _ = std::fs::remove_file(&copia);
    subida?;

    Ok(url_de_asset(&tag, &nombre))
}

fn subir(ruta: &str, contenido_b64: &str, mensaje: &str) -> Result<String, AdminError> {
    // El `sha` se relee en cada intento a propósito: si el anterior llegó a escribir y
    // solo se perdió la respuesta, repetir con el viejo sería un conflicto seguro.
    let commit = con_reintentos(3, || {
        let mut cuerpo = serde_json::json!({ "message": mensaje, "content": contenido_b64 });
        if let Some(sha) = sha_remoto(ruta) {
            cuerpo["sha"] = serde_json::json!(sha);
        }
        // Por stdin y no por argumentos: el contenido en base64 desborda el límite de
        // longitud de la línea de comandos en cuanto crece un poco.
        gh(
            &[
                "api",
                "-X",
                "PUT",
                &format!("repos/{RELEASES_REPO}/contents/{ruta}"),
                "--input",
                "-",
                "--jq",
                ".commit.sha",
            ],
            Some(&cuerpo.to_string()),
        )
    })?;
    let commit = commit.trim().to_string();
    if commit.len() < 7 || commit == "null" {
        return Err(AdminError::Gh(format!("GitHub no devolvió el commit al subir {ruta}")));
    }
    Ok(commit)
}

/// El catálogo tal como está ahora mismo en el repo, leído con el `gh` autenticado.
///
/// Distingue «no existe» de «no se pudo leer». Lo primero es un repo recién creado y
/// vale como catálogo vacío; lo segundo tiene que parar, porque seguir con un catálogo
/// vacío acabaría escribiendo uno con un solo pack y se llevaría por delante el resto.
fn catalogo_remoto() -> Result<String, AdminError> {
    use base64::Engine;
    match gh(&["api", &format!("repos/{RELEASES_REPO}/contents/catalog.json"), "--jq", ".content"], None) {
        Ok(b64) => {
            let limpio: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(limpio)
                .map_err(|e| AdminError::Gh(format!("el catálogo del repo no se puede leer: {e}")))?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        Err(AdminError::Gh(msg)) if msg.contains("404") => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// Pares (código normalizado, id) de lo que hay en el catálogo.
fn entradas(existente: &str) -> Result<Vec<(String, String)>, AdminError> {
    if existente.trim().is_empty() {
        return Ok(Vec::new());
    }
    let v: serde_json::Value = serde_json::from_str(existente)?;
    Ok(v["packs"]
        .as_array()
        .map(|l| {
            l.iter()
                .map(|p| {
                    (
                        crate::catalog::normalizar_codigo(p["code"].as_str().unwrap_or("")),
                        p["id"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Qué id le toca a lo que se publica, a la vista del catálogo que ya hay.
///
/// El id nombra los archivos del repo (`packs/<id>.mrpack`), y sale del nombre que el
/// `.mrpack` trae **por dentro**, no del que se escribe en el panel. Reutilizar un
/// archivo para publicar un pack distinto repetía el id sin avisar, subía el archivo
/// encima del del otro pack y le rompía la descarga por hash.
///
/// - Un código que ya estaba conserva su id aunque el archivo traiga otro.
/// - Salvo que ese id lo comparta con otro código —un catálogo que ya se pisó—: entonces
///   recibe uno libre, para que publicar deshaga el choque en vez de perpetuarlo.
/// - Un código nuevo con el id ocupado prueba con el suyo propio y, si también lo está,
///   numera: `id-2`, `id-3`…
pub fn id_para_publicar(existente: &str, id: &str, codigo: &str) -> Result<String, AdminError> {
    let packs = entradas(existente)?;
    let codigo = crate::catalog::normalizar_codigo(codigo);
    let de_otro = |cand: &str| packs.iter().any(|(c, i)| i == cand && *c != codigo);

    if let Some((_, suyo)) = packs.iter().find(|(c, _)| *c == codigo) {
        if !suyo.is_empty() && !de_otro(suyo) {
            return Ok(suyo.clone());
        }
    }
    // Pasado por `slug` aunque ya lo sea: acaba en una ruta del repo, y no conviene que
    // de ahí pueda salir un `../`.
    let base = slug(if id.trim().is_empty() { &codigo } else { id });
    for cand in [base.clone(), slug(&codigo)] {
        if !de_otro(&cand) {
            return Ok(cand);
        }
    }
    Ok((2..).map(|n| format!("{base}-{n}")).find(|c| !de_otro(c)).unwrap_or_else(|| base.clone()))
}

/// URL pública de un archivo **tal como quedó en un commit concreto**.
///
/// No de `main`. El CDN guarda cada ruta cinco minutos, y al actualizar un pack el
/// catálogo —que ya se lee al día— anunciaría el hash nuevo mientras el CDN sigue
/// sirviendo el archivo viejo: la descarga fallaría por hash. Lo que hay en un commit no
/// cambia nunca, así que no hay caché que pueda quedarse atrás.
pub fn url_fijada(commit: &str, ruta: &str) -> String {
    format!("https://raw.githubusercontent.com/{RELEASES_REPO}/{commit}/{ruta}")
}

/// Publica el pack: sube el archivo y actualiza el catálogo.
pub fn publicar(b: &Borrador, mrpack: &std::path::Path) -> Result<PackRemoto, AdminError> {
    use base64::Engine;
    let b64 = |d: &[u8]| base64::engine::general_purpose::STANDARD.encode(d);

    let codigo = crate::catalog::normalizar_codigo(&b.code);
    if codigo.is_empty() {
        return Err(AdminError::Campo("Hace falta un código para compartirlo.".into()));
    }
    if b.name.trim().is_empty() {
        return Err(AdminError::Campo("Hace falta un nombre.".into()));
    }

    // El catálogo va **antes** de subir nada: de él sale qué id le toca, y con eso el
    // nombre de los archivos. Leerlo al final era lo que dejaba subir el `.mrpack`
    // encima del de otro pack sin ocasión de darse cuenta.
    let id = id_para_publicar(&catalogo_remoto()?, &b.id, &codigo)?;

    // 2 GB es el tope de un adjunto de release. Mejor decirlo antes de subir nada que
    // después de esperar media hora.
    let peso = std::fs::metadata(mrpack)?.len();
    if peso > 2_000_000_000 {
        return Err(AdminError::Campo("El .mrpack pasa de 2 GB, que es lo máximo que acepta GitHub.".into()));
    }
    let hash = hex512_de_archivo(mrpack)?;
    let url_pack = subir_pack(mrpack, &id, &hash, b.name.trim())?;

    let icono = match &b.icon_b64 {
        Some(png) => {
            let ruta = format!("packs/{id}.png");
            let commit = subir(&ruta, png, &format!("Icono de {}", b.name))?;
            Some(url_fijada(&commit, &ruta))
        }
        None => None,
    };

    let entrada = PackRemoto {
        code: codigo,
        id,
        name: b.name.trim().to_string(),
        version: b.version.clone(),
        mrpack: url_pack,
        server: b.server.clone().filter(|s| !s.trim().is_empty()),
        sha512: Some(hash),
        bytes: peso,
        minecraft: (!b.minecraft.is_empty()).then(|| b.minecraft.clone()),
        loader: (!b.loader.is_empty()).then(|| b.loader.clone()),
        mods: b.mods,
        icon: icono,
    };

    // Releído justo antes de escribir: subir un pack grande tarda, y fusionar sobre el
    // catálogo del principio borraría lo que se hubiera publicado mientras. Si en ese
    // rato alguien tomó este id, `merge_catalogo` lo rechaza en vez de pisarlo.
    let nuevo = merge_catalogo(&catalogo_remoto()?, &entrada)?;
    subir("catalog.json", &b64(nuevo.as_bytes()), &format!("Catalogo: {} {}", entrada.name, entrada.version))?;

    Ok(entrada)
}

/// Lo que ya está publicado, para poder actualizarlo.
///
/// Del repo por el `gh`, no del catálogo público: el panel es de quien publica, y leerlo
/// del CDN hacía que un pack recién publicado tardara cinco minutos en salir en la lista.
pub fn publicados() -> Result<Vec<PackRemoto>, AdminError> {
    let txt = catalogo_remoto()?;
    if txt.trim().is_empty() {
        return Ok(Vec::new());
    }
    crate::catalog::parse_catalog(&txt).map(|c| c.packs).map_err(|e| AdminError::Gh(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(code: &str, version: &str) -> PackRemoto {
        PackRemoto {
            code: code.into(),
            // Un id por código, como sale de `id_para_publicar`. Con el mismo para todos,
            // estos tests daban por buena justo la situación que rompió Dino SMP.
            id: slug(code),
            name: "Dino SMP".into(),
            version: version.into(),
            mrpack: "https://e/x.mrpack".into(),
            server: None,
            sha512: None,
            bytes: 0,
            minecraft: None,
            loader: None,
            mods: 0,
            icon: None,
        }
    }

    #[test]
    fn el_slug_sirve_como_nombre_de_archivo() {
        assert_eq!(slug("Dino SMP"), "dino-smp");
        assert_eq!(slug("  ¡Pack de Yahir!  "), "pack-de-yahir");
        assert_eq!(slug("Fabulously Optimized 14.0.0"), "fabulously-optimized-14-0-0");
        assert_eq!(slug("---"), "pack", "nunca vacío: sería una ruta rota");
    }

    #[test]
    fn publicar_de_nuevo_actualiza_en_vez_de_duplicar() {
        // Si esto fallara, el catálogo acabaría con dos entradas del mismo pack y
        // `buscar` devolvería la primera, que es justo la vieja.
        let uno = merge_catalogo("", &pack("DINOSMP", "1.0.0")).unwrap();
        let dos = merge_catalogo(&uno, &pack("DINOSMP", "1.1.0")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&dos).unwrap();
        let packs = v["packs"].as_array().unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0]["version"], "1.1.0");
    }

    #[test]
    fn actualizar_uno_no_borra_los_demas() {
        let uno = merge_catalogo("", &pack("OTRO", "1.0.0")).unwrap();
        let dos = merge_catalogo(&uno, &pack("DINOSMP", "1.0.0")).unwrap();
        let tres = merge_catalogo(&dos, &pack("DINOSMP", "2.0.0")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&tres).unwrap();
        let packs = v["packs"].as_array().unwrap();
        assert_eq!(packs.len(), 2, "se perdió un pack");
        assert!(packs.iter().any(|p| p["code"] == "OTRO"));
    }

    #[test]
    fn el_codigo_se_guarda_normalizado() {
        // Se compara normalizado al buscar, así que guardarlo de otra forma haría que
        // publicar «dino-smp» y luego «DINO SMP» crease dos entradas.
        let out = merge_catalogo("", &pack("dino smp", "1.0.0")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["packs"][0]["code"], "DINOSMP");
        let dos = merge_catalogo(&out, &pack("DINO-SMP", "2.0.0")).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&dos).unwrap();
        assert_eq!(v2["packs"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn un_catalogo_ilegible_no_se_pisa_a_ciegas() {
        assert!(merge_catalogo("{roto", &pack("X", "1")).is_err());
    }

    #[test]
    fn la_clave_desbloquea_aunque_lleve_teclas_delante() {
        assert!(termina_en_la_clave("dinoGOD"));
        assert!(termina_en_la_clave("asdf jkl dinoGOD"));
    }

    #[test]
    fn la_clave_tiene_que_estar_entera_y_con_sus_mayusculas() {
        for texto in ["", "dinoGO", "dinogod", "DINOGOD", "dinoGODx", "dino GOD"] {
            assert!(!termina_en_la_clave(texto), "{texto:?} no debería desbloquear");
        }
    }

    #[test]
    fn el_desbloqueo_queda_guardado_en_la_maquina() {
        let root = std::env::temp_dir().join("dino-admin-marca");
        let _ = std::fs::remove_dir_all(&root);
        assert!(!desbloqueado(&root));
        desbloquear(&root).unwrap();
        assert!(desbloqueado(&root));
        // Un archivo con ese nombre pero otro contenido no cuenta.
        std::fs::write(marca(&root), "si").unwrap();
        assert!(!desbloqueado(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// El catálogo tal como quedó publicado el 2026-09-13: dos códigos con el mismo id.
    const CATALOGO_PISADO: &str = r#"{ "formatVersion": 1, "packs": [
        { "code": "DINOSMP", "id": "dinosmp", "name": "Dino SMP", "version": "1.0.0",
          "mrpack": "https://e/main/packs/dinosmp.mrpack" },
        { "code": "AMEN", "id": "dinosmp", "name": "dinotesteooo", "version": "1.0.0",
          "mrpack": "https://e/main/packs/dinosmp.mrpack" }
    ] }"#;

    const SOLO_DINOSMP: &str = r#"{ "formatVersion": 1, "packs": [
        { "code": "DINOSMP", "id": "dinosmp", "name": "Dino SMP", "version": "1.0.0",
          "mrpack": "https://e/x.mrpack" } ] }"#;

    #[test]
    fn se_reintenta_lo_que_github_llama_temporal() {
        // El mensaje exacto que devolvió al publicar un pack de 257 MB.
        let real = AdminError::Gh(
            "gh: Repository rule violations found Timed out validating rule, please try again (HTTP 409)".into(),
        );
        assert!(merece_reintento(&real));
        assert!(merece_reintento(&AdminError::Gh("502 Bad Gateway".into())));
    }

    #[test]
    fn no_se_reintenta_lo_que_no_va_a_cambiar() {
        // Repetir un rechazo con motivo solo hace esperar al que publica.
        for msg in ["Not Found (HTTP 404)", "file is too large", "Bad credentials"] {
            assert!(!merece_reintento(&AdminError::Gh(msg.into())), "{msg}");
        }
        assert!(!merece_reintento(&AdminError::SinGh));
    }

    #[test]
    fn el_reintento_para_en_cuanto_sale_bien() {
        let mut vueltas = 0;
        let r = con_reintentos(3, || {
            vueltas += 1;
            if vueltas < 2 {
                Err(AdminError::Gh("timed out".into()))
            } else {
                Ok(vueltas)
            }
        });
        assert_eq!(r.unwrap(), 2);
    }

    #[test]
    fn el_reintento_no_insiste_para_siempre() {
        let mut vueltas = 0;
        let r: Result<(), _> = con_reintentos(2, || {
            vueltas += 1;
            Err(AdminError::Gh("timed out".into()))
        });
        assert!(r.is_err());
        assert_eq!(vueltas, 2, "insistió de más");
    }

    #[test]
    fn cada_contenido_tiene_su_propia_url() {
        // Si dos versiones compartieran nombre, quien estuviera descargando la vieja se
        // encontraría la nueva a mitad, y el hash del catálogo ya no cuadraría.
        let a = nombre_de_asset("dinosmp", &"a".repeat(128));
        let b = nombre_de_asset("dinosmp", &"b".repeat(128));
        assert_ne!(a, b);
        assert!(a.starts_with("dinosmp-") && a.ends_with(".mrpack"), "{a}");
        let u = url_de_asset(&tag_de_pack("dinosmp"), &a);
        assert!(u.contains("/releases/download/pack-dinosmp/"), "{u}");
        assert!(!u.contains(' '), "una URL con espacios se rompe al compartirla");
    }

    #[test]
    fn el_hash_por_trozos_da_lo_mismo_que_de_golpe() {
        let ruta = std::env::temp_dir().join("dino-hash-trozos.bin");
        let datos: Vec<u8> = (0..(3 << 20)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&ruta, &datos).unwrap();
        assert_eq!(hex512_de_archivo(&ruta).unwrap(), hex512(&datos));
        let _ = std::fs::remove_file(&ruta);
    }

    #[test]
    fn un_pack_nuevo_no_hereda_el_id_de_otro_aunque_venga_del_mismo_archivo() {
        // Lo que pasó: un .mrpack cuyo nombre interno es «dinosmp», publicado con otro
        // código. Antes quedaba con id `dinosmp` y pisaba el archivo de Dino SMP.
        assert_eq!(id_para_publicar(SOLO_DINOSMP, "dinosmp", "AMEN").unwrap(), "amen");
    }

    #[test]
    fn si_su_propio_codigo_tambien_esta_ocupado_se_numera() {
        let cat = r#"{ "formatVersion": 1, "packs": [
            { "code": "DINOSMP", "id": "dinosmp", "name": "a", "version": "1", "mrpack": "x" },
            { "code": "OTRO", "id": "amen", "name": "b", "version": "1", "mrpack": "x" } ] }"#;
        assert_eq!(id_para_publicar(cat, "dinosmp", "AMEN").unwrap(), "dinosmp-2");
    }

    #[test]
    fn actualizar_un_pack_conserva_su_id_aunque_el_archivo_traiga_otro() {
        // Cambiar de id movería sus archivos de sitio sin necesidad.
        assert_eq!(id_para_publicar(SOLO_DINOSMP, "dino-smp-v2", "dino-smp").unwrap(), "dinosmp");
    }

    #[test]
    fn un_pack_de_verdad_nuevo_usa_el_id_de_su_archivo() {
        assert_eq!(id_para_publicar(SOLO_DINOSMP, "otro-pack", "OTRO").unwrap(), "otro-pack");
        assert_eq!(id_para_publicar("", "otro-pack", "OTRO").unwrap(), "otro-pack");
    }

    #[test]
    fn volver_a_publicar_sobre_el_catalogo_pisado_lo_desenreda() {
        // Republicar el pack de prueba tiene que sacarlo del id compartido, no
        // confirmarlo en él.
        let id = id_para_publicar(CATALOGO_PISADO, "dinosmp", "AMEN").unwrap();
        assert_ne!(id, "dinosmp");
        let mut p = pack("AMEN", "1.0.1");
        p.id = id;
        let out = merge_catalogo(CATALOGO_PISADO, &p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let ids: Vec<&str> = v["packs"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1], "siguen compartiendo id: {ids:?}");
    }

    #[test]
    fn el_catalogo_no_acepta_dos_codigos_con_el_mismo_id() {
        let mut p = pack("AMEN", "1.0.0");
        p.id = "dinosmp".into();
        assert!(merge_catalogo(SOLO_DINOSMP, &p).is_err());
    }

    #[test]
    fn con_el_catalogo_ilegible_no_se_elige_id() {
        // Elegir a ciegas podría repetir uno que sí existe.
        assert!(id_para_publicar("{roto", "x", "X").is_err());
    }

    #[test]
    fn el_id_no_puede_salirse_de_la_carpeta_de_packs() {
        let id = id_para_publicar("", "../catalog", "X").unwrap();
        assert!(!id.contains('/') && !id.contains(".."), "{id}");
    }

    #[test]
    fn la_url_del_pack_queda_fijada_al_commit() {
        let u = url_fijada("abc1234", "packs/x.mrpack");
        assert!(u.ends_with("/abc1234/packs/x.mrpack"), "{u}");
        assert!(!u.contains("/main/"), "de main la sirve el CDN con cinco minutos de retraso");
    }

    /// Depende de la máquina, así que no corre en CI: `cargo test -- --ignored gh_`
    #[test]
    #[ignore]
    fn gh_se_encuentra_aunque_no_este_en_el_path() {
        println!("  gh elegido: {}", gh_bin());
        assert!(disponible(), "no encontré un gh autenticado");
    }

    #[test]
    fn lee_el_mrpack_de_verdad() {
        // El pack real de Dino SMP; si no está, la prueba se salta en vez de fallar.
        let p = std::path::Path::new(r"C:\Users\pici\Downloads\dinosmp 1.0.0.mrpack");
        if !p.exists() {
            return;
        }
        let b = leer_mrpack(p).unwrap();
        assert_eq!(b.version, "1.0.0");
        assert_eq!(b.minecraft, "26.2");
        assert!(b.loader.starts_with("Fabric"), "loader legible: {}", b.loader);
        assert!(b.mods > 0);
        assert_eq!(b.sha512.len(), 128);
        assert_eq!(b.id, slug(&b.name));
    }
}
