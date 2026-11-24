//! `dino-cli`: el núcleo de Dino 2.0 sin interfaz.
//!
//!     dino-cli --version
//!     dino-cli install <ruta-o-url.mrpack> <instancia>
//!     dino-cli launch  <instancia> <jugador> [--msa] [--server host] [--mem MB] [--wait]
//!     dino-cli accounts | verify <cuenta> | migrate-plan <raiz-1.x> <salida.json>
//!     dino-cli diagnostics <salida.zip>
//!
//! Escribe los mismos archivos que la app (`library.json`, `instances/<id>/instance.json`)
//! y registra en `logs/` como ella.
//! Los datos van a `DINO_HOME` o, si no está, a `%USERPROFILE%\dino-v2-data`: a propósito
//! fuera de `AppData`, para no mezclarse con el 1.x instalado.

use dino_core::schema::{self, InstallState, InstanceManifest, JavaChoice, LibraryEntry, PackSource, Paths};
use dino_download::{Downloader, Progress};
use dino_minecraft::{install, mrpack};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const USAGE: &str = "uso:
  dino-cli --version
  dino-cli install <ruta-o-url.mrpack> <instancia>
  dino-cli launch <instancia> [cuenta] [--msa] [--server host] [--mem MB] [--wait]
  dino-cli status | stop
  dino-cli account list | add-offline <nombre> | select <uuid|nombre> | remove <uuid>
  dino-cli account sync | login | renew [--force]
  dino-cli engine ensure
  dino-cli verify <cuenta-microsoft>
  dino-cli migrate-plan <raiz-1.x> <salida.json>
  dino-cli diagnostics <salida.zip>
  dino-cli bench-download <pack.mrpack> <concurrencias, p. ej. 1,4,8,16,32>";

fn home() -> PathBuf {
    std::env::var_os("DINO_HOME").map(PathBuf::from).unwrap_or_else(|| {
        let user =
            std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).expect("sin carpeta de usuario");
        PathBuf::from(user).join("dino-v2-data")
    })
}

