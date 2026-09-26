//! Descarga del motor e instalación de modpacks.
//!
//! Todo lo de red vive aquí; `engine.rs` y `pack.rs` se quedan puros y testeables.

use crate::engine::{self, Layout};
use crate::pack::{self, MrpackIndex};
use sha2::{Digest, Sha512};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("error de red: {0}")]
    Http(#[from] reqwest::Error),
    #[error("error de disco: {0}")]
    Io(#[from] std::io::Error),
    #[error("el zip está corrupto: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("{0}")]
    Pack(#[from] pack::PackError),
    #[error("esta plataforma no tiene build del motor")]
    UnsupportedPlatform,
    #[error("{archivo}: el hash no coincide, la descarga vino mal")]
    BadHash { archivo: String },
    #[error("el pack no trae modrinth.index.json")]
    NoIndex,
    #[error("descarga fallida ({0}) de {1}")]
    Status(u16, String),
}

/// Reporta avance a la interfaz.
pub type Progress<'a> = &'a (dyn Fn(&str, f32) + Send + Sync);

async fn get_bytes(url: &str) -> Result<Vec<u8>, InstallError> {
    let r = reqwest::get(url).await?;
    if !r.status().is_success() {
        return Err(InstallError::Status(r.status().as_u16(), url.to_string()));
    }
    Ok(r.bytes().await?.to_vec())
}

// ---------------------------------------------------------------- motor

/// Deja el motor listo y devuelve la ruta a su ejecutable.
/// Si ya está instalado no descarga nada.
pub async fn ensure_engine(
    layout: &Layout,
    version: &str,
    progress: Progress<'_>,
) -> Result<PathBuf, InstallError> {
    let dir = layout.engine_app();
    let bin = engine::engine_binary(&dir);
    if bin.exists() {
        set_dock_visibility(&dir, true); // idempotente: arregla también instalaciones ya hechas
        return Ok(bin);
    }

    let asset = engine::asset_for_host(version).ok_or(InstallError::UnsupportedPlatform)?;
    progress("Descargando el motor", 0.0);
    let bytes = get_bytes(&engine::asset_url(version, &asset)).await?;

    progress("Instalando el motor", 0.5);
    std::fs::create_dir_all(&dir)?;

    if asset.ends_with(".AppImage") {
        // Linux: el AppImage es el ejecutable, no un contenedor.
        std::fs::write(&bin, &bytes)?;
        make_executable(&bin)?;
    } else {
        unzip_into(&bytes, &dir)?;
    }

    // El portable de Windows trae `portable.txt`: le dice al motor que guarde sus datos
    // junto al ejecutable. Nosotros siempre pasamos `--dir`, que manda por encima, pero
    // si alguna vez dejara de mandar, los datos irían a `bin/` y `accounts.json` quedaría
    // donde nadie lo busca: el launcher pediría iniciar sesión eternamente. Upstream
    // documenta el archivo como borrable; borrarlo quita la ambigüedad de raíz.
    let _ = std::fs::remove_file(dir.join("portable.txt"));

    // Los releases vienen firmados adhoc; sin esto Gatekeeper los bloquea.
    drop_quarantine(&dir);
    set_dock_visibility(&dir, true);

    if !bin.exists() {
        return Err(InstallError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no encontré el ejecutable en {}", bin.display()),
        )));
    }
    make_executable(&bin)?;
    progress("Motor listo", 1.0);
    Ok(bin)
}

fn unzip_into(bytes: &[u8], dest: &Path) -> Result<(), InstallError> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue }; // descarta rutas que se escapan
        let rel = rel.to_path_buf();
        // Los zips de macOS traen metadatos que no sirven de nada.
        if rel.components().any(|c| c.as_os_str() == "__MACOSX") {
            continue;
        }
        let out = dest.join(&rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        std::fs::write(&out, buf)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = f.unix_mode() {
                let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

fn make_executable(p: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(p)?.permissions();
        perm.set_mode(perm.mode() | 0o755);
        std::fs::set_permissions(p, perm)?;
    }
    #[cfg(not(unix))]
    let _ = p;
    Ok(())
}

/// Quita `com.apple.quarantine`. El motor está firmado adhoc y sin TeamIdentifier,
/// así que sin esto macOS se niega a abrirlo.
fn drop_quarantine(dir: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(dir)
            .status();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = dir;
}

/// Decide si el motor aparece en el Dock de macOS.
///
/// Una `.app` de Qt aparece en el Dock y roba el foco aunque no muestre ventana —
/// y con `QuitAfterGameStop` vive toda la partida. `LSUIElement` la convierte en
/// agente: sin icono, sin menú, sin foco.
///
/// Para **jugar** la queremos oculta. Para el **login** hay que devolvérsela: una
/// ventana de una app agente sale sin foco y no se puede escribir en ella.
///
/// Tocar el `Info.plist` invalida la firma adhoc, así que hay que volver a firmar.
/// Sin `--deep`: eso re-firma los frameworks embebidos y los deja inconsistentes.
pub fn set_dock_visibility(dir: &Path, hidden: bool) {
    #[cfg(target_os = "macos")]
    {
        let app = dir.join("ElyPrismLauncher.app");
        let plist = app.join("Contents/Info.plist");
        if !plist.exists() {
            return;
        }
        let current = std::process::Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :LSUIElement"])
            .arg(&plist)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");

        if current == Some(hidden) {
            return; // ya está como queremos: no tocar ni volver a firmar
        }

        let want = if hidden { "true" } else { "false" };
        let cmd = match current {
            Some(_) => format!("Set :LSUIElement {want}"),
            None => format!("Add :LSUIElement bool {want}"),
        };
        let ok = std::process::Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", &cmd])
            .arg(&plist)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            let _ = std::process::Command::new("codesign")
                .args(["--force", "--sign", "-"])
                .arg(&app)
                .status();
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (dir, hidden);
    }
}

/// Clase principal con la que el motor arranca el juego. Si aparece en la línea de
/// comandos, ese proceso **es la partida** y no se toca pase lo que pase.
const GAME_MAIN_CLASS: &str = "org.prismlauncher.EntryPoint";

/// Un proceso vivo: lo mínimo para decidir si es basura nuestra.
pub struct Proceso {
    pub pid: u32,
    pub ppid: u32,
    pub args: String,
}

/// ¿Esta línea de comandos apunta dentro de nuestra carpeta?
///
/// En Windows hay que normalizar antes de comparar: Qt escribe el classpath que le
/// pasa a Java con `/` aunque nosotros le demos `--dir` con `\`, y el sistema de
/// archivos no distingue mayúsculas. Sin esto el Java del pack nunca coincidiría con
/// la raíz y la limpieza no encontraría nada que limpiar.
pub fn en_nuestra_carpeta(args: &str, root: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.replace('\\', "/");
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s
        }
    }
    norm(args).contains(&norm(root))
}

