//! Llevar al motor directamente a su ventana «Add Microsoft Account», sin enseñar nada más.
//!
//! El motor (ElyPrism 11.1.0) no tiene ninguna opción de línea de comandos para abrir esa
//! ventana: se llega por Settings → Accounts → Add Microsoft. Aquí se hace ese camino con
//! el motor fuera de la pantalla:
//!
//! 1. la ventana principal se aparta a (-32000, -32000) en cuanto aparece;
//! 2. su botón Settings se pulsa por UI Automation (la ventana principal sí es accesible);
//! 3. Ajustes también se aparta, y como sus controles no son accesibles, se le mandan los
//!    clics a la página Accounts y al botón Add Microsoft con `PostMessage` (sin mover el
//!    ratón del jugador). Las posiciones son las del 11.1.0, cuyo SHA-256 fija Dino: la
//!    lista de páginas va anclada arriba a la izquierda y la botonera, a la derecha;
//! 4. la ventana de Microsoft se centra en la pantalla y se trae al frente.
//!
//! Si algo no aparece a tiempo, [`show_all`] devuelve todas las ventanas del motor a la
//! pantalla: el jugador ve el motor como antes de esto y puede seguir a mano.

#![cfg(windows)]

use std::time::{Duration, Instant};
use windows::core::{Interface, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationInvokePattern, TreeScope_Descendants, UIA_InvokePatternId,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId, IsWindow, IsWindowVisible, PostMessageW,
    SetForegroundWindow, SetWindowPos, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    WM_LBUTTONDOWN, WM_LBUTTONUP,
};

const FAR: i32 = -32000;
/// Centro de la página «Accounts» en la lista de Ajustes, desde la esquina superior
/// izquierda de la ventana, a 96 ppp.
const ACCOUNTS_PAGE: (i32, i32) = (66, 252);
/// Centro de «Add Microsoft», desde la esquina superior **derecha** de la ventana.
const ADD_MICROSOFT: (i32, i32) = (54, 75);
/// Nombres del botón Settings según el idioma del motor; si no, es el tercero de la barra.
const SETTINGS_NAMES: [&str; 3] = ["Settings", "Ajustes", "Configuración"];
const TOOLBAR_BUTTON_ID: &str = "Application.MainWindow.mainToolBar.QToolButton";

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

fn wait_new(pid: u32, known: &[isize], deadline: Instant) -> Option<isize> {
    while Instant::now() < deadline {
        if let Some(h) = windows_of(pid).into_iter().find(|h| !known.contains(h)) {
            return Some(h);
        }
        std::thread::sleep(Duration::from_millis(40));
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

/// Un clic donde diga `(x, y)` en coordenadas de pantalla, enviado a la ventana.
fn click(h: isize, x: i32, y: i32) {
    let mut p = POINT { x, y };
    let _ = unsafe { ScreenToClient(hwnd(h), &mut p) };
    let l = LPARAM(((p.y as u16 as isize) << 16) | (p.x as u16 as isize));
    unsafe {
        let _ = PostMessageW(Some(hwnd(h)), WM_LBUTTONDOWN, WPARAM(1), l);
        std::thread::sleep(Duration::from_millis(60));
        let _ = PostMessageW(Some(hwnd(h)), WM_LBUTTONUP, WPARAM(0), l);
    }
}

/// Pulsa Settings en otro hilo: `Invoke` no vuelve mientras el diálogo modal siga abierto.
fn press_settings(main: isize) {
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(uia) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) else { return };
        let Ok(root) = uia.ElementFromHandle(hwnd(main)) else { return };
        let Ok(all) = uia.CreateTrueCondition().and_then(|c| root.FindAll(TreeScope_Descendants, &c)) else { return };
        let mut buttons = Vec::new();
        for i in 0..all.Length().unwrap_or(0) {
            let Ok(el) = all.GetElement(i) else { continue };
            if el.CurrentAutomationId().is_ok_and(|id| id == TOOLBAR_BUTTON_ID) {
                buttons.push(el);
            }
        }
        let named = buttons
            .iter()
            .position(|b| b.CurrentName().is_ok_and(|n| SETTINGS_NAMES.contains(&n.to_string().as_str())));
        let Some(settings) = named.or((buttons.len() >= 3).then_some(2)).map(|i| &buttons[i]) else { return };
        if let Ok(p) =
            settings.GetCurrentPattern(UIA_InvokePatternId).and_then(|p| p.cast::<IUIAutomationInvokePattern>())
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
    press_settings(main);
    let settings = wait_new(pid, &[main], deadline).ok_or("no se abrió Ajustes")?;
    move_to(settings, FAR, FAR);
    let scale = unsafe { GetDpiForWindow(hwnd(settings)) }.max(96) as f64 / 96.0;
    let px = |v: i32| (v as f64 * scale).round() as i32;
    let r = rect(settings);
    click(settings, r.left + px(ACCOUNTS_PAGE.0), r.top + px(ACCOUNTS_PAGE.1));
    std::thread::sleep(Duration::from_millis(400));
    let r = rect(settings);
    click(settings, r.right - px(ADD_MICROSOFT.0), r.top + px(ADD_MICROSOFT.1));
    let msa = wait_new(pid, &[main, settings], deadline.min(Instant::now() + Duration::from_secs(8)))
        .ok_or("no apareció la ventana de Microsoft")?;
    center(msa);
    let _ = unsafe { SetForegroundWindow(hwnd(msa)) };
    Ok(msa)
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
