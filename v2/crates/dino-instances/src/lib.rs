//! Lo que la app hace con los packs: la capa que traduce cada comando del contrato
//! (`v2/docs/contratos.md`) a llamadas del núcleo, y que cuenta lo que pasa con eventos.
//!
//! `dino-app` (Tauri) solo envuelve [`Core`]: así todo esto se prueba sin interfaz.

pub mod admin;
pub mod catalog;
pub mod status;

use dino_auth::{Accounts, Engine};
use dino_config::{LibraryExt, Store};
use dino_core::api::{
    icon_url, AccountView, InstanceDetails, InstanceSettings, InstanceView, JavaOption, JavaOrigin, PackPreview,
    PlayOutcome, PlayRequest, PrimaryAction, ServerStatus,
};
use dino_core::events::{Event, Phase, Progress, Session, SessionState};
use dino_core::schema::{
    self, AccountKind, InstallState, InstanceManifest, JavaChoice, LibraryEntry, LoaderSpec, Memory, PackSource, Paths,
};
use dino_core::{os, DinoError, ErrorCode};
use dino_download::Downloader;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Emit = Arc<dyn Fn(Event) + Send + Sync>;
type R<T> = Result<T, DinoError>;

fn e(code: ErrorCode, detail: impl std::fmt::Display) -> DinoError {
    DinoError::new(code, detail.to_string())
}

fn internal(detail: impl std::fmt::Display) -> DinoError {
    e(ErrorCode::Internal, detail)
}

pub struct Core {
    pub paths: Paths,
    pub store: Store,
    pub accounts: Accounts,
    pub engine: Engine,
    emit: Emit,
    /// Una sola tarea larga a la vez (instalar o jugar): el segundo clic recibe `busy`.
    busy: Mutex<Option<String>>,
    cancel: AtomicBool,
}

/// Suelta el cerrojo de tarea al salir, pase lo que pase.
struct BusyGuard<'a>(&'a Mutex<Option<String>>);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

impl Core {
    pub fn new(paths: Paths, emit: Emit) -> Arc<Self> {
        Arc::new(Self {
            store: Store::new(paths.clone()),
            accounts: Accounts::new(paths.clone()),
            engine: Engine::new(paths.root.clone()),
            paths,
            emit,
            busy: Mutex::new(None),
            cancel: AtomicBool::new(false),
        })
    }

    fn downloader(&self) -> Downloader {
        let cfg = self.store.config().unwrap_or_default();
        Downloader::new(cfg.download_concurrency.max(1) as usize)
            .with_cache(self.paths.shared_files())
            .with_rate_limit(cfg.download_limit)
    }

