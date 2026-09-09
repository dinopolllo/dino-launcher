//! Orquestación de ElyPrismLauncher.
//!
//! Nunca modificamos su código: lo descargamos de sus releases y lo invocamos como
//! proceso aparte, siempre con `--dir` apuntando a nuestra propia carpeta para no
//! tocar cualquier Prism que el usuario ya tenga instalado.
//!
//! Las banderas están verificadas contra `launcher/Application.cpp` del upstream.

use std::path::{Path, PathBuf};

/// Dónde vive todo. `engine` es la raíz de datos que le pasamos a ElyPrism.
#[derive(Debug, Clone)]
pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    /// Raíz de datos de ElyPrism (`--dir`).
    pub fn engine(&self) -> PathBuf {
        self.root.join("engine")
    }
    pub fn instances(&self) -> PathBuf {
        self.engine().join("instances")
    }
    pub fn instance(&self, id: &str) -> PathBuf {
        self.instances().join(id)
    }
    /// Dónde vive el ejecutable del motor, aparte de sus datos.
    pub fn engine_app(&self) -> PathBuf {
        self.root.join("bin")
    }
    /// Lista de cuentas del motor. Si no existe, nadie ha iniciado sesión todavía.
    pub fn accounts(&self) -> PathBuf {
        self.engine().join("accounts.json")
    }
    /// `.mrpack` descargados del catálogo.
    pub fn packs(&self) -> PathBuf {
        self.root.join("packs")
    }
    /// Icono del pack, extraído del `.mrpack` si lo traía.
    pub fn pack_icon(&self, id: &str) -> PathBuf {
        self.packs().join(format!("{id}.png"))
    }
    pub fn catalog(&self) -> PathBuf {
        self.root.join("catalog.json")
    }
}

/// Con qué cuenta se entra al juego.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Account {
    /// Perfil ya registrado en el motor (Microsoft o Ely.by, da igual cuál).
    Profile(String),
    /// Sin conexión, con el nombre dado. ElyPrism lo permite solo si ya hay
    /// al menos una cuenta real registrada.
    Offline(String),
}

/// Argumentos para lanzar una instancia.
///
/// Deliberadamente **sin** `--show-window`: así el usuario nunca ve la ventana
/// del motor, le da a Jugar en Dino Launcher y lo siguiente es Minecraft.
pub fn launch_args(
    layout: &Layout,
    instance_id: &str,
    account: &Account,
    server: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "--dir".into(),
        path_arg(&layout.engine()),
        "--launch".into(),
        instance_id.to_string(),
        "--alive".into(),
    ];
    match account {
        Account::Profile(name) => {
            args.push("--profile".into());
            args.push(name.clone());
        }
        Account::Offline(name) => {
            args.push("--offline".into());
            args.push(name.clone());
        }
    }
    if let Some(addr) = server {
        args.push("--server".into());
        args.push(addr.to_string());
    }
    args
}

