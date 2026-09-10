pub mod auth;
pub mod engine;
pub mod install;
pub mod pack;
pub mod status;

use tauri::{Emitter, Manager, Window};

/// La ventana no tiene decoración del sistema: los controles los dibuja la interfaz.
#[tauri::command]
fn window_minimize(window: Window) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

/// Devuelve el estado **nuevo**, no el viejo.
///
/// Windows dibuja dos iconos distintos: un cuadro para maximizar y dos superpuestos
/// para restaurar. Una ventana maximizada que sigue mostrando el cuadro es de las
/// cosas que delatan al instante que la barra de título no es del sistema.
#[tauri::command]
fn window_toggle_maximize(window: Window) -> Result<bool, String> {
    let maximized = window.is_maximized().map_err(|e| e.to_string())?;
    if maximized {
        window.unmaximize().map_err(|e| e.to_string())?;
    } else {
        window.maximize().map_err(|e| e.to_string())?;
    }
    Ok(!maximized)
}

/// El estado al arrancar, y tras un maximizado que no salió de nuestro botón:
/// arrastrar la ventana contra el borde superior también la maximiza.
#[tauri::command]
fn window_is_maximized(window: Window) -> Result<bool, String> {
    window.is_maximized().map_err(|e| e.to_string())
}

#[tauri::command]
fn window_close(window: Window) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

/// Diagnóstico: la interfaz reporta lo que hace, a consola y a un archivo.
///
/// El archivo importa: la app normal no se lanza desde una terminal, así que sin
/// esto no hay forma de saber qué pasó cuando algo falla en la máquina de alguien.
#[tauri::command]
fn boot_ping(app: tauri::AppHandle, msg: String) {
    println!("[BOOT] {msg}");
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let line = format!(
            "{:?} {msg}\n",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("dino.log"))
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}


/// Versión del motor que fijamos. Subirla es una decisión, no un accidente.
///
/// El formato de `accounts.json` que escribe `auth.rs` está verificado contra esta
/// versión concreta. Si se sube, hay que volver a comprobarlo.
const ENGINE_VERSION: &str = "11.1.0";

/// ¿Hay alguna cuenta registrada en el motor?
///
/// Sin cuenta, `--launch` no puede autenticar. La primera vez abrimos el motor
/// con su ventana para que el usuario inicie sesión; después ya nunca se ve.
fn has_account(layout: &engine::Layout) -> bool {
    let Ok(txt) = std::fs::read_to_string(layout.accounts()) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&txt)
        .ok()
        .and_then(|v| v.get("accounts").and_then(|a| a.as_array()).map(|a| !a.is_empty()))
        .unwrap_or(false)
}

#[derive(serde::Deserialize)]
pub struct PlayArgs {
    /// Ruta a un `.mrpack` local. Provisional: saldrá del catálogo.
    pub mrpack: String,
    pub instance_id: String,
    pub name: String,
    pub version: String,
    pub server: Option<String>,
    pub max_mem: u32,
    pub jvm_args: String,
    /// Perfil del motor. Si falta, usamos el primero que haya.
    pub profile: Option<String>,
    /// Instalar y verificar, sin abrir el juego.
    #[serde(default)]
    pub install_only: bool,
}