    fn begin(&self, what: &str) -> R<BusyGuard<'_>> {
        let mut b = self.busy.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = b.as_ref() {
            return Err(e(ErrorCode::Busy, format!("ya hay una tarea en marcha: {t}")));
        }
        *b = Some(what.to_string());
        self.cancel.store(false, Ordering::SeqCst);
        Ok(BusyGuard(&self.busy))
    }

    fn check_cancel(&self) -> R<()> {
        if self.cancel.swap(false, Ordering::SeqCst) {
            return Err(e(ErrorCode::Busy, "cancelado"));
        }
        Ok(())
    }

    // ------------------------------------------------------------ biblioteca

    pub async fn library_list(&self) -> R<Vec<InstanceView>> {
        let lib = self.store.library().map_err(internal)?;
        let catalog = if lib.instances.iter().any(|x| matches!(x.source, PackSource::Catalog { .. })) {
            catalog::fetch_catalog().await.ok()
        } else {
            None
        };
        let playing = dino_session::running(&self.paths).map(|r| r.instance);
        let selected = lib.selected_entry().map(|x| x.id.clone());
        Ok(lib
            .instances
            .iter()
            .map(|entry| {
                let remote = match (&entry.source, &catalog) {
                    (PackSource::Catalog { code }, Some(cat)) => catalog::buscar(cat, code).cloned(),
                    _ => None,
                };
                self.view(entry, remote.as_ref(), playing.as_deref(), selected.as_deref())
            })
            .collect())
    }

    fn view(
        &self,
        entry: &LibraryEntry,
        remote: Option<&catalog::PackRemoto>,
        playing: Option<&str>,
        selected: Option<&str>,
    ) -> InstanceView {
        let manifest: Option<InstanceManifest> = schema::load(&self.paths.instance_manifest(&entry.id)).ok().flatten();
        let installed = manifest.as_ref().is_some_and(|m| m.state == InstallState::Ready);
        let mods = match (&manifest, remote) {
            (Some(m), _) if installed && !m.managed_files.is_empty() => {
                m.managed_files.iter().filter(|f| f.starts_with("mods/")).count() as u32
            }
            // Instalada antes de anotar sus archivos: lo que haya en `mods/`.
            (Some(_), _) if installed => count_jars(&self.paths.game_dir(&entry.id).join("mods")),
            (_, Some(r)) => r.mods,
            _ => 0,
        };
        let action = if playing == Some(entry.id.as_str()) {
            PrimaryAction::Playing
        } else if let Some(r) = remote.filter(|r| r.version != entry.pack_version && installed) {
            PrimaryAction::Update { to_version: r.version.clone(), bytes: Some(r.bytes).filter(|b| *b > 0) }
        } else if !installed {
            PrimaryAction::Install { bytes: remote.map(|r| r.bytes).filter(|b| *b > 0) }
        } else {
            PrimaryAction::Play
        };
        InstanceView {
            id: entry.id.clone(),
            name: entry.name.clone(),
            source: entry.source.clone(),
            pack_version: entry.pack_version.clone(),
            minecraft: manifest
                .as_ref()
                .map(|m| m.minecraft.clone())
                .or_else(|| remote.and_then(|r| r.minecraft.clone())),
            loader: manifest
                .as_ref()
                .map(|m| m.loader.clone())
                .or_else(|| remote.and_then(|r| parse_loader(r.loader.as_deref()))),
            server: entry.server.clone(),
            icon: self.paths.pack_icon(&entry.id).exists().then(|| icon_url(&entry.id)),
            mods,
            disk_bytes: installed.then(|| dir_size(&self.paths.instance_dir(&entry.id))),
            selected: selected == Some(entry.id.as_str()),
            action,
        }
    }

    pub fn library_select(&self, id: &str) -> R<()> {
        let ok = self.store.update_library(|l| l.select(id)).map_err(internal)?;
        if !ok {
            return Err(e(ErrorCode::NotInstalled, format!("no hay ninguna instancia «{id}»")));
        }
        (self.emit)(Event::LibraryChanged);
        Ok(())
    }

    pub fn library_remove(&self, id: &str, delete_files: bool) -> R<()> {
        if delete_files && dino_session::running(&self.paths).is_some_and(|r| r.instance == id) {
            return Err(e(ErrorCode::Busy, "hay una partida abierta de esta instancia"));
        }
        let removed = self.store.update_library(|l| l.remove(id)).map_err(internal)?;
        if removed.is_none() {
            return Err(e(ErrorCode::NotInstalled, format!("no hay ninguna instancia «{id}»")));
        }
        if delete_files {
            if let Err(x) = std::fs::remove_dir_all(self.paths.instance_dir(id)) {
                if x.kind() != std::io::ErrorKind::NotFound {
                    return Err(e(ErrorCode::DiskPermission, x));
                }
            }
            let _ = std::fs::remove_file(self.paths.pack_file(id));
            let _ = std::fs::remove_file(self.paths.pack_icon(id));
        }
        (self.emit)(Event::LibraryChanged);
        Ok(())
    }

    pub fn instance_details(&self, id: &str) -> R<InstanceDetails> {
        let m = self.manifest_ready(id)?;
        let game = self.paths.game_dir(id);
        let worlds = std::fs::read_dir(game.join("saves"))
            .map(|rd| rd.flatten().filter(|x| x.path().join("level.dat").exists()).count() as u32)
            .unwrap_or(0);
        Ok(InstanceDetails {
            id: id.into(),
            path: game.to_string_lossy().into_owned(),
            minecraft: m.minecraft.clone(),
            loader: m.loader.clone(),
            mods: m.managed_files.iter().filter(|f| f.starts_with("mods/")).count() as u32,
            worlds,
            disk_bytes: dir_size(&self.paths.instance_dir(id)),
        })
    }

    fn manifest_ready(&self, id: &str) -> R<InstanceManifest> {
        let m: InstanceManifest = schema::load(&self.paths.instance_manifest(id))
            .map_err(internal)?
            .ok_or_else(|| e(ErrorCode::NotInstalled, format!("«{id}» no está instalada")))?;
        if m.state != InstallState::Ready {
            return Err(e(ErrorCode::NotInstalled, format!("«{id}» quedó a medias")));
        }
        Ok(m)
    }

    pub fn instance_settings_get(&self, id: &str) -> R<InstanceSettings> {
        let m = self.manifest_ready(id)?;
        let ram = os::total_memory_mb().unwrap_or(8192);
        Ok(InstanceSettings {
            memory: m.memory,
            jvm_args: m.jvm_args.clone(),
            java: m.java.clone(),
            available_java: self.java_options(),
            machine_ram_mb: ram,
            recommended_max_mb: os::recommended_memory(ram).1,
        })
    }

    fn java_options(&self) -> Vec<JavaOption> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.paths.java()) {
            for x in rd.flatten() {
                let bin = dino_minecraft::java::bin_for(&self.paths, &x.file_name().to_string_lossy());
                if bin.exists() {
                    out.push(JavaOption {
                        path: bin.to_string_lossy().into_owned(),
                        major: None,
                        origin: JavaOrigin::Mojang,
                    });
                }
            }
        }
        out
    }

    pub fn instance_settings_set(&self, id: &str, memory: Memory, jvm_args: Vec<String>, java: JavaChoice) -> R<()> {
        let mut m = self.manifest_ready(id)?;
        m.memory = memory;
        m.jvm_args = jvm_args;
        m.java = java;
        schema::save(&self.paths.instance_manifest(id), &m, schema::INSTANCE_SCHEMA).map_err(internal)
    }

    // ------------------------------------------------------------ catálogo e importar

    pub async fn catalog_lookup(&self, text: &str) -> R<PackPreview> {
        let r = catalog::resolver(text).await.map_err(map_catalog)?;
        Ok(preview_of_remote(&r))
    }

    /// Añade un pack del catálogo a la biblioteca: baja el `.mrpack` y el icono, y lo deja
    /// seleccionado listo para instalar con Jugar.
    pub async fn catalog_add(&self, text: &str) -> R<InstanceView> {
        let _g = self.begin("añadir un pack")?;
        let r = catalog::resolver(text).await.map_err(map_catalog)?;
        let code = catalog::normalizar_codigo(&r.code);
        let lib = self.store.library().map_err(internal)?;
        let id = lib.free_id(&slug(&r.id), Some(&code));
        self.download_pack(&r, &id).await?;
        let entry = LibraryEntry {
            id: id.clone(),
            name: r.name.clone(),
            source: PackSource::Catalog { code },
            pack_version: r.version.clone(),
            server: r.server.clone(),
            last_played: None,
        };
        self.store
            .update_library(|l| {
                l.upsert(entry.clone());
                l.select(&id);
            })
            .map_err(internal)?;
        (self.emit)(Event::LibraryChanged);
        Ok(self.view(&entry, Some(&r), None, Some(&id)))
    }

    async fn download_pack(&self, r: &catalog::PackRemoto, id: &str) -> R<()> {
        let task = format!("pack-{id}");
        let emit = self.emit.clone();
        let inst = id.to_string();
        let on = move |done: u64, total: u64| {
            emit(Event::Progress(Progress {
                task: task.clone(),
                instance: Some(inst.clone()),
                task_bytes_done: done,
                task_bytes_total: total,
                phase: Phase::DownloadingPack,
                files_done: 0,
                files_total: 1,
                bytes_done: done,
                bytes_total: total,
                finished: false,
            }))
        };
        catalog::descargar_pack(r, &self.paths.pack_file(id), &on).await.map_err(map_catalog)?;
        if let Err(x) = catalog::descargar_icono(r, &self.paths.pack_icon(id)).await {
            tracing::warn!(error = %x, "no pude bajar el icono del pack");
        }
        Ok(())
    }

    pub fn mrpack_inspect(&self, path: &Path) -> R<PackPreview> {
        let idx = dino_minecraft::mrpack::read_index(path).map_err(|x| e(ErrorCode::PackInvalid, x))?;
        let (loader, supported) = match idx.loader() {
            Ok(l) => (l, true),
            Err(_) => (LoaderSpec::Vanilla, false),
        };
        Ok(PackPreview {
            code: None,
            name: idx.name.clone(),
            version: idx.version_id.clone(),
            minecraft: idx.minecraft().map(String::from).unwrap_or_default(),
            loader,
            mods: idx.files.iter().filter(|f| f.path.starts_with("mods/")).count() as u32,
            icon: None,
            server: None,
            pack_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            download_bytes: Some(idx.files.iter().map(|f| f.file_size).sum()),
            supported,
        })
    }

    /// Importa un `.mrpack` local y lo instala.
    pub async fn mrpack_import(&self, path: &Path, server: Option<String>) -> R<InstanceView> {
        let pre = self.mrpack_inspect(path)?;
        if !pre.supported {
            return Err(e(ErrorCode::UnsupportedLoader, "Forge y Quilt están fuera del alcance de 2.0"));
        }
        let lib = self.store.library().map_err(internal)?;
        let id = lib.free_id(&slug(&pre.name), None);
        let dest = self.paths.pack_file(&id);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d).map_err(|x| e(ErrorCode::DiskPermission, x))?;
        }
        if !same_path(path, &dest) {
            std::fs::copy(path, &dest).map_err(|x| e(ErrorCode::DiskPermission, x))?;
        }
        extract_icon(&dest, &self.paths.pack_icon(&id));
        let entry = LibraryEntry {
            id: id.clone(),
            name: pre.name.clone(),
            source: PackSource::File,
            pack_version: pre.version.clone(),
            server,
            last_played: None,
        };
        self.store
            .update_library(|l| {
                l.upsert(entry.clone());
                l.select(&id);
            })
            .map_err(internal)?;
        (self.emit)(Event::LibraryChanged);
        let _g = self.begin("instalar")?;
        self.install(&id).await?;
        Ok(self.view(&entry, None, None, Some(&id)))
    }

    // ------------------------------------------------------------ instalar

    /// Instala (o repara, o verifica) la instancia desde su `.mrpack`. Un archivo que ya
    /// está con el hash bueno no se baja: verificar es lo mismo que instalar sin cambios.
    async fn install(&self, id: &str) -> R<()> {
        let mrpack = self.paths.pack_file(id);
        if !mrpack.exists() {
            return Err(e(ErrorCode::PackInvalid, format!("falta el .mrpack de «{id}»")));
        }
        let index = dino_minecraft::mrpack::read_index(&mrpack).map_err(|x| e(ErrorCode::PackInvalid, x))?;
        let mc = index.minecraft().map_err(|x| e(ErrorCode::PackInvalid, x))?.to_string();
        let loader = index.loader().map_err(|x| e(ErrorCode::UnsupportedLoader, x))?;
        let previo: Option<InstanceManifest> = schema::load(&self.paths.instance_manifest(id)).map_err(internal)?;
        let mut manifest = InstanceManifest {
            schema: schema::INSTANCE_SCHEMA,
            id: id.into(),
            minecraft: mc.clone(),
            loader: loader.clone(),
            state: InstallState::Incomplete,
            memory: previo.as_ref().map(|m| m.memory).unwrap_or_default(),
            jvm_args: previo.as_ref().map(|m| m.jvm_args.clone()).unwrap_or_default(),
            java: previo.as_ref().map(|m| m.java.clone()).unwrap_or_default(),
            managed_files: previo.as_ref().map(|m| m.managed_files.clone()).unwrap_or_default(),
        };
        schema::save(&self.paths.instance_manifest(id), &manifest, schema::INSTANCE_SCHEMA).map_err(internal)?;

        let tracker = Tracker::new(
            format!("install-{id}"),
            Some(id.to_string()),
            self.emit.clone(),
            &[Phase::DownloadingLibraries, Phase::DownloadingAssets, Phase::InstallingJava, Phase::DownloadingMods],
        );
        let report = |label: &str, p: Option<dino_download::Progress>| tracker.report(label, p);
        let dl = self.downloader();
        let game = self.paths.game_dir(id);
        let previous = manifest.managed_files.clone();
        let (mc_res, pack_res) = tokio::join!(
            dino_minecraft::install::install(&dl, &self.paths, &mc, &loader, &report),
            dino_minecraft::mrpack::install(&dl, &mrpack, &game, &previous, &report),
        );
        mc_res.map_err(map_mc)?;
        let packed = pack_res.map_err(map_mc)?;
        manifest.managed_files = packed.managed;
        manifest.state = InstallState::Ready;
        schema::save(&self.paths.instance_manifest(id), &manifest, schema::INSTANCE_SCHEMA).map_err(internal)?;
        self.store
            .update_library(|l| {
                if let Some(x) = l.instances.iter_mut().find(|x| x.id == id) {
                    x.pack_version = index.version_id.clone();
                }
            })
            .map_err(internal)?;
        tracker.finish();
        (self.emit)(Event::LibraryChanged);
        Ok(())
    }

    /// «Verificar archivos»: reinstala comprobando cada archivo por su hash.
    pub async fn instance_verify(&self, id: &str) -> R<()> {
        let _g = self.begin("verificar")?;
        self.install(id).await
    }

    // ------------------------------------------------------------ jugar

    /// Abrir → Jugar → Minecraft: busca versión nueva del pack, instala si hace falta,
    /// renueva la sesión de Microsoft si le queda poco y lanza. La partida la sigue un hilo
    /// que emite los eventos `session`.
    pub async fn play(self: &Arc<Self>, req: PlayRequest) -> R<PlayOutcome> {
        let guard = self.begin("jugar")?;
        let id = req.instance.clone();
        let lib = self.store.library().map_err(internal)?;
        let entry = lib.find(&id).cloned().ok_or_else(|| e(ErrorCode::NotInstalled, format!("no hay «{id}»")))?;
        (self.emit)(Event::Session(Session { state: SessionState::Preparing, instance: Some(id.clone()) }));
        let result = self.play_inner(&entry, req.launch).await;
        drop(guard);
        match result {
            Ok(Some(session)) => {
                let core = self.clone();
                std::thread::spawn(move || core.watch(session));
                Ok(PlayOutcome::Launched)
            }
            Ok(None) => {
                (self.emit)(Event::Session(Session { state: SessionState::Idle, instance: None }));
                Ok(if req.launch { PlayOutcome::NeedsAccount } else { PlayOutcome::Installed })
            }
            Err(x) => {
                (self.emit)(Event::Session(Session { state: SessionState::Idle, instance: None }));
                Err(x)
            }
        }
    }

    async fn play_inner(&self, entry: &LibraryEntry, launch: bool) -> R<Option<dino_session::Session>> {
        let id = &entry.id;
        let mut need_install = self.manifest_ready(id).is_err();
        let mut server = entry.server.clone();

        // ¿Versión nueva en el catálogo? Sin red se juega con lo instalado.
        if let PackSource::Catalog { code } = &entry.source {
            match catalog::resolver(code).await {
                Ok(r) => {
                    server = r.server.clone();
                    if r.version != entry.pack_version || !self.paths.pack_file(id).exists() {
                        match self.download_pack(&r, id).await {
                            Ok(()) => need_install = true,
                            Err(x) if !need_install => {
                                tracing::warn!(error = ?x, "no pude bajar la versión nueva; juego con la instalada")
                            }
                            Err(x) => return Err(x),
                        }
                    }
                    let name = r.name.clone();
                    let _ = self.store.update_library(|l| {
                        if let Some(x) = l.instances.iter_mut().find(|x| &x.id == id) {
                            x.server = r.server.clone();
                            x.name = name;
                        }
                    });
                }
                Err(x) => tracing::warn!(error = %x, "sin catálogo: juego con lo instalado"),
            }
        }
        self.check_cancel()?;
        if need_install || !launch {
            self.install(id).await?;
        }
        if !launch {
            return Ok(None);
        }
        self.check_cancel()?;

        let accounts = self.accounts.list(&self.engine.accounts()).map_err(internal)?;
        let Some(active) = accounts.iter().find(|a| a.active) else { return Ok(None) };
        if active.kind == AccountKind::MicrosoftBridge && self.accounts.needs_renewal(&self.engine.accounts()) {
            (self.emit)(Event::Progress(phase_only(id, Phase::RenewingSession)));
            let engine = Engine::new(self.paths.root.clone());
            let r = tokio::task::spawn_blocking(move || engine.renew(Duration::from_secs(30)))
                .await
                .map_err(internal)?
                .map_err(|x| e(ErrorCode::SessionExpired, x))?;
            tracing::info!(renewed = ?r.renewed, "sesión renovada antes de jugar");
        }
        self.check_cancel()?;
        (self.emit)(Event::Progress(phase_only(id, Phase::Launching)));
        let opts = dino_session::LaunchOptions { server, max_mem_mb: None, account: Some(active.uuid.clone()) };
        let s = dino_session::launch(&self.paths, &self.accounts, &self.engine.accounts(), id, &opts)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = self.store.update_library(|l| {
            if let Some(x) = l.instances.iter_mut().find(|x| &x.id == id) {
                x.last_played = Some(now);
            }
        });
        Ok(Some(s))
    }

    fn watch(&self, s: dino_session::Session) {
        let id = s.record.instance.clone();
        let emit = self.emit.clone();
        // `preparing` ya lo anunció `play` al empezar; aquí solo lo que viene después.
        let out = dino_session::wait(&self.paths, s, &|st| {
            if st != SessionState::Preparing {
                emit(Event::Session(Session { state: st, instance: Some(id.clone()) }))
            }
        });
        tracing::info!(?out, "partida terminada");
        (self.emit)(Event::LibraryChanged);
    }

    /// Al abrir la app: si una partida seguía abierta, se la vuelve a seguir.
    pub fn reattach(self: &Arc<Self>) -> Option<String> {
        let rec = dino_session::running(&self.paths)?;
        let id = rec.instance.clone();
        let core = self.clone();
        std::thread::spawn(move || core.watch(dino_session::attach(rec)));
        Some(id)
    }

    pub fn play_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub async fn game_stop(&self) -> R<()> {
        let paths = self.paths.clone();
        tokio::task::spawn_blocking(move || dino_session::stop(&paths, Duration::from_secs(30)))
            .await
            .map_err(internal)?;
        Ok(())
    }

    // ------------------------------------------------------------ cuentas y servidor

    pub fn accounts_list(&self) -> R<Vec<AccountView>> {
        let _ = self.accounts.sync_bridge(&self.engine.accounts());
        self.accounts.list(&self.engine.accounts()).map_err(internal)
    }

    pub fn account_offline_check(&self, name: &str) -> Option<String> {
        dino_core::accounts::validate_name(name).err().map(|p| p.to_string())
    }

    pub fn account_offline_create(&self, name: &str) -> R<AccountView> {
        let a = self.accounts.add_offline(name).map_err(|x| e(ErrorCode::AccountRequired, x))?;
        (self.emit)(Event::AccountsChanged);
        Ok(AccountView { kind: a.kind, name: a.name, uuid: a.uuid, active: true, session_minutes_left: None })
    }

    pub fn account_select(&self, uuid: &str) -> R<()> {
        self.accounts.select(uuid).map_err(|x| e(ErrorCode::AccountRequired, x))?;
        (self.emit)(Event::AccountsChanged);
        Ok(())
    }

    pub fn account_remove(&self, uuid: &str) -> R<()> {
        self.accounts.remove(uuid).map_err(|x| e(ErrorCode::AccountRequired, x))?;
        (self.emit)(Event::AccountsChanged);
        Ok(())
    }

    /// Abre el motor para iniciar sesión con Microsoft; al cerrarlo se sincronizan las cuentas.
    pub async fn account_microsoft_add(self: &Arc<Self>) -> R<()> {
        self.engine.ensure(&self.downloader()).await.map_err(|x| e(ErrorCode::DownloadFailed, x))?;
        let mut child = self.engine.open_for_login().map_err(internal)?;
        let core = self.clone();
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = core.accounts.sync_bridge(&core.engine.accounts());
            (core.emit)(Event::AccountsChanged);
        });
        Ok(())
    }

    pub async fn server_status(&self, addr: &str) -> ServerStatus {
        let s = status::ping(addr).await;
        ServerStatus {
            online: s.online,
            motd: s.motd,
            players: s.players,
            max_players: s.max_players,
            version: s.version,
            ping_ms: s.ping_ms,
        }
    }
}

