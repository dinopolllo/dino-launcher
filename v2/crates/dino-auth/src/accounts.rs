//! Las cuentas de Dino 2.0, en su propio `accounts.json` (sin secretos).
//!
//! - **Offline**: nativas, se crean aquí.
//! - **Microsoft**: viven en el motor (puente). Aquí solo se refleja que existen
//!   ([`Accounts::sync_bridge`]); la sesión se lee del motor en el momento de jugar.

use crate::{AccountInfo, AuthError};
use dino_core::accounts::{offline_uuid, validate_name, NameProblem};
use dino_core::api::AccountView;
use dino_core::schema::{self, Account, AccountKind, AccountsFile, Paths, StoreError};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum AccountsError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Name(NameProblem),
    #[error("no hay ninguna cuenta con uuid {0}")]
    NotFound(String),
    #[error(transparent)]
    Auth(#[from] AuthError),
}

/// Con qué identidad se lanza el juego.
#[derive(Clone)]
pub struct LaunchIdentity {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    /// `legacy` (offline) o `msa`.
    pub user_type: String,
}

impl std::fmt::Debug for LaunchIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchIdentity")
            .field("name", &self.name)
            .field("uuid", &self.uuid)
            .field("access_token", &"<oculto>")
            .field("user_type", &self.user_type)
            .finish()
    }
}

/// Qué cambió al sincronizar con el motor.
#[derive(Debug, Default, PartialEq)]
pub struct SyncReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// UUID de las cuentas añadidas, en el mismo orden que `added`.
    pub added_uuids: Vec<String>,
}

pub struct Accounts {
    paths: Paths,
    lock: Mutex<()>,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Accounts {
    pub fn new(paths: Paths) -> Self {
        Self { paths, lock: Mutex::new(()) }
    }

    fn load(&self) -> Result<AccountsFile, StoreError> {
        Ok(schema::load(&self.paths.accounts())?.unwrap_or_default())
    }

    fn change<T>(&self, f: impl FnOnce(&mut AccountsFile) -> Result<T, AccountsError>) -> Result<T, AccountsError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut a = self.load()?;
        let out = f(&mut a)?;
        // La activa siempre apunta a una que existe (o a ninguna si no queda ninguna).
        if !a.active.as_ref().is_some_and(|u| a.accounts.iter().any(|x| &x.uuid == u)) {
            a.active = a.accounts.first().map(|x| x.uuid.clone());
        }
        schema::save(&self.paths.accounts(), &a, schema::ACCOUNTS_SCHEMA)?;
        Ok(out)
    }

    /// Las cuentas para la interfaz, con los minutos de sesión que les quedan a las de
    /// Microsoft (leídos del motor, sin el token).
    pub fn list(&self, engine_accounts: &Path) -> Result<Vec<AccountView>, AccountsError> {
        let a = self.load()?;
        let bridge: Vec<AccountInfo> = crate::list(engine_accounts).unwrap_or_default();
        let t = now();
        Ok(a.accounts
            .iter()
            .map(|x| AccountView {
                kind: x.kind,
                name: x.name.clone(),
                uuid: x.uuid.clone(),
                active: a.active.as_deref() == Some(&x.uuid),
                skin: dino_core::api::skin_url(&x.name),
                session_minutes_left: match x.kind {
                    AccountKind::MicrosoftBridge => bridge
                        .iter()
                        .find(|b| b.kind == "MSA" && b.uuid == x.uuid)
                        .and_then(|b| b.expires)
                        .map(|e| e.saturating_sub(t) / 60),
                    AccountKind::Offline => None,
                },
            })
            .collect())
    }

    /// Crea (o vuelve a activar) una cuenta sin conexión y la deja activa.
    pub fn add_offline(&self, name: &str) -> Result<Account, AccountsError> {
        let name = validate_name(name).map_err(AccountsError::Name)?.to_string();
        let uuid = offline_uuid(&name);
        self.change(|a| {
            if !a.accounts.iter().any(|x| x.kind == AccountKind::Offline && x.uuid == uuid) {
                a.accounts.push(Account { kind: AccountKind::Offline, name: name.clone(), uuid: uuid.clone() });
            }
            a.active = Some(uuid.clone());
            Ok(Account { kind: AccountKind::Offline, name, uuid })
        })
    }

    pub fn select(&self, uuid: &str) -> Result<(), AccountsError> {
        self.change(|a| {
            if !a.accounts.iter().any(|x| x.uuid == uuid) {
                return Err(AccountsError::NotFound(uuid.into()));
            }
            a.active = Some(uuid.into());
            Ok(())
        })
    }

