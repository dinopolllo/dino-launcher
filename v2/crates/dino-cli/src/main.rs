//! `dino-cli`: el núcleo de Dino 2.0 sin interfaz.
//!
//!     dino-cli install <ruta-o-url.mrpack> <instancia>
//!     dino-cli launch  <instancia> <jugador> [--server host] [--mem MB] [--wait]
//!
//! Los datos van a `DINO_HOME` o, si no está, a `%USERPROFILE%\dino-v2-data`: a propósito
//! fuera de `AppData`, para no mezclarse con el 1.x instalado.

use dino_download::{Downloader, Progress};
use dino_minecraft::install::Installed;
use dino_minecraft::launch::{Account, LaunchSpec};
use dino_minecraft::model::VersionJson;
use dino_minecraft::{install, launch, mrpack, Dirs};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize)]
struct InstanceMeta {
    pack: String,
    pack_version: String,
    minecraft: String,
    version_id: String,
    java: PathBuf,
    client_jar: PathBuf,
}

fn home() -> PathBuf {
    std::env::var_os("DINO_HOME").map(PathBuf::from).unwrap_or_else(|| {
        let user = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).expect("sin carpeta de usuario");
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
        let done = p.map_or(true, |p| p.files_done == p.files_total);
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
                p.files_done, p.files_total, mb(p.bytes_done), mb(p.bytes_total), mb(p.bytes_downloaded)
            ),
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("install") if args.len() >= 3 => cmd_install(&args[1], &args[2]).await,
        Some("launch") if args.len() >= 3 => cmd_launch(&args[1..]).await,
        _ => {
            eprintln!("uso:\n  dino-cli install <ruta-o-url.mrpack> <instancia>\n  dino-cli launch <instancia> <jugador> [--server host] [--mem MB] [--wait]");
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

async fn cmd_install(src: &str, id: &str) -> Result<(), String> {
    let t0 = Instant::now();
    let dirs = Dirs::new(home());
    let dl = Downloader::new(32);
    let pr = Printer { last: Mutex::new(Default::default()) };
    let report = |l: &str, p: Option<Progress>| pr.report(l, p);

    let mrpack = if src.starts_with("http://") || src.starts_with("https://") {
        let dest = dirs.root.join("packs").join(format!("{id}.mrpack"));
        dl.fetch_all(vec![dino_download::Item { url: src.into(), dest: dest.clone(), sha1: None, size: None }], &|p| report("Pack", Some(p)))
            .await
            .map_err(|e| e.to_string())?;
        dest
    } else {
        PathBuf::from(src)
    };

    let index = mrpack::read_index(&mrpack).map_err(|e| e.to_string())?;
    let mc = index.minecraft().map_err(|e| e.to_string())?.to_string();
    let loader = index.loader().map_err(|e| e.to_string())?;
    eprintln!("Pack «{}» {} → Minecraft {mc}, {loader:?}", index.name, index.version_id);

    let game_dir = dirs.instance(id).join("minecraft");
    let t_mc = Instant::now();
    let mc_fut = install::install(&dl, &dirs, &mc, &loader, &report);
    let pack_fut = mrpack::install(&dl, &mrpack, &game_dir, &report);
    let (mc_res, pack_res) = tokio::join!(mc_fut, pack_fut);
    let (inst, st) = mc_res.map_err(|e| e.to_string())?;
    let (_, pack_stats) = pack_res.map_err(|e| e.to_string())?;

    let meta = InstanceMeta {
        pack: index.name.clone(),
        pack_version: index.version_id.clone(),
        minecraft: mc.clone(),
        version_id: inst.version.id.clone(),
        java: inst.java.clone(),
        client_jar: inst.client_jar.clone(),
    };
    let p = dirs.instance(id).join("instance.json");
    std::fs::write(&p, serde_json::to_vec_pretty(&meta).unwrap()).map_err(|e| e.to_string())?;

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
            "version_id": inst.version.id,
        })
    );
    Ok(())
}

async fn cmd_launch(args: &[String]) -> Result<(), String> {
    let id = &args[0];
    let player = &args[1];
    let mut server = None;
    let mut mem = 4096u32;
    let mut wait = false;
    let mut it = args[2..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--server" => server = it.next().cloned(),
            "--mem" => mem = it.next().and_then(|m| m.parse().ok()).unwrap_or(mem),
            "--wait" => wait = true,
            o => return Err(format!("opción desconocida: {o}")),
        }
    }
    let dirs = Dirs::new(home());
    let meta: InstanceMeta = serde_json::from_slice(
        &std::fs::read(dirs.instance(id).join("instance.json")).map_err(|e| format!("instancia sin instalar: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let vjson = dirs.version_dir(&meta.version_id).join(format!("{}.json", meta.version_id));
    let version: VersionJson =
        serde_json::from_slice(&std::fs::read(&vjson).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let inst = Installed { version, java: meta.java, client_jar: meta.client_jar };

    let account = Account::offline(player);
    let game_dir = dirs.instance(id).join("minecraft");
    let jvm = vec!["-XX:+UseG1GC".to_string(), "-XX:MaxGCPauseMillis=50".to_string()];
    let spec = LaunchSpec {
        account: &account,
        game_dir: &game_dir,
        max_mem_mb: mem,
        min_mem_mb: 2048.min(mem),
        extra_jvm_args: &jvm,
        server: server.as_deref(),
    };
    let mut cmd = launch::command(&dirs, &inst, &spec).map_err(|e| e.to_string())?;
    let logdir = dirs.instance(id).join("dino-logs");
    std::fs::create_dir_all(&logdir).map_err(|e| e.to_string())?;
    let out = std::fs::File::create(logdir.join("latest.log")).map_err(|e| e.to_string())?;
    cmd.stdout(out.try_clone().map_err(|e| e.to_string())?).stderr(out);
    let t0 = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("no pude lanzar Java: {e}"))?;
    println!("{}", serde_json::json!({"launched": true, "pid": child.id(), "spawn_ms": t0.elapsed().as_millis()}));
    if wait {
        let st = child.wait().map_err(|e| e.to_string())?;
        println!("{}", serde_json::json!({"exited": st.code(), "played_s": t0.elapsed().as_secs_f64()}));
    }
    Ok(())
}