/// ¿Hay una partida abierta? (Para que la app no se cierre con la interfaz cerrada.)
pub fn is_playing(paths: &Paths) -> bool {
    dino_session::running(paths).is_some()
}

// ------------------------------------------------------------ progreso de una tarea

/// Por fase: (bytes hechos, bytes totales); cuándo se emitió el último evento; de qué fase.
type TrackerState = (HashMap<Phase, (u64, u64)>, Instant, Option<Phase>);

/// Junta el avance de las fases que corren a la vez en una sola barra que no retrocede:
/// mientras alguna fase no ha dicho su total, el total de la tarea es 0 («no se sabe»).
struct Tracker {
    task: String,
    instance: Option<String>,
    emit: Emit,
    expected: Vec<Phase>,
    state: Mutex<TrackerState>,
}

impl Tracker {
    fn new(task: String, instance: Option<String>, emit: Emit, expected: &[Phase]) -> Self {
        Self {
            task,
            instance,
            emit,
            expected: expected.to_vec(),
            state: Mutex::new((HashMap::new(), Instant::now(), None)),
        }
    }

    fn report(&self, label: &str, p: Option<dino_download::Progress>) {
        let phase = phase_of(label);
        let mut st = self.state.lock().unwrap_or_else(|x| x.into_inner());
        if let Some(p) = p {
            st.0.insert(phase, (p.bytes_done, p.bytes_total));
        }
        // Como mucho ~10 eventos por segundo, salvo cambio de fase o fase terminada.
        let done_phase = p.is_some_and(|p| p.files_done == p.files_total);
        if st.2 == Some(phase) && !done_phase && st.1.elapsed() < Duration::from_millis(100) {
            return;
        }
        st.1 = Instant::now();
        st.2 = Some(phase);
        let known = self.expected.iter().all(|x| st.0.contains_key(x));
        let done: u64 = st.0.values().map(|v| v.0).sum();
        let total: u64 = if known { st.0.values().map(|v| v.1).sum() } else { 0 };
        let (bd, bt) = st.0.get(&phase).copied().unwrap_or((0, 0));
        (self.emit)(Event::Progress(Progress {
            task: self.task.clone(),
            instance: self.instance.clone(),
            task_bytes_done: if known { done.min(total) } else { done },
            task_bytes_total: total,
            phase,
            files_done: p.map_or(0, |p| p.files_done),
            files_total: p.map_or(0, |p| p.files_total),
            bytes_done: bd,
            bytes_total: bt,
            finished: done_phase,
        }));
    }