/// ¿Este proceso es basura nuestra que se puede matar?
///
/// Pura y testeable a propósito: es la decisión que, mal tomada, le cierra el
/// juego a alguien en plena partida.
///
/// `huerfano` lo calcula cada sistema a su manera y por eso entra ya resuelto: Unix
/// reparenta a `launchd`/`init` y basta con `ppid == 1`, mientras que Windows no
/// reparenta nada y deja el ppid apuntando a un proceso que ya no existe.
pub fn is_killable(args: &str, huerfano: bool, root: &str, self_pid: u32, pid: u32) -> bool {
    if !huerfano || !en_nuestra_carpeta(args, root) {
        return false; // no es huérfano, o no es nuestro
    }
    if args.contains(GAME_MAIN_CLASS) {
        return false; // es la partida: jamás
    }
    pid != self_pid // no nos suicidemos
}

#[cfg(unix)]
fn procesos() -> Vec<Proceso> {
    let Ok(out) = std::process::Command::new("ps").args(["-eo", "pid=,ppid=,args="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            Some(Proceso { pid, ppid, args: line.to_string() })
        })
        .collect()
}

/// Lista de procesos en Windows, con línea de comandos.
///
/// Vía CIM porque es lo único que da `CommandLine`: `tasklist` no la trae, y sin ella
/// habría que matar por nombre de ejecutable, que es justo lo que cerraría el
/// ElyPrism que alguien tuviera instalado por su cuenta.
///
/// `CREATE_NO_WINDOW` no es cosmético: sin esa bandera cada llamada abre una consola
/// negra encima del launcher. El `OutputEncoding` tampoco: sin él, PowerShell emite en
/// la codificación de la consola y una ruta como `C:\Users\José` vuelve rota, con lo
/// que su propia carpeta dejaría de reconocerse como suya.
#[cfg(windows)]
fn procesos() -> Vec<Proceso> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // Sin comillas dentro del script: se pasa como un solo argumento y cualquier
    // comilla tendría que sobrevivir al encomillado de Rust y al de PowerShell.
    let script = concat!(
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8; ",
        "Get-CimInstance Win32_Process | ForEach-Object { ",
        "($_.ProcessId, $_.ParentProcessId, $_.CommandLine) -join [char]9 }"
    );
    let Ok(out) = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut it = line.splitn(3, '\t');
            let pid = it.next()?.trim().parse().ok()?;
            let ppid = it.next()?.trim().parse().ok()?;
            Some(Proceso { pid, ppid, args: it.next().unwrap_or("").to_string() })
        })
        .collect()
}

#[cfg(unix)]
fn matar(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .is_ok()
}

/// Termina un proceso por PID.
///
/// Por API y no con `taskkill`: cada `taskkill` sería otra consola negra parpadeando,
/// y aquí se llama una vez por proceso a limpiar.
#[cfg(windows)]
fn matar(pid: u32) -> bool {
    #[link(name = "kernel32")]
    #[allow(non_snake_case)] // los nombres son los de la API, no se traducen
    extern "system" {
        // `HANDLE` es un puntero opaco: `isize` tiene su mismo tamaño en 32 y 64 bits.
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn TerminateProcess(handle: isize, code: u32) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const PROCESS_TERMINATE: u32 = 0x0001;

    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h == 0 {
            return false; // ya murió, o no tenemos permiso: en ninguno de los dos insistimos
        }
        let ok = TerminateProcess(h, 1) != 0;
        CloseHandle(h);
        ok
    }
}

/// Cuántas ventanas visibles tiene ahora mismo el motor.
///
/// Sirve para saber que el motor terminó sin que apareciera ninguna cuenta —o sea,
/// que alguien canceló el login— y poder decirlo en la interfaz en vez de dejarla
/// esperando para siempre.
///
/// No sirve para adivinar el momento exacto de la cancelación: medido contra el
/// release 11.1.0, al cerrar el aviso «No Accounts» el motor abre acto seguido otro
/// diálogo, «Play demo?», así que el conteo pasa de uno a uno. Ese diálogo pregunta
/// antes de nada y trae *Cancelar* como botón por defecto (`askPlayDemo()`, sin ajuste
/// que lo desactive), de modo que la demo no arranca sola. Quien quiera cortar antes
/// tiene el botón de cancelar de nuestra propia ventana, que llama a `close_engine`.
///
/// `None` significa «no se puede saber en este sistema»: solo está implementado en
/// Windows, y quien llama debe tratarlo como «sigue esperando», nunca como «cero».
pub fn engine_windows(root: &Path) -> Option<usize> {
    #[cfg(not(windows))]
    {
        let _ = root;
        None
    }
    #[cfg(windows)]
    {
        use std::sync::Mutex;

        #[link(name = "user32")]
        #[allow(non_snake_case)] // los nombres son los de la API, no se traducen
        extern "system" {
            fn EnumWindows(cb: extern "system" fn(isize, isize) -> i32, param: isize) -> i32;
            fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
            fn IsWindowVisible(hwnd: isize) -> i32;
        }

        // `EnumWindows` toma una función suelta, sin contexto propio, así que los PIDs
        // que buscamos y la cuenta tienen que vivir fuera. El candado hace que dos
        // llamadas a la vez no se pisen los datos.
        static ESTADO: Mutex<(Vec<u32>, usize)> = Mutex::new((Vec::new(), 0));

        extern "system" fn visitar(hwnd: isize, _p: isize) -> i32 {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            if let Ok(mut e) = ESTADO.lock() {
                if e.0.contains(&pid) && unsafe { IsWindowVisible(hwnd) } != 0 {
                    e.1 += 1;
                }
            }
            1 // seguir enumerando
        }

        let root = root.to_str()?;
        let nuestros: Vec<u32> = procesos()
            .iter()
            .filter(|p| {
                en_nuestra_carpeta(&p.args, root)
                    && p.args.to_lowercase().contains("elyprismlauncher")
            })
            .map(|p| p.pid)
            .collect();
        if nuestros.is_empty() {
            return Some(0); // sin motor no hay ventanas que contar
        }

        let mut guard = ESTADO.lock().ok()?;
        *guard = (nuestros, 0);
        drop(guard);
        unsafe { EnumWindows(visitar, 0) };
        Some(ESTADO.lock().ok()?.1)
    }
}

