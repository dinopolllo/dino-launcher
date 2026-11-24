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
//! Si algo no sale, [`show_all`] devuelve las ventanas del motor a la vista: el jugador
//! queda en la página de cuentas (o en la ventana principal) y puede seguir a mano.

#![cfg(windows)]

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
    EnumWindows, GetClientRect, GetSystemMetrics, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindow,
    IsWindowVisible, PostMessageW, SetForegroundWindow, SetWindowPos, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE,
    SWP_NOSIZE, SWP_NOZORDER, WM_CLOSE, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
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
                center(h);
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
