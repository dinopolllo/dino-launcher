//! Construir la línea de comandos de Java y lanzar el juego como hijo directo de Dino.
//!
//! En 1.x el juego era nieto de Dino (Dino → motor → Java) y para saber de él había que
//! listar procesos con PowerShell. Aquí `launch` devuelve el `Child`: PID, salida y fin de
//! la partida llegan sin preguntar a nadie.

use crate::install::Installed;
use crate::model::{allowed, Arg, Features};
use crate::{io_err, Dirs, Result};
use md5::{Digest, Md5};
use std::collections::HashMap;
use std::path::Path;

/// Cuenta con la que se lanza.
#[derive(Debug, Clone)]
pub struct Account {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    /// `msa` para Microsoft, `legacy` para offline.
    pub user_type: String,
}

impl Account {
    /// Cuenta sin conexión: el UUID es el de `UUID.nameUUIDFromBytes("OfflinePlayer:<nombre>")`,
    /// igual que en 1.x (`auth.rs`) y que el servidor. Mismo nombre, mismo inventario.
    pub fn offline(name: &str) -> Self {
        let mut h: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}").as_bytes()).into();
        h[6] = (h[6] & 0x0f) | 0x30;
        h[8] = (h[8] & 0x3f) | 0x80;
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        Self { name: name.into(), uuid: hex, access_token: "0".into(), user_type: "legacy".into() }
    }
}

pub struct LaunchSpec<'a> {
    pub account: &'a Account,
    pub game_dir: &'a Path,
    pub max_mem_mb: u32,
    pub min_mem_mb: u32,
    pub extra_jvm_args: &'a [String],
    /// `host[:puerto]` para entrar directo (Quick Play, ≥ 1.20).
    pub server: Option<&'a str>,
}

pub fn command(dirs: &Dirs, inst: &Installed, spec: &LaunchSpec) -> Result<std::process::Command> {
    let v = &inst.version;
    let f = Features { quick_play_multiplayer: spec.server.is_some() };
    let sep = if cfg!(windows) { ";" } else { ":" };

    // classpath: librerías permitidas que van al classpath, sin repetir, y el cliente al final.
    let mut seen = std::collections::HashSet::new();
    let mut cp: Vec<String> = Vec::new();
    for l in &v.libraries {
        if !l.include_in_classpath || !allowed(l.rules.as_deref(), &f) {
            continue;
        }
        if let Some(p) = l.path() {
            let full = dirs.libraries().join(p).to_string_lossy().into_owned();
            if seen.insert(full.clone()) {
                cp.push(full);
            }
        }
    }
    cp.push(inst.client_jar.to_string_lossy().into_owned());

    let natives = dirs.version_dir(&v.id).join("natives");
    std::fs::create_dir_all(&natives).map_err(io_err(&natives))?;
    let assets_index = v.asset_index.as_ref().map(|a| a.id.clone()).or_else(|| v.assets.clone()).unwrap_or_default();

    let vars: HashMap<&str, String> = HashMap::from([
        ("auth_player_name", spec.account.name.clone()),
        ("auth_uuid", spec.account.uuid.clone()),
        ("auth_access_token", spec.account.access_token.clone()),
        ("auth_xuid", String::new()),
        ("clientid", String::new()),
        ("user_type", spec.account.user_type.clone()),
        ("version_name", v.id.clone()),
        ("version_type", v.kind.clone().unwrap_or_else(|| "release".into())),
        ("game_directory", spec.game_dir.to_string_lossy().into_owned()),
        ("assets_root", dirs.assets().to_string_lossy().into_owned()),
        ("assets_index_name", assets_index),
        ("natives_directory", natives.to_string_lossy().into_owned()),
        ("library_directory", dirs.libraries().to_string_lossy().into_owned()),
        ("classpath_separator", sep.into()),
        ("classpath", cp.join(sep)),
        ("launcher_name", "dino-launcher".into()),
        ("launcher_version", env!("CARGO_PKG_VERSION").into()),
        ("quickPlayMultiplayer", spec.server.unwrap_or_default().into()),
    ]);
    let subst = |s: &str| -> String {
        let mut out = s.to_string();
        for (k, val) in &vars {
            out = out.replace(&format!("${{{k}}}"), val);
        }
        out
    };
    let expand = |args: &[Arg]| -> Vec<String> {
        let mut out = Vec::new();
        for a in args {
            match a {
                Arg::Plain(s) => out.push(subst(s)),
                Arg::Ruled { rules, value } => {
                    if allowed(Some(rules), &f) {
                        out.extend(value.iter().map(|s| subst(s)));
                    }
                }
            }
        }
        out
    };

    let args = v.arguments.clone().unwrap_or_default();
    let mut cmd = std::process::Command::new(&inst.java);
    cmd.arg(format!("-Xmx{}M", spec.max_mem_mb)).arg(format!("-Xms{}M", spec.min_mem_mb));
    cmd.args(spec.extra_jvm_args);
    cmd.args(expand(&args.jvm));
    cmd.arg(&v.main_class);
    cmd.args(expand(&args.game));
    cmd.current_dir(spec.game_dir);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_uuid_offline_es_el_de_minecraft() {
        // Mismo valor que comprueba 1.x en auth.rs (UUID.nameUUIDFromBytes de Java).
        assert_eq!(Account::offline("Notch").uuid, "b50ad385829d3141a2167e7d7539ba7f");
    }
}