/// Esconde las ventanas de un proceso y devuelve los títulos de las que escondió.
///
/// Al preparar el lanzamiento el motor abre su propio diálogo Qt —«Please wait…», con
/// barra y un registro de URLs— y no hay forma de desactivarlo: ni por configuración ni
/// por línea de comandos. Esconderlo por API es lo único que queda. La descarga sigue
/// corriendo exactamente igual; `SW_HIDE` no cancela nada, solo deja de dibujar.
///
/// Va por **PID concreto y no por «todas las del motor» a propósito**: durante el login
/// el motor enseña su página de Cuentas, y esconderla dejaría al usuario mirando una
/// ventana que nunca llega. Quien llama decide cuándo procede.
///
/// Devuelve los títulos porque son la única pista de qué se tapó. Si algún día el motor
/// abre ahí un error en vez de un progreso, esto es lo que lo delataría.
/// ¿Es una ventana del motor que se puede esconder sin dejar a nadie esperando?
///
/// Las suyas llevan el nombre del launcher detrás de un guion —«Please wait... - Nombre
/// 11.1.0», «Nombre - Nombre 11.1.0»—, mientras que sus preguntas son `QMessageBox`
/// tituladas solo con ese nombre.
///
/// **Ante la duda no se esconde.** Tapar una pregunta deja al motor esperando una
/// respuesta que nadie puede dar, y eso no se nota: se ve como una descarga que no
/// avanza nunca. Que se vea una ventana ajena de más es mucho más barato que eso.
pub fn se_puede_esconder(titulo: &str) -> bool {
    let t = titulo.trim().to_lowercase();
    !t.is_empty() && (t.starts_with("please wait") || t.contains(" - "))
}

#[cfg(windows)]
pub fn hide_windows_of(pid: u32) -> Vec<String> {
    use std::sync::Mutex;

    #[link(name = "user32")]
    #[allow(non_snake_case)] // los nombres son los de la API, no se traducen
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(isize, isize) -> i32, param: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn GetWindowTextW(hwnd: isize, buf: *mut u16, len: i32) -> i32;
    }
    const SW_HIDE: i32 = 0;

    // `EnumWindows` recibe una función suelta, sin contexto propio, así que el PID
    // buscado y lo recogido tienen que vivir fuera. El candado evita que dos llamadas
    // simultáneas se mezclen los datos.
    static ESTADO: Mutex<(u32, Vec<(isize, String)>)> = Mutex::new((0, Vec::new()));

    extern "system" fn visitar(hwnd: isize, _p: isize) -> i32 {
        let mut suyo = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut suyo) };
        if let Ok(mut e) = ESTADO.lock() {
            if suyo == e.0 && unsafe { IsWindowVisible(hwnd) } != 0 {
                let mut buf = [0u16; 256];
                let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), 256) }.max(0) as usize;
                e.1.push((hwnd, String::from_utf16_lossy(&buf[..n])));
            }
        }
        1 // seguir enumerando
    }

    {
        let Ok(mut g) = ESTADO.lock() else { return Vec::new() };
        *g = (pid, Vec::new());
    }
    unsafe { EnumWindows(visitar, 0) };
    let ventanas = ESTADO.lock().map(|g| g.1.clone()).unwrap_or_default();

    // Se decide **después** de enumerar y no dentro del recorrido: lo que no se esconde
    // hay que poder contarlo, y desde la función suelta no hay a dónde llevarlo.
    let mut escondidas = Vec::new();
    let mut preguntas = Vec::new();
    for (hwnd, titulo) in ventanas {
        if se_puede_esconder(&titulo) {
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            escondidas.push(titulo);
        } else {
            preguntas.push(titulo);
        }
    }
    // La interfaz lo cuenta para que nadie se quede mirando una barra parada sin saber
    // que hay una ventana del motor esperándole. Se limpia solo: cuando la contestan,
    // la ventana deja de estar y aquí deja de aparecer.
    sesion::preguntando(preguntas.into_iter().next());
    escondidas
}

#[cfg(not(windows))]
pub fn hide_windows_of(pid: u32) -> Vec<String> {
    // En macOS haría falta pasar por las APIs de Accesibilidad, que además piden
    // permiso al usuario. Por ahora el diálogo del motor se ve allí.
    let _ = pid;
    Vec::new()
}

/// ¿Sigue vivo ese proceso?
///
/// Por API y no enumerando: esto se llama en bucle mientras se esconde la ventana, y
/// listar procesos en Windows cuesta un PowerShell entero cada vez.
#[cfg(windows)]
pub fn proceso_vivo(pid: u32) -> bool {
    #[link(name = "kernel32")]
    #[allow(non_snake_case)] // los nombres son los de la API, no se traducen
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn GetExitCodeProcess(handle: isize, code: *mut u32) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;

    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE
    }
}

#[cfg(not(windows))]
pub fn proceso_vivo(pid: u32) -> bool {
    let _ = pid;
    false
}

