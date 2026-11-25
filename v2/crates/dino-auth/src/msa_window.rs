//! Llevar al motor directamente a su ventana «Add Microsoft Account», sin enseñar nada más.
//!
//! El motor (ElyPrism 11.1.0) no tiene ninguna opción de línea de comandos para abrir esa
//! ventana. Aquí se hace el camino con el motor fuera de la pantalla, sin depender del
//! tema ni del idioma del motor (con el tema oscuro las filas miden distinto, y un clic
//! por posición en la lista de Ajustes caía en «Services»):
//!
//! 1. la ventana principal se aparta a (-32000, -32000) en cuanto aparece;
//! 2. su botón **Accounts** (la ventana principal sí es accesible) se pulsa por UI
//!    Automation: abre un menú cuya última opción es «Manage Accounts...», que lleva a
//!    Ajustes directamente en la página de cuentas. Se elige con el teclado (↑ desde nada
//!    seleccionado va a la última; luego Intro), enviado a la ventana del menú;
//! 3. Ajustes se aparta también. Sus controles no son accesibles, así que se recorre de
//!    arriba abajo la columna de botones de la derecha con clics enviados a la ventana
//!    (sin mover el ratón del jugador) hasta que se abre una ventana con «Microsoft» en el
//!    título. «Add Microsoft» es el primero de la columna; si se abriera otra cosa (Ely.by,
//!    Offline), se cierra y se para;
//! 4. la ventana de Microsoft se centra en la pantalla y se trae al frente.
//!
//! **Sin destellos.** Apartar una ventana *después* de verla aparecer deja un parpadeo: el
//! motor la crea, la muestra y recién entonces se le hace caso. Por eso, desde antes de que
//! el motor cree su primera ventana corre un [`Concealer`] que a cada ventana nueva de su
//! interfaz le pone, **mientras todavía no se ha mostrado**, opacidad 0 y el estilo de
//! ventana de herramientas (sin botón en la barra de tareas), y la aparta de la pantalla.
//! Cuando Qt la muestra ya es invisible. Solo la ventana de Microsoft se devuelve a la
//! normalidad, ya centrada y vestida de Dino, justo antes de enseñarla.
//!
//! Si algo no sale, [`show_all`] devuelve las ventanas del motor a la vista: el jugador
//! queda en la página de cuentas (o en la ventana principal) y puede seguir a mano.

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::{Interface, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationInvokePattern, TreeScope_Descendants, UIA_ButtonControlTypeId,
    UIA_InvokePatternId,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC, VK_RETURN, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetClientRect, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, PostMessageW, SetForegroundWindow, SetLayeredWindowAttributes,
    SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, LWA_ALPHA, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, WM_CLOSE, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
};

const FAR: i32 = -32000;
const TOOLBAR_BUTTON_ID: &str = "Application.MainWindow.mainToolBar.QToolButton";
/// Nombre del botón Accounts según el idioma del motor; si no, es el último botón de la barra.
const ACCOUNTS_NAMES: [&str; 2] = ["Accounts", "Cuentas"];
/// La columna de botones de la página de cuentas: a esta distancia del borde derecho del
/// área de cliente (a 96 ppp), y se recorre entre estas alturas.
const BUTTONS_FROM_RIGHT: i32 = 50;
const SWEEP: (i32, i32, i32) = (12, 180, 7);

/// Ventanas visibles del proceso, en orden de Z.
fn windows_of(pid: u32) -> Vec<isize> {
    struct S {
        pid: u32,
        out: Vec<isize>,
    }
    extern "system" fn visit(h: HWND, l: LPARAM) -> BOOL {
        // SAFETY: `l` apunta a la `S` de abajo, viva durante la enumeración.
        let s = unsafe { &mut *(l.0 as *mut S) };
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut owner)) };
        if owner == s.pid && unsafe { IsWindowVisible(h) }.as_bool() {
            s.out.push(h.0 as isize);
        }
        BOOL(1)
    }
    let mut s = S { pid, out: Vec::new() };
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut s as *mut S as isize)) };
    s.out
}