fn mb(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

/// Imprime el avance como mucho dos veces por segundo por etiqueta.
struct Printer {
    last: Mutex<std::collections::HashMap<String, Instant>>,
}

impl Printer {
    fn report(&self, label: &str, p: Option<Progress>) {
        let mut last = self.last.lock().unwrap();
        let now = Instant::now();
        let done = p.is_none_or(|p| p.files_done == p.files_total);
        if !done {
            if let Some(t) = last.get(label) {
                if now.duration_since(*t) < Duration::from_millis(500) {
                    return;
                }
            }
        }
        last.insert(label.to_string(), now);
        match p {
            None => eprintln!("· {label}"),
            Some(p) => eprintln!(
                "· {label}: {}/{} archivos · {:.1}/{:.1} MB (bajados {:.1} MB)",
                p.files_done,
                p.files_total,
                mb(p.bytes_done),
                mb(p.bytes_total),
                mb(p.bytes_downloaded)
            ),
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("dino-cli {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let paths = Paths::new(home());
    let _log = dino_log::init(&paths, std::env::var_os("DINO_LOG").is_some()).ok();
    let r = match args.first().map(String::as_str) {
        Some("install") if args.len() >= 3 => cmd_install(&paths, &args[1], &args[2]).await,
        Some("launch") if args.len() >= 2 => cmd_launch(&paths, &args[1..]).await,
        Some("status") => cmd_status(&paths),
        Some("stop") => cmd_stop(&paths),
        Some("accounts") => cmd_account(&paths, &["list".to_string()]).await,
        Some("account") if args.len() >= 2 => cmd_account(&paths, &args[1..]).await,
        Some("engine") if args.get(1).map(String::as_str) == Some("ensure") => cmd_engine_ensure().await,
        Some("verify") if args.len() >= 2 => cmd_verify(&args[1]).await,
        Some("migrate-plan") if args.len() >= 3 => cmd_migrate_plan(&args[1], &args[2]),
        Some("bench-download") if args.len() >= 3 => cmd_bench_download(&paths, &args[1], &args[2]).await,
        Some("diagnostics") if args.len() >= 2 => dino_log::export_report(&paths, std::path::Path::new(&args[1]))
            .map(|n| println!("{n} archivos en {}", args[1]))
            .map_err(|e| e.to_string()),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        tracing::error!(error = %e, "comando fallido");
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

/// Benchmark de descargas: los mods de un pack, desde cero y sin caché, con cada
/// concurrencia. Cada pasada usa una carpeta vacía que se borra al acabar.
async fn cmd_bench_download(paths: &Paths, mrpack: &str, list: &str) -> Result<(), String> {
    let index = mrpack::read_index(std::path::Path::new(mrpack)).map_err(|e| e.to_string())?;
    let levels: Vec<usize> = list.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    for c in levels {
        let dir = paths.root.join("bench").join(format!("c{c}"));
        let _ = std::fs::remove_dir_all(&dir);
        let items: Vec<dino_download::Item> = index
            .files
            .iter()
            .filter_map(|f| {
                Some(dino_download::Item {
                    url: f.downloads.first()?.clone(),
                    dest: dir.join(mrpack::safe_relative(&f.path).ok()?),
                    sha1: f.hashes.get("sha1").cloned(),
                    size: Some(f.file_size).filter(|s| *s > 0),
                    shared: false,
                })
            })
            .collect();
        let t0 = Instant::now();
        let st = Downloader::new(c).fetch_all(items, &|_| {}).await.map_err(|e| e.to_string())?;
        let secs = t0.elapsed().as_secs_f64();
        println!(
            "{}",
            serde_json::json!({"concurrency": c, "files": st.files, "mb": mb(st.downloaded_bytes),
                "secs": secs, "mb_per_s": mb(st.downloaded_bytes) / secs})
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}

/// Raíz del motor (ElyPrism): `bin/` y `engine/`. En la app es la misma carpeta de datos;
/// en `dino-cli` es la del 1.x instalado salvo que se diga otra con `DINO_ENGINE_ROOT`.
fn engine() -> dino_auth::Engine {
    dino_auth::Engine::new(std::env::var_os("DINO_ENGINE_ROOT").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("APPDATA").expect("sin APPDATA")).join("gg.dinolab.launcher")
    }))
}

fn engine_accounts() -> PathBuf {
    engine().accounts()
}

async fn cmd_account(paths: &Paths, args: &[String]) -> Result<(), String> {
    let acc = dino_auth::Accounts::new(paths.clone());
    let e = engine();
    match args.first().map(String::as_str) {
        Some("list") => {
            for v in ok(acc.list(&e.accounts()))? {
                let ses = v.session_minutes_left.map(|m| format!("sesión {m} min")).unwrap_or_default();
                println!("{:16} {:18} {} activa={} {}", format!("{:?}", v.kind), v.name, v.uuid, v.active, ses);
            }
        }
        Some("add-offline") if args.len() >= 2 => {
            let a = ok(acc.add_offline(&args[1]))?;
            println!("{}", serde_json::json!({"name": a.name, "uuid": a.uuid}));
        }
        Some("select") if args.len() >= 2 => {
            let uuid = resolve(&acc, &e, &args[1])?;
            ok(acc.select(&uuid))?;
        }
        Some("remove") if args.len() >= 2 => {
            let a = ok(acc.remove(&args[1]))?;
            println!("quitada {}", a.name);
        }
        Some("sync") => {
            let r = ok(acc.sync_bridge(&e.accounts()))?;
            println!("{}", serde_json::json!({"added": r.added, "removed": r.removed}));
        }
        Some("login") => {
            // El jugador añade su cuenta en la ventana del motor; al cerrarla se sincroniza.
            e.ensure(&Downloader::new(4)).await.map_err(|x| x.to_string())?;
            let mut child = e.open_for_login().map_err(|x| x.to_string())?;
            child.wait().map_err(|x| x.to_string())?;
            let r = ok(acc.sync_bridge(&e.accounts()))?;
            println!("{}", serde_json::json!({"added": r.added, "removed": r.removed}));
        }
        Some("renew") => {
            let force = args.iter().any(|a| a == "--force");
            if !force && !acc.needs_renewal(&e.accounts()) {
                println!("{}", serde_json::json!({"renewed": [], "skipped": "a todas les quedan más de 12 h"}));
                return Ok(());
            }
            let r = e.renew(Duration::from_secs(30)).map_err(|x| x.to_string())?;
            tracing::info!(renewed = ?r.renewed, ms = r.elapsed.as_millis() as u64, hidden = r.windows_hidden, "renovación");
            println!(
                "{}",
                serde_json::json!({"renewed": r.renewed, "ms": r.elapsed.as_millis() as u64, "windows_hidden": r.windows_hidden})
            );
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

fn ok<T>(r: Result<T, dino_auth::AccountsError>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}

/// Una cuenta por uuid o por nombre.
fn resolve(acc: &dino_auth::Accounts, e: &dino_auth::Engine, who: &str) -> Result<String, String> {
    let l = acc.list(&e.accounts()).map_err(|x| x.to_string())?;
    l.iter()
        .find(|v| v.uuid == who)
        .or_else(|| l.iter().find(|v| v.name == who))
        .map(|v| v.uuid.clone())
        .ok_or_else(|| format!("no hay ninguna cuenta «{who}»"))
}

async fn cmd_engine_ensure() -> Result<(), String> {
    let e = engine();
    let t0 = Instant::now();
    let bin = e.ensure(&Downloader::new(4)).await.map_err(|x| x.to_string())?;
    println!("{}", serde_json::json!({"binary": bin, "secs": t0.elapsed().as_secs_f64()}));
    Ok(())
}

async fn cmd_verify(name: &str) -> Result<(), String> {
    let s = dino_auth::session(&engine_accounts(), name).map_err(|e| e.to_string())?;
    let dl = Downloader::new(1);
    let perfil = dino_auth::verify(dl.client(), &s).await.map_err(|e| e.to_string())?;
    println!("{}", serde_json::json!({"valid": true, "profile": perfil, "uuid": s.uuid, "expires": s.expires}));
    Ok(())
}

/// Plan de migración 1.x → 2.0 **en seco**: lee los datos de 1.x en `root` y escribe en
/// `out` qué haría, sin mover ni escribir nada en `root`. Sin secretos.
fn cmd_migrate_plan(root: &str, out: &str) -> Result<(), String> {
    use dino_core::{migrate, schema::Paths};
    let paths = Paths::new(root);
    let (packs, accounts, inst) = migrate::read_v1(&paths).ok_or("no encuentro packs.json de 1.x")?;
    let plan = migrate::plan(&paths, &packs, accounts.as_deref(), &inst)?;
    let size = |p: &std::path::Path| -> u64 {
        fn walk(p: &std::path::Path) -> u64 {
            std::fs::read_dir(p)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| {
                            let t = e.file_type().ok();
                            if t.is_some_and(|t| t.is_dir()) {
                                walk(&e.path())
                            } else {
                                e.metadata().map(|m| m.len()).unwrap_or(0)
                            }
                        })
                        .sum()
                })
                .unwrap_or(0)
        }
        walk(p)
    };
    let report = serde_json::json!({
        "library": plan.library,
        "manifests": plan.manifests.iter().map(|m| serde_json::json!({
            "id": m.id, "minecraft": m.minecraft, "loader": m.loader, "memory": m.memory,
            "jvm_args": m.jvm_args, "managed_files": m.managed_files.len(),
        })).collect::<Vec<_>>(),
        "accounts": plan.accounts,
        "moves": plan.moves.iter().map(|m| serde_json::json!({
            "from": m.from, "to": m.to, "exists": m.from.exists(), "dest_exists": m.to.exists(), "mb": size(&m.from) / 1_048_576,
        })).collect::<Vec<_>>(),
        "warnings": plan.warnings,
    });
    std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap()).map_err(|e| e.to_string())?;
    Ok(())
}

async fn cmd_install(paths: &Paths, src: &str, id: &str) -> Result<(), String> {
    if !schema::valid_id(id) {
        return Err(format!("«{id}» no vale como id: minúsculas, números y guiones"));
    }
    let t0 = Instant::now();
    let store = dino_config::Store::new(paths.clone());
    let cfg = store.config().map_err(|e| e.to_string())?;
    let dl = Downloader::new(cfg.download_concurrency as usize)
        .with_cache(paths.shared_files())
        .with_rate_limit(cfg.download_limit);
    let pr = Printer { last: Mutex::new(Default::default()) };
    let report = |l: &str, p: Option<Progress>| pr.report(l, p);

    let mrpack = if src.starts_with("http://") || src.starts_with("https://") {
        let dest = paths.pack_file(id);
        dl.fetch_all(
            vec![dino_download::Item { url: src.into(), dest: dest.clone(), sha1: None, size: None, shared: false }],
            &|p| report("Pack", Some(p)),
        )
        .await
        .map_err(|e| e.to_string())?;
        dest
    } else {
        // El `.mrpack` se guarda en `packs/`: sin él no se puede verificar ni reparar luego.
        let dest = paths.pack_file(id);
        if std::fs::canonicalize(src).ok() != std::fs::canonicalize(&dest).ok() {
            if let Some(d) = dest.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            std::fs::copy(src, &dest).map_err(|e| e.to_string())?;
        }
        dest
    };

    let index = mrpack::read_index(&mrpack).map_err(|e| e.to_string())?;
    let mc = index.minecraft().map_err(|e| e.to_string())?.to_string();
    let loader = index.loader().map_err(|e| e.to_string())?;
    tracing::info!(pack = %index.name, version = %index.version_id, %mc, ?loader, "instalando");

    // Lo que el jugador eligió (memoria, args, Java) sobrevive a reinstalar.
    let previo: Option<InstanceManifest> = schema::load(&paths.instance_manifest(id)).map_err(|e| e.to_string())?;
    let mut manifest = InstanceManifest {
        schema: schema::INSTANCE_SCHEMA,
        id: id.into(),
        minecraft: mc.clone(),
        loader: loader.clone(),
        state: InstallState::Incomplete,
        memory: previo.as_ref().map(|m| m.memory).unwrap_or_default(),
        jvm_args: previo.as_ref().map(|m| m.jvm_args.clone()).unwrap_or_default(),
        java: previo.as_ref().map(|m| m.java.clone()).unwrap_or(JavaChoice::Auto),
        managed_files: previo.as_ref().map(|m| m.managed_files.clone()).unwrap_or_default(),
    };
    // Se marca incompleta antes de empezar: un corte a mitad deja constancia.
    schema::save(&paths.instance_manifest(id), &manifest, schema::INSTANCE_SCHEMA).map_err(|e| e.to_string())?;

    let t_mc = Instant::now();
    let mc_fut = install::install(&dl, paths, &mc, &loader, &report);
    let game_dir = paths.game_dir(id);
    let previous = manifest.managed_files.clone();
    let pack_fut = mrpack::install(&dl, &mrpack, &game_dir, &previous, &report);
    let (mc_res, pack_res) = tokio::join!(mc_fut, pack_fut);
    let (inst, st) = mc_res.map_err(|e| e.to_string())?;
    let packed = pack_res.map_err(|e| e.to_string())?;
    let pack_stats = packed.stats;
    for r in &packed.removed {
        tracing::info!(file = %r, "quitado: la versión nueva del pack ya no lo trae");
    }

    manifest.managed_files = packed.managed;
    manifest.state = InstallState::Ready;
    schema::save(&paths.instance_manifest(id), &manifest, schema::INSTANCE_SCHEMA).map_err(|e| e.to_string())?;
    store
        .update_library(|lib| {
            use dino_config::LibraryExt;
            lib.upsert(LibraryEntry {
                id: id.into(),
                name: index.name.clone(),
                source: PackSource::File,
                pack_version: index.version_id.clone(),
                server: None,
                last_played: None,
            });
            if lib.selected.is_none() {
                lib.select(id);
            }
        })
        .map_err(|e| e.to_string())?;

    tracing::info!(secs = t0.elapsed().as_secs_f64(), "instalación terminada");
    println!(
        "{}",
        serde_json::json!({
            "ok": true,
            "total_s": t0.elapsed().as_secs_f64(),
            "minecraft_and_pack_s": t_mc.elapsed().as_secs_f64(),
            "libraries": {"files": st.libraries.files, "skipped": st.libraries.skipped, "mb": mb(st.libraries.downloaded_bytes)},
            "assets": {"files": st.assets.files, "skipped": st.assets.skipped, "mb": mb(st.assets.downloaded_bytes)},
            "mods": {"files": pack_stats.files, "skipped": pack_stats.skipped, "mb": mb(pack_stats.downloaded_bytes)},
            "processors_run": st.processors_run,
            "removed": packed.removed,
            "version_id": inst.version.id,
        })
    );
    Ok(())
}

async fn cmd_launch(paths: &Paths, args: &[String]) -> Result<(), String> {
    let id = &args[0];
    let mut who: Option<String> = None;
    let mut opts = dino_session::LaunchOptions::default();
    let mut wait = false;
    let mut msa = false;
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--msa" => msa = true,
            "--server" => opts.server = it.next().cloned(),
            "--mem" => opts.max_mem_mb = it.next().and_then(|m| m.parse().ok()),
            "--wait" => wait = true,
            o if !o.starts_with("--") && who.is_none() => who = Some(o.to_string()),
            o => return Err(format!("opción desconocida: {o}")),
        }
    }
    let accs = dino_auth::Accounts::new(paths.clone());
    let e = engine();
    // La cuenta: una guardada (uuid o nombre); si no hay ninguna con ese nombre y no es
    // Microsoft, se crea offline. Sin nombre, la activa.
    if let Some(w) = &who {
        let l = accs.list(&e.accounts()).map_err(|x| x.to_string())?;
        let found = l.iter().find(|v| {
            (v.uuid == *w || v.name == *w) && (v.kind == dino_core::schema::AccountKind::MicrosoftBridge) == msa
        });
        opts.account = match found {
            Some(v) => Some(v.uuid.clone()),
            None if msa => {
                return Err(format!("no hay ninguna cuenta Microsoft «{w}»: prueba `dino-cli account sync`"))
            }
            None => Some(accs.add_offline(w).map_err(|x| x.to_string())?.uuid),
        };
    }
    let t0 = Instant::now();
    let session = dino_session::launch(paths, &accs, &e.accounts(), id, &opts)
        .map_err(|x| format!("{:?}: {}", x.code, x.detail))?;
    println!(
        "{}",
        serde_json::json!({"launched": true, "pid": session.record.pid, "spawn_ms": t0.elapsed().as_millis()})
    );
    if wait {
        let out = dino_session::wait(paths, session, &|st| {
            println!("{}", serde_json::json!({"state": st, "ms": t0.elapsed().as_millis()}));
        });
        println!(
            "{}",
            serde_json::json!({"exit_code": out.exit_code, "reached_window": out.reached_window, "played_s": out.played.as_secs()})
        );
    }
    Ok(())
}

fn cmd_status(paths: &Paths) -> Result<(), String> {
    match dino_session::running(paths) {
        Some(r) => println!(
            "{}",
            serde_json::json!({"playing": true, "instance": r.instance, "pid": r.pid, "started": r.started})
        ),
        None => println!("{}", serde_json::json!({"playing": false})),
    }
    Ok(())
}

fn cmd_stop(paths: &Paths) -> Result<(), String> {
    let t0 = Instant::now();
    let r = dino_session::stop(paths, Duration::from_secs(30));
    println!("{}", serde_json::json!({"result": format!("{r:?}"), "ms": t0.elapsed().as_millis()}));
    Ok(())
}
