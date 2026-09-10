//! Login de Ely.by sin abrir el motor.
//!
//! El motor resuelve la autenticación, pero solo desde su propia ventana: no hay
//! bandera de línea de comandos que abra su diálogo de cuentas (verificado contra
//! `--help` del release 11.1.0). Eso obliga a que la primera vez el usuario vea un
//! launcher que no es el nuestro, que es justo lo que este módulo evita.
//!
//! Aquí se replica **solo** el flujo de Ely.by —código de dispositivo— y se escribe
//! la cuenta en el `accounts.json` del motor. A partir de ahí el motor la refresca y
//! la usa como si la hubiera creado él; no hace falta tocar nada más.
//!
//! Microsoft se queda fuera a propósito: su flujo necesita un `client_id` de Azure
//! con acceso a la API de Minecraft aprobado por Mojang, que es un trámite externo.
//! Esa rama sigue pasando por la ventana del motor.
//!
//! Extremos y formato verificados leyendo el fork en el release 11.1.0:
//! `ElyDeviceCodeStep.cpp`, `ElyStep.cpp`, `ElyYggdrasilTokenStep.cpp`,
//! `MinecraftProfileStepEly.cpp`, `AccountData.cpp` y `AccountList.cpp`.
//! Como `ENGINE_VERSION` está fijado, el formato no se mueve por su cuenta.

use crate::engine::Layout;
use serde_json::{json, Value};

/// Versión del contenedor de `accounts.json`, de `AccountList.cpp`.
///
/// Importa más de lo que parece: si el número no coincide, el motor **renombra el
/// archivo a `accounts-old.json`** y arranca con la lista vacía. Escribirlo mal no
/// da error, se lleva por delante las cuentas que ya hubiera.
const ACCOUNTS_FORMAT_VERSION: u64 = 3;

const DEVICE_CODE_URL: &str = "https://account.ely.by/api/oauth2/v1/devicecode";
const TOKEN_URL: &str = "https://account.ely.by/api/oauth2/v1/token";
const PROFILE_URL: &str = "https://account.ely.by/api/mojang/services/minecraft/profile";

/// Los mismos que pide el motor en `ElyStep.cpp`. `offline_access` es el que trae el
/// `refresh_token`: sin él la sesión se muere en una hora y hay que volver a entrar.
const SCOPES: &str = "account_info offline_access minecraft_server_session";

const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("error de red: {0}")]
    Http(#[from] reqwest::Error),
    #[error("error de disco: {0}")]
    Io(#[from] std::io::Error),
    #[error("respuesta inesperada de Ely.by: {0}")]
    Json(#[from] serde_json::Error),
    #[error("falta configurar el client_id de Ely.by")]
    NoClientId,
    #[error("Ely.by respondió {0}: {1}")]
    Remote(u16, String),
    #[error("el código caducó; hay que pedir uno nuevo")]
    Expired,
    #[error("el accounts.json del motor está corrupto")]
    BadAccountsFile,
}

/// Lo que se le enseña al usuario mientras espera.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceCode {
    /// El código corto que teclea. El de la captura era `8VYNRUKD`.
    pub user_code: String,
    /// Dónde lo teclea.
    pub verification_uri: String,
    /// Algunas implementaciones dan la URL con el código ya puesto.
    pub verification_uri_complete: Option<String>,
    /// Secreto: es lo que nosotros mandamos al sondear. No se enseña.
    pub device_code: String,
    /// Segundos entre sondeos. Sondear más rápido hace que Ely.by responda
    /// `slow_down`, así que se respeta.
    pub interval: u64,
    pub expires_in: u64,
}

/// Resultado de un sondeo.
#[derive(Debug)]
pub enum Poll {
    /// Todavía no aprobó. Hay que volver a preguntar tras `interval`.
    Pending,
    /// Aprobó: estos son los tokens.
    Ready(Tokens),
    /// Dijo que no. No se reintenta.
    Denied,
}

#[derive(Debug, Clone)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Segundos de vida del `access_token`.
    pub expires_in: u64,
}

fn ahora() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn leer_json(r: reqwest::Response) -> Result<Value, AuthError> {
    let status = r.status();
    let bytes = r.bytes().await?;
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !status.is_success() {
        // Los errores de OAuth vienen en el cuerpo aunque el código sea 4xx, y el
        // sondeo depende de distinguirlos: `authorization_pending` no es un fallo.
        if v.get("error").is_some() {
            return Ok(v);
        }
        let texto = String::from_utf8_lossy(&bytes).chars().take(200).collect::<String>();
        return Err(AuthError::Remote(status.as_u16(), texto));
    }
    Ok(v)
}