/// Espera a que un proceso termine, hasta `ms` milisegundos.
///
/// Devuelve `true` si terminó, o si ya no existía. Es la forma de enterarse **en el
/// mismo instante** en que acaba: sondear con `proceso_vivo` cada tanto deja pasar ese
/// rato, y en él el motor ya puede haber sacado una ventana.
#[cfg(windows)]
pub fn esperar_proceso(pid: u32, ms: u32) -> bool {
    #[link(name = "kernel32")]
    #[allow(non_snake_case)] // los nombres son los de la API, no se traducen
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn WaitForSingleObject(handle: isize, ms: u32) -> u32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0;

    unsafe {
        let h = OpenProcess(SYNCHRONIZE, 0, pid);
        if h == 0 {
            return true; // ya no está
        }
        let r = WaitForSingleObject(h, ms);
        CloseHandle(h);
        r == WAIT_OBJECT_0
    }
}

#[cfg(not(windows))]
pub fn esperar_proceso(pid: u32, ms: u32) -> bool {
    let _ = ms;
    !proceso_vivo(pid)
}

/// Pide a las ventanas de un proceso que se cierren, como al pulsar su X.
///
/// Con Minecraft es la diferencia entre salir guardando y morir sin guardar: su ventana
/// recibe el mismo `WM_CLOSE` que manda la X, y el juego sale por su camino normal.
///
/// Devuelve cuántas aceptaron el mensaje. `PostMessage` y no `SendMessage`: no hay que
/// quedarse bloqueado mientras el juego guarda.
#[cfg(windows)]
pub fn cerrar_ventanas_de(pid: u32) -> usize {
    use std::sync::Mutex;

    #[link(name = "user32")]
    #[allow(non_snake_case)] // los nombres son los de la API, no se traducen
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(isize, isize) -> i32, param: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    }
    const WM_CLOSE: u32 = 0x0010;

    // Mismo esquema que `hide_windows_of`, con su propio estado: las dos pueden correr a
    // la vez desde hilos distintos —el que vigila la sesión y el que la detiene—.
    static ESTADO: Mutex<(u32, Vec<isize>)> = Mutex::new((0, Vec::new()));

    extern "system" fn visitar(hwnd: isize, _p: isize) -> i32 {
        let mut suyo = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut suyo) };
        if let Ok(mut e) = ESTADO.lock() {
            if suyo == e.0 && unsafe { IsWindowVisible(hwnd) } != 0 {
                e.1.push(hwnd);
            }
        }
        1 // seguir enumerando
    }

    {
        let Ok(mut g) = ESTADO.lock() else { return 0 };
        *g = (pid, Vec::new());
    }
    unsafe { EnumWindows(visitar, 0) };
    let ventanas = ESTADO.lock().map(|g| g.1.clone()).unwrap_or_default();
    ventanas
        .into_iter()
        .filter(|&h| unsafe { PostMessageW(h, WM_CLOSE, 0, 0) } != 0)
        .count()
}

#[cfg(not(windows))]
pub fn cerrar_ventanas_de(pid: u32) -> usize {
    // Sin equivalente directo fuera de Windows: `stop_game` pasa al cierre a la fuerza.
    let _ = pid;
    0
}

/// En qué punto está la partida, para que la interfaz sepa qué enseñar.
///
/// Se guarda en un atómico global y no en el estado de Tauri porque quien lo escribe es
/// un hilo suelto: así el comando que lo lee no cuesta absolutamente nada, y puede
/// preguntarse tan a menudo como haga falta sin pagar una enumeración de procesos.
pub mod sesion {
    use std::sync::atomic::{AtomicU8, Ordering};

    pub const INACTIVA: u8 = 0;
    pub const PREPARANDO: u8 = 1;
    pub const JUGANDO: u8 = 2;

    static ESTADO: AtomicU8 = AtomicU8::new(INACTIVA);

    pub fn poner(v: u8) {
        ESTADO.store(v, Ordering::Relaxed);
    }
    pub fn actual() -> u8 {
        ESTADO.load(Ordering::Relaxed)
    }
    /// Ventana del motor que está esperando una respuesta, si la hay.
    static PREGUNTA: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    pub fn preguntando(titulo: Option<String>) {
        if let Ok(mut g) = PREGUNTA.lock() {
            if *g != titulo {
                if let Some(t) = &titulo {
                    println!("[PLAY] el motor está preguntando algo: {t:?}");
                }
                *g = titulo;
            }
        }
    }

    pub fn pregunta() -> Option<String> {
        PREGUNTA.lock().ok().and_then(|g| g.clone())
    }

    pub fn nombre() -> &'static str {
        match actual() {
            PREPARANDO => "preparando",
            JUGANDO => "jugando",
            _ => "inactiva",
        }
    }
}

/// PID de la partida en curso, si la hay.
///
/// Cuesta una enumeración de procesos entera, así que se llama lo mínimo: una vez al
/// pasar de preparar a jugar. A partir de ahí basta preguntar por ese PID, que es
/// gratis.
pub fn game_pid(root: &Path) -> Option<u32> {
    let root = root.to_str()?;
    procesos()
        .into_iter()
        .find(|p| p.args.contains(GAME_MAIN_CLASS) && en_nuestra_carpeta(&p.args, root))
        .map(|p| p.pid)
}

/// Deja que el motor se cierre solo al acabar la partida, sin que se le vea.
///
/// Con `CloseAfterLaunch` en `false` no debería abrir nada y se cierra por su cuenta.
/// Esto es la red por si algo se porta distinto: mientras siga vivo se le esconde cada
/// 15 ms, lo mismo que al preparar.
///
/// **No se le mata nada más acabar la partida.** Justo en ese momento guarda
/// `instance.cfg` y `accounts.json` —se ve en su log—, y cortarlo a mitad de escribir
/// las cuentas dejaría al jugador sin sesión. Solo se le fuerza si pasado el margen
/// sigue ahí.
pub fn despedir_motor(pid_motor: u32) {
    let fin = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < fin {
        for titulo in hide_windows_of(pid_motor) {
            println!("[PLAY] escondida ventana del motor al terminar: {titulo:?}");
        }
        if esperar_proceso(pid_motor, 15) {
            return;
        }
    }
    if matar(pid_motor) {
        println!("[PLAY] el motor seguía abierto tras la partida: cerrado a la fuerza");
    }
}