    /// Quita la cuenta de Dino. Una de Microsoft sigue en el motor hasta que se quite allí.
    pub fn remove(&self, uuid: &str) -> Result<Account, AccountsError> {
        self.change(|a| {
            let i =
                a.accounts.iter().position(|x| x.uuid == uuid).ok_or_else(|| AccountsError::NotFound(uuid.into()))?;
            let gone = a.accounts.remove(i);
            if gone.kind == AccountKind::MicrosoftBridge && !a.hidden_bridge.contains(&gone.uuid) {
                a.hidden_bridge.push(gone.uuid.clone());
            }
            if a.active.as_deref() == Some(uuid) {
                a.active = a.accounts.first().map(|x| x.uuid.clone());
            }
            Ok(gone)
        })
    }

    /// Deja de esconder una cuenta Microsoft (el jugador volvió a iniciar sesión con ella).
    pub fn unhide(&self, uuid: &str) -> Result<(), AccountsError> {
        self.change(|a| {
            a.hidden_bridge.retain(|h| h != uuid);
            Ok(())
        })
    }

    /// Refleja en Dino las cuentas Microsoft del motor: añade las nuevas, actualiza el
    /// nombre si cambió y quita las que ya no están allí.
    pub fn sync_bridge(&self, engine_accounts: &Path) -> Result<SyncReport, AccountsError> {
        let engine: Vec<AccountInfo> = crate::list(engine_accounts).unwrap_or_default();
        let msa: Vec<&AccountInfo> = engine.iter().filter(|e| e.kind == "MSA" && !e.uuid.is_empty()).collect();
        self.change(|a| {
            let mut r = SyncReport::default();
            // Lo que ya no está en el motor deja de estar escondido.
            a.hidden_bridge.retain(|h| msa.iter().any(|m| &m.uuid == h));
            let hidden = a.hidden_bridge.clone();
            a.accounts.retain(|x| {
                let keep = x.kind != AccountKind::MicrosoftBridge
                    || (msa.iter().any(|m| m.uuid == x.uuid) && !hidden.contains(&x.uuid));
                if !keep {
                    r.removed.push(x.name.clone());
                }
                keep
            });
            for m in msa.iter().filter(|m| !hidden.contains(&m.uuid)) {
                match a.accounts.iter_mut().find(|x| x.kind == AccountKind::MicrosoftBridge && x.uuid == m.uuid) {
                    Some(x) => x.name = m.name.clone(),
                    None => {
                        a.accounts.push(Account {
                            kind: AccountKind::MicrosoftBridge,
                            name: m.name.clone(),
                            uuid: m.uuid.clone(),
                        });
                        r.added.push(m.name.clone());
                        r.added_uuids.push(m.uuid.clone());
                    }
                }
            }
            Ok(r)
        })
    }

    /// La identidad para lanzar con la cuenta `uuid` (o la activa si es `None`).
    ///
    /// Una cuenta Microsoft con la sesión caducada da `AuthError::Expired`: hay que renovar
    /// con el motor antes de jugar.
    pub fn launch_identity(&self, uuid: Option<&str>, engine_accounts: &Path) -> Result<LaunchIdentity, AccountsError> {
        let a = self.load()?;
        let uuid =
            uuid.map(String::from).or(a.active.clone()).ok_or_else(|| AccountsError::NotFound("(ninguna)".into()))?;
        let acc = a.accounts.iter().find(|x| x.uuid == uuid).ok_or_else(|| AccountsError::NotFound(uuid.clone()))?;
        Ok(match acc.kind {
            AccountKind::Offline => LaunchIdentity {
                name: acc.name.clone(),
                uuid: acc.uuid.clone(),
                access_token: "0".into(),
                user_type: "legacy".into(),
            },
            AccountKind::MicrosoftBridge => {
                let s = crate::session(engine_accounts, &acc.name)?;
                LaunchIdentity { name: s.name, uuid: s.uuid, access_token: s.access_token, user_type: "msa".into() }
            }
        })
    }