/// Ventanas de la interfaz del motor (Qt) del proceso, **visibles o no**: las que aún no se
/// han mostrado son justo las que hay que alcanzar a tiempo. Se reconocen por su clase
/// (`Qt6QWindowIcon`…); las ventanas internas de mensajes de Qt no se tocan.
fn ui_windows_of(pid: u32) -> Vec<isize> {
    struct S {
        pid: u32,
        out: Vec<isize>,
    }
    extern "system" fn visit(h: HWND, l: LPARAM) -> BOOL {
        // SAFETY: `l` apunta a la `S` de abajo, viva durante la enumeración.
        let s = unsafe { &mut *(l.0 as *mut S) };
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut owner)) };
        if owner == s.pid {
            let mut buf = [0u16; 64];
            let n = unsafe { GetClassNameW(h, &mut buf) };
            if String::from_utf16_lossy(&buf[..n.max(0) as usize]).contains("QWindow") {
                s.out.push(h.0 as isize);
            }
        }
        BOOL(1)
    }
    let mut s = S { pid, out: Vec::new() };
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut s as *mut S as isize)) };
    s.out
}

/// Esconde cada ventana nueva del motor antes de que se vea. Ver la documentación del módulo.
struct Concealer {
    stop: Arc<AtomicBool>,
    /// Ventana → estilo extendido que tenía antes de esconderla.
    seen: Arc<Mutex<HashMap<isize, isize>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Concealer {
    fn start(pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let seen: Arc<Mutex<HashMap<isize, isize>>> = Arc::default();
        let (st, sn) = (stop.clone(), seen.clone());
        let thread = std::thread::Builder::new()
            .name("dino-login-conceal".into())
            .spawn(move || {
                while !st.load(Ordering::SeqCst) {
                    for h in ui_windows_of(pid) {
                        let mut seen = sn.lock().unwrap_or_else(|p| p.into_inner());
                        if seen.contains_key(&h) {
                            continue;
                        }
                        // Lo mínimo y en este orden: sin estilo de capa no hay opacidad.
                        let ex = unsafe { GetWindowLongPtrW(hwnd(h), GWL_EXSTYLE) };
                        seen.insert(h, ex);
                        drop(seen);
                        unsafe {
                            SetWindowLongPtrW(
                                hwnd(h),
                                GWL_EXSTYLE,
                                ex | WS_EX_LAYERED.0 as isize | WS_EX_TOOLWINDOW.0 as isize,
                            );
                            let _ = SetLayeredWindowAttributes(hwnd(h), Default::default(), 0, LWA_ALPHA);
                        }
                        move_to(h, FAR, FAR);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
            .ok();
        Self { stop, seen, thread }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn original(&self, h: isize) -> Option<isize> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).get(&h).copied()
    }

    /// Devuelve todas las ventanas escondidas a su estilo de antes (no las mueve).
    fn restore_all(&self) {
        for (&h, &ex) in self.seen.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            restore_style(h, ex);
        }
    }
}

impl Drop for Concealer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Opacidad completa y el estilo de antes.
fn restore_style(h: isize, ex: isize) {
    if !unsafe { IsWindow(Some(hwnd(h))) }.as_bool() {
        return;
    }
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd(h), Default::default(), 255, LWA_ALPHA);
        SetWindowLongPtrW(hwnd(h), GWL_EXSTYLE, ex);
    }
}

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut _)
}