    fn finish(&self) {
        let st = self.state.lock().unwrap_or_else(|x| x.into_inner());
        let total: u64 = st.0.values().map(|v| v.1).sum();
        (self.emit)(Event::Progress(Progress {
            task: self.task.clone(),
            instance: self.instance.clone(),
            task_bytes_done: total,
            task_bytes_total: total,
            phase: Phase::Done,
            files_done: 0,
            files_total: 0,
            bytes_done: 0,
            bytes_total: 0,
            finished: true,
        }));
    }
}

fn phase_of(label: &str) -> Phase {
    match label {
        "Librerías" => Phase::DownloadingLibraries,
        "Assets" => Phase::DownloadingAssets,
        "Mods" | "Copiando configuración del pack" => Phase::DownloadingMods,
        "Pack" => Phase::DownloadingPack,
        l if l.starts_with("Java") || l.starts_with("Comprobando Java") => Phase::InstallingJava,
        l if l.starts_with("Preparando NeoForge") => Phase::PreparingLoader,
        _ => Phase::ReadingVersions,
    }
}

fn phase_only(id: &str, phase: Phase) -> Progress {
    Progress {
        task: format!("play-{id}"),
        instance: Some(id.into()),
        task_bytes_done: 0,
        task_bytes_total: 0,
        phase,
        files_done: 0,
        files_total: 0,
        bytes_done: 0,
        bytes_total: 0,
        finished: false,
    }
}