/// Deja todo listo y lanza Minecraft.
#[tauri::command]
async fn play(app: tauri::AppHandle, args: PlayArgs) -> Result<String, String> {
    let root = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no encuentro la carpeta de datos: {e}"))?;
    let layout = engine::Layout::new(root);

    let emit = |paso: &str, frac: f32| {
        let _ = app.emit("install", serde_json::json!({ "paso": paso, "frac": frac }));
    };

    // Restos de la sesión anterior: el Java del crash-assistant que se queda en el
    // Dock, y el motor que a veces no muere y bloquea el siguiente lanzamiento.
    let limpiados = install::kill_orphans(&layout.root);
    if limpiados > 0 {
        println!("[BOOT] limpiados {limpiados} procesos huérfanos");
    }

    let bin = install::ensure_engine(&layout, ENGINE_VERSION, &emit)
        .await
        .map_err(|e| e.to_string())?;

    // max_mem = 0 significa "decídelo tú según esta máquina".
    let (min_mem, max_mem) = if args.max_mem == 0 {
        engine::recommended_memory(engine::total_memory_mb().unwrap_or(8192))
    } else {
        (2048.min(args.max_mem), args.max_mem)
    };

    let settings = engine::PackSettings {
        name: args.name.clone(),
        pack_id: args.instance_id.clone(),
        version: args.version.clone(),
        min_mem,
        max_mem,
        // Un flag experimental sin desbloquear impide que la JVM arranque siquiera.
        jvm_args: engine::normalize_jvm_args(&args.jvm_args),
        server: args.server.clone(),
    };

    // El motor tiene que ser invisible: nada de consola, y que se cierre solo
    // cuando el juego arranca. `ShowConsoleOnError` viene en true de fábrica y es
    // lo que hace aparecer su ventana cuando algo falla.
    install::apply_silent_config(&layout);

    let mrpack = std::path::PathBuf::from(&args.mrpack);
    if !mrpack.exists() {
        return Err(format!("no encuentro el modpack en {}", mrpack.display()));
    }
    install::install_pack(&layout, &args.instance_id, &mrpack, &settings, &emit)
        .await
        .map_err(|e| e.to_string())?;

    if args.install_only {
        emit("Listo", 1.0);
        return Ok("installed".into());
    }

    // Primera vez: hace falta iniciar sesión. El motor NO se abre desde aquí: quien
    // dirige eso es el setup, que primero pregunta qué cuenta tiene el usuario. Abrirlo
    // a ciegas era lo que le plantaba delante un launcher ajeno sin haberlo pedido.
    if !has_account(&layout) {
        emit("Falta iniciar sesión", 1.0);
        return Ok("login".into());
    }

    let account = match &args.profile {
        Some(p) => engine::Account::Profile(p.clone()),
        None => engine::Account::Profile(String::new()), // el motor usa la cuenta activa
    };
    let mut launch = engine::launch_args(
        &layout,
        &args.instance_id,
        &account,
        args.server.as_deref(),
    );
    // Un perfil vacío confundiría al motor; mejor no pasar la bandera.
    if matches!(&account, engine::Account::Profile(p) if p.is_empty()) {
        if let Some(i) = launch.iter().position(|a| a == "--profile") {
            launch.drain(i..=i + 1);
        }
    }

    // Hay cuenta: el motor vuelve a ser invisible antes de jugar.
    install::set_dock_visibility(&layout.engine_app(), true);

    emit("Abriendo Minecraft", 1.0);
    std::process::Command::new(&bin)
        .args(&launch)
        .spawn()
        .map_err(|e| format!("no pude lanzar el juego: {e}"))?;
    Ok("launched".into())
}

/// Abre el motor para iniciar sesión con Microsoft.
///
/// Microsoft no se puede resolver aquí dentro: su flujo necesita un `client_id` de
/// Azure con acceso a la API de Minecraft aprobado por Mojang, que es un trámite
/// externo. Pero sí se puede evitar que aparezca el launcher ajeno completo: con
/// `--launch` y sin `--show-window`, el motor va directo a su página de Cuentas.
///
/// Devuelve `"direct"` si se pudo hacer así, o `"full"` si hubo que caer a la ventana
/// principal por no haber instancia todavía. La interfaz redacta el aviso según cuál.
///
/// Si alguien cierra el login sin añadir cuenta, el motor no se detiene ahí: pregunta
/// «Play demo?» y, si le dicen que no —es el botón por defecto—, se va. La interfaz
/// ofrece cancelar desde nuestra ventana, que mata el motor antes de ese rodeo.
#[tauri::command]
async fn open_engine_login(
    app: tauri::AppHandle,
    instance: Option<String>,
) -> Result<String, String> {
    let layout = layout_of(&app)?;
    let emit = |paso: &str, frac: f32| {
        let _ = app.emit("install", serde_json::json!({ "paso": paso, "frac": frac }));
    };
    let bin = install::ensure_engine(&layout, ENGINE_VERSION, &emit)
        .await
        .map_err(|e| e.to_string())?;
    install::apply_silent_config(&layout);
    // Para el login sí queremos que se vea y tenga foco: una ventana de app agente
    // sale sin foco en macOS y no se podría escribir en ella.
    install::set_dock_visibility(&layout.engine_app(), false);

    // `--launch` solo vale si la instancia existe de verdad en disco.
    let id = instance.filter(|id| layout.instance(id).exists());
    let modo = if id.is_some() { "direct" } else { "full" };
    let args = engine::login_args(&layout, id.as_deref());

    std::process::Command::new(&bin)
        .args(&args)
        .spawn()
        .map_err(|e| format!("no pude abrir el motor: {e}"))?;
    Ok(modo.into())
}