fn title(h: isize) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd(h), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn wait_new(pid: u32, known: &[isize], deadline: Instant) -> Option<isize> {
    while Instant::now() < deadline {
        if let Some(h) = windows_of(pid).into_iter().find(|h| !known.contains(h)) {
            return Some(h);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    None
}

fn move_to(h: isize, x: i32, y: i32) {
    let _ = unsafe { SetWindowPos(hwnd(h), None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
}

fn rect(h: isize) -> RECT {
    let mut r = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd(h), &mut r) };
    r
}

/// Un clic en `(x, y)` del área de cliente, enviado a la ventana.
fn click(h: isize, x: i32, y: i32) {
    let l = LPARAM(((y as u16 as isize) << 16) | (x as u16 as isize));
    unsafe {
        let _ = PostMessageW(Some(hwnd(h)), WM_LBUTTONDOWN, WPARAM(1), l);
        std::thread::sleep(Duration::from_millis(40));
        let _ = PostMessageW(Some(hwnd(h)), WM_LBUTTONUP, WPARAM(0), l);
    }
}

/// Una tecla, enviada a la ventana (bajar y soltar).
fn key(h: isize, vk: u16, extended: bool) {
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as isize;
    let base = 1 | (scan << 16) | if extended { 1 << 24 } else { 0 };
    unsafe {
        let _ = PostMessageW(Some(hwnd(h)), WM_KEYDOWN, WPARAM(vk as usize), LPARAM(base));
        std::thread::sleep(Duration::from_millis(40));
        let _ = PostMessageW(Some(hwnd(h)), WM_KEYUP, WPARAM(vk as usize), LPARAM(base | (0b11 << 30)));
    }
}

/// Pulsa Accounts en otro hilo: `Invoke` no vuelve mientras el menú siga abierto.
fn press_accounts(main: isize) {
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(uia) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) else { return };
        let Ok(root) = uia.ElementFromHandle(hwnd(main)) else { return };
        let Ok(all) = uia.CreateTrueCondition().and_then(|c| root.FindAll(TreeScope_Descendants, &c)) else { return };
        let mut buttons = Vec::new();
        for i in 0..all.Length().unwrap_or(0) {
            let Ok(el) = all.GetElement(i) else { continue };
            let is_button = el.CurrentControlType().is_ok_and(|t| t == UIA_ButtonControlTypeId);
            if is_button && el.CurrentAutomationId().is_ok_and(|id| id == TOOLBAR_BUTTON_ID) {
                buttons.push(el);
            }
        }
        let named = buttons
            .iter()
            .position(|b| b.CurrentName().is_ok_and(|n| ACCOUNTS_NAMES.contains(&n.to_string().as_str())));
        let Some(accounts) = named.or(buttons.len().checked_sub(1)).map(|i| &buttons[i]) else { return };
        if let Ok(p) =
            accounts.GetCurrentPattern(UIA_InvokePatternId).and_then(|p| p.cast::<IUIAutomationInvokePattern>())
        {
            let _ = p.Invoke();
        }
    });
}

/// Hace el camino entero. Devuelve la ventana de Microsoft, o por qué no se llegó.
pub fn open(pid: u32, timeout: Duration) -> Result<isize, &'static str> {
    let mut hide = Concealer::start(pid);
    let r = open_hidden(pid, timeout, &mut hide);
    hide.stop();
    if r.is_err() {
        // El motor va a quedar a la vista (ver `show_all`): que no sea transparente.
        hide.restore_all();
    }
    r
}