// ------------------------------------------------------------ ayudas

fn map_catalog(x: catalog::CatalogError) -> DinoError {
    use catalog::CatalogError as C;
    match x {
        C::NoExiste => e(ErrorCode::PackNotFound, x),
        C::Http(_) => e(ErrorCode::Offline, x),
        C::HashMalo => e(ErrorCode::HashMismatch, x),
        C::Estado(_) => e(ErrorCode::DownloadFailed, x),
        C::Io(_) => e(ErrorCode::DiskPermission, x),
        _ => internal(x),
    }
}

fn map_mc(x: dino_minecraft::Error) -> DinoError {
    use dino_download::DownloadError as D;
    match &x {
        dino_minecraft::Error::Download(D::Hash { .. }) => e(ErrorCode::HashMismatch, x),
        dino_minecraft::Error::Download(D::Http(_)) => e(ErrorCode::Offline, x),
        dino_minecraft::Error::Download(_) => e(ErrorCode::DownloadFailed, x),
        dino_minecraft::Error::Io(_, io) if io.raw_os_error() == Some(112) => e(ErrorCode::DiskFull, x),
        dino_minecraft::Error::Io(..) => e(ErrorCode::DiskPermission, x),
        _ => internal(x),
    }
}

/// `"neoforge 21.1.251"` (catálogo) → `LoaderSpec`.
pub fn parse_loader(text: Option<&str>) -> Option<LoaderSpec> {
    let t = text?.trim().to_lowercase();
    let (kind, version) = t.split_once(' ').map(|(k, v)| (k.to_string(), v.trim().to_string()))?;
    match kind.as_str() {
        "neoforge" => Some(LoaderSpec::NeoForge { version }),
        "fabric" | "fabric-loader" => Some(LoaderSpec::Fabric { version }),
        "vanilla" | "minecraft" => Some(LoaderSpec::Vanilla),
        _ => None,
    }
}