/// Vigila una partida de principio a fin: esconde, detecta y publica el estado.
///
/// 1. **Preparando** — se esconden las ventanas del motor cada 15 ms. No 300, que era
///    lo que dejaba al diálogo alcanzar a dibujarse y parpadear; 15 ms es un fotograma
///    a 60 Hz y `EnumWindows` es lo bastante barato para permitírselo.
/// 2. **Jugando** — en cuanto aparece el proceso del juego.
/// 3. **Inactiva** — cuando termina.
///
/// El paso a «jugando» lo dispara **que aparezca el juego**, no que muera el motor.
/// Parece lo mismo y no lo es: el motor sigue vivo mientras se juega —`CloseAfterLaunch`
/// no lo cierra, pese al nombre; ver `SILENT_ENGINE`—, y esperándolo la interfaz se
/// quedaba en la barra de carga para siempre, sin ofrecer detener.
///
/// Buscar el juego cuesta una enumeración de procesos entera, así que no se hace en
/// cada vuelta: una cada dos segundos basta y el resto del tiempo se dedica a esconder.
pub fn monitor_sesion(root: std::path::PathBuf, pid_motor: u32) {
    sesion::poner(sesion::PREPARANDO);

    const PASO: u64 = 15; // ms entre intentos de esconder
    const CADA: u32 = 133; // ~2 s: cada cuántas vueltas se busca el juego

    let limite = std::time::Instant::now() + std::time::Duration::from_secs(60 * 60);
    let mut vuelta: u32 = 0;
    let mut pid_juego = None;

    while std::time::Instant::now() < limite {
        for titulo in hide_windows_of(pid_motor) {
            println!("[PLAY] escondida ventana del motor: {titulo:?}");
        }

        if vuelta % CADA == 0 {
            if let Some(pid) = game_pid(&root) {
                pid_juego = Some(pid);
                break;
            }
            // Si el motor ya no está y el juego tampoco apareció, el lanzamiento se
            // fue al traste: no hay nada que vigilar ni nada que esconder.
            if !proceso_vivo(pid_motor) && vuelta > 0 {
                break;
            }
        }
        vuelta = vuelta.wrapping_add(1);
        std::thread::sleep(std::time::Duration::from_millis(PASO));
    }

    match pid_juego {
        Some(pid) => {
            println!("[PLAY] partida en curso, pid {pid}");
            sesion::poner(sesion::JUGANDO);
            // Se **espera** al proceso del juego en vez de sondearlo cada dos segundos:
            // así se sabe en el mismo instante en que termina, y no dos segundos después,
            // que era margen de sobra para que el motor enseñara su ventana. Entre medias
            // se sigue escondiendo lo que el motor saque.
            while !esperar_proceso(pid, 2_000) {
                let _ = hide_windows_of(pid_motor);
            }
            println!("[PLAY] la partida terminó");
            despedir_motor(pid_motor);
        }
        None => println!("[PLAY] el motor terminó sin abrir el juego"),
    }
    sesion::poner(sesion::INACTIVA);
}

/// Cuánto se espera a que Minecraft se cierre por su cuenta antes de forzarlo.
///
/// Guardar un mundo grande tarda; cortar antes de tiempo sería perder justo lo que se
/// estaba guardando.
const CIERRE_LIMPIO_MS: u32 = 30_000;

/// Detiene la partida en curso.
///
/// **Primero se le pide que se cierre, como al pulsar su X**: Minecraft guarda el mundo y
/// sale con normalidad. Matar el proceso directamente —lo que se hacía antes— no guardaba
/// nada, y ese final a la fuerza era además el que el motor tomaba por un crash.
///
/// Solo si no responde en `CIERRE_LIMPIO_MS` se corta. Y entonces se limpia también el
/// `crash_assistant` del pack, que salta con cualquier final anormal: aquí no hubo
/// ningún crash que contar.
///
/// Sigue sin llamarse nunca por su cuenta: cerrar la partida de alguien solo pasa porque
/// lo pidió quien está jugando.
pub fn stop_game(root: &Path) -> usize {
    let Some(pid) = game_pid(root) else { return 0 };

    let pedidas = cerrar_ventanas_de(pid);
    println!("[PLAY] pido a la partida que se cierre ({pedidas} ventana/s)");
    if pedidas > 0 && esperar_proceso(pid, CIERRE_LIMPIO_MS) {
        return 1;
    }

    println!("[PLAY] la partida no se cerró sola: la corto");
    if !matar(pid) {
        return 0;
    }
    let _ = esperar_proceso(pid, 5_000);
    let limpiados = kill_orphans(root);
    if limpiados > 0 {
        println!("[PLAY] limpiados {limpiados} procesos que dejó el cierre a la fuerza");
    }
    sesion::poner(sesion::INACTIVA);
    1
}

/// Cierra el motor que abrimos para iniciar sesión.
///
/// `kill_orphans` no sirve aquí: ese solo toca procesos que ya nadie sostiene, y este
/// lo acabamos de lanzar nosotros. Se usa cuando la cuenta ya aparece en
/// `accounts.json` y la ventana del motor no tiene nada más que hacer en pantalla.
///
/// La partida sigue siendo intocable: `GAME_MAIN_CLASS` la protege igual que siempre.
pub fn close_engine(root: &Path) -> usize {
    let Some(root) = root.to_str() else { return 0 };
    let yo = std::process::id();
    let mut cerrados = 0;
    for p in procesos() {
        if p.pid == yo || !en_nuestra_carpeta(&p.args, root) {
            continue;
        }
        if p.args.contains(GAME_MAIN_CLASS) {
            continue; // es la partida: jamás
        }
        if !p.args.to_lowercase().contains("elyprismlauncher") {
            continue;
        }
        if matar(p.pid) {
            cerrados += 1;
        }
    }
    cerrados
}

