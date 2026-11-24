//! La skin de una cuenta, para pintar su cabeza en la interfaz.
//!
//! Se busca por **nombre**: es lo que ve el jugador y sirve igual para una cuenta de
//! Microsoft que para una sin conexión con el nombre de alguien que sí tiene Minecraft.
//! Un nombre sin cuenta de pago no tiene skin propia: entonces no se devuelve nada y la
//! interfaz se queda con su cabeza dibujada (las skins por defecto de Mojang no se
//! incluyen: son suyas).
//!
//! Dos peticiones públicas, sin sesión: nombre → UUID (`api.minecraftservices.com`) y
//! UUID → texturas (`sessionserver.mojang.com`); luego el PNG de `textures.minecraft.net`.
//! Todo queda en `cache/skins/` un día; sin red se usa lo guardado aunque sea viejo.

use base64::Engine as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const FRESH: Duration = Duration::from_secs(24 * 3600);
const TIMEOUT: Duration = Duration::from_secs(6);
/// Una skin son 64×64 (o 64×32) píxeles: unos KB. Más que esto no es una skin.
const MAX_BYTES: usize = 512 * 1024;

/// Nombre válido de Minecraft (3–16 de `[A-Za-z0-9_]`); también evita rutas raras.
pub fn valid_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn files(cache: &Path, name: &str) -> (PathBuf, PathBuf) {
    let key = name.to_ascii_lowercase();
    (cache.join(format!("{key}.png")), cache.join(format!("{key}.none")))
}

fn fresh(p: &Path) -> bool {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < FRESH)
}

/// El PNG de la skin de `name`, o `None` si ese nombre no tiene skin propia (o no se
/// pudo saber y no hay nada guardado).
pub async fn skin(cache: &Path, name: &str) -> Option<Vec<u8>> {
    if !valid_name(name) {
        return None;
    }
    let (png, none) = files(cache, name);
    if fresh(&png) {
        return std::fs::read(&png).ok();
    }
    if fresh(&none) {
        return None;
    }
    match fetch(name).await {
        Ok(Some(bytes)) => {
            let _ = std::fs::create_dir_all(cache);
            let _ = dino_core::schema::write_atomic(&png, &bytes);
            let _ = std::fs::remove_file(&none);
            Some(bytes)
        }
        Ok(None) => {
            let _ = std::fs::create_dir_all(cache);
            let _ = std::fs::write(&none, b"");
            let _ = std::fs::remove_file(&png);
            None
        }
        // Sin red o Mojang caído: mejor la skin de ayer que ninguna.
        Err(x) => {
            tracing::debug!(%name, error = %x, "no pude consultar la skin");
            std::fs::read(&png).ok()
        }
    }
}

/// `Ok(None)` = Mojang responde que ese nombre no existe o no tiene skin.
async fn fetch(name: &str) -> Result<Option<Vec<u8>>, reqwest::Error> {
    let c = reqwest::Client::builder()
        .user_agent(concat!("dino-launcher/", env!("CARGO_PKG_VERSION")))
        .timeout(TIMEOUT)
        .build()?;
    let r = c.get(format!("https://api.minecraftservices.com/minecraft/profile/lookup/name/{name}")).send().await?;
    if r.status() == reqwest::StatusCode::NOT_FOUND || r.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    let profile: serde_json::Value = r.error_for_status()?.json().await?;
    let Some(uuid) = profile["id"].as_str().filter(|u| u.len() == 32 && u.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return Ok(None);
    };
    let session: serde_json::Value = c
        .get(format!("https://sessionserver.mojang.com/session/minecraft/profile/{uuid}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let Some(url) = skin_url(&session) else { return Ok(None) };
    let bytes = c.get(url).send().await?.error_for_status()?.bytes().await?;
    Ok(is_png(&bytes).then(|| bytes.to_vec()))
}

/// La URL del PNG dentro de la propiedad `textures` (JSON en base64). Solo se acepta el
/// servidor de texturas de Mojang, y por HTTPS aunque venga como `http://`.
fn skin_url(session: &serde_json::Value) -> Option<String> {
    let props = session["properties"].as_array()?;
    let value = props.iter().find(|p| p["name"] == "textures")?["value"].as_str()?;
    let raw = base64::engine::general_purpose::STANDARD.decode(value).ok()?;
    let textures: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let url = textures["textures"]["SKIN"]["url"].as_str()?;
    let path = url
        .strip_prefix("http://textures.minecraft.net/")
        .or_else(|| url.strip_prefix("https://textures.minecraft.net/"))?;
    Some(format!("https://textures.minecraft.net/{path}"))
}

fn is_png(b: &[u8]) -> bool {
    b.len() <= MAX_BYTES && b.starts_with(b"\x89PNG\r\n\x1a\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_validos() {
        assert!(valid_name("DinoTaco35"));
        assert!(valid_name("a_b"));
        assert!(!valid_name("../etc"));
        assert!(!valid_name("nombre-con-guion"));
        assert!(!valid_name("diecisietecaracte"));
    }

    #[test]
    fn la_url_sale_de_las_texturas_y_solo_de_mojang() {
        let enc = |j: &str| base64::engine::general_purpose::STANDARD.encode(j);
        let ok = serde_json::json!({"properties": [{"name": "textures", "value":
            enc(r#"{"textures":{"SKIN":{"url":"http://textures.minecraft.net/texture/abc"}}}"#)}]});
        assert_eq!(skin_url(&ok).as_deref(), Some("https://textures.minecraft.net/texture/abc"));
        let ajena = serde_json::json!({"properties": [{"name": "textures", "value":
            enc(r#"{"textures":{"SKIN":{"url":"https://evil.example/x.png"}}}"#)}]});
        assert_eq!(skin_url(&ajena), None);
        let sin_skin = serde_json::json!({"properties": [{"name": "textures", "value": enc(r#"{"textures":{}}"#)}]});
        assert_eq!(skin_url(&sin_skin), None);
    }

    #[test]
    fn la_cache_recuerda_tambien_que_no_hay_skin() {
        let d = std::env::temp_dir().join(format!("dino-skins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (png, none) = files(&d, "Alguien");
        std::fs::write(&none, b"").unwrap();
        // marcado como «sin skin» y reciente: ni se pregunta a la red
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert_eq!(rt.block_on(skin(&d, "alguien")), None);
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nresto").unwrap();
        std::fs::remove_file(&none).unwrap();
        assert!(rt.block_on(skin(&d, "ALGUIEN")).is_some_and(|b| is_png(&b)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    #[ignore = "red: consulta a Mojang"]
    async fn skin_real() {
        let png = fetch("DinoTaco35").await.unwrap().unwrap();
        assert!(is_png(&png));
        assert_eq!(fetch("DinoTako35").await.unwrap(), None);
    }
}
