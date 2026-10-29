pub mod admin;
pub mod auth;
pub mod catalog;
pub mod engine;
pub mod install;
pub mod library;
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
    /// Código del catálogo, si el pack vino de ahí. Es lo que permite mirar antes de
    /// jugar si hay una versión nueva.
    #[serde(default)]
    pub code: Option<String>,
    /// Perfil del motor. Si falta, usamos el primero que haya.
    pub profile: Option<String>,
    /// Instalar y verificar, sin abrir el juego.
    #[serde(default)]
    pub install_only: bool,
}

/// Deja todo listo y lanza Minecraft.
#[tauri::command]
async fn play(app: tauri::AppHandle, args: PlayArgs) -> Result<String, String> {
    // El estado se marca **antes** de instalar, no después de lanzar. Si no, durante
    // toda la instalación `session_state` seguía diciendo «inactiva», la interfaz
    // volvía al botón de Jugar a los pocos segundos y se podía pulsar otra vez.
    install::sesion::poner(install::sesion::PREPARANDO);
    let r = play_inner(app, args).await;
    // Solo un lanzamiento de verdad deja un hilo vigilando; en cualquier otro final
    // —error, falta de cuenta, o instalar sin jugar— hay que devolverlo a inactiva.
    if !matches!(r.as_deref(), Ok("launched")) {
        install::sesion::poner(install::sesion::INACTIVA);
    }
    r
}