/// Mata procesos huérfanos que dejamos atrás en sesiones anteriores.
///
/// Dos fuentes reales, vistas en pruebas:
///  - el `crash_assistant` del pack lanza un Java propio al terminar la partida y
///    queda suelto, con icono en el Dock o en la barra de tareas;
///  - el motor no siempre se cierra pese a `QuitAfterGameStop`, y su socket de
///    instancia única hace que el siguiente `--launch` no haga nada.
///
/// Solo tocamos procesos cuya línea de comandos apunta **dentro de nuestra carpeta**,
/// y solo si ya nadie los sostiene. Nunca matamos una partida en curso.
pub fn kill_orphans(root: &Path) -> usize {
    let Some(root) = root.to_str() else { return 0 };
    let ps = procesos();
    let vivos: Vec<u32> = ps.iter().map(|p| p.pid).collect();
    let yo = std::process::id();

    let mut killed = 0;
    for p in &ps {
        // Windows no reparenta: el huérfano se reconoce porque su padre ya no está en
        // la lista. Si ese PID fue reciclado por otro proceso vivo, este no se dará por
        // huérfano y no se limpiará — se prefiere eso a matar de más.
        let huerfano = if cfg!(windows) { !vivos.contains(&p.ppid) } else { p.ppid == 1 };
        if !is_killable(&p.args, huerfano, root, yo, p.pid) {
            continue;
        }
        if matar(p.pid) {
            killed += 1;
        }
    }
    killed
}

/// Lee un `.mrpack` sin instalarlo: para enseñar qué contiene antes de confirmar.
pub fn inspect(mrpack: &Path) -> Result<serde_json::Value, InstallError> {
    let bytes = std::fs::read(mrpack)?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes))?;
    let index = {
        let mut f = zip.by_name("modrinth.index.json").map_err(|_| InstallError::NoIndex)?;
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        pack::parse_index(&s)?
    };
    let icon = zip.by_name("icon.png").ok().and_then(|mut f| {
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok().filter(|_| !buf.is_empty()).map(|_| png_data_url(&buf))
    });
    let loader = index
        .dependencies
        .iter()
        .find(|(k, _)| *k != "minecraft")
        .map(|(k, v)| format!("{} {}", k.replace("-loader", ""), v));

    Ok(serde_json::json!({
        "name": index.name,
        "version": index.version_id,
        "minecraft": index.dependencies.get("minecraft"),
        "loader": loader,
        "files": index.files.len(),
        "bytes": index.files.iter().map(|f| f.file_size).sum::<u64>(),
        "icon": icon,
    }))
}