/// Ventanas visibles del motor, para saber si cancelaron el login.
///
/// `null` = este sistema no sabe contarlas; la interfaz debe seguir esperando en vez
/// de dar por hecho que no hay ninguna.
#[tauri::command]
fn engine_windows(app: tauri::AppHandle) -> Option<usize> {
    let layout = layout_of(&app).ok()?;
    install::engine_windows(&layout.root)
}

/// Cierra la ventana del motor cuando ya cumplió su papel.
#[tauri::command]
fn close_engine(app: tauri::AppHandle) -> Result<usize, String> {
    let layout = layout_of(&app)?;
    Ok(install::close_engine(&layout.root))
}

/// Marca de que el updater llegó a registrarse.
///
/// Hace falta porque `app.updater()` **entra en pánico** —no devuelve error— cuando el
/// plugin no está gestionado, y no lo está siempre que falte la configuración del
/// updater: en desarrollo, y en cualquier release compilada sin firma. Preguntar por
/// esta marca antes es lo que evita tumbar la app al arrancar.
struct UpdaterListo;

/// ¿Hay una versión nueva del launcher?
///
/// Devuelve `null` cuando no hay nada que instalar **y también cuando el updater no
/// está configurado** (sin `pubkey` ni `endpoints`). Se tratan igual a propósito: un
/// launcher sin actualizaciones configuradas debe verse normal, no enseñar un error
/// que su usuario no puede resolver.
#[tauri::command]
async fn update_check(app: tauri::AppHandle) -> Result<Option<serde_json::Value>, String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_updater::UpdaterExt;
        if app.try_state::<UpdaterListo>().is_none() {
            return Ok(None); // sin configurar: no hay actualizaciones y no es un fallo
        }
        let Ok(updater) = app.updater() else {
            return Ok(None);
        };
        return match updater.check().await {
            Ok(Some(u)) => Ok(Some(serde_json::json!({
                "version": u.version,
                "actual": u.current_version,
                "notas": u.body,
            }))),
            Ok(None) => Ok(None),
            // Quedarse sin internet no es un fallo del launcher: se calla y sigue.
            Err(e) => {
                println!("[UPDATE] no se pudo comprobar: {e}");
                Ok(None)
            }
        };
    }
    #[allow(unreachable_code)]
    {
        let _ = app;
        Ok(None)
    }
}

/// Descarga e instala la versión nueva.
///
/// En Windows el instalador NSIS **cierra el launcher** para poder reemplazarlo, así
/// que esta llamada normalmente no regresa: lo último que ve la interfaz es el
/// progreso. Por eso el avance se emite por evento y no como valor de retorno.
#[tauri::command]
async fn update_install(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_updater::UpdaterExt;
        if app.try_state::<UpdaterListo>().is_none() {
            return Err("esta versión no trae actualizaciones automáticas".into());
        }
        let updater = app.updater().map_err(|e| e.to_string())?;
        let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
            return Err("ya no hay ninguna versión nueva".into());
        };

        let emit = |paso: &str, frac: f32| {
            let _ = app.emit("update", serde_json::json!({ "paso": paso, "frac": frac }));
        };

        // `content_length` es opcional: hay servidores que no lo mandan. Sin él se
        // informa del avance sin porcentaje en vez de inventar uno.
        let mut bajado: u64 = 0;
        let mut total: Option<u64> = None;
        update
            .download_and_install(
                |trozo, largo| {
                    bajado += trozo as u64;
                    total = largo;
                    let frac = total.filter(|t| *t > 0).map_or(-1.0, |t| bajado as f32 / t as f32);
                    let _ = app.emit(
                        "update",
                        serde_json::json!({ "paso": "Descargando", "frac": frac }),
                    );
                },
                || emit("Instalando", 1.0),
            )
            .await
            .map_err(|e| e.to_string())?;

        // En macOS y Linux el proceso sigue vivo tras instalar y hay que reiniciarlo
        // a mano. En Windows no se llega aquí: el instalador ya cerró la app.
        app.restart();
    }
    #[allow(unreachable_code)]
    {
        let _ = app;
        Err("esta plataforma no tiene actualizaciones".into())
    }
}

