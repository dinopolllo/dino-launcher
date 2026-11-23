//! La partida: lanzarla, saber en qué estado está, cerrarla bien y reengancharse a ella.
//!
//! En 1.x el juego era nieto de Dino y su estado se adivinaba listando procesos con
//! PowerShell y escondiendo ventanas del motor cada 15 ms. Aquí el juego es hijo directo:
//! - [`launch`] deja anotado `session.json` (PID, instancia, ejecutable de Java);
//! - [`wait`] informa de `preparing → playing → idle` sin sondeo de la interfaz;
//! - [`stop`] pide cerrar como la X y solo fuerza tras 30 s (guardar un mundo grande tarda);
//! - [`running`] reconoce, al reabrir Dino, una partida que siguiera abierta.

use dino_auth::{Accounts, AccountsError, AuthError};
use dino_core::events::SessionState;
use dino_core::schema::{self, InstallState, InstanceManifest, JavaChoice, Memory, Paths};
use dino_core::{os, DinoError, ErrorCode};
use dino_minecraft::launch::{Account, LaunchSpec};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Lo que se anota de la partida en curso (`<datos>/session.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub pid: u32,
    pub instance: String,
    /// Segundos Unix.
    pub started: u64,
    /// Ejecutable de Java lanzado: un PID reutilizado por otro programa no se confunde.
    pub java: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// `host[:puerto]` para entrar directo (Quick Play).
    pub server: Option<String>,
    /// Sobrescribe la memoria de la instancia, en MiB.
    pub max_mem_mb: Option<u32>,
    /// Cuenta por uuid; `None` = la activa.
    pub account: Option<String>,
}

pub struct Session {
    pub record: SessionRecord,
    child: Option<std::process::Child>,
}

/// Cómo terminó la partida.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Código de salida, si Dino era el padre (`None` tras reengancharse).
    pub exit_code: Option<i32>,
    /// Llegó a abrir su ventana.
    pub reached_window: bool,
    pub played: Duration,
}

fn record_path(paths: &Paths) -> PathBuf {
    paths.root.join("session.json")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn err(code: ErrorCode, detail: impl std::fmt::Display) -> DinoError {
    DinoError::new(code, detail.to_string())
}

/// La memoria que le toca: la que diga quien lanza, la fijada en la instancia o la
/// recomendada para esta máquina.
pub fn memory_for(manifest: &InstanceManifest, override_mb: Option<u32>) -> (u32, u32) {
    if let Some(m) = override_mb {
        return (1024.min(m), m);
    }
    match manifest.memory {
        Memory::Fixed { max_mb } => (1024.min(max_mb), max_mb),
        Memory::Auto => os::recommended_memory(os::total_memory_mb().unwrap_or(8192)),
    }
}

/// Lanza la instancia `id`. Falla con un código estable si no se puede (sin instalar, ya
/// hay una partida, sin cuenta, sesión de Microsoft caducada…).
pub fn launch(
    paths: &Paths,
    accounts: &Accounts,
    engine_accounts: &Path,
    id: &str,
    opts: &LaunchOptions,
) -> Result<Session, DinoError> {
    if let Some(r) = running(paths) {
        return Err(err(ErrorCode::Busy, format!("ya hay una partida de «{}» (pid {})", r.instance, r.pid)));
    }
    let manifest: InstanceManifest = schema::load(&paths.instance_manifest(id))
        .map_err(|e| err(ErrorCode::Internal, e))?
        .ok_or_else(|| err(ErrorCode::NotInstalled, format!("«{id}» no está instalada")))?;
    if manifest.state != InstallState::Ready {
        return Err(err(ErrorCode::NotInstalled, format!("«{id}» quedó a medias: hay que repararla")));
    }
    let mut inst = dino_minecraft::install::installed(paths, &manifest.minecraft, &manifest.loader)
        .map_err(|e| err(ErrorCode::JavaUnavailable, e))?;
    if let JavaChoice::Path { path } = &manifest.java {
        if !path.exists() {
            return Err(err(ErrorCode::JavaUnavailable, format!("el Java elegido no está: {}", path.display())));
        }
        inst.java = path.clone();
    }
    let identity = accounts.launch_identity(opts.account.as_deref(), engine_accounts).map_err(|e| match e {
        AccountsError::Auth(AuthError::Expired(n)) => err(ErrorCode::SessionExpired, n),
        AccountsError::NotFound(n) => err(ErrorCode::AccountRequired, n),
        other => err(ErrorCode::Internal, other),
    })?;
    let (min, max) = memory_for(&manifest, opts.max_mem_mb);
    let account = Account {
        name: identity.name,
        uuid: identity.uuid,
        access_token: identity.access_token,
        user_type: identity.user_type,
    };

    // Los args del jugador van después de los nuestros: si repite uno, gana el suyo.
    let mut jvm = vec!["-XX:+UseG1GC".to_string(), "-XX:MaxGCPauseMillis=50".to_string()];
    jvm.extend(manifest.jvm_args.iter().cloned());
    let game_dir = paths.game_dir(id);
    let spec = LaunchSpec {
        account: &account,
        game_dir: &game_dir,
        max_mem_mb: max,
        min_mem_mb: min,
        extra_jvm_args: &jvm,
        server: opts.server.as_deref(),
    };
    let mut cmd = dino_minecraft::launch::command(paths, &inst, &spec).map_err(|e| err(ErrorCode::Internal, e))?;
    let logdir = paths.instance_dir(id).join("dino-logs");
    std::fs::create_dir_all(&logdir).map_err(|e| err(ErrorCode::DiskPermission, e))?;
    let out = std::fs::File::create(logdir.join("game.log")).map_err(|e| err(ErrorCode::DiskPermission, e))?;
    let out2 = out.try_clone().map_err(|e| err(ErrorCode::Internal, e))?;
    cmd.stdout(out).stderr(out2);
    let child = dino_minecraft::launch::spawn(cmd).map_err(|e| err(ErrorCode::JavaUnavailable, e))?;

    let record = SessionRecord { pid: child.id(), instance: id.into(), started: now(), java: inst.java.clone() };
    let raw = serde_json::to_vec_pretty(&record).expect("serializa");
    schema::write_atomic(&record_path(paths), &raw).map_err(|e| err(ErrorCode::Internal, e))?;
    tracing::info!(instance = %id, pid = record.pid, mem = max, "partida lanzada");
    Ok(Session { record, child: Some(child) })
}

/// La partida en curso, si la hay. Un `session.json` que apunta a un proceso muerto o a
/// otro programa con el mismo PID se borra.
pub fn running(paths: &Paths) -> Option<SessionRecord> {
    let p = record_path(paths);
    let rec: SessionRecord = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
    let same_exe = os::process_exe(rec.pid).is_some_and(|exe| same_file(&exe, &rec.java));
    if os::process_alive(rec.pid) && same_exe {
        Some(rec)
    } else {
        let _ = std::fs::remove_file(p);
        None
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
    norm(a) == norm(b) || std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

/// Reengancharse a una partida ya abierta (al reabrir Dino).
pub fn attach(rec: SessionRecord) -> Session {
    Session { record: rec, child: None }
}

/// ¿Abrió ya su ventana? En Windows se mira la ventana; en el resto, la línea que el juego
/// escribe al crearla.
fn window_open(paths: &Paths, rec: &SessionRecord) -> bool {
    if let Some(v) = os::has_visible_window(rec.pid) {
        return v;
    }
    let log = paths.game_dir(&rec.instance).join("logs").join("latest.log");
    let fresh = std::fs::metadata(&log)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) >= rec.started)
        .unwrap_or(false);
    fresh && std::fs::read_to_string(&log).is_ok_and(|s| s.contains("Backend library: LWJGL"))
}

/// Espera a que termine la partida, contando cada cambio de estado.
pub fn wait(paths: &Paths, mut s: Session, on_state: &dyn Fn(SessionState)) -> Outcome {
    let t0 = Instant::now();
    on_state(SessionState::Preparing);
    let mut reached = false;
    let exit_code = loop {
        if !reached && window_open(paths, &s.record) {
            reached = true;
            on_state(SessionState::Playing);
        }
        match s.child.as_mut() {
            Some(c) => {
                if let Ok(Some(st)) = c.try_wait() {
                    break st.code();
                }
            }
            None => {
                if !os::process_alive(s.record.pid) {
                    break None;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let _ = std::fs::remove_file(record_path(paths));
    on_state(SessionState::Idle);
    tracing::info!(instance = %s.record.instance, ?exit_code, secs = t0.elapsed().as_secs(), "partida terminada");
    Outcome { exit_code, reached_window: reached, played: t0.elapsed() }
}

/// Cómo se cerró.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Hizo caso al cierre normal.
    Clean,
    /// No respondió en `grace` y se forzó.
    Forced,
    /// No había partida.
    NotRunning,
}

/// Cierra la partida en curso: primero como la X (el juego guarda el mundo) y, si en
/// `grace` no se ha ido, a la fuerza. En 1.x el plazo era 30 s y se mantiene.
pub fn stop(paths: &Paths, grace: Duration) -> Stopped {
    let Some(rec) = running(paths) else { return Stopped::NotRunning };
    os::close_windows(rec.pid, false);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !os::process_alive(rec.pid) {
            let _ = std::fs::remove_file(record_path(paths));
            return Stopped::Clean;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    os::terminate(rec.pid);
    let _ = std::fs::remove_file(record_path(paths));
    Stopped::Forced
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(n: &str) -> Paths {
        let d = std::env::temp_dir().join(format!("dino-session-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Paths::new(d)
    }

    fn write(p: &Paths, rec: &SessionRecord) {
        std::fs::write(record_path(p), serde_json::to_vec(rec).unwrap()).unwrap();
    }

    /// Un proceso que dura: el «juego» de las pruebas.
    fn sleeper() -> std::process::Child {
        #[cfg(windows)]
        let c = std::process::Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn();
        #[cfg(not(windows))]
        let c = std::process::Command::new("sleep").arg("60").spawn();
        c.unwrap()
    }

    #[test]
    fn un_registro_de_un_proceso_muerto_no_cuenta_y_se_borra() {
        let p = paths("muerto");
        write(&p, &SessionRecord { pid: u32::MAX - 3, instance: "x".into(), started: 0, java: "java".into() });
        assert!(running(&p).is_none());
        assert!(!record_path(&p).exists());
    }

    #[test]
    fn un_pid_reutilizado_por_otro_programa_no_se_confunde_con_el_juego() {
        let p = paths("reutilizado");
        let me = std::process::id();
        write(&p, &SessionRecord { pid: me, instance: "x".into(), started: 0, java: "C:/no/es/java.exe".into() });
        assert!(running(&p).is_none(), "este proceso está vivo pero no es el Java anotado");
    }

    #[test]
    fn se_reconoce_una_partida_viva_y_se_cierra() {
        let p = paths("viva");
        let mut child = sleeper();
        let exe = os::process_exe(child.id()).unwrap();
        write(&p, &SessionRecord { pid: child.id(), instance: "x".into(), started: now(), java: exe });
        assert_eq!(running(&p).map(|r| r.pid), Some(child.id()));
        // Sin ventana que cerrar (o con SIGTERM en Unix): con 1 s de plazo se resuelve.
        let r = stop(&p, Duration::from_secs(1));
        assert!(matches!(r, Stopped::Clean | Stopped::Forced));
        let _ = child.wait();
        assert!(!os::process_alive(child.id()));
        assert!(running(&p).is_none());
    }

    #[test]
    fn la_memoria_sale_de_la_instancia_o_de_la_maquina() {
        let mut m: InstanceManifest = serde_json::from_str(
            r#"{"schema":1,"id":"x","minecraft":"1.21.1","loader":{"kind":"vanilla"},"state":"ready"}"#,
        )
        .unwrap();
        assert_eq!(memory_for(&m, Some(6144)), (1024, 6144));
        m.memory = Memory::Fixed { max_mb: 8192 };
        assert_eq!(memory_for(&m, None), (1024, 8192));
        m.memory = Memory::Auto;
        let (_, max) = memory_for(&m, None);
        assert!((1536..=8192).contains(&max));
    }
}
