//! Puente con las cuentas Microsoft del motor (ElyPrism).
//!
//! Dino no tiene `client_id` aprobado por Mojang, así que no puede iniciar sesión con
//! Microsoft por su cuenta. El motor sí: guarda en `accounts.json` (formato v3) la sesión
//! de Minecraft que obtuvo con su propio `client_id`. El puente **lee** esa sesión para
//! que el núcleo de Dino lance el juego; iniciar sesión y renovarla sigue siendo cosa del
//! motor.
//!
//! Reglas de seguridad:
//! - nunca se escribe en `accounts.json` desde aquí;
//! - el token nunca sale en `Debug`, en logs ni en errores;
//! - un token caducado no se usa: se pide renovarlo al motor.

pub mod accounts;
pub mod engine;
#[cfg(windows)]
mod msa_window;

pub use accounts::{Accounts, AccountsError, LaunchIdentity};
pub use engine::Engine;

use serde_json::Value;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("no pude leer las cuentas del motor ({0})")]
    Io(String),
    #[error("las cuentas del motor no tienen el formato esperado")]
    Format,
    #[error("no hay ninguna cuenta Microsoft con el nombre {0}")]
    NotFound(String),
    #[error("la sesión de {0} caducó: hay que renovarla con el motor")]
    Expired(String),
    #[error("Mojang no aceptó la sesión ({0})")]
    Rejected(u16),
    #[error("red: {0}")]
    Http(#[from] reqwest::Error),
}

/// Lo que se puede enseñar de una cuenta. Sin secretos.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountInfo {
    pub kind: String,
    pub name: String,
    pub uuid: String,
    pub active: bool,
    /// Segundos Unix en que caduca la sesión de Minecraft (solo cuentas Microsoft).
    pub expires: Option<u64>,
    /// Segundos Unix en que se emitió esa sesión: cambia al iniciar sesión otra vez (y al
    /// renovar).
    pub issued: Option<u64>,
}

/// Sesión lista para lanzar. El token no se imprime nunca.
#[derive(Clone)]
pub struct Session {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    pub expires: u64,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("name", &self.name)
            .field("uuid", &self.uuid)
            .field("access_token", &"<oculto>")
            .field("expires", &self.expires)
            .finish()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn read(path: &Path) -> Result<Value, AuthError> {
    let s = std::fs::read_to_string(path).map_err(|e| AuthError::Io(e.kind().to_string()))?;
    let v: Value = serde_json::from_str(&s).map_err(|_| AuthError::Format)?;
    if v.get("formatVersion").and_then(Value::as_u64) != Some(3) {
        return Err(AuthError::Format);
    }
    Ok(v)
}

fn accounts(v: &Value) -> Result<&Vec<Value>, AuthError> {
    v.get("accounts").and_then(Value::as_array).ok_or(AuthError::Format)
}

fn info(a: &Value) -> AccountInfo {
    let s = |p: &[&str]| -> String {
        let mut cur = a;
        for k in p {
            cur = &cur[*k];
        }
        cur.as_str().unwrap_or("").to_string()
    };
    AccountInfo {
        kind: s(&["type"]),
        name: s(&["profile", "name"]),
        uuid: s(&["profile", "id"]),
        active: a.get("active").and_then(Value::as_bool).unwrap_or(false),
        expires: a.pointer("/ygg/exp").and_then(Value::as_u64),
        issued: a.pointer("/ygg/iat").and_then(Value::as_u64),
    }
}

/// Cuentas del motor, sin secretos.
pub fn list(accounts_json: &Path) -> Result<Vec<AccountInfo>, AuthError> {
    Ok(accounts(&read(accounts_json)?)?.iter().map(info).collect())
}

/// La sesión de Minecraft de una cuenta Microsoft del motor.
///
/// Se considera caducada si le quedan menos de 5 minutos: mejor renovarla antes que
/// lanzar una partida que se queda sin sesión al entrar a un servidor.
pub fn session(accounts_json: &Path, name: &str) -> Result<Session, AuthError> {
    let v = read(accounts_json)?;
    let a = accounts(&v)?
        .iter()
        .find(|a| a["type"].as_str() == Some("MSA") && a["profile"]["name"].as_str() == Some(name))
        .ok_or_else(|| AuthError::NotFound(name.into()))?;
    let i = info(a);
    let token = a.pointer("/ygg/token").and_then(Value::as_str).unwrap_or("");
    let exp = i.expires.unwrap_or(0);
    if token.is_empty() || exp < now() + 300 {
        return Err(AuthError::Expired(name.into()));
    }
    Ok(Session { name: i.name, uuid: i.uuid, access_token: token.to_string(), expires: exp })
}

/// Pregunta a Mojang si la sesión vale, sin enseñarla. Devuelve el nombre del perfil.
pub async fn verify(client: &reqwest::Client, s: &Session) -> Result<String, AuthError> {
    let r =
        client.get("https://api.minecraftservices.com/minecraft/profile").bearer_auth(&s.access_token).send().await?;
    if !r.status().is_success() {
        return Err(AuthError::Rejected(r.status().as_u16()));
    }
    let v: Value = r.json().await?;
    Ok(v["name"].as_str().unwrap_or("").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(json: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("dino-auth-{}-{}.json", std::process::id(), json.len()));
        std::fs::write(&p, json).unwrap();
        p
    }

    #[test]
    fn el_token_no_sale_en_debug() {
        let s = Session { name: "a".into(), uuid: "b".into(), access_token: "SECRETO".into(), expires: 1 };
        assert!(!format!("{s:?}").contains("SECRETO"));
    }

    #[test]
    fn una_sesion_caducada_no_se_usa() {
        let p = file(
            r#"{"formatVersion":3,"accounts":[{"type":"MSA","ygg":{"token":"t","exp":10},"profile":{"id":"u","name":"Yahir"}}]}"#,
        );
        assert!(matches!(session(&p, "Yahir"), Err(AuthError::Expired(_))));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn una_sesion_vigente_se_lee() {
        let exp = now() + 3600;
        let p = file(&format!(
            r#"{{"formatVersion":3,"accounts":[{{"type":"Offline","profile":{{"name":"Yahir"}}}},{{"type":"MSA","active":true,"ygg":{{"token":"t","exp":{exp}}},"profile":{{"id":"u","name":"Yahir"}}}}]}}"#
        ));
        let s = session(&p, "Yahir").unwrap();
        assert_eq!((s.uuid.as_str(), s.access_token.as_str()), ("u", "t"));
        let l = list(&p).unwrap();
        assert_eq!(l.len(), 2);
        assert!(l[1].active && l[1].kind == "MSA");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn otro_formato_no_se_interpreta() {
        let p = file(r#"{"formatVersion":2,"accounts":[]}"#);
        assert!(matches!(list(&p), Err(AuthError::Format)));
        let _ = std::fs::remove_file(p);
    }
}
