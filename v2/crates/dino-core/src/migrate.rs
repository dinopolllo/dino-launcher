//! Migración de Dino 1.x a 2.0 sin perder nada.
//!
//! 1.x y 2.0 comparten carpeta de datos (`%APPDATA%\gg.dinolab.launcher`). La migración:
//! 1. lee `packs.json`, el `accounts.json` del motor y, por instancia, `mmc-pack.json`,
//!    `instance.cfg` y `dino-archivos.json`;
//! 2. arma un [`Plan`] puro (probado sin tocar disco);
//! 3. [`apply`] lo ejecuta **moviendo** (mismo disco: renombrado instantáneo, 0 bytes
//!    copiados) y anota cada movimiento en `migration-v1.json`;
//! 4. [`rollback`] deshace los movimientos con ese diario, para volver a 1.x.
//!
//! Nunca borra nada de 1.x: lo que no se mueve se queda donde estaba.

use crate::schema::{
    self, Account, AccountKind, AccountsFile, InstallState, InstanceManifest, JavaChoice, Library, LibraryEntry,
    LoaderSpec, Memory, PackSource, Paths,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Lo que se lee de una instancia del motor.
#[derive(Debug, Clone, Default)]
pub struct V1Instance {
    pub mmc_pack: Option<String>,
    pub instance_cfg: Option<String>,
    pub dino_archivos: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub library: Library,
    pub manifests: Vec<InstanceManifest>,
    pub accounts: AccountsFile,
    pub moves: Vec<Move>,
    /// Lo que no se pudo migrar y por qué. La interfaz lo enseña; los datos se quedan en 1.x.
    pub warnings: Vec<String>,
}

#[derive(Deserialize)]
struct PacksV1 {
    #[serde(default)]
    selected: Option<String>,
    #[serde(default)]
    packs: Vec<PackV1>,
}

#[derive(Deserialize)]
struct PackV1 {
    id: String,
    #[serde(default)]
    code: Option<String>,
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    max_mem: Option<u32>,
}

#[derive(Deserialize)]
struct MmcPack {
    components: Vec<MmcComponent>,
}

#[derive(Deserialize)]
struct MmcComponent {
    uid: String,
    #[serde(default)]
    version: Option<String>,
}

fn ini(cfg: &str) -> BTreeMap<String, String> {
    cfg.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn loader_of(mmc: &MmcPack) -> Result<(String, LoaderSpec), String> {
    let get = |uid: &str| mmc.components.iter().find(|c| c.uid == uid).and_then(|c| c.version.clone());
    let mc = get("net.minecraft").ok_or("no dice su versión de Minecraft")?;
    let loader = if let Some(v) = get("net.neoforged") {
        LoaderSpec::NeoForge { version: v }
    } else if let Some(v) = get("net.fabricmc.fabric-loader") {
        LoaderSpec::Fabric { version: v }
    } else if get("net.minecraftforge").is_some() {
        return Err("usa Forge, que 2.0 no soporta".into());
    } else if get("org.quiltmc.quilt-loader").is_some() {
        return Err("usa Quilt, que 2.0 no soporta".into());
    } else {
        LoaderSpec::Vanilla
    };
    Ok((mc, loader))
}

/// Arma el plan. `packs_json`: el `packs.json` de 1.x. `engine_accounts`: el
/// `accounts.json` del motor. `instances`: lo leído de cada instancia, por id.
pub fn plan(
    paths: &Paths,
    packs_json: &str,
    engine_accounts: Option<&str>,
    instances: &BTreeMap<String, V1Instance>,
) -> Result<Plan, String> {
    let v1: PacksV1 = serde_json::from_str(packs_json).map_err(|e| format!("packs.json dañado: {e}"))?;
    let engine = paths.root.join("engine");
    let mut lib = Library::default();
    let mut manifests = Vec::new();
    let mut moves = Vec::new();
    let mut warnings = Vec::new();

    for p in &v1.packs {
        if !schema::valid_id(&p.id) {
            warnings.push(format!("«{}»: id «{}» no es un nombre de carpeta seguro; se queda en 1.x", p.name, p.id));
            continue;
        }
        let inst = instances.get(&p.id).cloned().unwrap_or_default();
        let Some(mmc) = inst.mmc_pack.as_deref().and_then(|s| serde_json::from_str::<MmcPack>(s).ok()) else {
            warnings.push(format!("«{}»: sin mmc-pack.json legible; se queda en 1.x", p.name));
            continue;
        };
        let (minecraft, loader) = match loader_of(&mmc) {
            Ok(x) => x,
            Err(why) => {
                warnings.push(format!("«{}» {why}; se queda en 1.x", p.name));
                continue;
            }
        };
        let cfg = ini(inst.instance_cfg.as_deref().unwrap_or(""));
        let jvm_args = if cfg.get("OverrideJavaArgs").map(String::as_str) == Some("true") {
            cfg.get("JvmArgs").map(|a| a.split_whitespace().map(String::from).collect()).unwrap_or_default()
        } else {
            Vec::new()
        };
        let last_played = cfg.get("lastLaunchTime").and_then(|t| t.parse::<u64>().ok()).map(|ms| ms / 1000);
        let managed_files = inst
            .dino_archivos
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default();

        lib.instances.push(LibraryEntry {
            id: p.id.clone(),
            name: p.name.clone(),
            source: match p.code.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
                Some(c) => PackSource::Catalog { code: c.to_string() },
                None => PackSource::File,
            },
            pack_version: p.version.clone(),
            server: p.server.clone(),
            last_played,
        });
        manifests.push(InstanceManifest {
            schema: schema::INSTANCE_SCHEMA,
            id: p.id.clone(),
            minecraft,
            loader,
            // Los archivos del juego están; Java, librerías y versión son de 2.0 y se
            // comprueban (y bajan si faltan) al primer Jugar.
            state: InstallState::Incomplete,
            memory: p.max_mem.map(|mb| Memory::Fixed { max_mb: mb }).unwrap_or_default(),
            jvm_args,
            java: JavaChoice::Auto,
            managed_files,
        });
        moves.push(Move {
            from: engine.join("instances").join(&p.id).join("minecraft"),
            to: paths.game_dir(&p.id),
        });
    }
    lib.selected = v1.selected.filter(|s| lib.instances.iter().any(|i| &i.id == s));

    // Lo descargado por el motor es contenido por hash/coordenada: se reaprovecha.
    moves.push(Move { from: engine.join("assets"), to: paths.root.join("cache").join("assets") });
    moves.push(Move { from: engine.join("libraries"), to: paths.root.join("cache").join("libraries") });

    let mut accounts = AccountsFile::default();
    if let Some(s) = engine_accounts {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
            for a in v["accounts"].as_array().into_iter().flatten() {
                let kind = match a["type"].as_str() {
                    Some("Offline") => AccountKind::Offline,
                    Some("MSA") => AccountKind::MicrosoftBridge,
                    _ => continue,
                };
                let (Some(name), Some(uuid)) = (a["profile"]["name"].as_str(), a["profile"]["id"].as_str()) else {
                    continue;
                };
                // Mismo nombre en offline y en Microsoft son cuentas distintas: el uuid manda.
                if accounts.accounts.iter().any(|x| x.uuid == uuid && x.kind == kind) {
                    continue;
                }
                if a["active"].as_bool() == Some(true) {
                    accounts.active = Some(uuid.to_string());
                }
                accounts.accounts.push(Account { kind, name: name.to_string(), uuid: uuid.to_string() });
            }
        }
    }

    Ok(Plan { library: lib, manifests, accounts, moves, warnings })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Journal {
    pub done: Vec<Move>,
}

pub fn journal_path(paths: &Paths) -> PathBuf {
    paths.root.join("migration-v1.json")
}

/// Ejecuta el plan. Idempotente: lo ya movido no se vuelve a mover, y un destino que ya
/// existe no se pisa (se avisa).
pub fn apply(paths: &Paths, plan: &Plan) -> Result<Vec<String>, String> {
    let jp = journal_path(paths);
    let mut journal: Journal = schema::load(&jp).map_err(|e| e.to_string())?.unwrap_or_default();
    let mut notes = Vec::new();
    for m in &plan.moves {
        if journal.done.contains(m) || !m.from.exists() {
            continue;
        }
        if m.to.exists() {
            notes.push(format!("{} ya existe; no lo piso", m.to.display()));
            continue;
        }
        if let Some(p) = m.to.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&m.from, &m.to).map_err(|e| format!("no pude mover {}: {e}", m.from.display()))?;
        journal.done.push(m.clone());
        // Se anota en cada paso: si se corta la luz a mitad, el diario sigue siendo cierto.
        schema::write_atomic(&jp, &serde_json::to_vec_pretty(&journal).unwrap()).map_err(|e| e.to_string())?;
    }
    for m in &plan.manifests {
        schema::save(&paths.instance_manifest(&m.id), m, schema::INSTANCE_SCHEMA).map_err(|e| e.to_string())?;
    }
    schema::save(&paths.library(), &plan.library, schema::LIBRARY_SCHEMA).map_err(|e| e.to_string())?;
    schema::save(&paths.accounts(), &plan.accounts, schema::ACCOUNTS_SCHEMA).map_err(|e| e.to_string())?;
    Ok(notes)
}