fn preview_of_remote(r: &catalog::PackRemoto) -> PackPreview {
    let raw = r.loader.as_deref().unwrap_or("").to_lowercase();
    let loader = parse_loader(r.loader.as_deref());
    PackPreview {
        code: Some(catalog::normalizar_codigo(&r.code)),
        name: r.name.clone(),
        version: r.version.clone(),
        minecraft: r.minecraft.clone().unwrap_or_default(),
        supported: !(raw.starts_with("forge") || raw.starts_with("quilt")),
        loader: loader.unwrap_or(LoaderSpec::Vanilla),
        mods: r.mods,
        icon: r.icon.clone(),
        server: r.server.clone(),
        pack_bytes: r.bytes,
        download_bytes: None,
    }
}

/// Un id de carpeta a partir de un nombre: minúsculas, números y guiones.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let s: String = out.trim_matches('-').chars().take(64).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        "pack".into()
    } else {
        s
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).is_some_and(|(x, y)| x == y)
}

fn extract_icon(mrpack: &Path, dest: &Path) {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(mrpack) else { return };
    let Ok(mut z) = zip::ZipArchive::new(f) else { return };
    let Ok(mut icon) = z.by_name("icon.png") else { return };
    let mut buf = Vec::new();
    if icon.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
        let _ = std::fs::write(dest, buf);
    }
}

