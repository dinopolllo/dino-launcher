//! Lo que la app hace con los packs: la capa que traduce cada comando del contrato
//! (`v2/docs/contratos.md`) a llamadas del núcleo, y que cuenta lo que pasa con eventos.
//!
//! `dino-app` (Tauri) solo envuelve [`Core`]: así todo esto se prueba sin interfaz.

pub mod admin;
pub mod catalog;
pub mod skins;
pub mod status;

use dino_auth::{Accounts, Engine};
use dino_config::{LibraryExt, Store};
use dino_core::api::{
    icon_url, AccountView, InstanceDetails, InstanceSettings, InstanceSettingsPatch, InstanceView, JavaOption,
    JavaOrigin, MicrosoftLoginView, PackPreview, PlayOutcome, PlayRequest, PrimaryAction, ProfileView, ServerStatus,
};
use dino_core::events::{Event, Phase, Progress, Session, SessionState};
use dino_core::pack_meta::PresenceVars;
use dino_core::profile::{self, LauncherPrefs, Profile, WindowPref};
use dino_core::schema::{
    self, AccountKind, InstallState, InstanceManifest, LibraryEntry, LoaderSpec, PackSource, Paths,
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
    /// Se guarda también de qué instancia, para contarlo al recargar la interfaz.
    busy: Mutex<Option<(String, Option<String>)>>,
    /// La comparte el descargador: «Cancelar» corta entre archivos y en cada trozo.
    cancel: Arc<AtomicBool>,
    /// El login de Microsoft en curso, si lo hay. Uno solo: reintentar o cancelar cierra el
    /// motor del intento anterior, que si no se quedaría colgado sin que nadie lo vea.
    login: Mutex<Option<Arc<LoginSlot>>>,
}

/// Un intento de login de Microsoft: quien lo lanza lo puede cancelar y esperar a que acabe.
#[derive(Default)]
struct LoginSlot {
    cancel: AtomicBool,
    done: AtomicBool,
}

/// Marca el intento como terminado pase lo que pase (también si el hilo entra en pánico).
struct LoginDone(Arc<LoginSlot>);
impl Drop for LoginDone {
    fn drop(&mut self) {
        self.0.done.store(true, Ordering::SeqCst);
    }
}

/// Un intento de login que nadie completa no puede tener el motor abierto para siempre.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Página de invitación de un pack: su código se pega detrás (la misma que usa la interfaz).
const INVITE_BASE: &str = "https://dinopolllo.github.io/dino-launcher-releases/invitar/#";

/// Suelta el cerrojo de tarea al salir, pase lo que pase, y anuncia `idle`. Toda tarea
/// larga (instalar, verificar, importar, añadir, jugar) dice `preparing` al empezar e `idle`
/// al acabar, bien, mal o cancelada: la interfaz no tiene que sondear para enterarse.
/// La excepción es Jugar cuando el juego arranca: entonces `idle` lo dice el hilo que
/// sigue la partida, al terminarla ([`BusyGuard::hand_off`]).
struct BusyGuard<'a> {
    busy: &'a Mutex<Option<(String, Option<String>)>>,
    emit: Emit,
    instance: Option<String>,
    handed_off: bool,
}