/// Pide el código de dispositivo. Es el primer paso y el único que necesita
/// que el usuario no haya hecho nada todavía.
pub async fn start_device_code(client_id: &str) -> Result<DeviceCode, AuthError> {
    if client_id.trim().is_empty() {
        return Err(AuthError::NoClientId);
    }
    let v = leer_json(
        reqwest::Client::new()
            .post(DEVICE_CODE_URL)
            .form(&[("client_id", client_id), ("scope", SCOPES)])
            .send()
            .await?,
    )
    .await?;

    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        return Err(AuthError::Remote(400, e.to_string()));
    }
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    Ok(DeviceCode {
        user_code: s("user_code").ok_or(AuthError::BadAccountsFile)?,
        verification_uri: s("verification_uri").unwrap_or_else(|| "https://account.ely.by".into()),
        verification_uri_complete: s("verification_uri_complete"),
        device_code: s("device_code").ok_or(AuthError::BadAccountsFile)?,
        // El estándar recomienda 5 s cuando el servidor no dice otra cosa.
        interval: v.get("interval").and_then(|x| x.as_u64()).unwrap_or(5),
        expires_in: v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(600),
    })
}

/// Un sondeo. La espera entre llamadas la lleva quien llama, para que la interfaz
/// pueda enseñar una cuenta atrás y permitir cancelar.
pub async fn poll_once(client_id: &str, device_code: &str) -> Result<Poll, AuthError> {
    let v = leer_json(
        reqwest::Client::new()
            .post(TOKEN_URL)
            .form(&[
                ("client_id", client_id),
                ("device_code", device_code),
                ("grant_type", DEVICE_CODE_GRANT),
            ])
            .send()
            .await?,
    )
    .await?;

    if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
        return Ok(clasificar_error(e)?);
    }
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(Poll::Ready(Tokens {
        access_token: s("access_token"),
        refresh_token: s("refresh_token"),
        expires_in: v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600),
    }))
}

/// Los códigos de error del sondeo son estándar de OAuth 2.0 (RFC 8628 §3.5).
///
/// Pura y aparte porque confundir `authorization_pending` con un fallo real haría
/// que el login se rindiera justo mientras el usuario está tecleando el código.
pub fn clasificar_error(code: &str) -> Result<Poll, AuthError> {
    Ok(match code {
        "authorization_pending" | "slow_down" => Poll::Pending,
        "access_denied" => Poll::Denied,
        "expired_token" => return Err(AuthError::Expired),
        otro => return Err(AuthError::Remote(400, otro.to_string())),
    })
}

/// Perfil de Minecraft asociado a la cuenta, en el formato de Mojang.
pub async fn fetch_profile(access_token: &str) -> Result<Value, AuthError> {
    leer_json(
        reqwest::Client::new()
            .get(PROFILE_URL)
            .bearer_auth(access_token)
            .send()
            .await?,
    )
    .await
}

