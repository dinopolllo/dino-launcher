//! El motor (ElyPrismLauncher, publicado como PineconeMC) reducido a lo único que Dino 2.0
//! le pide: iniciar sesión con Microsoft y renovar esa sesión.
//!
//! Solo se descarga si alguien añade una cuenta Microsoft. Nunca lanza el juego (eso lo
//! hace el núcleo) y nunca se le deja una ventana delante del jugador salvo para iniciar
//! sesión, que es cuando hace falta verla.
//!
//! Disposición, la misma que en 1.x para que la migración no mueva nada:
//! `<datos>/bin/` el ejecutable, `<datos>/engine/` sus datos (`accounts.json`).

use dino_download::{Downloader, Item};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const ENGINE_VERSION: &str = "11.1.0";
const ENGINE_REPO: &str = "ElyPrismLauncher/Launcher";

/// SHA-256 de cada binario del release, tal como los publica GitHub para sus assets.
/// En 1.x el motor se descargaba y se ejecutaba **sin verificar**.
const ASSETS: [(&str, &str, &str, &str); 5] = [
    (
        "windows",
        "x86_64",
        "PineconeMC-Windows-MSVC-Portable-11.1.0.zip",
        "b13c5de4c638b71309071c917bcac445ebe49b4bb17e7012430347df53e1f80a",
    ),
    (
        "windows",
        "aarch64",
        "PineconeMC-Windows-MSVC-arm64-Portable-11.1.0.zip",
        "9c3db121caae1a1a323ab6c1f405c8d8c9fb24034d26e904299025464dae46f1",
    ),
    ("macos", "*", "PineconeMC-macOS-11.1.0.zip", "aae4ea1611de0a53c9b30d1be11c978b302fcfb4a59da27db36ee8718b8d1602"),
    (
        "linux",
        "x86_64",
        "PineconeMC-Linux-x86_64.AppImage",
        "9f8667afd37d08f307e9de1e4b601f5791d53cd217143d67afe442ad3eb03216",
    ),
    (
        "linux",
        "aarch64",
        "PineconeMC-Linux-aarch64.AppImage",
        "838f735383e7821d873a304461084c13f245e9bd2973aa27a81b6246495d29f6",
    ),
];

/// Ajustes que lo vuelven mudo (ver `engine.rs` de 1.x para el porqué de cada uno).
const SILENT: [(&str, &str); 7] = [
    ("ShowConsole", "false"),
    ("ShowConsoleOnError", "false"),
    ("AutoCloseConsole", "true"),
    ("CloseAfterLaunch", "false"),
    ("QuitAfterGameStop", "true"),
    ("AutomaticJavaDownload", "true"),
    ("IgnoreJavaWizard", "true"),
];
/// Solo si faltan: sin valor, el motor abre su asistente de primer arranque.
const DEFAULTS: [(&str, &str); 3] =
    [("Language", "es_ES"), ("ApplicationTheme", "system"), ("IconTheme", "pe_colored")];
/// Launchers de los que ofrecería migrar con un diálogo modal: se le dice que no de antemano.
const NO_MIGRATE: [&str; 3] = ["PolyMC", "MultiMC", "Prism Launcher"];

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("esta plataforma no tiene build del motor")]
    Unsupported,
    #[error(transparent)]
    Download(#[from] dino_download::DownloadError),
    #[error("el motor descargado no es el publicado (SHA-256 {got})")]
    BadHash { got: String },
    #[error("disco: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
}

pub struct Engine {
    root: PathBuf,
}

/// Lo que pasó al renovar.
#[derive(Debug, Clone, Default)]
pub struct Renewal {
    /// Cuentas Microsoft cuya sesión cambió.
    pub renewed: Vec<String>,
    /// Veces que la ventana del motor llegó a verse antes de esconderla.
    pub windows_hidden: u32,
    pub elapsed: Duration,
}