pub fn png_data_url(bytes: &[u8]) -> String {
    use base64::Engine;
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Deja escrita la configuración que hace del motor un proceso mudo.
pub fn apply_silent_config(layout: &Layout) {
    let cfg = layout.engine().join("elyprismlauncher.cfg");
    let _ = std::fs::create_dir_all(layout.engine());
    let previo = std::fs::read_to_string(&cfg).unwrap_or_default();
    let _ = std::fs::write(&cfg, engine::config_muda(&previo));

    // Y las marcas de «no migrar». Si el motor encuentra la carpeta de otro launcher en
    // la máquina —y el suyo propio, instalado aparte, cuenta como uno— pregunta con un
    // diálogo modal si quiere traerse esos datos a la nuestra. Escondido, ese diálogo
    // dejó a un amigo con la descarga girando para siempre (2026-09-25): el motor
    // esperaba una respuesta que nadie podía darle.
    //
    // El archivo es el mismo que escribe el motor cuando alguien contesta que no, así
    // que esto es decirle que no de antemano. Y es lo que queremos: su carpeta es suya,
    // la nuestra la administramos nosotros.
    for nombre in engine::MIGRACIONES {
        let marca = layout.engine().join(format!("{nombre}_nomigrate.txt"));
        if !marca.exists() {
            let _ = std::fs::write(&marca, b"");
        }
    }
}

/// ¿Ya hay un motor corriendo sobre nuestra carpeta de datos?
///
/// El nombre del ejecutable es el mismo en las tres plataformas (con `.exe` en
/// Windows), así que basta con buscarlo dentro de una línea que además apunte a
/// nuestra raíz: el ElyPrism que alguien tenga instalado aparte no coincide.
fn engine_running(root: &Path) -> bool {
    engine_pid(root).is_some()
}

/// PID del motor que corre sobre nuestra carpeta, si hay uno.
///
/// Nunca el de la partida: su línea de comandos también apunta a nuestra carpeta.
pub fn engine_pid(root: &Path) -> Option<u32> {
    let root = root.to_str()?;
    procesos()
        .into_iter()
        .find(|p| {
            !p.args.contains(GAME_MAIN_CLASS)
                && en_nuestra_carpeta(&p.args, root)
                && p.args.to_lowercase().contains("elyprismlauncher")
        })
        .map(|p| p.pid)
}

/// Arranca el motor en segundo plano, sin `--launch`, para que refresque los
/// tokens de cuenta antes de que el usuario pulse Jugar.
///
/// Sin esto el lanzamiento alcanza a `LaunchController::login()` mientras el
/// refresco sigue en curso, y el motor abre su diálogo **"Please wait..."** a
/// esperarlo: son ~2.5 s de ventana ajena en cada partida. Con el motor ya
/// caliente, el `--launch` posterior se lo entrega por el socket de instancia
/// única y no queda nada que esperar.
///
/// Comprobado en macOS: arranca sin ventana principal y sin icono en el Dock.
///
/// **En Windows no se hace.** Medido contra el release 11.1.0: arrancado sin
/// `--launch`, el motor abre su ventana principal («PineconeMC - PineconeMC 11.1.0»)
/// tanto en la primera corrida como en las siguientes, y con la config muda ya
/// escrita. Aquí no existe equivalente de `LSUIElement`, así que precalentar costaría
/// una ventana ajena encima del launcher durante toda la sesión — `QuitAfterGameStop`
/// la mantiene viva hasta que se juega. El precio de renunciar es el diálogo
/// «Please wait...» de ~2.5 s en el primer Jugar; mucho más barato que lo otro.
pub fn warm_engine(layout: &Layout) {
    if cfg!(windows) {
        return;
    }
    let bin = engine::engine_binary(&layout.engine_app());
    if !bin.exists() || engine_running(&layout.root) {
        return;
    }
    set_dock_visibility(&layout.engine_app(), true);
    apply_silent_config(layout);
    let _ = std::process::Command::new(&bin)
        .args(["--dir", &layout.engine().to_string_lossy()])
        .current_dir(layout.engine())
        .spawn();
}

// ---------------------------------------------------------------- modpack

/// Construye la instancia a partir de un `.mrpack` local.
///
/// Lo hacemos nosotros porque `--import` del motor no acepta modpacks (solo recursos
/// sueltos). Ver `docs/arquitectura.md`.
pub async fn install_pack(
    layout: &Layout,
    instance_id: &str,
    mrpack: &Path,
    settings: &engine::PackSettings,
    progress: Progress<'_>,
) -> Result<(), InstallError> {
    let dir = layout.instance(instance_id);
    let minecraft = dir.join("minecraft"); // el motor guarda el juego aquí dentro
    std::fs::create_dir_all(&minecraft)?;

    progress("Leyendo el pack", 0.0);
    let bytes = std::fs::read(mrpack)?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes))?;

    let index: MrpackIndex = {
        let mut f = zip
            .by_name("modrinth.index.json")
            .map_err(|_| InstallError::NoIndex)?;
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        pack::parse_index(&s)?
    };

    // 0. icono del pack, si lo trae y no hay ya uno. El formato no lo exige y el motor
    //    lo ignora, pero es lo que Dino Launcher enseña en vez de un cuadro genérico.
    //
    //    «Si no hay ya uno» porque el que haya puede ser el que eligió quien publicó el
    //    pack, que el catálogo sirve aparte: el del `.mrpack` es el original y lo
    //    pisaría en cada instalación. El precio es que reimportar a mano un `.mrpack`
    //    con otro icono conserva el anterior; quitar el pack borrando archivos lo limpia.
    if !layout.pack_icon(instance_id).exists() {
        if let Ok(mut f) = zip.by_name("icon.png") {
            let mut buf = Vec::new();
            if f.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                let _ = std::fs::create_dir_all(layout.packs());
                let _ = std::fs::write(layout.pack_icon(instance_id), buf);
            }
        }
    }

    // 1. overrides -> respetando lo que ya es del jugador
    progress("Copiando configuración", 0.05);
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        if f.is_dir() {
            continue;
        }
        let Some(name) = f.enclosed_name().map(|p| p.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(rel) = name
            .strip_prefix("overrides/")
            .or_else(|| name.strip_prefix("client-overrides/"))
        else {
            continue;
        };
        let rel = pack::safe_relative(rel)?;
        let out = minecraft.join(&rel);
        // Los archivos "seed" son del jugador: solo se escriben la primera vez.
        if pack::is_seed(&rel) && out.exists() {
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        std::fs::write(&out, buf)?;
    }

    // 2. mods y demás archivos indexados, por URL y verificados por hash
    let total = index.files.len().max(1);
    for (n, file) in index.files.iter().enumerate() {
        let rel = pack::safe_relative(&file.path)?;
        let out = minecraft.join(&rel);
        let frac = 0.1 + 0.85 * (n as f32 / total as f32);

        if out.exists() {
            if let (Ok(existing), Some(want)) = (std::fs::read(&out), file.sha512()) {
                if hex512(&existing) == want {
                    continue; // ya está y es el correcto
                }
            }
        }
        let Some(url) = file.downloads.first() else {
            continue;
        };
        progress(
            &format!("Descargando mods · {} de {}", n + 1, total),
            frac,
        );
        let data = get_bytes(url).await?;
        if let Some(want) = file.sha512() {
            if hex512(&data) != want {
                return Err(InstallError::BadHash { archivo: rel });
            }
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&out, data)?;
    }

    // 3. configuración de la instancia
    progress("Configurando la instancia", 0.97);
    std::fs::write(dir.join("mmc-pack.json"), pack::mmc_pack(&index.dependencies)?)?;
    std::fs::write(dir.join("instance.cfg"), engine::instance_cfg(settings))?;

    progress("Listo", 1.0);
    Ok(())
}