async fn play_inner(app: tauri::AppHandle, args: PlayArgs) -> Result<String, String> {
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

    // Antes de jugar se mira si el pack cambió. Va aquí y no en un botón porque la
    // idea es que a un amigo no le toque hacer nada: pulsa Jugar y entra con lo
    // último.
    //
    // **Sin red se juega igual.** Un fallo al comprobar no puede impedir entrar a un
    // mundo: se registra y se sigue con lo que haya instalado.
    let mut version = args.version.clone();
    let mut mrpack = std::path::PathBuf::from(&args.mrpack);
    if let Some(code) = args.code.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        emit("Buscando actualizaciones", -1.0);
        match catalog::resolver(code).await {
            Ok(remoto) if remoto.version != version => {
                println!("[PLAY] hay versión nueva del pack: {} -> {}", version, remoto.version);
                // Con el id local, no el del catálogo: es el que dice dónde vive este
                // pack en esta máquina.
                let destino = layout.packs().join(format!("{}.mrpack", args.instance_id));
                let bajar = |bajado: u64, total: u64| {
                    let frac = if total > 0 { bajado as f32 / total as f32 } else { -1.0 };
                    let _ = app.emit(
                        "install",
                        serde_json::json!({
                            "paso": "Actualizando el modpack",
                            "frac": frac, "bajado": bajado, "total": total,
                        }),
                    );
                };
                match catalog::descargar_pack(&remoto, &destino, &bajar).await {
                    Ok(()) => {
                        // Si quien publica cambió el icono con la versión nueva, que
                        // llegue. Sin red para el icono se juega igual.
                        if let Err(e) = catalog::descargar_icono(
                            &remoto,
                            &layout.pack_icon(&args.instance_id),
                        )
                        .await
                        {
                            println!("[PLAY] no pude actualizar el icono: {e}");
                        }

                        // La biblioteca se anota la versión nueva aunque el juego no
                        // llegue a abrirse, para que la próxima vez parta de aquí. El id
                        // y el código no se tocan: son de esta máquina, no del catálogo.
                        let mut b = biblioteca(&layout);
                        if let Some(antes) = b.buscar(&args.instance_id).cloned() {
                            b.upsert(library::Instalado {
                                name: remoto.name.clone(),
                                version: remoto.version.clone(),
                                server: remoto.server.clone(),
                                mrpack: destino.to_string_lossy().into_owned(),
                                ..antes
                            });
                            let _ = guardar_biblioteca(&layout, &b);
                        }
                        version = remoto.version.clone();
                        mrpack = destino;
                    }
                    // Igual que sin red: con la versión instalada se puede entrar.
                    // `descargar_pack` solo escribe después de verificar el hash, así que
                    // el `.mrpack` de antes sigue intacto. Antes esto cortaba el
                    // lanzamiento, y no poder jugar es peor que jugar con lo de ayer.
                    Err(e) => {
                        println!("[PLAY] no pude bajar la versión nueva ({e}); abro la instalada");
                        emit("No pude actualizar el modpack: abro la versión instalada", -1.0);
                    }
                }
            }
            Ok(_) => println!("[PLAY] el modpack ya está al día"),
            Err(e) => println!("[PLAY] no pude comprobar actualizaciones: {e}"),
        }
    }

    let settings = engine::PackSettings {
        name: args.name.clone(),
        pack_id: args.instance_id.clone(),
        version: version.clone(),
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

    if !mrpack.exists() {
        return Err(format!("no encuentro el modpack en {}", mrpack.display()));
    }
    // Reinstalar en cada Jugar significaba releer el `.mrpack` entero y reextraerlo
    // para acabar escribiendo los mismos archivos. El `instance.cfg` ya dice qué
    // versión hay montada, así que basta preguntárselo.
    //
    // `install_only` es la vía de Importar, y ahí sí se instala siempre: es también
    // el camino para reparar una instancia que se haya quedado a medias.
    let cfg_actual = std::fs::read_to_string(layout.instance(&args.instance_id).join("instance.cfg"))
        .unwrap_or_default();
    // Con una versión nueva, `ya_instalado` deja de coincidir y la instalación se
    // dispara sola: no hace falta ninguna bandera aparte para forzarla.
    if args.install_only || !engine::ya_instalado(&cfg_actual, &version) {
        install::install_pack(&layout, &args.instance_id, &mrpack, &settings, &emit)
            .await
            .map_err(|e| e.to_string())?;
    } else {
        // El cfg sí se reescribe: la memoria y el servidor pueden haber cambiado en
        // Ajustes desde la última vez, y eso no justifica reextraer nada.
        let _ = std::fs::write(
            layout.instance(&args.instance_id).join("instance.cfg"),
            engine::instance_cfg(&settings),
        );
        emit("Ya está instalado", 1.0);
    }

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
    let hijo = std::process::Command::new(&bin)
        .args(&launch)
        // El motor identifica su «instancia única» por el directorio de trabajo del
        // proceso, no por su carpeta de datos (`ApplicationId::fromPathAndVersion`, con
        // un FIXME de ellos mismos al lado). Heredar el nuestro es arriesgarse a que el
        // lanzamiento acabe en el ElyPrism que el usuario ya tuviera abierto; con el
        // suyo propio, nadie más puede coincidir.
        .current_dir(layout.engine())
        .spawn()
        .map_err(|e| format!("no pude lanzar el juego: {e}"))?;

    // El motor abre su propio diálogo de progreso al preparar el lanzamiento. Se
    // esconde en un hilo aparte, sobre el PID que acabamos de lanzar y no sobre «las
    // ventanas del motor» en general: durante el login su página de Cuentas tiene que
    // verse, y esta misma función no distingue una de otra.
    //
    // El progreso que sustituye a ese diálogo va en nuestra ventana; mientras no esté,
    // esconderlo deja al usuario sin ver nada durante la descarga.
    let pid = hijo.id();
    let raiz = layout.root.clone();
    std::thread::spawn(move || install::monitor_sesion(raiz, pid));

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
        // Su propio directorio de trabajo, como al jugar: el motor identifica su
        // instancia única por ahí, y heredar el nuestro puede mandar esto al ElyPrism
        // que el usuario ya tuviera abierto.
        .current_dir(layout.engine())
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

/// Título de la ventana con la que el motor está preguntando algo, si la hay.
///
/// La interfaz lo consulta mientras prepara: una ventana del motor que espera respuesta
/// no se esconde, y hay que decir por qué está ahí en vez de dejar una barra parada.
#[tauri::command]
fn engine_question() -> Option<String> {
    install::sesion::pregunta()
}

/// En qué punto está la partida: `"inactiva"`, `"preparando"` o `"jugando"`.
///
/// Lo lleva el hilo que vigila la sesión, así que esto es una lectura de un atómico:
/// la interfaz puede preguntarlo tan seguido como quiera sin coste.
#[tauri::command]
fn session_state() -> &'static str {
    install::sesion::nombre()
}