/// Crea una cuenta sin conexión con el nombre dado y la deja activa.
///
/// Es la rama de «no tengo Minecraft». No autentica contra nadie: el nombre y el UUID
/// que se deriva de él son toda la identidad, así que se puede hacer aquí dentro y el
/// usuario no llega a ver el motor.
///
/// Devuelve el nombre ya limpio de espacios, que es el que quedó guardado.
#[tauri::command]
fn offline_create(app: tauri::AppHandle, name: String) -> Result<String, String> {
    let layout = layout_of(&app)?;
    let cuenta = auth::offline_account(&name).map_err(|e| e.to_string())?;
    let guardado = cuenta["profile"]["name"].as_str().unwrap_or("").to_string();
    auth::save_account(&layout, cuenta).map_err(|e| e.to_string())?;
    Ok(guardado)
}

/// Comprueba el nombre sin crear nada, para avisar mientras se escribe.
///
/// Devuelve `null` si vale, o el motivo si no. Enterarse al teclear es mucho mejor
/// que enterarse cuando el servidor rechaza la conexión.
#[tauri::command]
fn offline_check(name: String) -> Option<String> {
    auth::validar_nombre(&name).err().map(|e| e.to_string())
}

/// Tamaño en disco de una carpeta, en bytes./// Tamaño en disco de una carpeta, en bytes.
fn dir_size(p: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

fn layout_of(app: &tauri::AppHandle) -> Result<engine::Layout, String> {
    app.path()
        .app_data_dir()
        .map(engine::Layout::new)
        .map_err(|e| format!("no encuentro la carpeta de datos: {e}"))
}

/// El modpack que este launcher tiene instalado.
///
/// Dino Launcher es de un solo pack a propósito: es lo que hace que un amigo
/// abra y juegue. Este archivo es la única fuente de verdad de cuál es.
#[tauri::command]
fn get_pack(app: tauri::AppHandle) -> serde_json::Value {
    let Ok(layout) = layout_of(&app) else { return serde_json::json!(null) };
    std::fs::read_to_string(layout.root.join("pack.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::json!(null))
}

#[tauri::command]
fn set_pack(app: tauri::AppHandle, pack: serde_json::Value) -> Result<(), String> {
    let layout = layout_of(&app)?;
    std::fs::create_dir_all(&layout.root).map_err(|e| e.to_string())?;
    std::fs::write(
        layout.root.join("pack.json"),
        serde_json::to_string_pretty(&pack).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

/// La cuenta con la que se va a jugar, leída de `accounts.json` del motor.
#[tauri::command]
fn active_account(app: tauri::AppHandle) -> serde_json::Value {
    let Ok(layout) = layout_of(&app) else { return serde_json::json!(null) };
    let Ok(txt) = std::fs::read_to_string(layout.accounts()) else {
        return serde_json::json!(null);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return serde_json::json!(null);
    };
    let empty = vec![];
    let accounts = v["accounts"].as_array().unwrap_or(&empty);
    // La activa manda; si ninguna lo está, la primera sirve para mostrar algo.
    let chosen = accounts
        .iter()
        .find(|a| a["active"].as_bool() == Some(true))
        .or_else(|| accounts.first());
    match chosen {
        Some(a) => serde_json::json!({
            "name": a["profile"]["name"].as_str(),
            "kind": a["type"].as_str(),
            "total": accounts.len(),
        }),
        None => serde_json::json!(null),
    }
}

/// Estado real del servidor: versión, jugadores y latencia.
#[tauri::command]
async fn server_status(addr: String) -> status::ServerStatus {
    status::ping(&addr).await
}

/// Lo que hay instalado de verdad en el disco para esta instancia.
#[tauri::command]
fn instance_info(app: tauri::AppHandle, id: String) -> Result<serde_json::Value, String> {
    let layout = layout_of(&app)?;
    let dir = layout.instance(&id);
    let mc = dir.join("minecraft");
    if !dir.exists() {
        return Ok(serde_json::json!({ "installed": false }));
    }
    let mods = std::fs::read_dir(mc.join("mods"))
        .map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "jar")).count())
        .unwrap_or(0);

    // El loader y la versión salen del mmc-pack.json que escribimos al instalar.
    let (mut minecraft, mut loader) = (None, None);
    if let Ok(txt) = std::fs::read_to_string(dir.join("mmc-pack.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            for c in v["components"].as_array().unwrap_or(&vec![]) {
                let (uid, ver) = (c["uid"].as_str().unwrap_or(""), c["version"].as_str());
                match uid {
                    "net.minecraft" => minecraft = ver.map(String::from),
                    u if u.contains("fabric") => loader = ver.map(|v| format!("Fabric {v}")),
                    u if u.contains("quilt") => loader = ver.map(|v| format!("Quilt {v}")),
                    u if u.contains("neoforged") => loader = ver.map(|v| format!("NeoForge {v}")),
                    u if u.contains("minecraftforge") => loader = ver.map(|v| format!("Forge {v}")),
                    _ => {}
                }
            }
        }
    }
    let icon = std::fs::read(layout.pack_icon(&id)).ok().map(|b| install::png_data_url(&b));

    Ok(serde_json::json!({
        "installed": true,
        "mods": mods,
        "bytes": dir_size(&mc),
        "minecraft": minecraft,
        "loader": loader,
        "icon": icon,
        "path": dir,
        "worlds": std::fs::read_dir(mc.join("saves")).map(|rd| rd.flatten().count()).unwrap_or(0),
    }))
}

/// Abre una carpeta en el explorador del sistema.
#[tauri::command]
fn open_folder(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("no existe: {path}"));
    }
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    std::process::Command::new(cmd)
        .arg(&p)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Lee un `.mrpack` sin instalarlo, para enseñar qué trae antes de confirmar.