fn hex512(bytes: &[u8]) -> String {
    let mut h = Sha512::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/Users/x/Library/Application Support/gg.dinolab.launcher";
    /// La misma raíz tal como la escribe Windows.
    #[cfg(windows)]
    const ROOT_WIN: &str = r"C:\Users\Pici\AppData\Roaming\gg.dinolab.launcher";

    #[test]
    fn una_pregunta_del_motor_nunca_se_esconde() {
        // El diálogo de migración que dejó colgado a un amigo: un `QMessageBox` titulado
        // solo con el nombre del launcher.
        for titulo in ["PineconeMC", "Update In Progress", "Error", ""] {
            assert!(!se_puede_esconder(titulo), "{titulo:?} podría estar preguntando algo");
        }
    }

    #[test]
    fn el_dialogo_de_progreso_y_la_ventana_principal_si_se_esconden() {
        // Los títulos reales vistos en el log de una partida.
        for titulo in [
            "Please wait... - PineconeMC 11.1.0",
            "PineconeMC - PineconeMC 11.1.0",
        ] {
            assert!(se_puede_esconder(titulo), "{titulo:?} es del motor y estorba");
        }
    }

    #[test]
    fn la_config_muda_desactiva_las_migraciones() {
        let root = std::env::temp_dir().join("dino-nomigrate");
        let _ = std::fs::remove_dir_all(&root);
        let layout = Layout::new(&root);
        apply_silent_config(&layout);
        for nombre in engine::MIGRACIONES {
            let marca = layout.engine().join(format!("{nombre}_nomigrate.txt"));
            assert!(marca.exists(), "sin marca para {nombre}: el motor preguntaría");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn jamas_matamos_una_partida_en_curso() {
        // Si el motor muriera, el juego queda huérfano bajo nuestra carpeta.
        // Aun así no se toca: hay alguien jugando.
        let game = format!("java -Xmx3072m -cp {ROOT}/engine/libraries/... org.prismlauncher.EntryPoint");
        assert!(!is_killable(&game, true, ROOT, 999, 500));
    }

    #[test]
    fn si_matamos_la_basura_que_dejamos() {
        let leftover = format!("{ROOT}/engine/java/java-runtime-epsilon/bin/java -jar CrashAssistant.jar");
        assert!(is_killable(&leftover, true, ROOT, 999, 500));
    }

    #[test]
    fn no_tocamos_lo_ajeno_ni_lo_vivo() {
        // Un Java de otro launcher, fuera de nuestra carpeta.
        assert!(!is_killable("/opt/other/java -jar cosa.jar", true, ROOT, 999, 500));
        // Nuestro, pero sostenido por un padre vivo: lo está usando alguien.
        let ours = format!("{ROOT}/bin/elyprismlauncher --dir x");
        assert!(!is_killable(&ours, false, ROOT, 999, 500));
        // Y nunca a nosotros mismos.
        let me = format!("{ROOT}/Dino Launcher.app/Contents/MacOS/dino-launcher");
        assert!(!is_killable(&me, true, ROOT, 999, 999));
    }

    /// En Windows la línea de comandos no vuelve como la escribimos: Qt arma el
    /// classpath con `/` aunque `--dir` lleve `\`, y el disco no distingue mayúsculas.
    /// Comparando literal, nada de lo nuestro se reconocería y la limpieza —la que
    /// destraba el siguiente `--launch`— no haría absolutamente nada.
    #[cfg(windows)]
    #[test]
    fn en_windows_reconocemos_lo_nuestro_pese_a_barras_y_mayusculas() {
        let java = format!(
            "{}/engine/java/bin/java.exe -jar CrashAssistant.jar",
            ROOT_WIN.replace('\\', "/").to_lowercase()
        );
        assert!(en_nuestra_carpeta(&java, ROOT_WIN));
        assert!(is_killable(&java, true, ROOT_WIN, 999, 500));

        // Y con ese mismo formato la partida sigue siendo intocable.
        let game = format!(
            "{}/engine/libs/x org.prismlauncher.EntryPoint",
            ROOT_WIN.replace('\\', "/")
        );
        assert!(!is_killable(&game, true, ROOT_WIN, 999, 500));
    }

    /// Alguien puede tener su propio ElyPrism instalado: mismo nombre de ejecutable,
    /// otra carpeta. Matarlo sería cerrarle su launcher sin explicación.
    #[cfg(windows)]
    #[test]
    fn no_confundimos_el_elyprism_ajeno_con_el_nuestro() {
        let ajeno = r"C:\Program Files\ElyPrismLauncher\elyprismlauncher.exe --dir D:\mc";
        assert!(!en_nuestra_carpeta(ajeno, ROOT_WIN));
        assert!(!is_killable(ajeno, true, ROOT_WIN, 999, 500));
    }

    #[test]
    fn el_hash_coincide_con_sha512_conocido() {
        // SHA-512 de la cadena vacía.
        assert!(hex512(b"").starts_with("cf83e1357eefb8bd"));
        assert_eq!(hex512(b"").len(), 128);
    }

    #[test]
    fn hashes_distintos_para_contenidos_distintos() {
        assert_ne!(hex512(b"sodium.jar"), hex512(b"sodium.jar "));
    }

    /// Prueba de verdad: descarga el motor y arma la instancia con un `.mrpack` real.
    /// Toca la red y tarda; se corre a mano con `cargo test -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn instala_de_extremo_a_extremo() {
        let mrpack = std::path::PathBuf::from(
            std::env::var("DINO_MRPACK").expect("falta DINO_MRPACK"),
        );
        let root = std::env::temp_dir().join("dino-e2e");
        let _ = std::fs::remove_dir_all(&root);
        let layout = Layout::new(&root);
        let log = |paso: &str, frac: f32| println!("  [{:>3.0}%] {paso}", frac * 100.0);

        let bin = ensure_engine(&layout, "11.1.0", &log).await.expect("motor");
        assert!(bin.exists(), "el ejecutable del motor no quedó en su sitio");

        let settings = engine::PackSettings {
            name: "Fabulously Optimized".into(),
            pack_id: "fabulously-optimized".into(),
            version: "1.0.0".into(),
            min_mem: 2048,
            max_mem: 6144,
            jvm_args: "-XX:+UseG1GC".into(),
            server: Some("mcpvp.club".into()),
        };
        install_pack(&layout, "fabulously-optimized", &mrpack, &settings, &log)
            .await
            .expect("instalación");

        let inst = layout.instance("fabulously-optimized");
        let mc = inst.join("minecraft");

        let cfg = std::fs::read_to_string(inst.join("instance.cfg")).unwrap();
        assert!(cfg.contains("JoinServerOnLaunchAddress=mcpvp.club"));
        assert!(cfg.contains("MaxMemAlloc=6144"));

        let mmc = std::fs::read_to_string(inst.join("mmc-pack.json")).unwrap();
        assert!(mmc.contains("net.fabricmc.fabric-loader"));
        assert!(mmc.contains("26.2"));

        let mods: Vec<_> = std::fs::read_dir(mc.join("mods")).unwrap().flatten().collect();
        println!("  mods instalados: {}", mods.len());
        assert!(mods.len() >= 45, "esperaba ~50 mods, hay {}", mods.len());
        assert!(mc.join("config").exists(), "faltaron los overrides");

        // Segunda pasada: no debe volver a descargar nada ni romper nada.
        install_pack(&layout, "fabulously-optimized", &mrpack, &settings, &log)
            .await
            .expect("reinstalación idempotente");
    }
}
