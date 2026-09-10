//! Cuentas escritas por nosotros en el `accounts.json` del motor.
//!
//! El motor resuelve la autenticación, pero solo desde su propia ventana: no hay
//! bandera de línea de comandos que abra su diálogo de cuentas (verificado contra
//! `--help` del release 11.1.0). Para Microsoft no queda otra que pasar por ahí, pero
//! una cuenta **sin conexión** no autentica contra nadie: es un nombre y un UUID
//! derivado de él, así que se puede crear aquí y evitar la ventana ajena entera.
//!
//! Todo verificado leyendo el fork en el release fijado: `MinecraftAccount.cpp`
//! (`createOffline`, `uuidFromUsername`), `AccountData.cpp` y `AccountList.cpp`.
//! Como `ENGINE_VERSION` está fijado, el formato no se mueve por su cuenta.

use crate::engine::Layout;
use serde_json::{json, Value};

/// Versión del contenedor de `accounts.json`, de `AccountList.cpp`.
///
/// Importa más de lo que parece: si el número no coincide, el motor **renombra el
/// archivo a `accounts-old.json`** y arranca con la lista vacía. Escribirlo mal no
/// da error, se lleva por delante las cuentas que ya hubiera.
const ACCOUNTS_FORMAT_VERSION: u64 = 3;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("error de disco: {0}")]
    Io(#[from] std::io::Error),
    #[error("respuesta inesperada: {0}")]
    Json(#[from] serde_json::Error),
    #[error("el accounts.json del motor está corrupto")]
    BadAccountsFile,
    #[error("{0}")]
    NombreInvalido(String),
}

/// Comprueba que el nombre sirva como nombre de Minecraft.
///
/// Las reglas son las de Mojang: de 3 a 16 caracteres, letras, dígitos y guion bajo.
/// No es purismo — el servidor rechaza lo que se salga de ahí, y es mucho mejor
/// decirlo mientras se escribe que dejar que falle al entrar a jugar.
pub fn validar_nombre(n: &str) -> Result<(), AuthError> {
    let n = n.trim();
    if n.chars().count() < 3 || n.chars().count() > 16 {
        return Err(AuthError::NombreInvalido(
            "El nombre debe tener entre 3 y 16 caracteres.".into(),
        ));
    }
    if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(AuthError::NombreInvalido(
            "Solo letras, números y guion bajo.".into(),
        ));
    }
    Ok(())
}