/// Argumentos para importar un `.mrpack`.
///
/// `ModrinthInstanceCreationTask` del motor resuelve el índice, descarga cada mod
/// por su hash y aplica los `overrides/`. Nosotros no tocamos nada de eso.
pub fn import_args(layout: &Layout, pack: &Path) -> Vec<String> {
    vec![
        "--dir".into(),
        path_arg(&layout.engine()),
        "--import".into(),
        path_arg(pack),
    ]
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Ajustes que el catálogo fija para una instancia.
#[derive(Debug, Clone)]
pub struct PackSettings {
    pub name: String,
    pub pack_id: String,
    pub version: String,
    /// MiB.
    pub min_mem: u32,
    pub max_mem: u32,
    pub jvm_args: String,
    /// Si está, la instancia entra sola al servidor.
    pub server: Option<String>,
}

/// Rinde el `instance.cfg` (INI) con los overrides del pack.
///
/// Solo escribimos las claves que administramos. El resto lo deja el motor,
/// que es justamente lo que protege los ajustes propios de cada quien.
pub fn instance_cfg(s: &PackSettings) -> String {
    let mut out = String::from("[General]\nInstanceType=OneSix\n");
    out.push_str(&format!("name={}\n", s.name));

    out.push_str("OverrideMemory=true\n");
    out.push_str(&format!("MinMemAlloc={}\n", s.min_mem));
    out.push_str(&format!("MaxMemAlloc={}\n", s.max_mem));

    // Por defecto NO tocamos los argumentos de la JVM. Minecraft 26.2 trae los suyos
    // en el manifiesto (Shenandoah generacional), y añadir otro `-XX:+Use*GC` encima
    // hace que la JVM aborte con "Multiple garbage collectors selected".
    if s.jvm_args.is_empty() {
        out.push_str("OverrideJavaArgs=false\n");
    } else {
        out.push_str("OverrideJavaArgs=true\n");
        out.push_str(&format!("JvmArgs={}\n", s.jvm_args));
    }

    // El motor pregunta con un diálogo modal cuando macOS reporta presión de memoria.
    // Con su ventana oculta ese diálogo bloquea el lanzamiento sin que nadie lo vea.
    out.push_str("LowMemWarning=false\n");

    match &s.server {
        Some(addr) => {
            out.push_str("JoinServerOnLaunch=true\n");
            out.push_str(&format!("JoinServerOnLaunchAddress={}\n", addr));
        }
        None => out.push_str("JoinServerOnLaunch=false\n"),
    }

    // Campos de upstream: dejan rastro de qué pack y versión originó la instancia,
    // que es exactamente lo que necesita la detección de actualizaciones.
    out.push_str("ManagedPack=true\n");
    out.push_str("ManagedPackType=modrinth\n");
    out.push_str(&format!("ManagedPackID={}\n", s.pack_id));
    out.push_str(&format!("ManagedPackName={}\n", s.name));
    out.push_str(&format!("ManagedPackVersionID={}\n", s.version));
    out.push_str(&format!("ManagedPackVersionName={}\n", s.version));

    out
}

/// Flags de la JVM que HotSpot considera experimentales: sin
/// `-XX:+UnlockExperimentalVMOptions` delante, la máquina virtual **no arranca**
/// y el motor solo reporta un genérico "Game crashed".
const EXPERIMENTAL_FLAGS: [&str; 4] = [
    "G1NewSizePercent",
    "G1MaxNewSizePercent",
    "G1MixedGCLiveThresholdPercent",
    "G1RSetUpdatingPauseTimePercent",
];

/// Añade el desbloqueo si hace falta, y solo si hace falta.
pub fn normalize_jvm_args(args: &str) -> String {
    let needs = EXPERIMENTAL_FLAGS.iter().any(|f| args.contains(f));
    if !needs || args.contains("UnlockExperimentalVMOptions") {
        return args.to_string();
    }
    format!("-XX:+UnlockExperimentalVMOptions {args}")
}

/// Mezcla claves en un INI de Qt conservando lo que ya hubiera.
///
/// El motor guarda ahí sus propios ajustes; sobrescribir el archivo entero
/// borraría cosas del usuario.
pub fn merge_ini(existing: &str, kv: &[(&str, &str)]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut has_header = false;

    for line in existing.lines() {
        let t = line.trim();
        if t == "[General]" {
            has_header = true;
        }
        match t.split_once('=') {
            Some((k, _)) if kv.iter().any(|(key, _)| *key == k.trim()) => {
                let key = k.trim();
                let val = kv.iter().find(|(kk, _)| *kk == key).unwrap().1;
                lines.push(format!("{key}={val}"));
                seen.push(kv.iter().find(|(kk, _)| *kk == key).unwrap().0);
            }
            _ => lines.push(line.to_string()),
        }
    }
    if !has_header {
        lines.insert(0, "[General]".into());
    }
    for (k, v) in kv {
        if !seen.contains(k) {
            lines.push(format!("{k}={v}"));
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Ajustes que hacen del motor un proceso invisible.
///
/// `ShowConsoleOnError` viene en `true` de fábrica: es lo que hace aparecer la
/// ventana del motor cuando el juego falla.
pub const SILENT_ENGINE: [(&str, &str); 5] = [
    ("ShowConsole", "false"),
    ("ShowConsoleOnError", "false"),
    ("AutoCloseConsole", "true"),
    ("CloseAfterLaunch", "true"),
    ("QuitAfterGameStop", "true"),
];

/// RAM física de la máquina, en MiB. Sin dependencias: cada sistema tiene su vía.
pub fn total_memory_mb() -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl").args(["-n", "hw.memsize"]).output().ok()?;
        let bytes: u64 = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
        return Some((bytes / 1024 / 1024) as u32);
    }
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = s.lines().find(|l| l.starts_with("MemTotal:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        return Some((kb / 1024) as u32);
    }
    #[cfg(target_os = "windows")]
    {
        // Vía API y no PowerShell a propósito. `Command::new("powershell")` desde una
        // app sin terminal abre una consola negra durante casi un segundo, y esto se
        // llama al abrir la ventana y en cada Jugar: se vería siempre.
        //
        // `GetPhysicallyInstalledSystemMemory` es el equivalente exacto de `hw.memsize`:
        // devuelve la RAM **instalada**, no la que queda tras las reservas del firmware
        // y la gráfica integrada. Los umbrales de `recommended_memory` están calibrados
        // sobre ese número; con el otro, una máquina de 16 GB que reporta 14.8 GB
        // utilizables caería un escalón y jugaría con menos memoria de la que puede.

        // La estructura tiene que estar completa aunque solo miremos `total_phys`:
        // el sistema escribe los nueve campos y `length` le dice cuántos caben.
        #[repr(C)]
        #[allow(dead_code)]
        struct MemoryStatusEx {
            length: u32,
            memory_load: u32,
            total_phys: u64,
            avail_phys: u64,
            total_page_file: u64,
            avail_page_file: u64,
            total_virtual: u64,
            avail_virtual: u64,
            avail_extended_virtual: u64,
        }
        #[link(name = "kernel32")]
        #[allow(non_snake_case)] // los nombres son los de la API, no se traducen
        extern "system" {
            fn GetPhysicallyInstalledSystemMemory(total_kb: *mut u64) -> i32;
            fn GlobalMemoryStatusEx(buf: *mut MemoryStatusEx) -> i32;
        }

        let mut kb: u64 = 0;
        if unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) } != 0 && kb > 0 {
            return Some((kb / 1024) as u32);
        }
        // Falla cuando el SMBIOS viene mal formado, cosa habitual en máquinas
        // virtuales. Ahí el total que ve el kernel es lo mejor que hay.
        let mut st = MemoryStatusEx {
            length: std::mem::size_of::<MemoryStatusEx>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        if unsafe { GlobalMemoryStatusEx(&mut st) } != 0 && st.total_phys > 0 {
            return Some((st.total_phys / 1024 / 1024) as u32);
        }
        return None;
    }
    #[allow(unreachable_code)]
    None
}

/// Cuánta RAM darle al juego según la que tenga la máquina, en MiB.
///
/// Pedir más de la que hay no acelera nada: hace que el sistema empiece a
/// intercambiar a disco, y en macOS además dispara el aviso de presión de memoria.
/// Siempre dejamos margen para el sistema operativo y el navegador del usuario.
pub fn recommended_memory(total_mb: u32) -> (u32, u32) {
    let max = match total_mb {
        t if t >= 32000 => 8192,
        t if t >= 16000 => 6144,
        t if t >= 12000 => 4096,
        t if t >= 8000 => 3072,
        t if t >= 6000 => 2048,
        _ => 1536,
    };
    (1024.min(max), max)
}

/// Repositorio del motor.
pub const ENGINE_REPO: &str = "ElyPrismLauncher/Launcher";

/// Ejecutable del motor dentro de su carpeta.
///
/// Verificado contra el release 11.1.0, leyendo el directorio central de cada zip:
///  - macOS: el bundle se llama `ElyPrismLauncher.app` pero el binario va en
///    **minúsculas**, y es universal (x86_64 + arm64).
///  - Windows: el portable descomprime **sin carpeta contenedora**, y el ejecutable
///    lleva la marca del fork: `elyprismlauncher.exe`, no `prismlauncher.exe`.
pub fn engine_binary(dir: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    return dir.join("ElyPrismLauncher.app/Contents/MacOS/elyprismlauncher");
    #[cfg(target_os = "windows")]
    return dir.join("elyprismlauncher.exe");
    #[cfg(all(unix, not(target_os = "macos")))]
    return dir.join("ElyPrismLauncher.AppImage");
}

/// Asset del release que le toca a esta plataforma.
///
/// Los releases se publican bajo la marca **PineconeMC**. Elegimos siempre las
/// variantes *portable*: se descomprimen en nuestra carpeta y no instalan nada
/// en el sistema. Los AppImage de Linux no llevan la versión en el nombre.
pub fn asset_name(version: &str, os: &str, arch: &str) -> Option<String> {
    Some(match (os, arch) {
        // Universal: sirve para Intel y Apple Silicon.
        ("macos", _) => format!("PineconeMC-macOS-{version}.zip"),
        ("windows", "aarch64") => {
            format!("PineconeMC-Windows-MSVC-arm64-Portable-{version}.zip")
        }
        ("windows", "x86_64") => format!("PineconeMC-Windows-MSVC-Portable-{version}.zip"),
        ("linux", "aarch64") => "PineconeMC-Linux-aarch64.AppImage".into(),
        ("linux", "x86_64") => "PineconeMC-Linux-x86_64.AppImage".into(),
        _ => return None,
    })
}

/// El asset de esta máquina.
pub fn asset_for_host(version: &str) -> Option<String> {
    asset_name(version, std::env::consts::OS, std::env::consts::ARCH)
}

pub fn asset_url(version: &str, asset: &str) -> String {
    format!("https://github.com/{ENGINE_REPO}/releases/download/{version}/{asset}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::new("/tmp/dino")
    }

    /// El valor que sigue a una bandera.
    ///
    /// Las rutas se rinden con el separador del sistema, así que compararlas contra
    /// una cadena POSIX pegada a mano solo funciona en macOS y Linux. Extraer el
    /// argumento y contrastarlo con el mismo `Layout` prueba lo mismo en las tres.
    fn arg_tras<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        let i = args.iter().position(|a| a == flag)?;
        args.get(i + 1).map(String::as_str)
    }

    #[test]
    fn lanzar_nunca_muestra_la_ventana_del_motor() {
        let a = launch_args(
            &layout(),
            "dino-smp",
            &Account::Profile("jugador".into()),
            None,
        );
        assert!(!a.iter().any(|x| x == "--show-window"));
    }

    #[test]
    fn lanzar_aisla_los_datos_y_pasa_la_instancia() {
        let a = launch_args(
            &layout(),
            "dino-smp",
            &Account::Profile("jugador".into()),
            Some("mc.dinolab.gg"),
        );
        assert_eq!(arg_tras(&a, "--dir"), Some(path_arg(&layout().engine()).as_str()));
        assert_eq!(arg_tras(&a, "--launch"), Some("dino-smp"));
        assert_eq!(arg_tras(&a, "--profile"), Some("jugador"));
        assert_eq!(arg_tras(&a, "--server"), Some("mc.dinolab.gg"));
    }

    #[test]
    fn cuenta_offline_usa_su_propia_bandera() {
        let a = launch_args(&layout(), "dino-smp", &Account::Offline("Kevs".into()), None);
        assert_eq!(arg_tras(&a, "--offline"), Some("Kevs"));
        assert!(!a.iter().any(|x| x == "--profile"));
    }

    #[test]
    fn importar_apunta_al_mrpack() {
        let pack = layout().packs().join("dino-smp-4.3.1.mrpack");
        let a = import_args(&layout(), &pack);
        assert_eq!(
            a,
            vec![
                "--dir".to_string(),
                path_arg(&layout().engine()),
                "--import".to_string(),
                path_arg(&pack),
            ]
        );
    }

    fn settings() -> PackSettings {
        PackSettings {
            name: "Dino SMP".into(),
            pack_id: "dino-smp".into(),
            version: "4.3.1".into(),
            min_mem: 2048,
            max_mem: 6144,
            jvm_args: "-XX:+UseG1GC -XX:MaxGCPauseMillis=50".into(),
            server: Some("mc.dinolab.gg".into()),
        }
    }

    #[test]
    fn el_cfg_fija_memoria_java_y_servidor() {
        let cfg = instance_cfg(&settings());
        assert!(cfg.contains("OverrideMemory=true"));
        assert!(cfg.contains("MaxMemAlloc=6144"));
        assert!(cfg.contains("OverrideJavaArgs=true"));
        assert!(cfg.contains("JvmArgs=-XX:+UseG1GC -XX:MaxGCPauseMillis=50"));
        assert!(cfg.contains("JoinServerOnLaunchAddress=mc.dinolab.gg"));
        assert!(cfg.contains("ManagedPackVersionID=4.3.1"));
    }

    #[test]
    fn sin_servidor_no_se_autoconecta() {
        let mut s = settings();
        s.server = None;
        let cfg = instance_cfg(&s);
        assert!(cfg.contains("JoinServerOnLaunch=false"));
        assert!(!cfg.contains("JoinServerOnLaunchAddress"));
    }

    #[test]
    fn la_memoria_deja_sitio_al_sistema() {
        // Una máquina de 8 GB no puede darle 6 al juego: es la de Yahir y así fallaba.
        let (min, max) = recommended_memory(8192);
        assert_eq!(max, 3072);
        assert!(min <= max);
        assert!(8192 - max >= 4096, "debe quedar RAM para el sistema");

        assert_eq!(recommended_memory(16384).1, 6144);
        assert_eq!(recommended_memory(4096).1, 1536, "en máquinas chicas, poco");
        assert_eq!(recommended_memory(65536).1, 8192, "y con tope: más no ayuda");
    }

    #[test]
    fn el_aviso_de_memoria_queda_apagado() {
        // Ese diálogo es modal; con la ventana del motor oculta, cuelga el lanzamiento.
        assert!(instance_cfg(&settings()).contains("LowMemWarning=false"));
    }

    #[test]
    fn sin_args_propios_no_pisamos_los_del_juego() {
        let mut st = settings();
        st.jvm_args = String::new();
        let cfg = instance_cfg(&st);
        assert!(cfg.contains("OverrideJavaArgs=false"));
        assert!(!cfg.contains("JvmArgs="), "no debe escribir args vacíos");
        // La memoria sí es nuestra: eso no choca con nada.
        assert!(cfg.contains("MaxMemAlloc=6144"));
    }

    #[test]
    fn desbloquea_los_flags_experimentales() {
        // Sin el desbloqueo, esto impide que arranque la JVM entera.
        let a = normalize_jvm_args("-XX:+UseG1GC -XX:G1NewSizePercent=30");
        assert!(a.starts_with("-XX:+UnlockExperimentalVMOptions "));
        assert!(a.contains("-XX:G1NewSizePercent=30"));
    }

    #[test]
    fn no_toca_los_args_que_ya_estan_bien() {
        let plain = "-XX:+UseG1GC -XX:MaxGCPauseMillis=50";
        assert_eq!(normalize_jvm_args(plain), plain, "no hay flags experimentales");

        let ya = "-XX:+UnlockExperimentalVMOptions -XX:G1NewSizePercent=30";
        assert_eq!(normalize_jvm_args(ya), ya, "no debe duplicar el desbloqueo");
    }

    #[test]
    fn el_ini_conserva_lo_que_no_es_nuestro() {
        let previo = "[General]\nLanguage=es_MX\nShowConsole=true\nTheme=dark\n";
        let out = merge_ini(previo, &SILENT_ENGINE);
        assert!(out.contains("Language=es_MX"), "borró un ajuste ajeno");
        assert!(out.contains("Theme=dark"), "borró un ajuste ajeno");
        assert!(out.contains("ShowConsole=false"), "no actualizó la clave");
        assert!(out.contains("ShowConsoleOnError=false"), "no añadió la clave nueva");
        assert_eq!(out.matches("ShowConsole=").count(), 1, "duplicó la clave");
    }

    #[test]
    fn el_ini_se_crea_si_no_existia() {
        let out = merge_ini("", &SILENT_ENGINE);
        assert!(out.starts_with("[General]"));
        assert!(out.contains("QuitAfterGameStop=true"));
    }

    #[test]
    fn el_motor_vive_aparte_de_sus_datos() {
        let l = layout();
        assert!(l.engine_app().starts_with("/tmp/dino"));
        assert_ne!(l.engine_app(), l.engine(), "el binario no va dentro de la carpeta de datos");
        assert!(l.accounts().ends_with("engine/accounts.json"));
    }

    #[test]
    fn el_ejecutable_lleva_el_nombre_real_del_release() {
        // Leído del directorio central de cada zip del release 11.1.0. Equivocarse
        // aquí no rompe la compilación: descarga el motor entero y recién entonces
        // falla con "no encontré el ejecutable", que es el peor momento para enterarse.
        let name = engine_binary(Path::new("bin"))
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        #[cfg(target_os = "windows")]
        assert_eq!(name, "elyprismlauncher.exe");
        #[cfg(target_os = "macos")]
        assert_eq!(name, "elyprismlauncher");
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(name, "ElyPrismLauncher.AppImage");
    }

    #[test]
    fn cada_plataforma_recibe_su_asset() {
        // Nombres verificados contra el release 11.1.0.
        assert_eq!(
            asset_name("11.1.0", "macos", "aarch64").unwrap(),
            "PineconeMC-macOS-11.1.0.zip"
        );
        assert_eq!(
            asset_name("11.1.0", "macos", "x86_64").unwrap(),
            "PineconeMC-macOS-11.1.0.zip",
            "en macOS el binario es universal, no hay variante por arquitectura"
        );
        assert_eq!(
            asset_name("11.1.0", "windows", "x86_64").unwrap(),
            "PineconeMC-Windows-MSVC-Portable-11.1.0.zip"
        );
        assert_eq!(
            asset_name("11.1.0", "linux", "x86_64").unwrap(),
            "PineconeMC-Linux-x86_64.AppImage",
            "los AppImage no llevan la versión en el nombre"
        );
        assert!(asset_name("11.1.0", "freebsd", "x86_64").is_none());
    }

    #[test]
    fn siempre_elegimos_variantes_portable() {
        for (os, arch) in [("windows", "x86_64"), ("windows", "aarch64")] {
            let a = asset_name("11.1.0", os, arch).unwrap();
            assert!(a.contains("Portable"), "{a} debería ser portable");
            assert!(!a.ends_with(".exe"), "{a} no debe ser un instalador");
        }
    }

    #[test]
    fn la_url_apunta_al_release_correcto() {
        assert_eq!(
            asset_url("11.1.0", "PineconeMC-macOS-11.1.0.zip"),
            "https://github.com/ElyPrismLauncher/Launcher/releases/download/11.1.0/PineconeMC-macOS-11.1.0.zip"
        );
    }

    #[test]
    fn el_cfg_no_toca_ajustes_que_son_del_usuario() {
        // options.txt, controles y saves son "seed": el motor y el jugador mandan ahí.
        let cfg = instance_cfg(&settings());
        for k in ["options.txt", "OverrideCommands", "OverrideConsole"] {
            assert!(!cfg.contains(k), "el cfg no debería escribir {k}");
        }
    }
}
