//! Dino Launcher 2.0: la app. Envuelve `dino_instances::Core` con los comandos y eventos del
//! contrato (`v2/docs/contratos.md`); aquí solo vive lo que es de Tauri:
//! - la ventana sin bordes y su vidrio nativo;
//! - el canal de eventos `dino://event`;
//! - el protocolo `dino-icon` que sirve los iconos de los packs;
//! - cerrar el WebView mientras se juega y recrearlo al terminar (etapa 2: de ~250 MB a ~11 MB);
//! - instancia única, links de invitación (`dinolauncher://join/CODIGO`) y actualizaciones.

use dino_core::api::{
    AccountView, AppInfo, InstanceDetails, InstanceSettings, InstanceView, PackPreview, PlayOutcome, PlayRequest,
    ServerStatus,
};
use dino_core::events::{Event, SessionState, CHANNEL};
use dino_core::schema::{JavaChoice, Memory, Paths, UpdateChannel};
use dino_core::{DinoError, ErrorCode};
use dino_instances::{admin, catalog, Core};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State, Window};

type R<T> = Result<T, DinoError>;

fn internal(x: impl std::fmt::Display) -> DinoError {
    DinoError::new(ErrorCode::Internal, x.to_string())
}

struct Glass(bool);
#[derive(Default)]
struct PendingInvite(Mutex<Option<String>>);
struct UpdaterReady;
/// Mantiene vivo el escritor del registro.
struct LogGuard(#[allow(dead_code)] Option<dino_log::Guard>);

// ------------------------------------------------------------ ventana

#[tauri::command]
fn window_minimize(window: Window) -> R<()> {
    window.minimize().map_err(internal)
}

/// Devuelve el estado **nuevo** (maximizada o no) para que el icono no mienta.
#[tauri::command]
fn window_toggle_maximize(window: Window) -> R<bool> {
    let m = window.is_maximized().map_err(internal)?;
    if m {
        window.unmaximize().map_err(internal)?;
    } else {
        window.maximize().map_err(internal)?;
    }
    Ok(!m)
}

#[tauri::command]
fn window_is_maximized(window: Window) -> R<bool> {
    window.is_maximized().map_err(internal)
}

#[tauri::command]
fn window_close(window: Window) -> R<()> {
    window.close().map_err(internal)
}

#[tauri::command]
fn app_info(app: tauri::AppHandle, core: State<'_, Arc<Core>>, glass: State<'_, Glass>) -> AppInfo {
    AppInfo {
        version: app.package_info().version.to_string(),
        data_dir: core.paths.root.to_string_lossy().into_owned(),
        glass: glass.0,
        platform: std::env::consts::OS.into(),
        session: core.session_snapshot(),
    }
}

// ------------------------------------------------------------ biblioteca

#[tauri::command(rename_all = "snake_case")]
async fn library_list(core: State<'_, Arc<Core>>) -> R<Vec<InstanceView>> {
    core.library_list().await
}

#[tauri::command(rename_all = "snake_case")]
fn library_select(core: State<'_, Arc<Core>>, id: String) -> R<()> {
    core.library_select(&id)
}

#[tauri::command(rename_all = "snake_case")]
fn library_remove(core: State<'_, Arc<Core>>, id: String, delete_files: bool) -> R<()> {
    core.library_remove(&id, delete_files)
}

#[tauri::command(rename_all = "snake_case")]
fn instance_details(core: State<'_, Arc<Core>>, id: String) -> R<InstanceDetails> {
    core.instance_details(&id)
}

#[tauri::command(rename_all = "snake_case")]
fn instance_settings_get(core: State<'_, Arc<Core>>, id: String) -> R<InstanceSettings> {
    core.instance_settings_get(&id)
}

#[tauri::command(rename_all = "snake_case")]
fn instance_settings_set(
    core: State<'_, Arc<Core>>,
    id: String,
    memory: Memory,
    jvm_args: Vec<String>,
    java: JavaChoice,
) -> R<()> {
    core.instance_settings_set(&id, memory, jvm_args, java)
}

#[tauri::command(rename_all = "snake_case")]
async fn instance_verify(core: State<'_, Arc<Core>>, id: String) -> R<()> {
    core.instance_verify(&id).await
}

#[tauri::command(rename_all = "snake_case")]
async fn catalog_lookup(core: State<'_, Arc<Core>>, code: String) -> R<PackPreview> {
    core.catalog_lookup(&code).await
}

#[tauri::command(rename_all = "snake_case")]
async fn catalog_add(core: State<'_, Arc<Core>>, code: String) -> R<InstanceView> {
    core.catalog_add(&code).await
}

#[tauri::command(rename_all = "snake_case")]
fn mrpack_inspect(core: State<'_, Arc<Core>>, path: String) -> R<PackPreview> {
    core.mrpack_inspect(std::path::Path::new(&path))
}

#[tauri::command(rename_all = "snake_case")]
async fn mrpack_import(core: State<'_, Arc<Core>>, path: String, server: Option<String>) -> R<InstanceView> {
    core.mrpack_import(std::path::Path::new(&path), server).await
}

// ------------------------------------------------------------ jugar

#[tauri::command(rename_all = "snake_case")]
async fn play(core: State<'_, Arc<Core>>, request: PlayRequest) -> R<PlayOutcome> {
    core.inner().play(request).await
}

#[tauri::command]
fn play_cancel(core: State<'_, Arc<Core>>) {
    core.play_cancel()
}

#[tauri::command]
async fn game_stop(core: State<'_, Arc<Core>>) -> R<()> {
    core.game_stop().await
}

#[tauri::command(rename_all = "snake_case")]
async fn server_status(core: State<'_, Arc<Core>>, addr: String) -> R<ServerStatus> {
    Ok(core.server_status(&addr).await)
}

/// Abre una carpeta que el núcleo conoce. Ya no acepta rutas arbitrarias.
#[tauri::command(rename_all = "snake_case")]
fn open_folder(core: State<'_, Arc<Core>>, target: String, id: Option<String>) -> R<()> {
    let p: PathBuf = match (target.as_str(), id) {
        ("instance", Some(id)) => core.paths.game_dir(&id),
        ("logs", _) => core.paths.logs(),
        ("data", _) => core.paths.root.clone(),
        _ => return Err(DinoError::new(ErrorCode::Internal, format!("carpeta desconocida: {target}"))),
    };
    std::fs::create_dir_all(&p).map_err(|x| DinoError::new(ErrorCode::DiskPermission, x.to_string()))?;
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let opener = "xdg-open";
    std::process::Command::new(opener).arg(&p).spawn().map_err(internal)?;
    Ok(())
}

// ------------------------------------------------------------ cuentas

#[tauri::command]
fn accounts_list(core: State<'_, Arc<Core>>) -> R<Vec<AccountView>> {
    core.accounts_list()
}

#[tauri::command(rename_all = "snake_case")]
fn account_select(core: State<'_, Arc<Core>>, uuid: String) -> R<()> {
    core.account_select(&uuid)
}

#[tauri::command(rename_all = "snake_case")]
fn account_remove(core: State<'_, Arc<Core>>, uuid: String) -> R<()> {
    core.account_remove(&uuid)
}

#[tauri::command(rename_all = "snake_case")]
fn account_offline_check(core: State<'_, Arc<Core>>, name: String) -> Option<String> {
    core.account_offline_check(&name)
}

#[tauri::command(rename_all = "snake_case")]
fn account_offline_create(core: State<'_, Arc<Core>>, name: String) -> R<AccountView> {
    core.account_offline_create(&name)
}

#[tauri::command]
async fn account_microsoft_add(core: State<'_, Arc<Core>>) -> R<()> {
    core.inner().account_microsoft_add().await
}

// ------------------------------------------------------------ invitaciones, admin, diagnóstico

#[tauri::command]
fn invite_pending(st: State<'_, PendingInvite>) -> Option<String> {
    st.0.lock().ok().and_then(|mut g| g.take())
}

#[tauri::command]
fn admin_unlocked(core: State<'_, Arc<Core>>) -> bool {
    core.store.config().is_ok_and(|c| c.admin_unlocked) || admin::desbloqueado(&core.paths.root)
}

#[tauri::command(rename_all = "snake_case")]
fn admin_unlock(core: State<'_, Arc<Core>>, texto: String) -> R<bool> {
    if !admin::termina_en_la_clave(&texto) {
        return Ok(false);
    }
    core.store.update_config(|c| c.admin_unlocked = true).map_err(internal)?;
    Ok(true)
}

#[tauri::command(rename_all = "snake_case")]
fn admin_read(path: String) -> R<admin::Borrador> {
    admin::leer_mrpack(std::path::Path::new(&path)).map_err(internal)
}

#[tauri::command]
async fn admin_published() -> R<Vec<catalog::PackRemoto>> {
    tauri::async_runtime::spawn_blocking(|| admin::publicados().map_err(internal)).await.map_err(internal)?
}

#[tauri::command(rename_all = "snake_case")]
async fn admin_publish(draft: admin::Borrador, path: String) -> R<catalog::PackRemoto> {
    tauri::async_runtime::spawn_blocking(move || admin::publicar(&draft, std::path::Path::new(&path)).map_err(internal))
        .await
        .map_err(internal)?
}

#[tauri::command]
fn diagnostics_export(core: State<'_, Arc<Core>>) -> R<String> {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let out = core.paths.root.join(format!("diagnostico-{secs}.zip"));
    dino_log::export_report(&core.paths, &out).map_err(internal)?;
    Ok(out.to_string_lossy().into_owned())
}

// ------------------------------------------------------------ actualizaciones

/// Manifiesto del canal beta. No puede ir como asset de `releases/latest`, que GitHub
/// reserva a la última versión estable: vive en la rama del repo de descargas.
#[cfg(desktop)]
const BETA_FEED: &str = "https://raw.githubusercontent.com/dinopolllo/dino-launcher-releases/main/latest-beta.json";

/// Canal efectivo. Una beta sigue siempre el canal beta aunque la configuración diga
/// estable (la recién migrada de 1.x lo dice): si no, nunca vería la beta siguiente.
fn effective_channel(chosen: UpdateChannel, version: &str) -> UpdateChannel {
    if version.contains('-') {
        UpdateChannel::Beta
    } else {
        chosen
    }
}

/// El updater con el manifiesto de su canal, o `None` si esta compilación no trae
/// actualizaciones (desarrollo, o una release sin firmar). `DINO_UPDATE_FEED` cambia el
/// manifiesto para pruebas; la firma se sigue comprobando con la clave compilada.
#[cfg(desktop)]
fn updater(app: &tauri::AppHandle) -> Result<Option<tauri_plugin_updater::Updater>, DinoError> {
    use tauri_plugin_updater::UpdaterExt;
    if app.try_state::<UpdaterReady>().is_none() {
        return Ok(None);
    }
    let fail = |x: &dyn std::fmt::Display| DinoError::new(ErrorCode::UpdateFailed, x.to_string());
    let chosen = app.state::<Arc<Core>>().store.config().map(|c| c.update_channel).unwrap_or_default();
    let feed = match std::env::var("DINO_UPDATE_FEED") {
        Ok(f) => Some(f),
        Err(_) => (effective_channel(chosen, &app.package_info().version.to_string()) == UpdateChannel::Beta)
            .then(|| BETA_FEED.to_string()),
    };
    let mut b = app.updater_builder();
    if let Some(f) = feed {
        let url = tauri::Url::parse(&f).map_err(|x| fail(&x))?;
        b = b.endpoints(vec![url]).map_err(|x| fail(&x))?;
    }
    b.build().map(Some).map_err(|x| fail(&x))
}

#[tauri::command]
async fn update_check(app: tauri::AppHandle) -> R<Option<serde_json::Value>> {
    #[cfg(desktop)]
    {
        let Ok(Some(updater)) = updater(&app) else { return Ok(None) };
        return Ok(match updater.check().await {
            Ok(Some(u)) => {
                let _ = app.emit(CHANNEL, Event::UpdateAvailable { version: u.version.clone(), notes: u.body.clone() });
                Some(serde_json::json!({"version": u.version, "actual": u.current_version, "notas": u.body}))
            }
            _ => None,
        });
    }
    #[allow(unreachable_code)]
    {
        let _ = app;
        Ok(None)
    }
}

#[tauri::command]
async fn update_install(app: tauri::AppHandle) -> R<()> {
    #[cfg(desktop)]
    {
        let Some(up) = updater(&app)? else {
            return Err(DinoError::new(ErrorCode::UpdateFailed, "esta versión no trae actualizaciones automáticas"));
        };
        let Some(update) = up.check().await.map_err(|x| DinoError::new(ErrorCode::UpdateFailed, x.to_string()))? else {
            return Err(DinoError::new(ErrorCode::UpdateFailed, "ya no hay ninguna versión nueva"));
        };
        let mut done: u64 = 0;
        update
            .download_and_install(
                |chunk, total| {
                    done += chunk as u64;
                    let frac = total.filter(|t| *t > 0).map_or(-1.0, |t| done as f32 / t as f32);
                    let _ = app.emit("update", serde_json::json!({"paso": "Descargando", "frac": frac}));
                },
                || {
                    let _ = app.emit("update", serde_json::json!({"paso": "Instalando", "frac": 1.0}));
                },
            )
            .await
            .map_err(|x| DinoError::new(ErrorCode::UpdateFailed, x.to_string()))?;
        app.restart();
    }
    #[allow(unreachable_code)]
    {
        let _ = app;
        Err(DinoError::new(ErrorCode::UpdateFailed, "esta plataforma no tiene actualizaciones"))
    }
}

// ------------------------------------------------------------ ventana y vidrio

/// Vidrio nativo detrás de la interfaz (ver 1.x: el radio tiene que coincidir con el CSS).
fn apply_glass(window: &tauri::WebviewWindow) -> bool {
    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
        apply_vibrancy(window, NSVisualEffectMaterial::HudWindow, Some(NSVisualEffectState::Active), Some(26.0)).is_ok()
    }
    #[cfg(target_os = "windows")]
    {
        window_vibrancy::apply_acrylic(window, Some((3, 6, 10, 180))).is_ok()
            || window_vibrancy::apply_mica(window, Some(true)).is_ok()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = window;
        false
    }
}