fn count_jars(dir: &Path) -> u32 {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter(|x| x.file_name().to_string_lossy().ends_with(".jar")).count() as u32)
        .unwrap_or(0)
}

fn dir_size(p: &Path) -> u64 {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .map(|x| match x.file_type() {
                    Ok(t) if t.is_dir() => dir_size(&x.path()),
                    _ => x.metadata().map(|m| m.len()).unwrap_or(0),
                })
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(n: &str) -> (Arc<Core>, Arc<Mutex<Vec<Event>>>) {
        let d = std::env::temp_dir().join(format!("dino-instances-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        (Core::new(Paths::new(d), Arc::new(move |ev| l.lock().unwrap().push(ev))), log)
    }

    #[test]
    fn el_loader_del_catalogo() {
        assert_eq!(parse_loader(Some("neoforge 21.1.251")), Some(LoaderSpec::NeoForge { version: "21.1.251".into() }));
        assert_eq!(parse_loader(Some("Fabric 0.19.3")), Some(LoaderSpec::Fabric { version: "0.19.3".into() }));
        assert_eq!(parse_loader(Some("forge 47.2.0")), None);
        assert_eq!(parse_loader(None), None);
    }

    #[test]
    fn ids_a_partir_de_nombres() {
        assert_eq!(
            slug("Fabulously Optimized 14.0.0-alpha.4 for 26.2"),
            "fabulously-optimized-14-0-0-alpha-4-for-26-2"
        );
        assert_eq!(slug("¡VoidSafio!"), "voidsafio");
        assert_eq!(slug("¿?"), "pack");
        assert!(schema::valid_id(&slug("Pack de Prueba")));
    }

    #[test]
    fn la_barra_no_sabe_el_total_hasta_que_todas_las_fases_lo_dicen() {
        let (c, log) = core("tracker");
        let t = Tracker::new("t".into(), None, c.emit.clone(), &[Phase::DownloadingMods, Phase::DownloadingAssets]);
        let p = |d, t| dino_download::Progress {
            files_done: 0,
            files_total: 10,
            bytes_done: d,
            bytes_total: t,
            bytes_downloaded: d,
        };
        t.report("Mods", Some(p(0, 100)));
        t.report("Assets", Some(p(0, 900)));
        t.report("Mods", Some(dino_download::Progress { files_done: 10, ..p(100, 100) }));
        t.finish();
        let evs = log.lock().unwrap();
        let totals: Vec<(u64, u64)> = evs
            .iter()
            .filter_map(|e| match e {
                Event::Progress(p) => Some((p.task_bytes_done, p.task_bytes_total)),
                _ => None,
            })
            .collect();
        assert_eq!(totals[0], (0, 0), "solo una fase conocida: total desconocido");
        assert_eq!(totals[1], (0, 1000));
        assert_eq!(totals[2], (100, 1000));
        assert_eq!(*totals.last().unwrap(), (1000, 1000));
        let fracs: Vec<f64> = totals.iter().filter(|t| t.1 > 0).map(|t| t.0 as f64 / t.1 as f64).collect();
        assert!(fracs.windows(2).all(|w| w[1] >= w[0]), "la barra nunca retrocede: {fracs:?}");
    }

    #[tokio::test]
    async fn una_biblioteca_vacia_y_un_pack_que_no_esta() {
        let (c, _) = core("vacia");
        assert!(c.library_list().await.unwrap().is_empty());
        assert_eq!(c.library_select("nada").unwrap_err().code, ErrorCode::NotInstalled);
        assert_eq!(c.instance_details("nada").unwrap_err().code, ErrorCode::NotInstalled);
    }

    #[tokio::test]
    async fn sin_cuenta_jugar_pide_una() {
        let (c, _) = core("sincuenta");
        // Una instancia «instalada» de mentira: lo justo para llegar a pedir la cuenta.
        c.store
            .update_library(|l| {
                l.upsert(LibraryEntry {
                    id: "x".into(),
                    name: "X".into(),
                    source: PackSource::File,
                    pack_version: "1".into(),
                    server: None,
                    last_played: None,
                })
            })
            .unwrap();
        let m = InstanceManifest {
            schema: 1,
            id: "x".into(),
            minecraft: "1.21.1".into(),
            loader: LoaderSpec::Vanilla,
            state: InstallState::Ready,
            memory: Memory::Auto,
            jvm_args: vec![],
            java: JavaChoice::Auto,
            managed_files: vec![],
        };
        schema::save(&c.paths.instance_manifest("x"), &m, 1).unwrap();
        let out = c.play(PlayRequest { instance: "x".into(), launch: true }).await.unwrap();
        assert_eq!(out, PlayOutcome::NeedsAccount);
    }

    #[test]
    fn dos_tareas_a_la_vez_no() {
        let (c, _) = core("busy");
        let _g = c.begin("una").unwrap();
        assert_eq!(c.begin("otra").err().unwrap().code, ErrorCode::Busy);
        drop(_g);
        assert!(c.begin("otra").is_ok());
    }
}