/// Traduce el perfil de Mojang al que guarda el motor.
///
/// **Todos los campos son obligatorios.** `AccountData.cpp::profileFromJSONV3`
/// descarta el perfil entero —y con él la cuenta— si falta `skin` con sus tres
/// cadenas o si `capes` no es un array. Y lo hace en silencio: el usuario vería un
/// launcher que le pide iniciar sesión una y otra vez sin decir por qué.
///
/// Por eso la piel vacía se escribe igual con cadenas vacías en vez de omitirse:
/// la validación de upstream solo exige que sean cadenas, no que tengan contenido.
pub fn profile_json(mojang: &Value) -> Value {
    let id = mojang.get("id").and_then(|x| x.as_str()).unwrap_or("");
    let name = mojang.get("name").and_then(|x| x.as_str()).unwrap_or("");

    let vacia = json!({ "id": "", "url": "", "variant": "CLASSIC" });
    let skin = mojang
        .get("skins")
        .and_then(|x| x.as_array())
        .and_then(|skins| {
            // La activa manda; si ninguna lo está, la primera sirve.
            skins
                .iter()
                .find(|s| s.get("state").and_then(|x| x.as_str()) == Some("ACTIVE"))
                .or_else(|| skins.first())
        })
        .map(|s| {
            json!({
                "id": s.get("id").and_then(|x| x.as_str()).unwrap_or(""),
                "url": s.get("url").and_then(|x| x.as_str()).unwrap_or(""),
                "variant": s.get("variant").and_then(|x| x.as_str()).unwrap_or("CLASSIC"),
            })
        })
        .unwrap_or(vacia);

    let capes: Vec<Value> = mojang
        .get("capes")
        .and_then(|x| x.as_array())
        .map(|cs| {
            cs.iter()
                .map(|c| {
                    json!({
                        "id": c.get("id").and_then(|x| x.as_str()).unwrap_or(""),
                        "url": c.get("url").and_then(|x| x.as_str()).unwrap_or(""),
                        "alias": c.get("alias").and_then(|x| x.as_str()).unwrap_or(""),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    json!({ "id": id, "name": name, "skin": skin, "capes": capes })
}

/// La entrada de cuenta tal como la guarda el motor.
///
/// `ygg` es una copia literal de `msa`: eso hace `ElyYggdrasilTokenStep`, que no
/// llama a ninguna API, solo asigna `yggdrasilToken = msaToken`.
pub fn account_json(client_id: &str, t: &Tokens, profile: Value) -> Value {
    let iat = ahora();
    let token = json!({
        "iat": iat,
        "exp": iat + t.expires_in,
        "token": t.access_token,
        "refresh_token": t.refresh_token,
    });
    json!({
        "type": "Ely",
        "msa-client-id": client_id,
        "msa": token,
        "ygg": token,
        "profile": profile,
        "active": true,
    })
}

/// Mete la cuenta en el `accounts.json` conservando las que ya hubiera.
///
/// Pura para poder probarla: es la función que, mal hecha, le borra a alguien la
/// cuenta de Microsoft que ya tenía puesta.
pub fn merge_into_accounts(existing: &str, nueva: Value) -> Result<String, AuthError> {
    let mut root: Value = if existing.trim().is_empty() {
        json!({ "formatVersion": ACCOUNTS_FORMAT_VERSION, "accounts": [] })
    } else {
        serde_json::from_str(existing).map_err(|_| AuthError::BadAccountsFile)?
    };
    if !root.is_object() {
        return Err(AuthError::BadAccountsFile);
    }
    root["formatVersion"] = json!(ACCOUNTS_FORMAT_VERSION);

    let id_nueva = nueva["profile"]["id"].as_str().unwrap_or("").to_string();
    let lista = root
        .get_mut("accounts")
        .and_then(|a| a.as_array_mut())
        .ok_or(AuthError::BadAccountsFile)?;

    // Entrar dos veces con la misma cuenta la actualiza, no la duplica.
    lista.retain(|a| a["profile"]["id"].as_str().unwrap_or("") != id_nueva);
    // La nueva pasa a ser la activa, así que ninguna otra puede seguir siéndolo.
    for a in lista.iter_mut() {
        if let Some(o) = a.as_object_mut() {
            o.remove("active");
        }
    }
    lista.push(nueva);

    Ok(serde_json::to_string_pretty(&root)?)
}

/// Escribe la cuenta en disco.
pub fn save_account(layout: &Layout, nueva: Value) -> Result<(), AuthError> {
    let ruta = layout.accounts();
    if let Some(p) = ruta.parent() {
        std::fs::create_dir_all(p)?;
    }
    let previo = std::fs::read_to_string(&ruta).unwrap_or_default();
    std::fs::write(&ruta, merge_into_accounts(&previo, nueva)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Réplica de `AccountData.cpp::profileFromJSONV3`.
    ///
    /// Copiar aquí las reglas de upstream es el único modo de que una prueba nuestra
    /// falle antes de que falle el motor. Sin esto, un perfil inválido no se nota
    /// hasta que alguien se queda sin poder entrar y sin mensaje que lo explique.
    fn el_motor_aceptaria(p: &Value) -> bool {
        let cadena = |v: &Value, k: &str| v.get(k).map(|x| x.is_string()).unwrap_or(false);
        if !cadena(p, "id") || !cadena(p, "name") {
            return false;
        }
        let Some(skin) = p.get("skin") else { return false };
        if !skin.is_object()
            || !cadena(skin, "id")
            || !cadena(skin, "url")
            || !cadena(skin, "variant")
        {
            return false;
        }
        let Some(capes) = p.get("capes").and_then(|c| c.as_array()) else {
            return false;
        };
        capes
            .iter()
            .all(|c| cadena(c, "id") && cadena(c, "url") && cadena(c, "alias"))
    }

    fn tokens() -> Tokens {
        Tokens {
            access_token: "acceso".into(),
            refresh_token: "refresco".into(),
            expires_in: 3600,
        }
    }

    #[test]
    fn el_perfil_cumple_lo_que_exige_el_motor() {
        let mojang = json!({
            "id": "0123456789abcdef0123456789abcdef",
            "name": "Kevs",
            "skins": [{ "id": "s1", "state": "ACTIVE", "url": "https://x/s.png", "variant": "SLIM" }],
            "capes": []
        });
        let p = profile_json(&mojang);
        assert!(el_motor_aceptaria(&p));
        assert_eq!(p["name"], "Kevs");
        assert_eq!(p["skin"]["variant"], "SLIM");
    }

    #[test]
    fn sin_piel_el_perfil_sigue_siendo_valido() {
        // Una cuenta recién creada en Ely.by no tiene piel. Si por eso omitiéramos
        // `skin`, el motor tiraría la cuenta entera y nadie sabría por qué.
        let p = profile_json(&json!({ "id": "abc", "name": "Nuevo", "skins": [] }));
        assert!(el_motor_aceptaria(&p), "una cuenta sin piel debe poder jugar");
        assert_eq!(p["skin"]["url"], "");
        assert!(p["capes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn entre_varias_pieles_gana_la_activa() {
        let p = profile_json(&json!({
            "id": "abc", "name": "N",
            "skins": [
                { "id": "vieja", "state": "INACTIVE", "url": "https://x/v.png", "variant": "CLASSIC" },
                { "id": "buena", "state": "ACTIVE",   "url": "https://x/b.png", "variant": "CLASSIC" }
            ]
        }));
        assert_eq!(p["skin"]["id"], "buena");
    }

    #[test]
    fn la_cuenta_lleva_ygg_igual_que_msa() {
        // `ElyYggdrasilTokenStep` no llama a ninguna API: copia el token. Si aquí no
        // se copiara, el motor tendría cuenta pero no podría autenticar el juego.
        let a = account_json("cid", &tokens(), profile_json(&json!({ "id": "a", "name": "N" })));
        assert_eq!(a["type"], "Ely");
        assert_eq!(a["msa-client-id"], "cid");
        assert_eq!(a["ygg"], a["msa"]);
        assert_eq!(a["msa"]["token"], "acceso");
        assert_eq!(a["msa"]["refresh_token"], "refresco");
        assert!(a["msa"]["exp"].as_u64().unwrap() > a["msa"]["iat"].as_u64().unwrap());
    }

    #[test]
    fn no_borramos_la_cuenta_que_ya_estaba() {
        let previo = json!({
            "formatVersion": 3,
            "accounts": [{ "type": "MSA", "active": true, "profile": { "id": "otra", "name": "Yahir" } }]
        })
        .to_string();
        let out = merge_into_accounts(
            &previo,
            account_json("cid", &tokens(), profile_json(&json!({ "id": "nueva", "name": "Kevs" }))),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let cuentas = v["accounts"].as_array().unwrap();
        assert_eq!(cuentas.len(), 2, "se perdió una cuenta");
        assert!(cuentas.iter().any(|a| a["profile"]["id"] == "otra"));
    }

    #[test]
    fn solo_una_cuenta_queda_activa() {
        let previo = json!({
            "formatVersion": 3,
            "accounts": [{ "type": "MSA", "active": true, "profile": { "id": "otra", "name": "Y" } }]
        })
        .to_string();
        let out = merge_into_accounts(
            &previo,
            account_json("cid", &tokens(), profile_json(&json!({ "id": "nueva", "name": "K" }))),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let activas: Vec<_> = v["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["active"] == json!(true))
            .collect();
        assert_eq!(activas.len(), 1);
        assert_eq!(activas[0]["profile"]["id"], "nueva");
    }

    #[test]
    fn entrar_dos_veces_no_duplica_la_cuenta() {
        let uno = merge_into_accounts(
            "",
            account_json("cid", &tokens(), profile_json(&json!({ "id": "yo", "name": "K" }))),
        )
        .unwrap();
        let dos = merge_into_accounts(
            &uno,
            account_json("cid", &tokens(), profile_json(&json!({ "id": "yo", "name": "K" }))),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&dos).unwrap();
        assert_eq!(v["accounts"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn siempre_escribimos_la_version_que_el_motor_espera() {
        // Con otro número el motor renombra el archivo a accounts-old.json y
        // arranca con la lista vacía: se llevaría por delante lo que hubiera.
        let out = merge_into_accounts(
            "",
            account_json("cid", &tokens(), profile_json(&json!({ "id": "a", "name": "N" }))),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["formatVersion"], 3);
    }

    #[test]
    fn un_archivo_ilegible_no_se_pisa_a_ciegas() {
        // Preferimos fallar a sobrescribir algo que no entendemos.
        let r = merge_into_accounts(
            "{esto no es json",
            account_json("cid", &tokens(), profile_json(&json!({ "id": "a", "name": "N" }))),
        );
        assert!(matches!(r, Err(AuthError::BadAccountsFile)));
    }

    #[test]
    fn el_sondeo_sabe_esperar_en_vez_de_rendirse() {
        // Mientras el usuario teclea el código, Ely.by responde `authorization_pending`
        // una y otra vez. Tomarlo por un fallo cancelaría el login a los pocos segundos.
        assert!(matches!(clasificar_error("authorization_pending"), Ok(Poll::Pending)));
        assert!(matches!(clasificar_error("slow_down"), Ok(Poll::Pending)));
        assert!(matches!(clasificar_error("access_denied"), Ok(Poll::Denied)));
        assert!(matches!(clasificar_error("expired_token"), Err(AuthError::Expired)));
        assert!(matches!(clasificar_error("cualquier_otra"), Err(AuthError::Remote(_, _))));
    }
}