/// Recrea la ventana principal tal como la describe la configuración.
fn open_main_window(app: &tauri::AppHandle) {
    if app.get_webview_window("main").is_some() {
        return;
    }
    let Some(conf) = app.config().app.windows.first().cloned() else { return };
    match tauri::WebviewWindowBuilder::from_config(app, &conf).and_then(|b| b.build()) {
        Ok(w) => {
            apply_glass(&w);
            tracing::info!("interfaz reabierta");
        }
        Err(x) => tracing::error!(error = %x, "no pude reabrir la ventana"),
    }
}

fn focus_main(app: &tauri::AppHandle) {
    match app.get_webview_window("main") {
        Some(w) => {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
        None => open_main_window(app),
    }
}

fn receive_invite(app: &tauri::AppHandle, urls: &[String]) {
    let Some(code) = urls.iter().find_map(|u| catalog::codigo_de_invitacion(u)) else { return };
    tracing::info!(%code, "invitación");
    if let Some(st) = app.try_state::<PendingInvite>() {
        if let Ok(mut g) = st.0.lock() {
            *g = Some(code.clone());
        }
    }
    let _ = app.emit(CHANNEL, Event::Invite { code });
    focus_main(app);
}

fn data_root(app: &tauri::AppHandle) -> PathBuf {
    std::env::var_os("DINO_HOME")
        .map(PathBuf::from)
        .or_else(|| app.path().app_data_dir().ok())
        .expect("sin carpeta de datos")
}

// ------------------------------------------------------------ arranque

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| focus_main(app)))
        .plugin(tauri_plugin_deep_link::init());

    let app = builder
        .manage(PendingInvite::default())
        .register_asynchronous_uri_scheme_protocol(dino_core::api::ICON_SCHEME, |ctx, req, responder| {
            // `dino-icon://localhost/<id>` o `http://dino-icon.localhost/<id>`: el id es el último tramo.
            let id = req.uri().path().trim_matches('/').to_string();
            let file = ctx.app_handle().try_state::<Arc<Core>>().map(|c| c.paths.pack_icon(&id));
            let body = file.filter(|_| dino_core::schema::valid_id(&id)).and_then(|f| std::fs::read(f).ok());
            let resp = match body {
                Some(b) => tauri::http::Response::builder().header("Content-Type", mime_of(&b)).body(b),
                None => tauri::http::Response::builder().status(404).body(Vec::new()),
            };
            responder.respond(resp.expect("respuesta"));
        })
        .setup(|app| {
            let paths = Paths::new(data_root(app.handle()));
            let guard = dino_log::init(&paths, false).ok();
            app.manage(LogGuard(guard));

            #[cfg(desktop)]
            match app.handle().plugin(tauri_plugin_updater::Builder::new().build()) {
                Ok(()) => {
                    app.manage(UpdaterReady);
                }
                Err(x) => tracing::info!(error = %x, "sin actualizaciones automáticas"),
            }

            let window = app.get_webview_window("main").expect("falta la ventana main");
            app.manage(Glass(apply_glass(&window)));

            // Los eventos del núcleo van a la interfaz; los de la partida además deciden si
            // la ventana sobra (se juega) o vuelve (se acabó la partida).
            let handle = app.handle().clone();
            let emit: dino_instances::Emit = Arc::new(move |ev: Event| {
                if let Event::Session(s) = &ev {
                    let close = handle
                        .try_state::<Arc<Core>>()
                        .and_then(|c| c.store.config().ok())
                        .is_none_or(|c| c.close_ui_while_playing);
                    match s.state {
                        SessionState::Playing if close => {
                            if let Some(w) = handle.get_webview_window("main") {
                                tracing::info!("partida abierta: cierro la interfaz para liberar memoria");
                                let _ = w.destroy();
                            }
                        }
                        SessionState::Idle => {
                            let h = handle.clone();
                            let _ = handle.run_on_main_thread(move || open_main_window(&h));
                        }
                        _ => {}
                    }
                }
                let _ = handle.emit(CHANNEL, ev);
            });
            let core = Core::new(paths, emit);
            // Primera apertura sobre los datos de 1.x: se migran antes de enseñar nada.
            match core.migrate_if_needed() {
                Ok(Some(notes)) => tracing::info!(?notes, "migración desde 1.x hecha"),
                Ok(None) => {}
                Err(x) => tracing::error!(error = ?x, "la migración desde 1.x falló; los datos de 1.x siguen intactos"),
            }
            if let Some(id) = core.reattach() {
                tracing::info!(instance = %id, "partida ya en curso al abrir: la sigo");
            } else {
                let c = core.clone();
                std::thread::spawn(move || c.sweep_strays());
            }
            app.manage(core);

            #[cfg(desktop)]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                #[cfg(debug_assertions)]
                let _ = app.deep_link().register_all();
                if let Ok(Some(urls)) = app.deep_link().get_current() {
                    let urls: Vec<String> = urls.iter().map(|u| u.to_string()).collect();
                    receive_invite(app.handle(), &urls);
                }
                let h = app.handle().clone();
                app.deep_link().on_open_url(move |ev| {
                    let urls: Vec<String> = ev.urls().iter().map(|u| u.to_string()).collect();
                    receive_invite(&h, &urls);
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            window_minimize,
            window_toggle_maximize,
            window_is_maximized,
            window_close,
            library_list,
            library_select,
            library_remove,
            instance_details,
            instance_settings_get,
            instance_settings_set,
            instance_verify,
            catalog_lookup,
            catalog_add,
            mrpack_inspect,
            mrpack_import,
            play,
            play_cancel,
            game_stop,
            server_status,
            open_folder,
            accounts_list,
            account_select,
            account_remove,
            account_offline_check,
            account_offline_create,
            account_microsoft_add,
            invite_pending,
            admin_unlocked,
            admin_unlock,
            admin_read,
            admin_published,
            admin_publish,
            diagnostics_export,
            update_check,
            update_install,
        ])
        .build(tauri::generate_context!())
        .expect("Dino Launcher no pudo arrancar");

    app.run(|handle, event| {
        // Con la interfaz cerrada para jugar, que la app no se vaya: sigue la partida.
        if let tauri::RunEvent::ExitRequested { api, code: None, .. } = event {
            if handle.try_state::<Arc<Core>>().is_some_and(|c| dino_session_running(&c)) {
                api.prevent_exit();
            }
        }
    });
}

fn dino_session_running(core: &Core) -> bool {
    dino_instances::is_playing(&core.paths)
}

fn mime_of(b: &[u8]) -> &'static str {
    if b.starts_with(b"\x89PNG") {
        "image/png"
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if b.len() >= 12 && &b[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn una_beta_sigue_el_canal_beta() {
        assert_eq!(effective_channel(UpdateChannel::Stable, "2.0.0-beta.1"), UpdateChannel::Beta);
        assert_eq!(effective_channel(UpdateChannel::Stable, "2.0.0"), UpdateChannel::Stable);
        assert_eq!(effective_channel(UpdateChannel::Beta, "2.0.0"), UpdateChannel::Beta);
    }
}