#[tauri::command]
fn inspect_mrpack(path: String) -> Result<serde_json::Value, String> {
    install::inspect(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

/// Runtimes de Java que el motor tiene descargados.
#[tauri::command]
fn java_runtimes(app: tauri::AppHandle) -> Vec<String> {
    let Ok(layout) = layout_of(&app) else { return vec![] };
    std::fs::read_dir(layout.engine().join("java"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// RAM de la máquina y lo que recomendamos darle al juego, en MiB.
#[tauri::command]
fn memory_info() -> serde_json::Value {
    let total = engine::total_memory_mb();
    let (min, max) = engine::recommended_memory(total.unwrap_or(8192));
    serde_json::json!({ "total": total, "min": min, "max": max })
}

/// Rutas que usa la interfaz para mostrar dónde vive la instancia.
#[tauri::command]
fn app_paths(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let root = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no encuentro la carpeta de datos: {e}"))?;
    let layout = engine::Layout::new(root);
    Ok(serde_json::json!({
        "root":      layout.root,
        "engine":    layout.engine(),
        "instances": layout.instances(),
        "packs":     layout.packs(),
    }))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // El updater se registra aquí y no en la cadena del builder porque en móvil
            // no existe. Fallar al registrarlo no debe impedir que la app abra: sin él
            // simplemente no hay actualizaciones.
            #[cfg(desktop)]
            match app.handle().plugin(tauri_plugin_updater::Builder::new().build()) {
                Ok(()) => {
                    app.manage(UpdaterListo);
                }
                // Sin `plugins.updater` en la configuración el plugin no entra, y eso
                // es normal: en desarrollo y en las releases sin firma no hay updater.
                Err(e) => println!("[UPDATE] sin actualizaciones automáticas: {e}"),
            }

            let window = app.get_webview_window("main").expect("falta la ventana main");
            // El vidrio va aquí, antes de que la interfaz pinte nada: lo que la ventana
            // pide al sistema no depende de la interfaz, pero lo que la interfaz pinta sí
            // depende de si el sistema se lo dio. Por eso el resultado se guarda.
            app.manage(Glass(apply_glass(&window)));

            // Limpiar restos de la sesión anterior y dejar el motor caliente: así
            // refresca los tokens mientras el usuario mira la interfaz, y al pulsar
            // Jugar no aparece su diálogo "Please wait...".
            if let Ok(root) = app.path().app_data_dir() {
                let layout = engine::Layout::new(root);
                std::thread::spawn(move || {
                    install::kill_orphans(&layout.root);
                    install::warm_engine(&layout);
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            window_minimize,
            window_toggle_maximize,
            window_is_maximized,
            window_glass,
            window_close,
            app_paths,
            boot_ping,
            play,
            memory_info,
            get_pack,
            set_pack,
            active_account,
            server_status,
            instance_info,
            open_folder,
            inspect_mrpack,
            java_runtimes,
            open_engine_login,
            engine_windows,
            close_engine,
            offline_create,
            offline_check,
            update_check,
            update_install,
        ])
        .run(tauri::generate_context!())
        .expect("Dino Launcher no pudo arrancar");
}

/// Vidrio nativo del sistema: el desenfoque de lo que hay detrás de la ventana.
///
/// Es lo único que separa un fondo translúcido de un agujero. Sin él, bajar la
/// opacidad no enseña vidrio: enseña el escritorio en crudo, y el texto encima
/// se vuelve ilegible en cuanto hay algo claro abierto detrás.
fn apply_glass(window: &tauri::WebviewWindow) -> bool {
    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
        // El radio tiene que ser el mismo que `--r-window` del CSS. Si no coinciden,
        // el vidrio asoma por fuera de las esquinas redondeadas de la interfaz.
        apply_vibrancy(
            window,
            NSVisualEffectMaterial::HudWindow,
            Some(NSVisualEffectState::Active),
            Some(26.0),
        )
        .is_ok()
    }
    #[cfg(target_os = "windows")]
    {
        // Acrylic antes que Mica, y es una decisión, no el orden que salió.
        // Mica solo muestrea el fondo de escritorio: da una tinción, no un reflejo,
        // y con una ventana encima no cambia nada. Acrylic muestrea lo que hay detrás
        // de verdad, que es lo que describe Apple para Liquid Glass — desenfocar y
        // ajustar la luminosidad del contenido de detrás. Mica queda de respaldo
        // porque es más barata y en equipos justos es mejor eso que nada.
        //
        // El color solo lo usa el camino viejo de Windows 10; en 11 el desenfoque lo
        // compone DWM y lo ignora. Es `--ink`, para que los dos caminos tiñan igual.
        window_vibrancy::apply_acrylic(window, Some((3, 6, 10, 180))).is_ok()
            || window_vibrancy::apply_mica(window, Some(true)).is_ok()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // No hay API: depende del compositor, y no todos desenfocan. La interfaz
        // se queda opaca, que es exactamente para lo que existe el respaldo.
        let _ = window;
        false
    }
}

/// Si la ventana consiguió vidrio nativo.
///
/// Se guarda en vez de recalcularse porque solo se puede pedir una vez, al arrancar.
struct Glass(bool);

/// La interfaz lo pregunta al cargar, y solo entonces se vuelve translúcida.
///
/// Va en este sentido y no en el contrario a propósito: empezar opaca y aclararse
/// cuesta un fundido de 280ms que parece intencionado. Empezar translúcida y tener
/// que taparse deja ver el escritorio sin desenfocar, aunque sea un frame.
#[tauri::command]
fn window_glass(glass: tauri::State<Glass>) -> bool {
    glass.0
}
