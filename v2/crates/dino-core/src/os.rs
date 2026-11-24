//! Lo poco que Dino necesita del sistema: RAM física, procesos y ventanas.
//!
//! En 1.x todo esto pasaba por PowerShell (≈ 400 ms por llamada y una consola negra si
//! no se tenía cuidado). Aquí son llamadas directas a la API de cada sistema.

use std::path::PathBuf;

/// RAM física instalada, en MiB (en Windows, la instalada y no la utilizable: los
/// umbrales de [`recommended_memory`] están calibrados sobre ella; ver 1.x `engine.rs`).
pub fn total_memory_mb() -> Option<u32> {
    imp::total_memory_mb()
}

/// Cuánta RAM darle al juego según la de la máquina: (mínima, máxima) en MiB. Siempre deja
/// margen al sistema; pedir más de la que hay no acelera nada, hace intercambiar a disco.
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

/// Espacio libre en el disco de `dir` (o del primer padre que exista), en bytes.
pub fn free_space(dir: &std::path::Path) -> Option<u64> {
    let mut d = dir.to_path_buf();
    while !d.exists() {
        if !d.pop() {
            return None;
        }
    }
    imp::free_space(&d)
}

/// Lo que hay que tener libre para instalar un pack de `pack_bytes`: el `.mrpack`, lo que
/// trae dentro y lo que se baja aparte, con margen. La misma regla que 1.x (×3,5).
pub fn space_needed(pack_bytes: u64) -> u64 {
    pack_bytes.saturating_mul(7) / 2
}

pub fn process_alive(pid: u32) -> bool {
    imp::process_alive(pid)
}

/// Ejecutable de un proceso vivo, para reconocer al nuestro tras reabrir Dino (un PID
/// suelto puede haberse reutilizado).
pub fn process_exe(pid: u32) -> Option<PathBuf> {
    imp::process_exe(pid)
}

/// Termina un proceso a la fuerza. Solo para los que lanzó Dino.
pub fn terminate(pid: u32) -> bool {
    imp::terminate(pid)
}

/// Esconde las ventanas visibles de un proceso. Devuelve cuántas. Solo en Windows.
pub fn hide_windows(pid: u32) -> u32 {
    imp::hide_windows(pid)
}

/// Pide cerrar las ventanas de un proceso, como pulsar la X. Devuelve cuántas.
/// En macOS y Linux manda `SIGTERM`, que el juego trata igual.
///
/// `include_hidden`: también las ocultas. Lo necesita el motor, cuya ventana Dino escondió;
/// el juego no: sus ventanas ocultas son internas (p. ej. el navegador CEF de algún mod) y
/// cerrarlas no es pedirle al juego que salga.
pub fn close_windows(pid: u32, include_hidden: bool) -> usize {
    imp::close_windows(pid, include_hidden)
}