impl Engine {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("engine")
    }
    pub fn accounts(&self) -> PathBuf {
        self.data_dir().join("accounts.json")
    }

    pub fn binary(&self) -> PathBuf {
        let d = self.bin_dir();
        if cfg!(target_os = "macos") {
            d.join("ElyPrismLauncher.app/Contents/MacOS/elyprismlauncher")
        } else if cfg!(windows) {
            d.join("elyprismlauncher.exe")
        } else {
            d.join("ElyPrismLauncher.AppImage")
        }
    }

    pub fn installed(&self) -> bool {
        self.binary().exists()
    }

    /// Deja el motor instalado y verificado. No hace nada si ya está.
    pub async fn ensure(&self, dl: &Downloader) -> Result<PathBuf, EngineError> {
        let bin = self.binary();
        if bin.exists() {
            return Ok(bin);
        }
        let (asset, sha) = asset_for(std::env::consts::OS, std::env::consts::ARCH).ok_or(EngineError::Unsupported)?;
        let url = format!("https://github.com/{ENGINE_REPO}/releases/download/{ENGINE_VERSION}/{asset}");
        let tmp = self.root.join(format!("{asset}.descarga"));
        dl.fetch_all(vec![Item { url, dest: tmp.clone(), sha1: None, size: None, shared: false }], &|_| {}).await?;
        let got = sha256_file(&tmp)?;
        if got != sha {
            let _ = std::fs::remove_file(&tmp);
            return Err(EngineError::BadHash { got });
        }
        std::fs::create_dir_all(self.bin_dir())?;
        if asset.ends_with(".AppImage") {
            std::fs::rename(&tmp, &bin)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
            }
        } else {
            unzip(&tmp, &self.bin_dir())?;
            let _ = std::fs::remove_file(&tmp);
            // `portable.txt` le diría que guarde sus datos junto al ejecutable; `--dir` manda,
            // pero sin el archivo no queda ambigüedad (comprobado en 1.x).
            let _ = std::fs::remove_file(self.bin_dir().join("portable.txt"));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755));
            }
        }
        if !bin.exists() {
            return Err(EngineError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "el zip no trae el ejecutable",
            )));
        }
        Ok(bin)
    }

    /// Escribe la configuración muda y las marcas de «no migrar». Idempotente.
    pub fn silence(&self) -> std::io::Result<()> {
        let dir = self.data_dir();
        std::fs::create_dir_all(&dir)?;
        let cfg = dir.join("elyprismlauncher.cfg");
        let prev = std::fs::read_to_string(&cfg).unwrap_or_default();
        let next = merge_ini(&merge_ini_if_missing(&prev, &DEFAULTS), &SILENT);
        if next != prev {
            dino_core::schema::write_atomic(&cfg, next.as_bytes())?;
        }
        for name in NO_MIGRATE {
            let mark = dir.join(format!("{name}_nomigrate.txt"));
            if !mark.exists() {
                std::fs::write(mark, b"")?;
            }
        }
        Ok(())
    }

    fn command(&self) -> std::process::Command {
        let mut c = std::process::Command::new(self.binary());
        // Su registro va a sus propios archivos (`engine/logs`); aquí solo ensuciaría.
        c.arg("--dir")
            .arg(self.data_dir())
            .current_dir(self.data_dir())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        c
    }

    /// Abre el motor **visible** para que el jugador añada su cuenta Microsoft. Devuelve el
    /// proceso: al cerrarse, quien llama sincroniza las cuentas.
    pub fn open_for_login(&self) -> std::io::Result<std::process::Child> {
        self.silence()?;
        self.command().spawn()
    }

    /// Arranca el motor con su ventana escondida para que renueve las sesiones de Microsoft.
    ///
    /// Al arrancar, el motor renueva toda cuenta que no haya renovado en esa sesión
    /// (`MinecraftAccount::shouldRefresh`, 11.1.0). Se espera a ver cambiar la sesión de
    /// cada cuenta Microsoft en `accounts.json`, o hasta `timeout`, y se cierra.
    /// Medido en la etapa 2: 2,3 s, con la ventana escondida al primer intento.
    pub fn renew(&self, timeout: Duration) -> std::io::Result<Renewal> {
        self.silence()?;
        let before = msa_issued(&self.accounts());
        let t0 = Instant::now();
        let mut child = self.command().spawn()?;
        let pid = child.id();
        let mut out = Renewal::default();
        let mut next_check = Instant::now();
        while t0.elapsed() < timeout {
            out.windows_hidden += win::hide_all(pid);
            if Instant::now() >= next_check {
                next_check = Instant::now() + Duration::from_millis(250);
                let now = msa_issued(&self.accounts());
                out.renewed =
                    now.iter().filter(|(n, iat)| before.get(*n) != Some(iat)).map(|(n, _)| n.clone()).collect();
                if !before.is_empty() && out.renewed.len() >= before.len() {
                    // Un respiro para que termine de escribir antes de cerrarlo.
                    std::thread::sleep(Duration::from_millis(500));
                    break;
                }
            }
            if child.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        // Cierre normal (como pulsar la X); a la fuerza solo si no hace caso, y solo el
        // proceso que lanzamos.
        win::close_all(pid);
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait()?.is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        out.elapsed = t0.elapsed();
        Ok(out)
    }
}