impl BusyGuard<'_> {
    fn hand_off(mut self) {
        self.handed_off = true;
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        *self.busy.lock().unwrap_or_else(|p| p.into_inner()) = None;
        if !self.handed_off {
            (self.emit)(Event::Session(Session { state: SessionState::Idle, instance: self.instance.clone() }));
        }
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
            cancel: Arc::new(AtomicBool::new(false)),
            login: Mutex::new(None),
        })
    }

    fn downloader(&self) -> Downloader {
        let cfg = self.store.config().unwrap_or_default();
        Downloader::new(cfg.download_concurrency.max(1) as usize)
            .with_cache(self.paths.shared_files())
            .with_rate_limit(cfg.download_limit)
            .with_cancel(self.cancel.clone())
    }

    fn begin(&self, what: &str, instance: Option<&str>) -> R<BusyGuard<'_>> {
        let mut b = self.busy.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((t, _)) = b.as_ref() {
            return Err(e(ErrorCode::Busy, format!("ya hay una tarea en marcha: {t}")));
        }
        *b = Some((what.to_string(), instance.map(String::from)));
        drop(b);
        self.cancel.store(false, Ordering::SeqCst);
        (self.emit)(Event::Session(Session { state: SessionState::Preparing, instance: instance.map(String::from) }));
        Ok(BusyGuard {
            busy: &self.busy,
            emit: self.emit.clone(),
            instance: instance.map(String::from),
            handed_off: false,
        })
    }

    fn check_cancel(&self) -> R<()> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(e(ErrorCode::Cancelled, "cancelado por el jugador"));
        }
        Ok(())
    }

    /// El estado de la partida ahora mismo, para la interfaz al cargar (sin sondeo).
    pub fn session_snapshot(&self) -> Session {
        if let Some(r) = dino_session::running(&self.paths) {
            return Session { state: SessionState::Playing, instance: Some(r.instance) };
        }
        match self.busy.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            Some((_, inst)) => Session { state: SessionState::Preparing, instance: inst.clone() },
            None => Session { state: SessionState::Idle, instance: None },
        }
    }

    // ------------------------------------------------------------ biblioteca

    pub async fn library_list(&self) -> R<Vec<InstanceView>> {
        let lib = self.store.library().map_err(internal)?;
        let catalog = if lib.instances.iter().any(|x| matches!(x.source, PackSource::Catalog { .. })) {
            // La biblioteca no espera a una red lenta o caída: sin respuesta en 2 s se enseña
            // con lo instalado («Actualizar» aparecerá la próxima vez; Jugar sí consulta).
            tokio::time::timeout(Duration::from_secs(2), catalog::fetch_catalog()).await.ok().and_then(|r| r.ok())
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
                if let Some(r) = &remote {
                    self.remember_extras(&entry.id, r);
                }
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
        // A medias pero con partida en disco (recién migrada de 1.x, o una reparación que se
        // cortó): para el jugador ya está instalada. «Jugar» verifica lo que falte y abre.
        let on_disk = manifest.is_some() && self.paths.game_dir(&entry.id).join("mods").is_dir();
        let mods = match (&manifest, remote) {
            (Some(m), _) if installed || on_disk => mods_of(m, &self.paths.game_dir(&entry.id)),
            (_, Some(r)) => r.mods,
            _ => 0,
        };
        let action = if playing == Some(entry.id.as_str()) {
            PrimaryAction::Playing
        } else if let Some(r) = remote.filter(|r| r.version != entry.pack_version && (installed || on_disk)) {
            // `r.bytes` del catálogo es el peso del `.mrpack`, no lo que se va a descargar
            // (mods, Minecraft, assets): eso no se sabe hasta leer el pack.
            PrimaryAction::Update { to_version: r.version.clone(), bytes: None }
        } else if !installed && !on_disk {
            PrimaryAction::Install { bytes: None }
        } else {
            PrimaryAction::Play
        };
        // Lo opcional del pack: del catálogo si se pudo leer, y si no, la copia de la última vez.
        let (extras, extras_version) = match remote {
            Some(r) => (r.extras.sanitized(), Some(r.version.clone())),
            None => {
                let (x, v) = self.store.pack_extras(&entry.id);
                (x.sanitized(), v)
            }
        };
        let notes_version = extras.notes.as_ref().and(extras_version.or_else(|| Some(entry.pack_version.clone())));
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
            disk_bytes: (installed || on_disk).then(|| dir_size(&self.paths.instance_dir(&entry.id))),
            selected: selected == Some(entry.id.as_str()),
            action,
            theme: extras.theme,
            notes: extras.notes,
            notes_version,
        }
    }

    /// Guarda lo opcional que acaba de publicar el catálogo para tenerlo sin red.
    fn remember_extras(&self, id: &str, r: &catalog::PackRemoto) {
        self.store.save_pack_extras(id, &r.extras, &r.version);
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
            let _ = std::fs::remove_file(self.paths.pack_extras(id));
            // Lo que cada jugador eligió para una instancia que ya no existe no sirve de nada.
            let _ = self.store.update_profiles(|f| {
                let uuids: Vec<String> = f.profiles.keys().cloned().collect();
                for u in uuids {
                    f.update(&u, |p| {
                        p.instances.remove(id);
                    });
                }
            });
        }
        (self.emit)(Event::LibraryChanged);
        Ok(())
    }

    /// Funciona también con una instancia a medias (una actualización que falló): sus
    /// mundos siguen en disco y el diálogo de quitar tiene que poder contarlos.
    pub fn instance_details(&self, id: &str) -> R<InstanceDetails> {
        let m: InstanceManifest = schema::load(&self.paths.instance_manifest(id))
            .map_err(internal)?
            .ok_or_else(|| e(ErrorCode::NotInstalled, format!("«{id}» no está instalada")))?;
        let game = self.paths.game_dir(id);
        let worlds = std::fs::read_dir(game.join("saves"))
            .map(|rd| rd.flatten().filter(|x| x.path().join("level.dat").exists()).count() as u32)
            .unwrap_or(0);
        Ok(InstanceDetails {
            id: id.into(),
            path: game.to_string_lossy().into_owned(),
            minecraft: m.minecraft.clone(),
            loader: m.loader.clone(),
            mods: mods_of(&m, &game),
            worlds,
            disk_bytes: dir_size(&self.paths.instance_dir(id)),
        })
    }

    /// El manifiesto, esté lista o a medias (los ajustes no dependen de eso: una instancia
    /// recién migrada se puede configurar antes de su primera verificación).
    fn manifest_any(&self, id: &str) -> R<InstanceManifest> {
        schema::load(&self.paths.instance_manifest(id))
            .map_err(internal)?
            .ok_or_else(|| e(ErrorCode::NotInstalled, format!("«{id}» no está instalada")))
    }

    fn manifest_ready(&self, id: &str) -> R<InstanceManifest> {
        let m = self.manifest_any(id)?;
        if m.state != InstallState::Ready {
            return Err(e(ErrorCode::NotInstalled, format!("«{id}» quedó a medias")));
        }
        Ok(m)
    }

    /// La cuenta activa: su UUID y su nombre.
    fn active_account(&self) -> Option<(String, String)> {
        self.accounts.list(&self.engine.accounts()).ok()?.into_iter().find(|a| a.active).map(|a| (a.uuid, a.name))
    }

    /// El perfil de una cuenta (vacío si nunca lo tocó o si el archivo no se pudo leer: un
    /// perfil ilegible no debe impedir jugar).
    fn profile_of(&self, uuid: &str) -> Profile {
        self.store.profiles().map(|f| f.of(uuid)).unwrap_or_default()
    }

    pub fn instance_settings_get(&self, id: &str) -> R<InstanceSettings> {
        let m = self.manifest_any(id)?;
        let active = self.active_account();
        let profile = active.as_ref().map(|(u, _)| self.profile_of(u));
        let r = profile::resolve(profile.as_ref(), id, &m);
        let ram = os::total_memory_mb().unwrap_or(8192);
        Ok(InstanceSettings {
            memory: r.effective.memory,
            jvm_args: r.effective.jvm_args.clone(),
            java: r.effective.java.clone(),
            window: r.effective.window,
            memory_from: r.memory_from,
            jvm_args_from: r.jvm_args_from,
            java_from: r.java_from,
            window_from: r.window_from,
            profile: active.map(|(_, n)| n),
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
                        major: java_major(&bin),
                        origin: JavaOrigin::Mojang,
                    });
                }
            }
        }
        out
    }

    /// Guarda lo que el jugador activo elige para esta instancia, **en su perfil**: otro
    /// jugador que abra el mismo pack no lo hereda, ni se pisa lo que trae el pack. Solo se
    /// toca lo que el parche trae. Sin ninguna cuenta no hay perfil donde guardarlo y se
    /// escribe en la propia instancia, como en 1.x.
    pub fn instance_settings_set(&self, id: &str, patch: InstanceSettingsPatch) -> R<()> {
        let mut m = self.manifest_any(id)?;
        let Some((uuid, _)) = self.active_account() else {
            if let Some(x) = patch.memory {
                m.memory = x;
            }
            if let Some(x) = patch.jvm_args {
                m.jvm_args = x;
            }
            if let Some(x) = patch.java {
                m.java = x;
            }
            return schema::save(&self.paths.instance_manifest(id), &m, schema::INSTANCE_SCHEMA).map_err(internal);
        };
        self.store
            .update_profiles(|f| {
                f.update(&uuid, |p| {
                    let o = p.instances.entry(id.to_string()).or_default();
                    if let Some(x) = patch.memory {
                        o.memory = Some(x);
                    }
                    if let Some(x) = patch.jvm_args {
                        o.jvm_args = Some(x);
                    }
                    if let Some(x) = patch.java {
                        o.java = Some(x);
                    }
                    if let Some(x) = patch.window {
                        o.window = Some(x.sanitized());
                    }
                    if o.is_default() {
                        p.instances.remove(id);
                    }
                })
            })
            .map_err(internal)
    }

    /// Vuelve a los valores del pack para esta instancia (borra lo que el jugador activo
    /// había elegido para ella).
    pub fn instance_settings_reset(&self, id: &str) -> R<()> {
        self.manifest_any(id)?;
        let Some((uuid, _)) = self.active_account() else { return Ok(()) };
        self.store
            .update_profiles(|f| {
                f.update(&uuid, |p| {
                    p.instances.remove(id);
                })
            })
            .map_err(internal)
    }

    // ------------------------------------------------------------ perfil del jugador

    fn profile_view(&self, active: Option<(String, String)>) -> ProfileView {
        let p = active.as_ref().map(|(u, _)| self.profile_of(u)).unwrap_or_default();
        ProfileView {
            uuid: active.as_ref().map(|(u, _)| u.clone()),
            name: active.map(|(_, n)| n),
            prefs: p.launcher.resolved(),
            raw: p.launcher,
            window: p.window,
        }
    }

    /// Las preferencias del jugador activo. Sin cuenta, las de fábrica.
    pub fn profile_get(&self) -> ProfileView {
        self.profile_view(self.active_account())
    }

    /// Reemplaza las preferencias del launcher y la ventana por omisión del jugador activo.
    /// Un campo en `null` vuelve a la de fábrica.
    pub fn profile_set(&self, launcher: LauncherPrefs, window: Option<WindowPref>) -> R<ProfileView> {
        let active = self.active_account().ok_or_else(|| e(ErrorCode::AccountRequired, "no hay ninguna cuenta"))?;
        let uuid = active.0.clone();
        self.store
            .update_profiles(|f| {
                f.update(&uuid, |p| {
                    p.launcher = launcher;
                    p.window = window.map(WindowPref::sanitized);
                })
            })
            .map_err(internal)?;
        Ok(self.profile_view(Some(active)))
    }

    /// Borra todo lo que el jugador activo había elegido: queda como un jugador nuevo.
    pub fn profile_reset(&self) -> R<ProfileView> {
        let active = self.active_account().ok_or_else(|| e(ErrorCode::AccountRequired, "no hay ninguna cuenta"))?;
        let uuid = active.0.clone();
        self.store.update_profiles(|f| f.update(&uuid, |p| *p = Profile::default())).map_err(internal)?;
        Ok(self.profile_view(Some(active)))
    }

    // ------------------------------------------------------------ catálogo e importar

    pub async fn catalog_lookup(&self, text: &str) -> R<PackPreview> {
        let r = catalog::resolver(text).await.map_err(map_catalog)?;
        Ok(preview_of_remote(&r))
    }

    /// Añade un pack del catálogo a la biblioteca: baja el `.mrpack` y el icono, y lo deja
    /// seleccionado listo para instalar con Jugar.
    pub async fn catalog_add(&self, text: &str) -> R<InstanceView> {
        let _g = self.begin("añadir un pack", None)?;
        let r = catalog::resolver(text).await.map_err(map_catalog)?;
        let code = catalog::normalizar_codigo(&r.code);
        let lib = self.store.library().map_err(internal)?;
        let id = lib.free_id(&slug(&r.id), Some(&code));
        self.download_pack(&r, &id).await?;
        self.remember_extras(&id, &r);
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

    /// Avisa antes de empezar, en vez de fallar a mitad con el disco lleno.
    fn check_space(&self, pack_bytes: u64) -> R<()> {
        space_ok(os::free_space(&self.paths.root), pack_bytes)
    }

    /// Primera vez que 2.0 abre una carpeta de 1.x: migra (mueve, no copia, con diario).
    /// Devuelve los avisos (lo que se quedó en 1.x y por qué), o `None` si no había nada.
    pub fn migrate_if_needed(&self) -> R<Option<Vec<String>>> {
        if self.paths.library().exists() || !self.paths.root.join("packs.json").exists() {
            return Ok(None);
        }
        let Some((packs, accounts, inst)) = dino_core::migrate::read_v1(&self.paths) else { return Ok(None) };
        let plan = dino_core::migrate::plan(&self.paths, &packs, accounts.as_deref(), &inst).map_err(internal)?;
        let mut notes = dino_core::migrate::apply(&self.paths, &plan).map_err(internal)?;
        notes.extend(plan.warnings.clone());
        tracing::info!(instances = plan.library.instances.len(), ?notes, "datos de 1.x migrados");
        (self.emit)(Event::LibraryChanged);
        (self.emit)(Event::AccountsChanged);
        Ok(Some(notes))
    }

    async fn download_pack(&self, r: &catalog::PackRemoto, id: &str) -> R<()> {
        self.check_space(r.bytes)?;
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
        catalog::descargar_pack(r, &self.paths.pack_file(id), &on, &self.cancel).await.map_err(map_catalog)?;
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
        self.check_space(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))?;
        let id = lib.free_id(&slug(&pre.name), None);
        let dest = self.paths.pack_file(&id);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d).map_err(|x| e(ErrorCode::DiskPermission, x))?;
        }
        if !same_path(path, &dest) {
            std::fs::copy(path, &dest).map_err(|x| e(ErrorCode::DiskPermission, x))?;
        }
        extract_icon(&dest, &self.paths.pack_icon(&id));
        // Reimportar el mismo pack (una versión nueva) lo actualiza: lo que el jugador ya
        // tenía y el archivo no dice —servidor, última partida— se conserva.
        let before = lib.find(&id);
        let entry = LibraryEntry {
            id: id.clone(),
            name: pre.name.clone(),
            source: PackSource::File,
            pack_version: pre.version.clone(),
            server: server.or_else(|| before.and_then(|b| b.server.clone())),
            last_played: before.and_then(|b| b.last_played),
        };
        self.store
            .update_library(|l| {
                l.upsert(entry.clone());
                l.select(&id);
            })
            .map_err(internal)?;
        (self.emit)(Event::LibraryChanged);
        let _g = self.begin("instalar", Some(&id))?;
        self.install(&id, false).await?;
        Ok(self.view(&entry, None, None, Some(&id)))
    }

    // ------------------------------------------------------------ instalar

    /// Instala (o repara, o verifica) la instancia desde su `.mrpack`. Un archivo que ya
    /// está con el hash bueno no se baja: verificar es lo mismo que instalar sin cambios.
    async fn install(&self, id: &str, verify: bool) -> R<()> {
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
            verify.then_some(Phase::VerifyingFiles),
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
        let _g = self.begin("verificar", Some(id))?;
        self.install(id, true).await
    }

    // ------------------------------------------------------------ jugar

    /// Abrir → Jugar → Minecraft: busca versión nueva del pack, instala si hace falta,
    /// renueva la sesión de Microsoft si le queda poco y lanza. La partida la sigue un hilo
    /// que emite los eventos `session`.
    pub async fn play(self: &Arc<Self>, req: PlayRequest) -> R<PlayOutcome> {
        let guard = self.begin("jugar", Some(&req.instance))?;
        let id = req.instance.clone();
        let lib = self.store.library().map_err(internal)?;
        let entry = lib.find(&id).cloned().ok_or_else(|| e(ErrorCode::NotInstalled, format!("no hay «{id}»")))?;
        let result = self.play_inner(&entry, req.launch).await;
        match result {
            Ok(Some(session)) => {
                // Desde aquí el estado lo cuenta el hilo de la partida (playing → idle).
                guard.hand_off();
                let core = self.clone();
                std::thread::spawn(move || core.watch(session));
                Ok(PlayOutcome::Launched)
            }
            Ok(None) => Ok(if req.launch { PlayOutcome::NeedsAccount } else { PlayOutcome::Installed }),
            Err(x) => Err(x),
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
                    self.remember_extras(id, &r);
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
            self.install(id, false).await?;
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
        let profile = self.profile_of(&active.uuid);
        let opts = dino_session::LaunchOptions {
            server,
            max_mem_mb: None,
            account: Some(active.uuid.clone()),
            profile: Some(profile),
        };
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
        // La presencia de Discord vive lo que dura la partida: se pone al abrirse la ventana
        // y se quita al terminar, también si el juego se cierra mal.
        let presence: Mutex<Option<dino_presence::Handle>> = Mutex::new(None);
        // `preparing` ya lo anunció `play` al empezar; aquí solo lo que viene después.
        let out = dino_session::wait(&self.paths, s, &|st| {
            if st == SessionState::Playing {
                *presence.lock().unwrap_or_else(|p| p.into_inner()) = self.presence_for(&id);
            }
            if st != SessionState::Preparing {
                emit(Event::Session(Session { state: st, instance: Some(id.clone()) }))
            }
        });
        if let Some(h) = presence.lock().unwrap_or_else(|p| p.into_inner()).take() {
            h.stop();
        }
        tracing::info!(?out, "partida terminada");
        (self.emit)(Event::LibraryChanged);
        // Tras un cierre limpio no queda nada que enseñar: lo que siga vivo del Java de Dino
        // es un resto. Tras un fallo se deja en paz (el asistente de fallos puede estar
        // preparando su informe); lo recogerá la próxima apertura si sigue sin ventana.
        if out.exit_code == Some(0) {
            std::thread::sleep(Duration::from_secs(3));
            self.sweep_strays();
        }
    }

    /// La actividad de Discord de esta instancia, ya puesta, o `None` si no toca: el pack no
    /// la activó, el jugador la apagó, o no hay aplicación de Discord con la que hablar.
    /// Nunca se inventa una actividad por defecto: lo que el pack no dice, no se manda.
    fn presence_for(&self, id: &str) -> Option<dino_presence::Handle> {
        let (extras, _) = self.store.pack_extras(id);
        let presence = extras.sanitized().presence?;
        let (uuid, player) = self.active_account()?;
        if !self.profile_of(&uuid).launcher.resolved().discord_presence {
            tracing::info!("Discord: el jugador la tiene apagada");
            return None;
        }
        let client_id = presence
            .client_id
            .clone()
            .or_else(|| self.store.config().ok().and_then(|c| c.discord_client_id))
            .or_else(|| option_env!("DINO_DISCORD_CLIENT_ID").map(String::from))
            .filter(|c| dino_core::pack_meta::is_snowflake(c));
        let Some(client_id) = client_id else {
            tracing::info!("Discord: no hay aplicación de Discord configurada, no se manda presencia");
            return None;
        };
        let lib = self.store.library().ok()?;
        let entry = lib.find(id)?;
        let minecraft = self.manifest_any(id).map(|m| m.minecraft).unwrap_or_default();
        let code = match &entry.source {
            PackSource::Catalog { code } => Some(code.as_str()),
            PackSource::File => None,
        };
        let vars = PresenceVars {
            pack: &entry.name,
            server: entry.server.as_deref(),
            version: &entry.pack_version,
            minecraft: &minecraft,
            player: &player,
        };
        Some(dino_presence::start(client_id, presence.render(&vars, code, INVITE_BASE)))
    }

    /// Cierra los procesos del Java de Dino que siguen vivos sin ventana y sin partida.
    /// En 1.x lo hacía `kill_orphans`; aquí ya no hay motores colgados, pero algunos
    /// modpacks dejan un proceso suyo vivo que retiene los archivos del Java.
    pub fn sweep_strays(&self) -> usize {
        let except: Vec<u32> = dino_session::running(&self.paths).map(|r| vec![r.pid]).unwrap_or_default();
        let found = os::strays_under(&self.paths.java(), &except);
        for pid in &found {
            let exe = os::process_exe(*pid);
            tracing::info!(pid, ?exe, ok = os::terminate(*pid), "resto de una partida cerrado");
        }
        found.len()
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
        let skin = dino_core::api::skin_url(&a.name);
        Ok(AccountView { kind: a.kind, name: a.name, uuid: a.uuid, active: true, session_minutes_left: None, skin })
    }

    /// El PNG de la skin de un nombre (con caché de un día en `cache/skins`).
    pub async fn skin(&self, name: &str) -> Option<Vec<u8>> {
        skins::skin(&self.paths.root.join("cache").join("skins"), name).await
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
    ///
    /// Un solo intento a la vez: si había otro abierto (el jugador reintenta), se cierra y se
    /// espera a que acabe antes de abrir el nuevo; dos motores con la misma carpeta de datos
    /// se estorban, y el viejo se quedaría colgado sin ventana que lo cierre.
    pub async fn account_microsoft_add(self: &Arc<Self>) -> R<MicrosoftLoginView> {
        self.engine.ensure(&self.downloader()).await.map_err(|x| e(ErrorCode::DownloadFailed, x))?;
        let slot = Arc::new(LoginSlot::default());
        let previous = self.login.lock().unwrap_or_else(|p| p.into_inner()).replace(slot.clone());
        if let Some(old) = previous {
            old.cancel.store(true, Ordering::SeqCst);
            let t0 = Instant::now();
            while !old.done.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(15) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let engine = self.engine.clone();
        let opened = tokio::task::spawn_blocking(move || engine.open_microsoft_login())
            .await
            .map_err(internal)
            .and_then(|r| r.map_err(internal));
        let login = match opened {
            Ok(l) => l,
            Err(x) => {
                slot.done.store(true, Ordering::SeqCst);
                return Err(x);
            }
        };
        let view = MicrosoftLoginView { only_microsoft_window: login.only_microsoft() };
        if let Some(why) = login.fallback {
            tracing::info!(why, "login de Microsoft con el motor entero a la vista");
        }
        let _ = self.accounts.sync_bridge(&self.engine.accounts());
        let before = self.msa_uuids();
        let shown =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let core = self.clone();
        std::thread::spawn(move || {
            let _done = LoginDone(slot.clone());
            // Se mira `accounts.json` mientras el jugador inicia sesión: en cuanto aparece
            // la cuenta, se avisa y se cierra el motor (como en 1.x), sin pedirle que lo cierre.
            let mut login = login;
            let pid = login.child.id();
            let started = Instant::now();
            let mut closed_at: Option<Instant> = None;
            let mut cancelled = false;
            loop {
                if matches!(login.child.try_wait(), Ok(Some(_)) | Err(_)) {
                    break;
                }
                // El jugador canceló o reintentó (otro intento ocupa su sitio), o nadie lo
                // completó en mucho rato: el motor, que quizá ni se ve, no se queda abierto.
                let timed_out = started.elapsed() > LOGIN_TIMEOUT;
                if slot.cancel.load(Ordering::SeqCst) || timed_out {
                    cancelled = !timed_out;
                    close_engine(&mut login.child, pid);
                    break;
                }
                // Cerró la ventana de Microsoft: si en unos segundos no llega la cuenta, es
                // que canceló, y el motor (que nadie ve) se cierra también.
                if !login.window_open() {
                    let t = *closed_at.get_or_insert_with(Instant::now);
                    if t.elapsed() > Duration::from_secs(5) {
                        close_engine(&mut login.child, pid);
                        break;
                    }
                }
                // Nueva = no estaba al empezar. No vale fiarse de lo que añada *esta*
                // sincronización: `accounts_list` también sincroniza y puede adelantarse.
                let _ = core.accounts.sync_bridge(&core.engine.accounts());
                let mut pick = core.msa_uuids().into_iter().rfind(|u| !before.contains(u));
                if pick.is_none() {
                    pick = relogin(&core.engine.accounts(), shown, &login);
                }
                if let Some(uuid) = pick.as_ref() {
                    // Quien acaba de iniciar sesión con Microsoft quiere jugar con esa cuenta,
                    // no con la que tuviera activa (p. ej. una sin conexión).
                    let _ = core.accounts.unhide(uuid);
                    let _ = core.accounts.sync_bridge(&core.engine.accounts());
                    if let Err(x) = core.accounts.select(uuid) {
                        tracing::warn!(error = %x, "no pude activar la cuenta Microsoft nueva");
                    }
                    let name = core
                        .accounts
                        .list(&core.engine.accounts())
                        .ok()
                        .and_then(|l| l.into_iter().find(|a| &a.uuid == uuid).map(|a| a.name));
                    (core.emit)(Event::AccountsChanged);
                    (core.emit)(Event::MicrosoftLogin { account: name });
                    os::close_windows(pid, true);
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while matches!(login.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    let _ = login.child.kill();
                    let _ = login.child.wait();
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            let _ = core.accounts.sync_bridge(&core.engine.accounts());
            (core.emit)(Event::AccountsChanged);
            // Cancelado a propósito: la interfaz ya sabe que lo dejó (o está esperando otro
            // intento, al que este aviso tardío le diría que «se cerró sin entrar»).
            if !cancelled {
                (core.emit)(Event::MicrosoftLogin { account: None });
            }
        });
        Ok(view)
    }

    /// Cancela el login de Microsoft en curso y cierra su motor. No espera: el cierre lo
    /// hace el hilo que lo vigila. No pasa nada si no había ninguno.
    pub fn account_microsoft_cancel(&self) {
        if let Some(slot) = self.login.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            slot.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Al salir de la app: que no quede ningún motor de login vivo. Espera un momento a que
    /// el hilo lo cierre; pasado el plazo, la app sale igual.
    pub fn shutdown(&self) {
        let slot = self.login.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(slot) = slot {
            slot.cancel.store(true, Ordering::SeqCst);
            let t0 = Instant::now();
            while !slot.done.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(6) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    fn msa_uuids(&self) -> Vec<String> {
        self.accounts
            .list(&self.engine.accounts())
            .map(|l| l.into_iter().filter(|a| a.kind == AccountKind::MicrosoftBridge).map(|a| a.uuid).collect())
            .unwrap_or_default()
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
    /// Si se da, es la fase que se anuncia siempre (verificar es instalar sin cambios,
    /// pero la interfaz no debe decir «Descargando» cuando no baja nada).
    display: Option<Phase>,
    state: Mutex<TrackerState>,
}

impl Tracker {
    fn new(task: String, instance: Option<String>, emit: Emit, expected: &[Phase], display: Option<Phase>) -> Self {
        Self {
            task,
            instance,
            emit,
            expected: expected.to_vec(),
            display,
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
            phase: self.display.unwrap_or(phase),
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
        C::Cancelado => e(ErrorCode::Cancelled, x),
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
        dino_minecraft::Error::Download(D::Cancelled) => e(ErrorCode::Cancelled, x),
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

/// Mods de una instancia: los que anotó el pack o, si es de antes de anotarlos, los `.jar`
/// que haya en `mods/`. La ficha y el detalle usan esta misma cuenta.
/// Cierra el motor como pulsando la X y, si no hace caso en unos segundos, a la fuerza.
/// Solo el proceso que lanzó Dino.
fn close_engine(child: &mut std::process::Child, pid: u32) {
    os::close_windows(pid, true);
    let deadline = Instant::now() + Duration::from_secs(3);
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Iniciar sesión con una cuenta que el motor **ya tenía** no añade nada: el motor la
/// reemplaza con una sesión nueva. Se reconoce por la hora de esa sesión (`iat`): posterior a
/// cuando se enseñó la ventana. Pero el motor también renueva todas las cuentas al arrancar,
/// así que solo cuenta una vez cerrada la ventana de Microsoft (el login correcto la cierra) y
/// con margen; con el motor entero a la vista, con más margen, porque llegar a mano a Cuentas
/// e iniciar sesión lleva bastante más que esa renovación.
fn relogin(engine_accounts: &Path, shown: u64, login: &dino_auth::engine::MicrosoftLogin) -> Option<String> {
    let (ready, margin) = if login.only_microsoft() { (!login.window_open(), 2) } else { (true, 15) };
    if !ready {
        return None;
    }
    issued_since(dino_auth::list(engine_accounts).unwrap_or_default(), shown + margin)
}

/// La cuenta Microsoft cuya sesión se emitió más tarde, si fue a partir de `since`.
fn issued_since(engine: Vec<dino_auth::AccountInfo>, since: u64) -> Option<String> {
    engine
        .into_iter()
        .filter(|a| a.kind == "MSA" && !a.uuid.is_empty() && a.issued.is_some_and(|i| i >= since))
        .max_by_key(|a| a.issued)
        .map(|a| a.uuid)
}

/// Sin dato del disco no se bloquea: mejor intentar que impedir jugar por una medida fallida.
fn space_ok(free: Option<u64>, pack_bytes: u64) -> R<()> {
    let need = os::space_needed(pack_bytes);
    match free {
        Some(free) if free < need => Err(e(
            ErrorCode::DiskFull,
            format!("hacen falta {} MB libres y hay {} MB", need / 1_048_576, free / 1_048_576),
        )),
        _ => Ok(()),
    }
}

fn mods_of(m: &InstanceManifest, game: &Path) -> u32 {
    if m.managed_files.is_empty() {
        count_jars(&game.join("mods"))
    } else {
        m.managed_files.iter().filter(|f| f.starts_with("mods/") && f.ends_with(".jar")).count() as u32
    }
}

fn count_jars(dir: &Path) -> u32 {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter(|x| x.file_name().to_string_lossy().ends_with(".jar")).count() as u32)
        .unwrap_or(0)
}

/// La versión mayor de un Java por su archivo `release` (`JAVA_VERSION="21.0.3"`, o
/// `"1.8.0_402"` para Java 8).
fn java_major(bin: &Path) -> Option<u32> {
    let home = bin.parent()?.parent()?;
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    let v = text.lines().find_map(|l| l.strip_prefix("JAVA_VERSION="))?.trim_matches('"');
    let mut parts = v.split(['.', '_', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
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
    #[test]
    fn volver_a_entrar_con_una_cuenta_que_ya_estaba() {
        let acc = |uuid: &str, kind: &str, iat: Option<u64>| dino_auth::AccountInfo {
            kind: kind.into(),
            name: uuid.into(),
            uuid: uuid.into(),
            active: false,
            expires: None,
            issued: iat,
        };
        // renovada al arrancar (antes) y la del login (después): gana la del login
        let l = vec![acc("vieja", "MSA", Some(100)), acc("login", "MSA", Some(140)), acc("x", "Offline", Some(200))];
        assert_eq!(issued_since(l, 120).as_deref(), Some("login"));
        // solo renovaciones: nada
        assert_eq!(issued_since(vec![acc("vieja", "MSA", Some(100))], 120), None);
    }

    use super::*;
    use dino_core::schema::{JavaChoice, Memory};

    #[test]
    fn sin_sitio_se_avisa_antes_de_empezar() {
        let x = space_ok(Some(500 * 1_048_576), 300 * 1_048_576).unwrap_err();
        assert_eq!(x.code, ErrorCode::DiskFull);
        assert!(x.detail.contains("1050 MB") && x.detail.contains("500 MB"), "{}", x.detail);
        assert!(space_ok(Some(2_000 * 1_048_576), 300 * 1_048_576).is_ok());
        assert!(space_ok(None, u64::MAX).is_ok(), "sin medida no se bloquea");
    }

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
        let t =
            Tracker::new("t".into(), None, c.emit.clone(), &[Phase::DownloadingMods, Phase::DownloadingAssets], None);
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

    #[tokio::test]
    async fn una_tarea_que_falla_tambien_anuncia_preparing_e_idle() {
        let (c, log) = core("estados");
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
        // Sin .mrpack: verificar falla, pero la interfaz tiene que saber que acabó.
        assert_eq!(c.instance_verify("x").await.unwrap_err().code, ErrorCode::PackInvalid);
        let states: Vec<SessionState> = log
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::Session(s) => Some(s.state),
                _ => None,
            })
            .collect();
        assert_eq!(states, vec![SessionState::Preparing, SessionState::Idle]);
    }

    #[test]
    fn dos_tareas_a_la_vez_no() {
        let (c, _) = core("busy");
        let _g = c.begin("una", Some("x")).unwrap();
        assert_eq!(c.begin("otra", None).err().unwrap().code, ErrorCode::Busy);
        let s = c.session_snapshot();
        assert_eq!((s.state, s.instance.as_deref()), (SessionState::Preparing, Some("x")));
        drop(_g);
        assert_eq!(c.session_snapshot().state, SessionState::Idle);
        assert!(c.begin("otra", None).is_ok());
    }

    /// Una instancia instalada a mano en disco, sin pasar por la red.
    fn installed(c: &Core, id: &str, memory: Memory) {
        let m = InstanceManifest {
            schema: schema::INSTANCE_SCHEMA,
            id: id.into(),
            minecraft: "1.21.1".into(),
            loader: LoaderSpec::Vanilla,
            state: InstallState::Ready,
            memory,
            jvm_args: vec![],
            java: JavaChoice::Auto,
            managed_files: vec![],
        };
        schema::save(&c.paths.instance_manifest(id), &m, schema::INSTANCE_SCHEMA).unwrap();
    }

    fn uuid_de(c: &Core, name: &str) -> String {
        c.accounts.list(&c.engine.accounts()).unwrap().iter().find(|a| a.name == name).unwrap().uuid.clone()
    }

    #[test]
    fn los_ajustes_son_de_cada_jugador_y_no_se_pisan_al_alternar_cuentas() {
        let (c, _) = core("perfiles");
        installed(&c, "vs", Memory::Fixed { max_mb: 4096 });
        c.accounts.add_offline("Ana_01").unwrap();
        let ana = uuid_de(&c, "Ana_01");
        // Ana sube la memoria de este pack...
        c.instance_settings_set(
            "vs",
            InstanceSettingsPatch { memory: Some(Memory::Fixed { max_mb: 8192 }), ..Default::default() },
        )
        .unwrap();
        let s = c.instance_settings_get("vs").unwrap();
        assert_eq!(s.memory, Memory::Fixed { max_mb: 8192 });
        assert_eq!(s.memory_from, profile::Source::Profile);
        assert_eq!(s.profile.as_deref(), Some("Ana_01"));
        // ...y un jugador nuevo, en la misma máquina, ve lo del pack y no lo de Ana.
        c.accounts.add_offline("Beto_02").unwrap();
        c.accounts.select(&uuid_de(&c, "Beto_02")).unwrap();
        let s = c.instance_settings_get("vs").unwrap();
        assert_eq!(s.memory, Memory::Fixed { max_mb: 4096 });
        assert_eq!(s.memory_from, profile::Source::Pack);
        // Volver a Ana le devuelve lo suyo.
        c.accounts.select(&ana).unwrap();
        assert_eq!(c.instance_settings_get("vs").unwrap().memory, Memory::Fixed { max_mb: 8192 });
        // Lo del pack en disco no se tocó.
        let m: InstanceManifest = schema::load(&c.paths.instance_manifest("vs")).unwrap().unwrap();
        assert_eq!(m.memory, Memory::Fixed { max_mb: 4096 });
        // Restablecer vuelve a los valores del pack, solo para ella.
        c.instance_settings_reset("vs").unwrap();
        assert_eq!(c.instance_settings_get("vs").unwrap().memory, Memory::Fixed { max_mb: 4096 });
    }

    #[test]
    fn un_parche_solo_toca_lo_que_trae() {
        let (c, _) = core("parche");
        installed(&c, "vs", Memory::Auto);
        c.accounts.add_offline("Ana_01").unwrap();
        c.instance_settings_set(
            "vs",
            InstanceSettingsPatch { window: Some(WindowPref::Fullscreen), ..Default::default() },
        )
        .unwrap();
        let s = c.instance_settings_get("vs").unwrap();
        assert_eq!(s.window, WindowPref::Fullscreen);
        assert_eq!(s.window_from, profile::Source::Profile);
        assert_eq!(
            (s.memory_from, s.java_from, s.jvm_args_from),
            (profile::Source::Pack, profile::Source::Pack, profile::Source::Pack)
        );
    }

    #[test]
    fn sin_cuenta_los_ajustes_van_a_la_instancia_como_en_1x() {
        let (c, _) = core("sincuenta");
        installed(&c, "vs", Memory::Auto);
        c.instance_settings_set(
            "vs",
            InstanceSettingsPatch { memory: Some(Memory::Fixed { max_mb: 3072 }), ..Default::default() },
        )
        .unwrap();
        let m: InstanceManifest = schema::load(&c.paths.instance_manifest("vs")).unwrap().unwrap();
        assert_eq!(m.memory, Memory::Fixed { max_mb: 3072 });
        assert!(c.instance_settings_get("vs").unwrap().profile.is_none());
    }

    #[test]
    fn las_preferencias_del_launcher_son_de_cada_jugador_y_con_fabrica_por_defecto() {
        let (c, _) = core("prefs");
        assert!(c.profile_get().prefs.head_tracking, "sin cuenta, valores de fabrica");
        assert_eq!(c.profile_set(LauncherPrefs::default(), None).unwrap_err().code, ErrorCode::AccountRequired);
        c.accounts.add_offline("Ana_01").unwrap();
        let ana = uuid_de(&c, "Ana_01");
        let v = c
            .profile_set(LauncherPrefs { head_tracking: Some(false), ui_volume: Some(0.2), ..Default::default() }, None)
            .unwrap();
        assert!(!v.prefs.head_tracking && v.prefs.ui_sounds);
        c.accounts.add_offline("Beto_02").unwrap();
        c.accounts.select(&uuid_de(&c, "Beto_02")).unwrap();
        assert!(c.profile_get().prefs.head_tracking, "Beto no hereda lo de Ana");
        c.accounts.select(&ana).unwrap();
        assert!(!c.profile_get().prefs.head_tracking);
        c.profile_reset().unwrap();
        assert!(c.profile_get().prefs.head_tracking);
        assert!(c.store.profiles().unwrap().profiles.is_empty(), "un perfil de fabrica no deja rastro");
    }

    #[test]
    fn las_extras_del_pack_llegan_a_la_vista_y_sin_ellas_no_hay_nada() {
        use dino_core::pack_meta::*;
        let (c, _) = core("extras");
        let entry = LibraryEntry {
            id: "vs".into(),
            name: "Voidasio".into(),
            source: PackSource::Catalog { code: "VOIDASIO".into() },
            pack_version: "1.0.0".into(),
            server: None,
            last_played: None,
        };
        let sin = c.view(&entry, None, None, None);
        assert!(sin.theme.is_none() && sin.notes.is_none() && sin.notes_version.is_none());
        // lo que el catalogo publico se guarda y sirve sin red
        let extras = PackExtras {
            theme: Some(Theme {
                enabled: true,
                colors: ThemeColors { primary: Some("#7a3cff".into()), ..Default::default() },
                ..Default::default()
            }),
            notes: Some(PatchNotes {
                title: Some("Novedades".into()),
                summary: Some("hola".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        c.store.save_pack_extras("vs", &extras, "1.1.0");
        let v = c.view(&entry, None, None, None);
        assert_eq!(v.theme.unwrap().colors.primary.as_deref(), Some("#7a3cff"));
        assert_eq!(v.notes.unwrap().title.as_deref(), Some("Novedades"));
        assert_eq!(v.notes_version.as_deref(), Some("1.1.0"));
        // un pack que deja de publicar extras no arrastra los viejos
        c.store.save_pack_extras("vs", &PackExtras::default(), "1.2.0");
        assert!(c.view(&entry, None, None, None).theme.is_none());
    }

    #[test]
    fn cancelar_un_login_que_no_existe_no_hace_nada_y_salir_no_espera() {
        let (c, _) = core("login");
        c.account_microsoft_cancel();
        let t = Instant::now();
        c.shutdown();
        assert!(t.elapsed() < Duration::from_secs(1));
        // con un intento registrado, `shutdown` lo cancela y espera a que acabe
        let slot = Arc::new(LoginSlot::default());
        *c.login.lock().unwrap() = Some(slot.clone());
        let s2 = slot.clone();
        let h = std::thread::spawn(move || {
            let _d = LoginDone(s2.clone());
            while !s2.cancel.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        c.shutdown();
        assert!(slot.done.load(Ordering::SeqCst));
        h.join().unwrap();
    }
}