/// Aborta la preparación del lanzamiento.
///
/// Es lo que sustituye al botón *Abort* del motor, que desapareció al esconderle la
/// ventana. Mata el motor mientras descarga; la partida, si ya arrancó, no se toca —
/// para eso está `stop_game`.
#[tauri::command]
fn abort_launch(app: tauri::AppHandle) -> Result<usize, String> {
    let layout = layout_of(&app)?;
    let n = install::close_engine(&layout.root);
    install::sesion::poner(install::sesion::INACTIVA);
    Ok(n)
}

/// Detiene la partida en curso: primero pidiéndole que se cierre y, solo si no
/// responde, a la fuerza. Ver `install::stop_game`.
///
/// En un hilo aparte porque puede esperar hasta medio minuto a que Minecraft guarde.
#[tauri::command]
async fn stop_game(app: tauri::AppHandle) -> Result<usize, String> {
    let layout = layout_of(&app)?;
    tauri::async_runtime::spawn_blocking(move || install::stop_game(&layout.root))
        .await
        .map_err(|e| e.to_string())
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

/// ¿Se desbloqueó ya la pestaña de admin en esta máquina?
#[tauri::command]
fn admin_unlocked(app: tauri::AppHandle) -> bool {
    layout_of(&app).map(|l| admin::desbloqueado(&l.root)).unwrap_or(false)
}

/// Mira si lo último que se tecleó termina en la clave y, si es así, desbloquea la
/// pestaña de admin para siempre en esta máquina.
#[tauri::command]
fn admin_unlock(app: tauri::AppHandle, texto: String) -> Result<bool, String> {
    if !admin::termina_en_la_clave(&texto) {
        return Ok(false);
    }
    let layout = layout_of(&app)?;
    admin::desbloquear(&layout.root).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Invitación que llegó por link y la interfaz todavía no ha recogido.
///
/// Hace falta guardarla porque con el launcher cerrado el link llega **antes** de que
/// exista la interfaz, y un evento emitido entonces no lo oiría nadie. La interfaz la
/// pide al arrancar y cada vez que se le avisa de que llegó otra.
#[derive(Default)]
struct InvitacionPendiente(std::sync::Mutex<Option<String>>);

/// Recoge los links `dinolauncher://…` que abrieron o despertaron el launcher.
#[cfg(desktop)]
fn recibir_invitacion(app: &tauri::AppHandle, urls: &[String]) {
    let Some(codigo) = urls.iter().find_map(|u| catalog::codigo_de_invitacion(u)) else {
        return;
    };
    println!("[INVITE] invitación a {codigo}");
    if let Some(st) = app.try_state::<InvitacionPendiente>() {
        if let Ok(mut g) = st.0.lock() {
            *g = Some(codigo);
        }
    }
    let _ = app.emit("invitacion", ());
    enfocar_ventana(app);
}

/// Trae la ventana al frente: quien pulsa un link espera ver el launcher, no que se
/// abra detrás del navegador.
#[cfg(desktop)]
fn enfocar_ventana(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Entrega la invitación pendiente, una sola vez.
#[tauri::command]
fn invitacion_pendiente(estado: tauri::State<'_, InvitacionPendiente>) -> Option<String> {
    estado.0.lock().ok().and_then(|mut g| g.take())
}

/// Lee un `.mrpack` y devuelve el borrador con todo lo deducible.
#[tauri::command]
fn admin_read(path: String) -> Result<admin::Borrador, String> {
    admin::leer_mrpack(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

/// Lo que ya está publicado, para poder actualizarlo.
#[tauri::command]
async fn admin_published() -> Result<Vec<catalog::PackRemoto>, String> {
    // En un hilo aparte, como `admin_publish`: `gh` es un proceso externo y bloquearía
    // el runtime asíncrono.
    tauri::async_runtime::spawn_blocking(|| admin::publicados().map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

/// Publica el pack: sube el archivo y actualiza el catálogo.
///
/// Se hace en un hilo aparte porque `gh` es un proceso externo y bloquearía el
/// runtime asíncrono mientras sube.
#[tauri::command]
async fn admin_publish(
    draft: admin::Borrador,
    path: String,
) -> Result<catalog::PackRemoto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        admin::publicar(&draft, std::path::Path::new(&path)).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Resuelve un código —o un link pegado— contra el catálogo público.
///
/// Devuelve la ficha del pack para poder enseñar qué es **antes** de descargar nada:
/// instalar a ciegas algo que llega de un chat no es razonable.
#[tauri::command]
async fn catalog_lookup(code: String) -> Result<catalog::PackRemoto, String> {
    catalog::resolver(&code).await.map_err(|e| e.to_string())
}

/// Descarga el `.mrpack` del catálogo y lo deja como pack activo.
///
/// No instala: solo deja el archivo en nuestra carpeta y apunta `pack.json` a él. La
/// instalación es la de siempre, que ya sabe respetar mundos y ajustes del jugador.
///
/// Y ahí está el arreglo de fondo: `pack.json` guardaba la ruta del disco de quien
/// importó el pack, así que el launcher no servía en otra máquina. Ahora apunta a la
/// carpeta de datos de la app, que existe en todas.
#[tauri::command]
async fn catalog_install(
    app: tauri::AppHandle,
    code: String,
) -> Result<library::Instalado, String> {
    let layout = layout_of(&app)?;
    let pack = catalog::resolver(&code).await.map_err(|e| e.to_string())?;

    // El id con el que vive aquí puede no ser el del catálogo: si esa carpeta ya es de
    // otro pack, instalarle encima le borraría los mundos.
    let id = biblioteca(&layout).id_libre(&pack.id, Some(&pack.code));
    let destino = layout.packs().join(format!("{id}.mrpack"));
    // Con los bytes dentro: en una descarga de cientos de megas, un porcentaje a secas
    // no dice si quedan dos minutos o veinte.
    let emit = |bajado: u64, total: u64| {
        let frac = if total > 0 { bajado as f32 / total as f32 } else { -1.0 };
        let _ = app.emit(
            "install",
            serde_json::json!({
                "paso": "Descargando el modpack",
                "frac": frac, "bajado": bajado, "total": total,
            }),
        );
    };
    catalog::descargar_pack(&pack, &destino, &emit)
        .await
        .map_err(|e| e.to_string())?;

    // Antes de bajar nada: si no cabe, decirlo ahora. A mitad de instalar, el error que
    // sale es del motor y no se entiende; aquí se puede explicar y se ahorra la espera.
    if let Some(libre) = install::espacio_libre(&layout.root) {
        let falta = install::espacio_necesario(pack.bytes);
        if pack.bytes > 0 && libre < falta {
            return Err(format!(
                "No hay espacio suficiente: hacen falta unos {} y quedan {} libres en el disco.",
                en_gigas(falta),
                en_gigas(libre)
            ));
        }
    }

    // El icono va aparte y no puede tumbar la instalación: sin él el pack funciona
    // igual, solo se ve el dino de siempre.
    if let Err(e) = catalog::descargar_icono(&pack, &layout.pack_icon(&id)).await {
        println!("[PACKS] no pude bajar el icono de {}: {e}", pack.code);
    }

    // Releída después de la descarga y no antes: entre una cosa y otra ha pasado una
    // espera larga, y guardar la biblioteca de hace un minuto perdería lo que hubiera
    // cambiado mientras.
    let mut b = biblioteca(&layout);
    let entrada = library::Instalado {
        id: id.clone(),
        code: Some(pack.code.clone()),
        name: pack.name.clone(),
        version: pack.version.clone(),
        server: pack.server.clone(),
        mrpack: destino.to_string_lossy().into_owned(),
        // Volver a añadir un pack que ya estaba no puede borrarle la RAM que se ajustó.
        max_mem: b.buscar(&id).and_then(|p| p.max_mem),
    };
    b.upsert(entrada.clone());
    b.seleccionar(&id);
    guardar_biblioteca(&layout, &b)?;
    Ok(entrada)
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

/// Un tamaño en texto, para un mensaje que lee alguien que solo quiere jugar.
fn en_gigas(bytes: u64) -> String {
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{} MB", bytes / 1_048_576)
    }
}

/// Tamaño en disco de una carpeta, en bytes.
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

/// La biblioteca leída del disco.
///
/// `packs.json` manda. Si no está, se migra el `pack.json` de cuando solo cabía un
/// pack: quien ya tiene el launcher instalado no puede encontrarse la biblioteca vacía
/// con su instancia ahí en el disco. El archivo viejo se deja donde está, que es la
/// única copia si la migración saliera mal.
///
/// Uno ilegible se aparta en vez de ignorarse: seguir como si no hubiera nada haría que
/// el primer guardado se llevara por delante la lista buena.
fn biblioteca(layout: &engine::Layout) -> library::Biblioteca {
    let ruta = layout.root.join("packs.json");
    let Ok(txt) = std::fs::read_to_string(&ruta) else {
        let viejo = std::fs::read_to_string(layout.root.join("pack.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .unwrap_or(serde_json::Value::Null);
        return library::migrar(&viejo);
    };
    match library::parse(&txt) {
        Ok(b) => b,
        Err(e) => {
            println!("[PACKS] packs.json ilegible ({e}); lo aparto como packs.json.roto");
            let _ = std::fs::rename(&ruta, ruta.with_extension("json.roto"));
            library::Biblioteca::default()
        }
    }
}

fn guardar_biblioteca(layout: &engine::Layout, b: &library::Biblioteca) -> Result<(), String> {
    std::fs::create_dir_all(&layout.root).map_err(|e| e.to_string())?;
    std::fs::write(layout.root.join("packs.json"), library::a_json(b)).map_err(|e| e.to_string())
}

/// El modpack con el que se juega ahora.
///
/// Sigue existiendo aparte de `list_packs` porque casi toda la interfaz —el botón de
/// Jugar, los ajustes, el estado del servidor— habla de uno solo: el elegido.
#[tauri::command]
fn get_pack(app: tauri::AppHandle) -> serde_json::Value {
    let Ok(layout) = layout_of(&app) else { return serde_json::json!(null) };
    serde_json::to_value(biblioteca(&layout).seleccionado()).unwrap_or(serde_json::Value::Null)
}

/// Todo lo que hay en la biblioteca, para poder pintarla.
///
/// Solo datos baratos: si la instancia está montada y su icono. El tamaño en disco y el
/// número de mods obligan a recorrer carpetas de gigas, y hacerlo con cada pack cada vez
/// que se abre la biblioteca se notaría; para eso está `instance_info`, que se pide del
/// que se esté mirando.
#[tauri::command]
fn list_packs(app: tauri::AppHandle) -> serde_json::Value {
    let Ok(layout) = layout_of(&app) else {
        return serde_json::json!({ "selected": null, "packs": [] });
    };
    let b = biblioteca(&layout);
    let packs: Vec<serde_json::Value> = b
        .packs
        .iter()
        .map(|p| {
            let mut v = serde_json::to_value(p).unwrap_or_default();
            // Montada de verdad es tener `instance.cfg`: la carpeta puede existir a
            // medias de una instalación que se abortó.
            v["installed"] =
                serde_json::json!(layout.instance(&p.id).join("instance.cfg").exists());
            v["icon"] = serde_json::json!(std::fs::read(layout.pack_icon(&p.id))
                .ok()
                .map(|d| install::png_data_url(&d)));
            v
        })
        .collect();
    // El seleccionado que se devuelve es el efectivo, no el que ponga el archivo: si
    // apuntaba a uno que ya no está, la interfaz debe marcar el que se va a jugar.
    serde_json::json!({
        "selected": b.seleccionado().map(|p| p.id.clone()),
        "packs": packs,
    })
}

/// Mete un pack en la biblioteca y lo deja elegido.
///
/// Es la vía de Importar un `.mrpack` del disco. Devuelve la ficha tal como quedó
/// guardada: el id puede no ser el que venía si esa carpeta ya era de otro pack.
#[tauri::command]
fn set_pack(app: tauri::AppHandle, pack: serde_json::Value) -> Result<library::Instalado, String> {
    let layout = layout_of(&app)?;
    let mut entrada: library::Instalado =
        serde_json::from_value(pack).map_err(|e| format!("modpack mal formado: {e}"))?;
    let mut b = biblioteca(&layout);
    entrada.id = b.id_libre(&entrada.id, entrada.code.as_deref());
    // La interfaz no manda la RAM al importar; reimportar no debe resetearla.
    if entrada.max_mem.is_none() {
        entrada.max_mem = b.buscar(&entrada.id).and_then(|p| p.max_mem);
    }
    b.upsert(entrada.clone());
    b.seleccionar(&entrada.id);
    guardar_biblioteca(&layout, &b)?;
    Ok(entrada)
}

/// Guarda la RAM elegida para un pack. `None` la devuelve a automática.
#[tauri::command]
fn set_pack_memory(app: tauri::AppHandle, id: String, mb: Option<u32>) -> Result<(), String> {
    let layout = layout_of(&app)?;
    let mut b = biblioteca(&layout);
    let antes = b
        .buscar(&id)
        .cloned()
        .ok_or_else(|| format!("no tengo ningún modpack con id {id}"))?;
    // Un cero no es una cantidad de RAM: es la interfaz diciendo «automática».
    b.upsert(library::Instalado { max_mem: mb.filter(|&m| m > 0), ..antes });
    guardar_biblioteca(&layout, &b)
}

/// Qué va a pasar si se pulsa el botón, para que lo diga antes de pulsarlo.
///
/// Devuelve `instalar`, `actualizar` o `jugar`, más el tamaño de lo que haya que bajar
/// cuando se sepa. Un amigo que abre el launcher por primera vez ve «Instalar · 386 MB»
/// en vez de un «Jugar» que se queda media hora pensando sin haber avisado de nada.
///
/// **Mirar el catálogo cuesta red, así que esto nunca falla por su culpa**: sin conexión
/// se responde con lo que diga el disco. Si resultaba que había versión nueva, `play` la
/// instala igual antes de abrir el juego; aquí solo se decide una etiqueta.
#[tauri::command]
async fn pack_state(app: tauri::AppHandle, id: String) -> serde_json::Value {
    let Ok(layout) = layout_of(&app) else {
        return serde_json::json!({ "estado": "jugar" });
    };
    let Some(pack) = biblioteca(&layout).buscar(&id).cloned() else {
        return serde_json::json!({ "estado": "jugar" });
    };

    let cfg = std::fs::read_to_string(layout.instance(&id).join("instance.cfg")).unwrap_or_default();
    let instalado = engine::ya_instalado(&cfg, &pack.version);

    let mut estado = if instalado { "jugar" } else { "instalar" };
    let mut bytes = 0u64;
    let mut version = pack.version.clone();

    if let Some(code) = pack.code.as_deref().filter(|c| !c.trim().is_empty()) {
        match catalog::resolver(code).await {
            Ok(remoto) => {
                if remoto.version != pack.version {
                    // Instalar manda sobre actualizar: quien no tiene nada montado no
                    // está «actualizando», está empezando.
                    estado = if instalado { "actualizar" } else { "instalar" };
                    version = remoto.version.clone();
                }
                if !instalado || estado == "actualizar" {
                    bytes = remoto.bytes;
                }
            }
            Err(e) => println!("[PACK] sin catálogo para {code}: {e}"),
        }
    }

    serde_json::json!({ "estado": estado, "bytes": bytes, "version": version })
}

/// Cambia con qué pack se juega.
#[tauri::command]
fn select_pack(app: tauri::AppHandle, id: String) -> Result<serde_json::Value, String> {
    let layout = layout_of(&app)?;
    let mut b = biblioteca(&layout);
    if !b.seleccionar(&id) {
        return Err(format!("no tengo ningún modpack con id {id}"));
    }
    guardar_biblioteca(&layout, &b)?;
    Ok(serde_json::to_value(b.seleccionado()).unwrap_or(serde_json::Value::Null))
}

/// Saca un pack de la biblioteca; con `delete_files`, además borra lo que ocupa.
///
/// Son dos cosas distintas a propósito: dentro de la instancia están los mundos del
/// jugador, y borrarlos no puede ser el efecto secundario de quitar algo de una lista.
///
/// Con una partida en marcha no se borra nada: esos archivos están abiertos y el motor
/// los reescribe al salir.
#[tauri::command]
fn remove_pack(app: tauri::AppHandle, id: String, delete_files: bool) -> Result<(), String> {
    let layout = layout_of(&app)?;
    if delete_files && install::sesion::actual() != install::sesion::INACTIVA {
        return Err("hay una sesión abierta: ciérrala antes de borrar el modpack".into());
    }
    let mut b = biblioteca(&layout);
    let fuera = b
        .quitar(&id)
        .ok_or_else(|| format!("no tengo ningún modpack con id {id}"))?;
    // La lista primero: si luego falla un borrado quedan archivos sueltos, que se
    // arreglan; al revés quedaría una entrada apuntando a una instancia a medio borrar.
    guardar_biblioteca(&layout, &b)?;

    if delete_files {
        if let Err(e) = std::fs::remove_dir_all(layout.instance(&fuera.id)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(format!(
                    "quité «{}» de la lista, pero no pude borrar sus archivos: {e}",
                    fuera.name
                ));
            }
        }
        let _ = std::fs::remove_file(layout.pack_icon(&fuera.id));
        // El `.mrpack` solo si es la copia que descargamos nosotros. El que alguien
        // importó de su disco es suyo y no se toca.
        let origen = std::path::PathBuf::from(&fuera.mrpack);
        if origen.starts_with(layout.packs()) {
            let _ = std::fs::remove_file(origen);
        }
    }
    Ok(())
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
    // El icono va aunque la instancia no esté montada: el del catálogo se descarga al
    // añadir el pack, antes de instalar nada, y sin él aquí la primera partida entera se
    // veía con el dino genérico.
    let icon = std::fs::read(layout.pack_icon(&id)).ok().map(|b| install::png_data_url(&b));
    if !dir.exists() {
        return Ok(serde_json::json!({ "installed": false, "icon": icon }));
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
    let builder = tauri::Builder::default();
    // Instancia única **la primera**, como pide el plugin: si va detrás, el segundo
    // launcher llega a arrancar antes de ceder. Con su función `deep-link`, un link
    // abierto con el launcher ya en marcha le llega a este en vez de abrir otro.
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            enfocar_ventana(app)
        }))
        .plugin(tauri_plugin_deep_link::init());
    builder
        .manage(InvitacionPendiente::default())
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

            #[cfg(desktop)]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                // Quien registra `dinolauncher://` es el instalador; en desarrollo nadie
                // ha pasado por él, así que se registra aquí para poder probar links.
                #[cfg(debug_assertions)]
                let _ = app.deep_link().register_all();
                // Con el launcher cerrado, el link llega como argumento al arrancar.
                if let Ok(Some(urls)) = app.deep_link().get_current() {
                    let urls: Vec<String> = urls.iter().map(|u| u.to_string()).collect();
                    recibir_invitacion(app.handle(), &urls);
                }
                // Con el launcher abierto, lo trae el plugin de instancia única.
                let h = app.handle().clone();
                app.deep_link().on_open_url(move |ev| {
                    let urls: Vec<String> = ev.urls().iter().map(|u| u.to_string()).collect();
                    recibir_invitacion(&h, &urls);
                });
            }

            // Limpiar restos de la sesión anterior y dejar el motor caliente: así
            // refresca los tokens mientras el usuario mira la interfaz, y al pulsar
            // Jugar no aparece su diálogo "Please wait...".
            if let Ok(root) = app.path().app_data_dir() {
                let layout = engine::Layout::new(root);
                std::thread::spawn(move || {
                    install::kill_orphans(&layout.root);
                    install::warm_engine(&layout);

                    // Una partida puede seguir viva de una sesión anterior del
                    // launcher: si alguien lo cerró y volvió a abrirlo con Minecraft
                    // corriendo, la interfaz tiene que encontrárselo ya en marcha en
                    // vez de ofrecerle Jugar sobre algo que ya está jugando.
                    if let Some(pid) = install::game_pid(&layout.root) {
                        println!("[PLAY] partida ya en curso al abrir, pid {pid}");
                        // El motor que la lanzó, si sigue vivo, se despide igual que en
                        // una sesión normal: al acabar la partida es el que podría
                        // enseñar su ventana.
                        let motor = install::engine_pid(&layout.root);
                        install::sesion::poner(install::sesion::JUGANDO);
                        while !install::esperar_proceso(pid, 2_000) {
                            if let Some(m) = motor {
                                let _ = install::hide_windows_of(m);
                            }
                        }
                        if let Some(m) = motor {
                            install::despedir_motor(m);
                        }
                        install::sesion::poner(install::sesion::INACTIVA);
                    }
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
            list_packs,
            select_pack,
            pack_state,
            remove_pack,
            set_pack_memory,
            active_account,
            server_status,
            instance_info,
            open_folder,
            inspect_mrpack,
            java_runtimes,
            open_engine_login,
            engine_windows,
            close_engine,
            session_state,
            engine_question,
            abort_launch,
            stop_game,
            admin_unlocked,
            admin_unlock,
            invitacion_pendiente,
            admin_read,
            admin_published,
            admin_publish,
            catalog_lookup,
            catalog_install,
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
        //
        // En 11 el acrílico tampoco elige entre claro y oscuro: sigue el tema de la
        // ventana, que por defecto es el de las apps del sistema. Con Windows en claro
        // salía un vidrio blanquecino y la interfaz oscura encima lo dejaba en un gris
        // que no dejaba ver nada detrás. Por eso `tauri.conf.json` fija `"theme":
        // "Dark"`: la interfaz solo existe en oscuro, así que la ventana también.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Carpeta propia para cada test: comparten el temp del sistema, y pisarse daría
    /// fallos que van y vienen según el orden en que corran.
    fn carpeta(nombre: &str) -> engine::Layout {
        let root = std::env::temp_dir().join(format!("dino-biblioteca-{nombre}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        engine::Layout::new(root)
    }

    fn entrada(id: &str) -> library::Instalado {
        library::Instalado {
            id: id.into(),
            code: None,
            name: id.into(),
            version: "1.0.0".into(),
            server: None,
            mrpack: format!("/packs/{id}.mrpack"),
            max_mem: None,
        }
    }

    #[test]
    fn sin_nada_en_disco_la_biblioteca_sale_vacia() {
        assert!(biblioteca(&carpeta("vacia")).packs.is_empty());
    }

    #[test]
    fn el_pack_json_de_antes_se_migra_al_leerlo() {
        // Este es el camino que pisa todo el que ya tiene el launcher cuando actualice.
        let l = carpeta("migra");
        std::fs::write(
            l.root.join("pack.json"),
            r#"{ "id": "fo", "name": "FO", "mrpack": "C:/x/fo.mrpack", "version": "1.0.0" }"#,
        )
        .unwrap();

        let b = biblioteca(&l);
        assert_eq!(b.seleccionado().unwrap().id, "fo");
        // Y el archivo viejo sigue ahí: es la única copia si la migración saliera mal.
        assert!(l.root.join("pack.json").exists());
    }

    #[test]
    fn packs_json_manda_sobre_el_pack_json_viejo() {
        // Si no, cada arranque resucitaría el pack de antes y se llevaría por delante
        // todo lo que se hubiera instalado después.
        let l = carpeta("manda");
        std::fs::write(
            l.root.join("pack.json"),
            r#"{ "id": "viejo", "name": "Viejo", "mrpack": "/x.mrpack" }"#,
        )
        .unwrap();
        let mut b = library::Biblioteca::default();
        b.upsert(entrada("nuevo"));
        b.seleccionar("nuevo");
        guardar_biblioteca(&l, &b).unwrap();

        let leida = biblioteca(&l);
        assert_eq!(leida.packs.len(), 1);
        assert_eq!(leida.seleccionado().unwrap().id, "nuevo");
    }

    #[test]
    fn un_packs_json_roto_se_aparta_en_vez_de_sobrescribirse() {
        // Tratarlo como «no hay nada» haría que el siguiente guardado se llevara la
        // lista buena sin dejar rastro.
        let l = carpeta("roto");
        std::fs::write(l.root.join("packs.json"), "{ esto no es json").unwrap();

        assert!(biblioteca(&l).packs.is_empty());
        assert!(
            l.root.join("packs.json.roto").exists(),
            "lo ilegible tiene que quedar recuperable"
        );
        assert!(!l.root.join("packs.json").exists());
    }

    #[test]
    fn lo_que_se_guarda_se_vuelve_a_leer_igual_del_disco() {
        let l = carpeta("ida-y-vuelta");
        let mut b = library::Biblioteca::default();
        b.upsert(entrada("a"));
        b.upsert(entrada("b"));
        b.seleccionar("b");
        guardar_biblioteca(&l, &b).unwrap();

        let leida = biblioteca(&l);
        assert_eq!(leida.packs.len(), 2);
        assert_eq!(leida.seleccionado().unwrap().id, "b");
    }
}