/// ¿Tiene el proceso alguna ventana visible? Solo en Windows; fuera, `None`.
pub fn has_visible_window(pid: u32) -> Option<bool> {
    imp::has_visible_window(pid)
}

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;

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
    extern "system" {
        fn GetPhysicallyInstalledSystemMemory(total_kb: *mut u64) -> i32;
        fn GlobalMemoryStatusEx(buf: *mut MemoryStatusEx) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn CloseHandle(h: isize) -> i32;
        fn GetExitCodeProcess(h: isize, code: *mut u32) -> i32;
        fn TerminateProcess(h: isize, code: u32) -> i32;
        fn QueryFullProcessImageNameW(h: isize, flags: u32, buf: *mut u16, size: *mut u32) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(isize, isize) -> i32, param: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn PostMessageW(hwnd: isize, msg: u32, w: usize, l: isize) -> i32;
    }

    const PROCESS_TERMINATE: u32 = 0x0001;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;

    pub fn total_memory_mb() -> Option<u32> {
        let mut kb: u64 = 0;
        // SAFETY: puntero a un u64 válido.
        if unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) } != 0 && kb > 0 {
            return Some((kb / 1024) as u32);
        }
        // Falla con SMBIOS mal formado (máquinas virtuales): el total del kernel sirve.
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
        // SAFETY: estructura completa con `length` puesto, como pide la API.
        if unsafe { GlobalMemoryStatusEx(&mut st) } != 0 && st.total_phys > 0 {
            return Some((st.total_phys / 1024 / 1024) as u32);
        }
        None
    }

    fn with_process<T>(pid: u32, access: u32, f: impl FnOnce(isize) -> T) -> Option<T> {
        // SAFETY: el handle se cierra siempre antes de salir.
        let h = unsafe { OpenProcess(access, 0, pid) };
        if h == 0 {
            return None;
        }
        let out = f(h);
        unsafe { CloseHandle(h) };
        Some(out)
    }

    pub fn free_space(dir: &std::path::Path) -> Option<u64> {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetDiskFreeSpaceExW(dir: *const u16, free: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
        }
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let (mut free, mut total, mut tf) = (0u64, 0u64, 0u64);
        // SAFETY: cadena terminada en cero y punteros a u64 válidos.
        (unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut tf) } != 0).then_some(free)
    }

    pub fn process_alive(pid: u32) -> bool {
        with_process(pid, PROCESS_QUERY_LIMITED_INFORMATION, |h| {
            let mut code = 0u32;
            let ok = unsafe { GetExitCodeProcess(h, &mut code) };
            ok != 0 && code == STILL_ACTIVE
        })
        .unwrap_or(false)
    }

    pub fn process_exe(pid: u32) -> Option<PathBuf> {
        with_process(pid, PROCESS_QUERY_LIMITED_INFORMATION, |h| {
            let mut buf = vec![0u16; 1024];
            let mut n = buf.len() as u32;
            // SAFETY: buffer y tamaño coherentes.
            (unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut n) } != 0)
                .then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n as usize])))
        })
        .flatten()
    }

    pub fn terminate(pid: u32) -> bool {
        with_process(pid, PROCESS_TERMINATE, |h| (unsafe { TerminateProcess(h, 1) }) != 0).unwrap_or(false)
    }

    struct Search {
        pid: u32,
        visible_only: bool,
        found: Vec<isize>,
    }

    extern "system" fn visit(hwnd: isize, param: isize) -> i32 {
        // SAFETY: `param` apunta a la `Search` de `windows_of`, viva durante la enumeración.
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

    pub fn hide_windows(pid: u32) -> u32 {
        let w = windows_of(pid, true);
        for h in &w {
            unsafe { ShowWindow(*h, 0) }; // SW_HIDE
        }
        w.len() as u32
    }

    pub fn close_windows(pid: u32, include_hidden: bool) -> usize {
        let w = windows_of(pid, !include_hidden);
        for h in &w {
            unsafe { PostMessageW(*h, 0x0010, 0, 0) }; // WM_CLOSE
        }
        w.len()
    }

    pub fn has_visible_window(pid: u32) -> Option<bool> {
        Some(!windows_of(pid, true).is_empty())
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;
    use std::process::Command;

    pub fn total_memory_mb() -> Option<u32> {
        #[cfg(target_os = "macos")]
        {
            let out = Command::new("sysctl").args(["-n", "hw.memsize"]).output().ok()?;
            let bytes: u64 = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
            Some((bytes / 1024 / 1024) as u32)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let s = std::fs::read_to_string("/proc/meminfo").ok()?;
            let kb: u64 = s.lines().find(|l| l.starts_with("MemTotal:"))?.split_whitespace().nth(1)?.parse().ok()?;
            Some((kb / 1024) as u32)
        }
    }

    pub fn free_space(dir: &std::path::Path) -> Option<u64> {
        let out = Command::new("df").arg("-Pk").arg(dir).output().ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let kb: u64 = text.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()?;
        Some(kb * 1024)
    }

    pub fn process_alive(pid: u32) -> bool {
        Command::new("kill").args(["-0", &pid.to_string()]).status().is_ok_and(|s| s.success())
    }

    pub fn process_exe(pid: u32) -> Option<PathBuf> {
        if let Ok(p) = std::fs::read_link(format!("/proc/{pid}/exe")) {
            return Some(p);
        }
        let out = Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).output().ok()?;
        let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (!s.is_empty()).then(|| PathBuf::from(s))
    }

    pub fn terminate(pid: u32) -> bool {
        Command::new("kill").args(["-9", &pid.to_string()]).status().is_ok_and(|s| s.success())
    }

    pub fn hide_windows(_pid: u32) -> u32 {
        0
    }

    pub fn close_windows(pid: u32, _include_hidden: bool) -> usize {
        usize::from(Command::new("kill").args(["-TERM", &pid.to_string()]).status().is_ok_and(|s| s.success()))
    }

    pub fn has_visible_window(_pid: u32) -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_memoria_deja_sitio_al_sistema() {
        assert_eq!(recommended_memory(8192), (1024, 3072));
        assert_eq!(recommended_memory(16384), (1024, 6144));
        assert_eq!(recommended_memory(4096), (1024, 1536));
        assert_eq!(recommended_memory(65536).1, 8192);
    }

    #[test]
    fn el_disco_de_esta_maquina_se_puede_medir() {
        let d = std::env::temp_dir().join("no-existe-aun").join("tampoco");
        assert!(free_space(&d).is_some_and(|b| b > 0), "sube hasta el primer padre que existe");
        assert_eq!(space_needed(100), 350);
    }

    #[test]
    fn la_ram_de_esta_maquina_se_puede_medir() {
        assert!(total_memory_mb().is_some_and(|m| m > 512));
    }

    #[test]
    fn este_proceso_esta_vivo_y_se_reconoce() {
        let me = std::process::id();
        assert!(process_alive(me));
        assert!(process_exe(me).is_some());
        assert!(!process_alive(u32::MAX - 7));
    }
}