/// UUID sin conexión derivado del nombre.
///
/// Reimplementa `UUID.nameUUIDFromBytes` de Java sobre `OfflinePlayer:<nombre>`, que
/// es lo que hace `MinecraftAccount::uuidFromUsername` y, antes que él, el propio
/// servidor de Minecraft. **Tiene que coincidir exactamente**: es el identificador con
/// el que el mundo guarda el inventario y la posición, así que derivarlo de otra forma
/// haría que alguien reapareciera sin nada de lo suyo.
///
/// Se devuelve en 32 hex sin guiones, el formato `QUuid::Id128` que escribe el motor.
pub fn uuid_offline(nombre: &str) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(format!("OfflinePlayer:{nombre}").as_bytes());
    let mut d: [u8; 16] = h.finalize().into();
    d[6] = (d[6] & 0x0f) | 0x30; // versión 3
    d[8] = (d[8] & 0x3f) | 0x80; // variante IETF
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn ahora() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// La cuenta sin conexión tal como la guarda el motor.
///
/// Calcado de `MinecraftAccount::createOffline` y de cómo lo serializa
/// `AccountData::saveState`. Tres cosas que parecen adorno y no lo son:
///
///  - **`token: "0"`** es literal. Versiones anteriores a la 7.2 usaban la cadena
///    `"offline"` y el motor todavía la traduce a `"0"` al cargar; escribir cualquier
///    otra cosa daría una cuenta que no arranca el juego.
///  - **`extra.userName`** es de donde sale el nombre del jugador al lanzar, no del
///    perfil. Sin él la partida entra sin nombre.
///  - **el bloque `profile` completo** —con `skin` de cadenas vacías y `capes` vacío—
///    porque `profileFromJSONV3` descarta el perfil entero si falta cualquiera de
///    esos campos, y sin perfil válido el motor considera que la cuenta no posee
///    Minecraft y **lanza el juego en modo Demo**. Ese es el fallo que más caro
///    saldría: no avisa, simplemente se juega una demo de 100 minutos.
pub fn offline_account(nombre: &str) -> Result<Value, AuthError> {
    validar_nombre(nombre)?;
    let nombre = nombre.trim();

    Ok(json!({
        "type": "Offline",
        "ygg": {
            "iat": ahora(),
            "token": "0",
            "extra": {
                "userName": nombre,
                // El motor mete aquí un UUID nuevo cada vez; solo tiene que existir.
                "clientToken": uuid_offline(&format!("cliente:{nombre}")),
            },
        },
        "profile": {
            "id": uuid_offline(nombre),
            "name": nombre,
            "skin": { "id": "", "url": "", "variant": "" },
            "capes": [],
        },
        "active": true,
    }))
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

    // Entrar dos veces con el mismo nombre la actualiza, no la duplica.
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
    /// hasta que alguien se encuentra jugando una demo sin saber por qué.
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
        p.get("capes").and_then(|c| c.as_array()).is_some()
    }

    #[test]
    fn el_uuid_es_el_mismo_que_calcula_minecraft() {
        // `UUID.nameUUIDFromBytes("OfflinePlayer:Notch")` da este valor; es el mismo
        // que usan el servidor y el motor. Si no coincidiera, alguien entraría con
        // otra identidad y aparecería sin su inventario ni su posición.
        assert_eq!(uuid_offline("Notch"), "b50ad385829d3141a2167e7d7539ba7f");
        // Versión 3 y variante IETF, en los nibbles que manda el RFC 4122.
        let u = uuid_offline("Kevs");
        assert_eq!(&u[12..13], "3");
        assert!(matches!(&u[16..17], "8" | "9" | "a" | "b"));
        assert_eq!(u.len(), 32, "32 hex sin guiones, como QUuid::Id128");
    }

    #[test]
    fn nombres_distintos_dan_uuids_distintos() {
        assert_ne!(uuid_offline("Kevs"), uuid_offline("kevs"), "distingue mayúsculas");
        assert_ne!(uuid_offline("Kevs"), uuid_offline("Yahir"));
    }

    #[test]
    fn la_cuenta_tiene_lo_que_el_motor_exige() {
        let a = offline_account("Kevs").unwrap();
        assert_eq!(a["type"], "Offline");
        assert_eq!(a["ygg"]["token"], "0", "el motor espera el literal \"0\"");
        assert_eq!(a["ygg"]["extra"]["userName"], "Kevs", "de aquí sale el nombre al jugar");
        assert!(a["ygg"]["extra"]["clientToken"].as_str().is_some());
        assert_eq!(a["profile"]["name"], "Kevs");
        assert_eq!(a["profile"]["id"], uuid_offline("Kevs"));
        assert!(el_motor_aceptaria(&a["profile"]), "sin perfil válido se jugaría en modo Demo");
    }

    #[test]
    fn no_escribimos_entitlement() {
        // Es lo que hace que el motor asuma que la cuenta posee Minecraft: sin bloque
        // `entitlement` y con perfil válido, `resumeStateFromV3` pone ownsMinecraft.
        // Escribirlo a mano sería justo lo que la mandaría a modo Demo.
        let a = offline_account("Kevs").unwrap();
        assert!(a.get("entitlement").is_none());
    }

    #[test]
    fn el_nombre_se_valida_como_lo_valida_mojang() {
        for malo in ["ab", "", "   ", "un_nombre_demasiado_largo", "con espacio", "acentós", "hey!"] {
            assert!(validar_nombre(malo).is_err(), "{malo:?} debería rechazarse");
        }
        for bueno in ["Kevs", "abc", "Un_Nombre_De_16C", "jugador_123"] {
            assert!(validar_nombre(bueno).is_ok(), "{bueno:?} debería aceptarse");
        }
    }

    #[test]
    fn los_espacios_de_los_bordes_no_cuentan() {
        let a = offline_account("  Kevs  ").unwrap();
        assert_eq!(a["profile"]["name"], "Kevs");
        assert_eq!(a["profile"]["id"], uuid_offline("Kevs"), "el UUID sale del nombre limpio");
    }

    #[test]
    fn no_borramos_la_cuenta_que_ya_estaba() {
        let previo = json!({
            "formatVersion": 3,
            "accounts": [{ "type": "MSA", "active": true, "profile": { "id": "otra", "name": "Yahir" } }]
        })
        .to_string();
        let out = merge_into_accounts(&previo, offline_account("Kevs").unwrap()).unwrap();
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
        let out = merge_into_accounts(&previo, offline_account("Kevs").unwrap()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let activas: Vec<_> = v["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["active"] == json!(true))
            .collect();
        assert_eq!(activas.len(), 1);
        assert_eq!(activas[0]["profile"]["name"], "Kevs");
    }

    #[test]
    fn el_mismo_nombre_dos_veces_no_duplica() {
        let uno = merge_into_accounts("", offline_account("Kevs").unwrap()).unwrap();
        let dos = merge_into_accounts(&uno, offline_account("Kevs").unwrap()).unwrap();
        let v: Value = serde_json::from_str(&dos).unwrap();
        assert_eq!(v["accounts"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn siempre_escribimos_la_version_que_el_motor_espera() {
        // Con otro número el motor renombra el archivo a accounts-old.json y
        // arranca con la lista vacía: se llevaría por delante lo que hubiera.
        let out = merge_into_accounts("", offline_account("Kevs").unwrap()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["formatVersion"], 3);
    }

    #[test]
    fn un_archivo_ilegible_no_se_pisa_a_ciegas() {
        // Preferimos fallar a sobrescribir algo que no entendemos.
        let r = merge_into_accounts("{esto no es json", offline_account("Kevs").unwrap());
        assert!(matches!(r, Err(AuthError::BadAccountsFile)));
    }
}