/// Devuelve a su sitio lo que movió [`apply`], en orden inverso.
pub fn rollback(paths: &Paths) -> Result<usize, String> {
    let jp = journal_path(paths);
    let mut journal: Journal = schema::load(&jp).map_err(|e| e.to_string())?.unwrap_or_default();
    let mut n = 0;
    while let Some(m) = journal.done.pop() {
        if m.to.exists() && !m.from.exists() {
            std::fs::rename(&m.to, &m.from).map_err(|e| format!("no pude devolver {}: {e}", m.to.display()))?;
            n += 1;
        }
        schema::write_atomic(&jp, &serde_json::to_vec_pretty(&journal).unwrap()).map_err(|e| e.to_string())?;
    }
    Ok(n)
}

/// Lee de disco lo que [`plan`] necesita.
pub fn read_v1(paths: &Paths) -> Option<(String, Option<String>, BTreeMap<String, V1Instance>)> {
    let packs = std::fs::read_to_string(paths.root.join("packs.json")).ok()?;
    let engine = paths.root.join("engine");
    let accounts = std::fs::read_to_string(engine.join("accounts.json")).ok();
    let mut inst = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(engine.join("instances")) {
        for e in rd.flatten() {
            let d = e.path();
            let r = |f: &str| std::fs::read_to_string(d.join(f)).ok();
            inst.insert(
                e.file_name().to_string_lossy().into_owned(),
                V1Instance { mmc_pack: r("mmc-pack.json"), instance_cfg: r("instance.cfg"), dino_archivos: r("dino-archivos.json") },
            );
        }
    }
    Some((packs, accounts, inst))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Formatos reales de 1.x (recortados).
    const PACKS: &str = r#"{"formatVersion":1,"selected":"voidsafio","packs":[
        {"id":"voidsafio","code":"VOIDSAFIO","name":"VoidSafio","version":"3.15.0","server":"voidsafio.minecra.fr","mrpack":"C:\\x\\voidsafio.mrpack","max_mem":null},
        {"id":"fabulously-optimized","code":null,"name":"Fabulously Optimized","version":"1.0.0","server":"mc.universocraft.com","mrpack":"C:\\x\\fo.mrpack","max_mem":10240},
        {"id":"viejo-forge","code":null,"name":"Pack Forge","version":"1","mrpack":"x"}]}"#;
    const MMC_NEO: &str = r#"{"components":[{"uid":"org.lwjgl3","version":"3.3.3"},{"uid":"net.minecraft","version":"1.21.1"},{"uid":"net.neoforged","version":"21.1.251"}],"formatVersion":1}"#;
    const MMC_FABRIC: &str = r#"{"components":[{"uid":"net.minecraft","version":"26.2"},{"uid":"net.fabricmc.intermediary","version":"26.2"},{"uid":"net.fabricmc.fabric-loader","version":"0.19.3"}],"formatVersion":1}"#;
    const MMC_FORGE: &str = r#"{"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"net.minecraftforge","version":"47.2.0"}],"formatVersion":1}"#;
    const CFG: &str = "[General]\nname=VoidSafio\nOverrideJavaArgs=false\nJvmArgs=-XX:+UseG1GC\nlastLaunchTime=1795434034343\n";
    const ACCOUNTS: &str = r#"{"formatVersion":3,"accounts":[
        {"type":"Offline","profile":{"id":"aaa","name":"JugadorOffline"}},
        {"type":"MSA","active":true,"ygg":{"token":"SECRETO","exp":1},"profile":{"id":"bbb","name":"JugadorMicrosoft"}}]}"#;

    fn fixture() -> (Paths, BTreeMap<String, V1Instance>) {
        let mut m = BTreeMap::new();
        m.insert("voidsafio".into(), V1Instance { mmc_pack: Some(MMC_NEO.into()), instance_cfg: Some(CFG.into()), dino_archivos: Some(r#"["mods/a.jar","config/b.toml"]"#.into()) });
        m.insert("fabulously-optimized".into(), V1Instance { mmc_pack: Some(MMC_FABRIC.into()), instance_cfg: Some("OverrideJavaArgs=true\nJvmArgs=-XX:+UseZGC -Dfoo=1\n".into()), dino_archivos: None });
        m.insert("viejo-forge".into(), V1Instance { mmc_pack: Some(MMC_FORGE.into()), ..Default::default() });
        (Paths::new("R"), m)
    }

    #[test]
    fn la_biblioteca_conserva_todo_lo_que_tenia() {
        let (p, inst) = fixture();
        let plan = plan(&p, PACKS, Some(ACCOUNTS), &inst).unwrap();
        assert_eq!(plan.library.selected.as_deref(), Some("voidsafio"));
        let v = &plan.library.instances[0];
        assert_eq!(v.source, PackSource::Catalog { code: "VOIDSAFIO".into() });
        assert_eq!((v.pack_version.as_str(), v.server.as_deref()), ("3.15.0", Some("voidsafio.minecra.fr")));
        assert_eq!(v.last_played, Some(1795434034));
        assert_eq!(plan.library.instances[1].source, PackSource::File);
    }

    #[test]
    fn version_loader_memoria_y_args_salen_de_la_instancia() {
        let (p, inst) = fixture();
        let plan = plan(&p, PACKS, None, &inst).unwrap();
        let void = &plan.manifests[0];
        assert_eq!(void.minecraft, "1.21.1");
        assert_eq!(void.loader, LoaderSpec::NeoForge { version: "21.1.251".into() });
        assert_eq!(void.memory, Memory::Auto);
        assert!(void.jvm_args.is_empty(), "OverrideJavaArgs=false: los args no estaban en uso");
        assert_eq!(void.managed_files, vec!["mods/a.jar", "config/b.toml"]);
        let fo = &plan.manifests[1];
        assert_eq!(fo.loader, LoaderSpec::Fabric { version: "0.19.3".into() });
        assert_eq!(fo.memory, Memory::Fixed { max_mb: 10240 });
        assert_eq!(fo.jvm_args, vec!["-XX:+UseZGC", "-Dfoo=1"]);
        assert!(plan.manifests.iter().all(|m| m.state == InstallState::Incomplete));
    }

    #[test]
    fn lo_que_no_se_puede_migrar_se_queda_en_1x_con_aviso() {
        let (p, inst) = fixture();
        let plan = plan(&p, PACKS, None, &inst).unwrap();
        assert_eq!(plan.library.instances.len(), 2);
        assert!(plan.warnings.iter().any(|w| w.contains("Pack Forge") && w.contains("Forge")));
        assert!(!plan.moves.iter().any(|m| m.from.to_string_lossy().contains("viejo-forge")));
    }

    #[test]
    fn las_cuentas_pasan_sin_secretos() {
        let (p, inst) = fixture();
        let plan = plan(&p, PACKS, Some(ACCOUNTS), &inst).unwrap();
        assert_eq!(plan.accounts.accounts.len(), 2);
        assert_eq!(plan.accounts.active.as_deref(), Some("bbb"));
        assert_eq!(plan.accounts.accounts[1].kind, AccountKind::MicrosoftBridge);
        assert!(!serde_json::to_string(&plan.accounts).unwrap().contains("SECRETO"));
    }

    #[test]
    fn mover_y_deshacer() {
        let root = std::env::temp_dir().join(format!("dino-migra-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::new(&root);
        let (_, inst) = fixture();
        let game = root.join("engine/instances/voidsafio/minecraft/saves/mundo");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("level.dat"), b"mi mundo").unwrap();
        std::fs::create_dir_all(root.join("engine/assets/objects")).unwrap();

        let plan = plan(&paths, PACKS, Some(ACCOUNTS), &inst).unwrap();
        apply(&paths, &plan).unwrap();
        assert_eq!(std::fs::read(paths.game_dir("voidsafio").join("saves/mundo/level.dat")).unwrap(), b"mi mundo");
        assert!(root.join("cache/assets/objects").exists());
        assert!(paths.library().exists() && paths.instance_manifest("voidsafio").exists());
        // aplicar dos veces no rompe nada
        apply(&paths, &plan).unwrap();

        assert_eq!(rollback(&paths).unwrap(), 2);
        assert_eq!(std::fs::read(game.join("level.dat")).unwrap(), b"mi mundo");
        assert!(root.join("engine/assets/objects").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
