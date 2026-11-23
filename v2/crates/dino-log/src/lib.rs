//! Registro y diagnóstico.
//!
//! En 1.x el backend escribía con `println!`, que en la versión publicada no va a ningún
//! sitio, y la interfaz dejaba trazas en `dino.log`. Diagnosticar el fallo de un amigo
//! dependía de que lo reprodujera. En 2.0:
//!
//! - [`init`] manda todo a `logs/dino.YYYY-MM-DD.log`, un archivo por día, y guarda 7;
//! - [`export_report`] junta registros, configuración y manifiestos en un zip que se puede
//!   mandar por chat, pasando cada archivo por [`redact`] antes de guardarlo.

use dino_core::schema::Paths;
use std::io::Write;
use std::path::Path;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Mantener vivo mientras corra la app: al soltarlo se vacía lo que quede por escribir.
pub struct Guard(#[allow(dead_code)] tracing_appender::non_blocking::WorkerGuard);

/// Arranca el registro. Nivel por defecto `info`; se cambia con la variable `DINO_LOG`
/// (p. ej. `DINO_LOG=debug`). Con `to_stderr`, además a la consola (para `dino-cli`).
pub fn init(paths: &Paths, to_stderr: bool) -> std::io::Result<Guard> {
    std::fs::create_dir_all(paths.logs())?;
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("dino")
        .filename_suffix("log")
        .max_log_files(7)
        .build(paths.logs())
        .map_err(std::io::Error::other)?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_env("DINO_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = fmt::layer().with_writer(writer).with_ansi(false).with_target(true);
    let stderr_layer = to_stderr.then(|| fmt::layer().with_writer(std::io::stderr).with_target(false).compact());
    // `try_init`: si ya había un registro (tests), no es un error.
    let _ = tracing_subscriber::registry().with(filter).with(file_layer).with(stderr_layer).try_init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), os = std::env::consts::OS, "registro iniciado");
    Ok(Guard(guard))
}

/// Tapa lo que parezca un secreto: JWT (los tokens de Minecraft y de Xbox lo son) y el
/// valor de cualquier campo JSON cuyo nombre contenga `token`.
pub fn redact(text: &str) -> String {
    let masked = mask_json_tokens(text);
    mask_jwts(&masked)
}

fn mask_json_tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('"') {
        out.push_str(&rest[..=i]);
        rest = &rest[i + 1..];
        let Some(end) = rest.find('"') else { break };
        let key = &rest[..end];
        out.push_str(key);
        out.push('"');
        rest = &rest[end + 1..];
        if !key.to_ascii_lowercase().contains("token") {
            continue;
        }
        // `"…token…" : "valor"` → `"…token…": "<oculto>"`
        let trimmed = rest.trim_start();
        if let Some(after_colon) = trimmed.strip_prefix(':') {
            let v = after_colon.trim_start();
            if let Some(val) = v.strip_prefix('"') {
                if let Some(close) = val.find('"') {
                    out.push_str(": \"<oculto>\"");
                    rest = &val[close + 1..];
                }
            }
        }
    }
    out.push_str(rest);
    out
}

fn mask_jwts(s: &str) -> String {
    let b64 = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let bytes: Vec<char> = s.chars().collect();
    while i < bytes.len() {
        let starts = bytes[i..].starts_with(&['e', 'y', 'J']);
        if starts {
            // eyJ… . … . …  (tres trozos base64url separados por puntos)
            let mut j = i;
            let mut dots = 0;
            while j < bytes.len() && (b64(bytes[j]) || (bytes[j] == '.' && dots < 2)) {
                if bytes[j] == '.' {
                    dots += 1;
                }
                j += 1;
            }
            if dots == 2 && j - i > 20 {
                out.push_str("<jwt oculto>");
                i = j;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Junta el informe de diagnóstico en `out` (zip). Devuelve cuántos archivos metió.
///
/// Entra: registros, `config.json`, `library.json` y cada `instance.json`, más un
/// `sistema.txt`. **No** entra `accounts.json` (nombres y uuids de las cuentas): solo
/// cuántas hay.
pub fn export_report(paths: &Paths, out: &Path) -> std::io::Result<usize> {
    let file = std::fs::File::create(out)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut n = 0;
    let mut add = |zip: &mut zip::ZipWriter<std::fs::File>, name: &str, path: &Path| -> std::io::Result<()> {
        if let Ok(raw) = std::fs::read(path) {
            zip.start_file(name, opts).map_err(std::io::Error::other)?;
            zip.write_all(redact(&String::from_utf8_lossy(&raw)).as_bytes())?;
            n += 1;
        }
        Ok(())
    };
    if let Ok(rd) = std::fs::read_dir(paths.logs()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            add(&mut zip, &format!("logs/{name}"), &e.path())?;
        }
    }
    add(&mut zip, "config.json", &paths.config())?;
    add(&mut zip, "library.json", &paths.library())?;
    if let Ok(rd) = std::fs::read_dir(paths.root.join("instances")) {
        for e in rd.flatten() {
            let id = e.file_name().to_string_lossy().into_owned();
            add(&mut zip, &format!("instances/{id}.json"), &paths.instance_manifest(&id))?;
        }
    }
    let accounts = std::fs::read_to_string(paths.accounts()).ok().map(|s| s.matches("\"uuid\"").count()).unwrap_or(0);
    zip.start_file("sistema.txt", opts).map_err(std::io::Error::other)?;
    writeln!(
        zip,
        "dino {}\nos {} {}\ncuentas {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        accounts
    )?;
    zip.finish().map_err(std::io::Error::other)?;
    Ok(n + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";

    #[test]
    fn los_jwt_no_salen() {
        let s = format!("Authorization: Bearer {JWT} y más texto");
        let r = redact(&s);
        assert!(!r.contains("eyJhbGci"));
        assert!(r.contains("<jwt oculto>") && r.ends_with("y más texto"));
    }

    #[test]
    fn los_campos_token_se_tapan() {
        let s = r#"{"ygg": {"token": "abc123", "exp": 5}, "refresh_token":"zzz", "name": "JugadorMicrosoft"}"#;
        let r = redact(s);
        assert!(!r.contains("abc123") && !r.contains("zzz"));
        assert!(r.contains("JugadorMicrosoft") && r.contains("\"exp\": 5"));
    }

    #[test]
    fn el_texto_normal_no_se_toca() {
        let s = "Descargando mods · 3 de 120 (eyJ suelto no es un token)";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn el_informe_no_lleva_cuentas_ni_secretos() {
        let root = std::env::temp_dir().join(format!("dino-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::new(&root);
        std::fs::create_dir_all(paths.logs()).unwrap();
        std::fs::write(paths.logs().join("dino.2026-11-23.log"), format!("token {JWT}")).unwrap();
        std::fs::write(paths.library(), r#"{"schema":1}"#).unwrap();
        std::fs::write(paths.accounts(), r#"{"accounts":[{"uuid":"a","name":"Yahir"}]}"#).unwrap();
        let out = root.join("informe.zip");
        let n = export_report(&paths, &out).unwrap();
        assert_eq!(n, 3);
        let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let names: Vec<String> = (0..z.len()).map(|i| z.by_index(i).unwrap().name().to_string()).collect();
        assert!(!names.iter().any(|n| n.contains("accounts")));
        let mut log = String::new();
        std::io::Read::read_to_string(&mut z.by_name("logs/dino.2026-11-23.log").unwrap(), &mut log).unwrap();
        assert!(!log.contains("eyJhbGci"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