/// Cuándo se emitió la sesión de cada cuenta Microsoft (`ygg.iat`), por nombre.
fn msa_issued(accounts: &Path) -> std::collections::HashMap<String, u64> {
    let Ok(s) = std::fs::read_to_string(accounts) else { return Default::default() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else { return Default::default() };
    v["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["type"].as_str() == Some("MSA"))
        .filter_map(|a| Some((a["profile"]["name"].as_str()?.to_string(), a["ygg"]["iat"].as_u64().unwrap_or(0))))
        .collect()
}

fn asset_for(os: &str, arch: &str) -> Option<(&'static str, &'static str)> {
    ASSETS.iter().find(|(o, a, _, _)| *o == os && (*a == "*" || *a == arch)).map(|(_, _, n, h)| (*n, *h))
}

fn sha256_file(p: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Descomprime sin dejar que una ruta del zip se salga de `dest`.
fn unzip(zip_path: &Path, dest: &Path) -> Result<(), EngineError> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    for i in 0..z.len() {
        let mut e = z.by_index(i)?;
        let Some(rel) = e.enclosed_name() else { continue };
        let out = dest.join(rel);
        if e.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut w = std::fs::File::create(&out)?;
        std::io::copy(&mut e, &mut w)?;
    }
    Ok(())
}

/// Pone `kv` en la sección `[General]` del INI, conservando todo lo demás.
fn merge_ini(existing: &str, kv: &[(&str, &str)]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut header = false;
    for line in existing.lines() {
        let t = line.trim();
        if t == "[General]" {
            header = true;
        }
        match t.split_once('=').and_then(|(k, _)| kv.iter().find(|(key, _)| *key == k.trim())) {
            Some((k, v)) => {
                lines.push(format!("{k}={v}"));
                seen.push(k);
            }
            None => lines.push(line.to_string()),
        }
    }
    if !header {
        lines.insert(0, "[General]".into());
    }
    for (k, v) in kv {
        if !seen.contains(k) {
            lines.push(format!("{k}={v}"));
        }
    }
    lines.join("\n") + "\n"
}

fn merge_ini_if_missing(existing: &str, kv: &[(&str, &str)]) -> String {
    let missing: Vec<(&str, &str)> = kv
        .iter()
        .copied()
        .filter(|(k, _)| {
            !existing
                .lines()
                .any(|l| matches!(l.trim().split_once('='), Some((key, v)) if key.trim() == *k && !v.trim().is_empty()))
        })
        .collect();
    merge_ini(existing, &missing)
}

/// Ventanas de un proceso. Solo hace algo en Windows: en macOS el motor arranca sin
/// ventana ni icono en el Dock cuando no se le pide.
mod win {
    #[cfg(windows)]
    mod imp {
        #[link(name = "user32")]
        extern "system" {
            fn EnumWindows(cb: extern "system" fn(isize, isize) -> i32, param: isize) -> i32;
            fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
            fn IsWindowVisible(hwnd: isize) -> i32;
            fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
            fn PostMessageW(hwnd: isize, msg: u32, w: usize, l: isize) -> i32;
        }

        struct Search {
            pid: u32,
            visible_only: bool,
            found: Vec<isize>,
        }

        extern "system" fn visit(hwnd: isize, param: isize) -> i32 {
            // SAFETY: `param` es el puntero a `Search` que pasa `windows_of`, vivo durante
            // toda la enumeración (EnumWindows es síncrona).
            let s = unsafe { &mut *(param as *mut Search) };
            let mut owner = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
            if owner == s.pid && (!s.visible_only || unsafe { IsWindowVisible(hwnd) } != 0) {
                s.found.push(hwnd);
            }
            1
        }

        fn windows_of(pid: u32, visible_only: bool) -> Vec<isize> {
            let mut s = Search { pid, visible_only, found: Vec::new() };
            unsafe { EnumWindows(visit, &mut s as *mut Search as isize) };
            s.found
        }

        pub fn hide_all(pid: u32) -> u32 {
            let w = windows_of(pid, true);
            for h in &w {
                unsafe { ShowWindow(*h, 0) }; // SW_HIDE
            }
            w.len() as u32
        }

        pub fn close_all(pid: u32) {
            for h in windows_of(pid, false) {
                unsafe { PostMessageW(h, 0x0010, 0, 0) }; // WM_CLOSE
            }
        }
    }

    #[cfg(not(windows))]
    mod imp {
        pub fn hide_all(_pid: u32) -> u32 {
            0
        }
        pub fn close_all(_pid: u32) {}
    }

    pub use imp::*;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cada_plataforma_tiene_su_binario_con_hash() {
        assert!(asset_for("windows", "x86_64").unwrap().0.contains("MSVC-Portable"));
        assert!(asset_for("windows", "aarch64").unwrap().0.contains("arm64"));
        assert!(asset_for("macos", "aarch64").unwrap().0.contains("macOS"));
        assert!(asset_for("linux", "x86_64").unwrap().0.ends_with(".AppImage"));
        assert!(asset_for("freebsd", "x86_64").is_none());
        assert!(ASSETS.iter().all(|a| a.3.len() == 64 && a.3.chars().all(|c| c.is_ascii_hexdigit())));
    }

    #[test]
    fn la_config_muda_respeta_lo_que_el_jugador_eligio() {
        let prev = "[General]\nLanguage=en_US\nShowConsole=true\nOtraCosa=1\n";
        let out = merge_ini(&merge_ini_if_missing(prev, &DEFAULTS), &SILENT);
        assert!(out.contains("Language=en_US"), "no se pisa el idioma elegido");
        assert!(out.contains("ShowConsole=false"), "lo mudo sí se impone");
        assert!(out.contains("OtraCosa=1") && out.contains("IgnoreJavaWizard=true"));
        assert_eq!(out.matches("[General]").count(), 1);
        let vacio = merge_ini(&merge_ini_if_missing("", &DEFAULTS), &SILENT);
        assert!(vacio.starts_with("[General]\n") && vacio.contains("Language=es_ES"));
    }

    #[test]
    fn silenciar_es_idempotente_y_deja_las_marcas() {
        let root = std::env::temp_dir().join(format!("dino-engine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let e = Engine::new(&root);
        e.silence().unwrap();
        let first = std::fs::read_to_string(e.data_dir().join("elyprismlauncher.cfg")).unwrap();
        e.silence().unwrap();
        assert_eq!(std::fs::read_to_string(e.data_dir().join("elyprismlauncher.cfg")).unwrap(), first);
        assert!(e.data_dir().join("Prism Launcher_nomigrate.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn la_sesion_de_cada_cuenta_microsoft_se_lee_sin_el_token() {
        let root = std::env::temp_dir().join(format!("dino-engine-iat-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let p = root.join("accounts.json");
        std::fs::write(&p, r#"{"accounts":[{"type":"Offline","profile":{"name":"a"}},{"type":"MSA","ygg":{"iat":42,"token":"x"},"profile":{"name":"b"}}]}"#).unwrap();
        let m = msa_issued(&p);
        assert_eq!(m.len(), 1);
        assert_eq!(m["b"], 42);
        let _ = std::fs::remove_dir_all(&root);
    }
}