    /// ¿Conviene renovar con el motor? Menos de 12 h de sesión, la misma regla que el motor.
    pub fn needs_renewal(&self, engine_accounts: &Path) -> bool {
        let t = now();
        crate::list(engine_accounts)
            .unwrap_or_default()
            .iter()
            .any(|e| e.kind == "MSA" && e.expires.is_some_and(|x| x < t + 12 * 3600))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(nombre: &str) -> (Accounts, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("dino-accounts-{}-{nombre}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let engine = root.join("engine-accounts.json");
        (Accounts::new(Paths::new(&root)), engine)
    }

    fn engine_file(p: &Path, msa: &[(&str, &str, u64)]) {
        let acc: Vec<String> = msa
            .iter()
            .map(|(name, uuid, exp)| {
                format!(r#"{{"type":"MSA","ygg":{{"token":"TOKEN-{name}","exp":{exp},"iat":1}},"profile":{{"id":"{uuid}","name":"{name}"}}}}"#)
            })
            .collect();
        std::fs::write(p, format!(r#"{{"formatVersion":3,"accounts":[{}]}}"#, acc.join(","))).unwrap();
    }

    #[test]
    fn quitar_una_microsoft_no_vuelve_con_la_sincronizacion() {
        let (a, e) = setup("esconder");
        engine_file(&e, &[("Mia", "u1", now() + 3600), ("Otra", "u2", now() + 3600)]);
        a.sync_bridge(&e).unwrap();
        a.select("u1").unwrap();
        a.remove("u1").unwrap();
        a.sync_bridge(&e).unwrap();
        let l = a.list(&e).unwrap();
        assert_eq!(
            l.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            vec!["Otra"],
            "sigue en el motor, no en Dino"
        );
        assert!(l[0].active, "la activa pasa a otra cuenta");
        // vuelve a iniciar sesión con ella
        a.unhide("u1").unwrap();
        assert_eq!(a.sync_bridge(&e).unwrap().added, vec!["Mia"]);
        // si desaparece del motor, deja de estar escondida (no se acumula)
        a.remove("u1").unwrap();
        engine_file(&e, &[("Otra", "u2", now() + 3600)]);
        a.sync_bridge(&e).unwrap();
        assert!(a.load().unwrap().hidden_bridge.is_empty());
    }

    #[test]
    fn crear_offline_la_deja_activa_y_no_duplica() {
        let (a, e) = setup("offline");
        let x = a.add_offline(" Steve ").unwrap();
        assert_eq!(x.uuid, offline_uuid("Steve"));
        a.add_offline("Steve").unwrap();
        let l = a.list(&e).unwrap();
        assert_eq!(l.len(), 1);
        assert!(l[0].active);
        assert!(matches!(a.add_offline("no vale"), Err(AccountsError::Name(NameProblem::Characters))));
    }

    #[test]
    fn el_archivo_de_dino_nunca_guarda_tokens() {
        let (a, e) = setup("secretos");
        engine_file(&e, &[("Jugador", "u1", now() + 3600)]);
        a.sync_bridge(&e).unwrap();
        a.add_offline("Otro").unwrap();
        let raw = std::fs::read_to_string(a.paths.accounts()).unwrap();
        assert!(!raw.contains("TOKEN") && !raw.to_lowercase().contains("token"), "{raw}");
    }

    #[test]
    fn sincronizar_con_el_motor_anade_renombra_y_quita() {
        let (a, e) = setup("sync");
        engine_file(&e, &[("Viejo", "u1", now() + 3600)]);
        assert_eq!(a.sync_bridge(&e).unwrap().added, vec!["Viejo"]);
        engine_file(&e, &[("Nuevo", "u1", now() + 3600), ("Otro", "u2", now() + 3600)]);
        let r = a.sync_bridge(&e).unwrap();
        assert_eq!(r.added, vec!["Otro"]);
        assert_eq!(r.added_uuids, vec!["u2"]);
        let names: Vec<String> = a.list(&e).unwrap().into_iter().map(|v| v.name).collect();
        assert_eq!(names, vec!["Nuevo", "Otro"], "cambiar de nombre en Microsoft no duplica la cuenta");
        engine_file(&e, &[("Otro", "u2", now() + 3600)]);
        assert_eq!(a.sync_bridge(&e).unwrap().removed, vec!["Nuevo"]);
    }

    #[test]
    fn lanzar_offline_y_microsoft() {
        let (a, e) = setup("lanzar");
        engine_file(&e, &[("Jugador", "u1", now() + 3600)]);
        a.sync_bridge(&e).unwrap();
        let off = a.add_offline("Steve").unwrap();
        let i = a.launch_identity(None, &e).unwrap();
        assert_eq!(
            (i.uuid.as_str(), i.user_type.as_str(), i.access_token.as_str()),
            (off.uuid.as_str(), "legacy", "0")
        );
        let m = a.launch_identity(Some("u1"), &e).unwrap();
        assert_eq!((m.user_type.as_str(), m.access_token.as_str()), ("msa", "TOKEN-Jugador"));
        assert!(!format!("{m:?}").contains("TOKEN"));
    }

    #[test]
    fn una_sesion_caducada_pide_renovar() {
        let (a, e) = setup("caducada");
        engine_file(&e, &[("Jugador", "u1", 10)]);
        a.sync_bridge(&e).unwrap();
        assert!(matches!(a.launch_identity(Some("u1"), &e), Err(AccountsError::Auth(AuthError::Expired(_)))));
        assert!(a.needs_renewal(&e));
        engine_file(&e, &[("Jugador", "u1", now() + 20 * 3600)]);
        assert!(!a.needs_renewal(&e), "con más de 12 h no hace falta");
    }

    #[test]
    fn quitar_la_activa_pasa_a_otra() {
        let (a, e) = setup("quitar");
        let x = a.add_offline("Uno").unwrap();
        a.add_offline("Dos").unwrap();
        a.select(&x.uuid).unwrap();
        a.remove(&x.uuid).unwrap();
        let l = a.list(&e).unwrap();
        assert_eq!(l.len(), 1);
        assert!(l[0].active);
        assert!(matches!(a.select("nada"), Err(AccountsError::NotFound(_))));
    }
}