fn open_hidden(pid: u32, timeout: Duration, hide: &mut Concealer) -> Result<isize, &'static str> {
    let deadline = Instant::now() + timeout;
    let main = wait_new(pid, &[], deadline).ok_or("el motor no abrió su ventana")?;
    move_to(main, FAR, FAR);

    // Menú de Accounts → «Manage Accounts...» (la última opción).
    press_accounts(main);
    let menu = wait_new(pid, &[main], deadline).ok_or("no se abrió el menú de cuentas")?;
    move_to(menu, FAR, FAR);
    key(menu, VK_UP.0, true);
    key(menu, VK_RETURN.0, false);
    let settings = wait_new(pid, &[main, menu], deadline.min(Instant::now() + Duration::from_secs(5)))
        .ok_or("no se abrió la página de cuentas")?;
    move_to(settings, FAR, FAR);
    // Que termine de colocar la página antes de clicar en ella.
    std::thread::sleep(Duration::from_millis(250));

    // «Add Microsoft»: el primer botón de la columna derecha.
    let scale = unsafe { GetDpiForWindow(hwnd(settings)) }.max(96) as f64 / 96.0;
    let px = |v: i32| (v as f64 * scale).round() as i32;
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd(settings), &mut client) };
    let x = client.right - px(BUTTONS_FROM_RIGHT);
    let (from, to, step) = SWEEP;
    let mut y = px(from);
    while y <= px(to) && Instant::now() < deadline {
        click(settings, x, y);
        if let Some(h) = wait_new(pid, &[main, menu, settings], Instant::now() + Duration::from_millis(250)) {
            if title(h).contains("Microsoft") {
                // Desde aquí nada más se esconde: esta ventana es la que va a verse.
                hide.stop();
                dress(h, &[main, menu, settings], hide.original(h));
                let _ = unsafe { SetForegroundWindow(hwnd(h)) };
                return Ok(h);
            }
            // Otro diálogo (Ely.by, Offline): no es el nuestro.
            let _ = unsafe { PostMessageW(Some(hwnd(h)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            return Err("se abrió otro diálogo de cuentas");
        }
        y += px(step);
    }
    Err("no apareció la ventana de Microsoft")
}

/// Que la ventana de Microsoft parezca de Dino por fuera (por dentro la viste el tema
/// «Dino» del motor): título e icono de Dino, su propio botón en la barra de tareas, la
/// barra de título oscura, y las ventanas del motor fuera también de la barra de tareas
/// (llevaban el logo de PineconeMC).
fn dress(msa: isize, engine: &[isize], original_ex: Option<isize>) {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE};
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SendMessageW, SetWindowLongPtrW, SetWindowTextW, ShowWindow, GWL_EXSTYLE, HICON, ICON_BIG,
        ICON_SMALL, SW_HIDE, SW_SHOWNA, WM_SETICON, WS_EX_APPWINDOW,
    };
    unsafe {
        for &w in engine {
            let _ = ShowWindow(hwnd(w), SW_HIDE);
        }
        let _ = SetWindowTextW(hwnd(msa), &HSTRING::from("Iniciar sesión con Microsoft · Dino Launcher"));
        if let Ok(exe) = std::env::current_exe() {
            let path = HSTRING::from(exe.as_os_str());
            let (mut big, mut small) = (HICON::default(), HICON::default());
            if ExtractIconExW(PCWSTR(path.as_ptr()), 0, Some(&mut big), Some(&mut small), 1) > 0 {
                let _ =
                    SendMessageW(hwnd(msa), WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(big.0 as isize)));
                let _ = SendMessageW(
                    hwnd(msa),
                    WM_SETICON,
                    Some(WPARAM(ICON_SMALL as usize)),
                    Some(LPARAM(small.0 as isize)),
                );
            }
        }
        // Con dueño, una ventana no sale en la barra de tareas: WS_EX_APPWINDOW la obliga.
        // Se aplica al volver a mostrarla.
        let _ = ShowWindow(hwnd(msa), SW_HIDE);
        // Opaca y con su estilo de antes (el `Concealer` la había vuelto invisible).
        if let Some(ex) = original_ex {
            restore_style(msa, ex);
        }
        let ex = GetWindowLongPtrW(hwnd(msa), GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd(msa), GWL_EXSTYLE, ex | WS_EX_APPWINDOW.0 as isize);
        taskbar_identity(msa);
        // Centrada **antes** de mostrarse: aparece ya en su sitio y no salta desde una esquina.
        center(msa);
        let _ = ShowWindow(hwnd(msa), SW_SHOWNA);
        // Barra de título oscura y del color del fondo de Dino (#161B18, en COLORREF 0x00BBGGRR).
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd(msa),
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const BOOL as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
        let caption: u32 = 0x0018_1B16;
        let _ = DwmSetWindowAttribute(hwnd(msa), DWMWA_CAPTION_COLOR, &caption as *const u32 as *const _, 4);
    }
}

/// La barra de tareas agrupa por programa y pinta el icono del **ejecutable** (el cubo de
/// PineconeMC), no el de la ventana. Con un AppUserModelID propio la ventana va aparte y
/// usa el icono y el nombre de Dino.
fn taskbar_identity(msa: isize) {
    use windows::Win32::Storage::EnhancedStorage::{
        PKEY_AppUserModel_ID, PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource,
    };
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
    let Ok(exe) = std::env::current_exe() else { return };
    unsafe {
        let Ok(store) = SHGetPropertyStoreForWindow::<IPropertyStore>(hwnd(msa)) else { return };
        let icon = format!("{},0", exe.display());
        let _ = store.SetValue(&PKEY_AppUserModel_ID, &PROPVARIANT::from("gg.dinolab.launcher.login"));
        let _ = store.SetValue(&PKEY_AppUserModel_RelaunchIconResource, &PROPVARIANT::from(icon.as_str()));
        let _ = store.SetValue(&PKEY_AppUserModel_RelaunchDisplayNameResource, &PROPVARIANT::from("Dino Launcher"));
        let _ = store.Commit();
    }
}

fn center(h: isize) {
    let r = rect(h);
    let (w, hh) = (r.right - r.left, r.bottom - r.top);
    let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    move_to(h, ((sw - w) / 2).max(0), ((sh - hh) / 2).max(0));
}

/// Vuelta atrás: todas las ventanas del motor otra vez a la vista, centradas.
pub fn show_all(pid: u32) {
    for h in windows_of(pid) {
        if rect(h).left <= FAR + 100 {
            center(h);
        }
    }
}

/// ¿Sigue abierta (y visible) la ventana de Microsoft?
pub fn is_open(h: isize) -> bool {
    unsafe { IsWindow(Some(hwnd(h))).as_bool() && IsWindowVisible(hwnd(h)).as_bool() }
}
