// Sin ventana de consola en Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[macro_use]
extern crate rust_i18n;
rust_i18n::i18n!("locales", fallback = "es");

mod browse;
mod actions;
mod connect;
mod credits;
mod catalog;
mod knowledge;
mod lyrics;
mod media;
mod notify;
mod native;
mod engine;
mod images;
mod queue_ui;
mod secrets;
mod settings;
mod soloist;
mod translate;
mod updater;

use engine::Engine;
use soloist::Soloist;
use spotify_core::queue::QueueOp;
use spotify_core::soloist_protocol::{Live, Update};
use images::{Decoded, Loader};
use serde_json::{json, Value};
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel};
use spotify_core::color;
use spotify_core::engine_protocol::{login_scopes, EngineEvent, EngineState, EngineStatus};
use spotify_core::text::format_time;
use spotify_core::{Page, Playlist, PlaylistItem, Track};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

// La interfaz se compila con la macro (sin build.rs): Windows bloquea ejecutables de compilación sin firmar.
slint::slint!(export { MainWindow, Theme, I18n, LibraryEntry, CardEntry, TrackEntry, QueueRow, LyricEntry, ConnectDevice, GenreEntry } from "ui/main.slint";);

/// Tipografía equivalente a SF Pro: Segoe UI Variable en Windows 11 (Display para títulos grandes,
/// Text para el resto) y Segoe UI en Windows 10. Sus cifras ya son tabulares (mismo ancho), así que
/// los tiempos no «bailan» al avanzar.
fn apply_fonts(ui: &MainWindow) {
    let theme = ui.global::<Theme>();
    #[cfg(windows)]
    {
        let fonts = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts");
        let variable = fonts.join("SegUIVar.ttf").is_file() && std::env::var_os("SPOTIFYMAC_CLASSIC_FONT").is_none();
        theme.set_display_font(if variable { "Segoe UI Variable Display" } else { "Segoe UI" }.into());
        theme.set_text_font(if variable { "Segoe UI Variable Text" } else { "Segoe UI" }.into());
    }
    #[cfg(target_os = "macos")]
    {
        theme.set_display_font("SF Pro Display".into());
        theme.set_text_font("SF Pro Text".into());
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        theme.set_display_font("Inter".into());
        theme.set_text_font("Inter".into());
    }
}

/// ¿Hay una letra a la vista (reproductor grande con letra, o panel lateral de letra)?
pub(crate) fn lyrics_visible(ui: &MainWindow) -> bool {
    (ui.get_now_playing_open() && ui.get_lyrics_open()) || ui.get_right_panel() == 1
}

/// Movimiento del ratón en el reproductor grande: controles visibles y temporizador de 2 s reiniciado.
fn np_activity(ui: &MainWindow, app: &mut App) {
    if !ui.get_np_controls_visible() {
        ui.set_np_controls_visible(true);
    }
    app.np_timer.start(TimerMode::SingleShot, Duration::from_secs(2), || {
        with_app(|ui, _| {
            let hovering = ui.get_np_hover_controls() || ui.get_np_hover_pill() || ui.get_np_hover_close();
            if ui.get_now_playing_open() && !hovering {
                ui.set_np_controls_visible(false);
            }
        });
    });
}

/// Convierte un código de borde de Slint (`0..=7`) a `ResizeDirection` de winit.
pub(crate) fn parse_resize_direction(
    dir: i32,
) -> Option<slint::winit_030::winit::window::ResizeDirection> {
    use slint::winit_030::winit::window::ResizeDirection;
    match dir {
        0 => Some(ResizeDirection::North),
        1 => Some(ResizeDirection::South),
        2 => Some(ResizeDirection::West),
        3 => Some(ResizeDirection::East),
        4 => Some(ResizeDirection::NorthWest),
        5 => Some(ResizeDirection::NorthEast),
        6 => Some(ResizeDirection::SouthWest),
        7 => Some(ResizeDirection::SouthEast),
        _ => None,
    }
}

#[cfg(windows)]
mod media_keys {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::time::Duration;
    use slint::ComponentHandle;
    use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE,
        VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP,
    };
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        NOTIFYICONDATAW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CallWindowProcW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW,
        SetForegroundWindow, SetWindowLongPtrW, ShowWindow, TrackPopupMenu, GWLP_WNDPROC,
        IDI_APPLICATION, MF_SEPARATOR, MF_STRING, SW_HIDE, SW_RESTORE, TPM_BOTTOMALIGN,
        TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, WM_APPCOMMAND, WM_DESTROY, WM_HOTKEY,
        WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP, WM_USER, WNDPROC,
    };

    const HK_PLAY_PAUSE: i32 = 1;
    const HK_STOP: i32 = 2;
    const HK_NEXT: i32 = 3;
    const HK_PREV: i32 = 4;

    const WM_TRAY: u32 = WM_USER + 1;
    const TRAY_ICON_ID: u32 = 1;
    const CMD_PLAY_PAUSE: usize = 1001;
    const CMD_NEXT: usize = 1002;
    const CMD_PREV: usize = 1003;
    const CMD_OPEN: usize = 1004;
    const CMD_EXIT: usize = 1005;

    const APP_MEDIA_NEXT: u16 = 11;
    const APP_MEDIA_PREV: u16 = 12;
    const APP_MEDIA_STOP: u16 = 13;
    const APP_MEDIA_PLAY_PAUSE: u16 = 14;
    const APP_MEDIA_PLAY: u16 = 46;
    const APP_MEDIA_PAUSE: u16 = 47;

    static INSTALLED: AtomicBool = AtomicBool::new(false);
    static PREV_WNDPROC: AtomicIsize = AtomicIsize::new(0);
    static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

    thread_local! {
        static SMTC: RefCell<Option<MediaControls>> = const { RefCell::new(None) };
    }

    #[derive(Clone, Copy)]
    enum MediaAction {
        TogglePlay,
        Play,
        Pause,
        Next,
        Prev,
    }

    fn dispatch(action: MediaAction) {
        let _ = slint::invoke_from_event_loop(move || {
            super::with_app(|ui, app| match action {
                MediaAction::TogglePlay => super::trigger_toggle_play(ui, app),
                MediaAction::Play => super::trigger_set_playing(ui, app, true),
                MediaAction::Pause => super::trigger_set_playing(ui, app, false),
                MediaAction::Next => super::trigger_next(app),
                MediaAction::Prev => super::trigger_previous(app),
            });
        });
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn fill_tip(buf: &mut [u16; 128], text: &str) {
        buf.fill(0);
        for (i, ch) in text.encode_utf16().take(127).enumerate() {
            buf[i] = ch;
        }
    }

    fn restore_window(hwnd: HWND) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }

    pub fn hide_to_tray() {
        let h = MAIN_HWND.load(Ordering::Relaxed);
        if h != 0 {
            unsafe {
                let _ = ShowWindow(h as HWND, SW_HIDE);
            }
        }
    }

    unsafe fn show_tray_menu(hwnd: HWND) {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let s_play = to_wide("Reproducir / Pausar");
        let s_next = to_wide("Siguiente");
        let s_prev = to_wide("Anterior");
        let s_open = to_wide("Abrir Spotify");
        let s_exit = to_wide("Salir");
        let _ = AppendMenuW(menu, MF_STRING, CMD_PLAY_PAUSE, s_play.as_ptr());
        let _ = AppendMenuW(menu, MF_STRING, CMD_NEXT, s_next.as_ptr());
        let _ = AppendMenuW(menu, MF_STRING, CMD_PREV, s_prev.as_ptr());
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, s_open.as_ptr());
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, s_exit.as_ptr());

        let mut pt = POINT { x: 0, y: 0 };
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        ) as usize;
        let _ = DestroyMenu(menu);

        match cmd {
            CMD_PLAY_PAUSE => dispatch(MediaAction::TogglePlay),
            CMD_NEXT => dispatch(MediaAction::Next),
            CMD_PREV => dispatch(MediaAction::Prev),
            CMD_OPEN => restore_window(hwnd),
            CMD_EXIT => {
                let _ = slint::invoke_from_event_loop(|| {
                    super::with_app(|ui, _| {
                        let _ = ui.hide();
                    });
                    let _ = slint::quit_event_loop();
                });
            }
            _ => {}
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TRAY => {
                let ev = lparam as u32;
                if ev == WM_LBUTTONUP || ev == WM_LBUTTONDBLCLK {
                    restore_window(hwnd);
                    return 0;
                } else if ev == WM_RBUTTONUP {
                    show_tray_menu(hwnd);
                    return 0;
                }
            }
            WM_HOTKEY => {
                let action = match wparam as i32 {
                    HK_PLAY_PAUSE => Some(MediaAction::TogglePlay),
                    HK_STOP => Some(MediaAction::Pause),
                    HK_NEXT => Some(MediaAction::Next),
                    HK_PREV => Some(MediaAction::Prev),
                    _ => None,
                };
                if let Some(a) = action {
                    dispatch(a);
                    return 0;
                }
            }
            WM_APPCOMMAND => {
                let cmd = ((lparam as usize >> 16) & 0x0fff) as u16;
                let action = match cmd {
                    APP_MEDIA_PLAY_PAUSE => Some(MediaAction::TogglePlay),
                    APP_MEDIA_PLAY => Some(MediaAction::Play),
                    APP_MEDIA_PAUSE | APP_MEDIA_STOP => Some(MediaAction::Pause),
                    APP_MEDIA_NEXT => Some(MediaAction::Next),
                    APP_MEDIA_PREV => Some(MediaAction::Prev),
                    _ => None,
                };
                if let Some(a) = action {
                    dispatch(a);
                    return 1;
                }
            }
            WM_KEYDOWN => {
                let vk = wparam as u16;
                let action = if vk == VK_MEDIA_PLAY_PAUSE {
                    Some(MediaAction::TogglePlay)
                } else if vk == VK_MEDIA_STOP {
                    Some(MediaAction::Pause)
                } else if vk == VK_MEDIA_NEXT_TRACK {
                    Some(MediaAction::Next)
                } else if vk == VK_MEDIA_PREV_TRACK {
                    Some(MediaAction::Prev)
                } else {
                    None
                };
                if let Some(a) = action {
                    dispatch(a);
                    return 0;
                }
            }
            WM_DESTROY => {
                for id in [HK_PLAY_PAUSE, HK_STOP, HK_NEXT, HK_PREV] {
                    let _ = UnregisterHotKey(hwnd, id);
                }
                let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
                nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                nid.hWnd = hwnd;
                nid.uID = TRAY_ICON_ID;
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            }
            _ => {}
        }
        let prev = PREV_WNDPROC.load(Ordering::Relaxed);
        if prev != 0 {
            let proc: WNDPROC = std::mem::transmute(prev);
            CallWindowProcW(proc, hwnd, msg, wparam, lparam)
        } else {
            0
        }
    }

    pub fn install(hwnd: HWND) {
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        MAIN_HWND.store(hwnd as isize, Ordering::Relaxed);
        unsafe {
            let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, wnd_proc as *const () as isize);
            PREV_WNDPROC.store(prev, Ordering::Relaxed);
            for (id, vk) in [
                (HK_PLAY_PAUSE, VK_MEDIA_PLAY_PAUSE as u32),
                (HK_STOP, VK_MEDIA_STOP as u32),
                (HK_NEXT, VK_MEDIA_NEXT_TRACK as u32),
                (HK_PREV, VK_MEDIA_PREV_TRACK as u32),
            ] {
                let _ = RegisterHotKey(hwnd, id, MOD_NOREPEAT, vk);
            }
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = TRAY_ICON_ID;
            nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY;
            nid.hIcon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
            fill_tip(&mut nid.szTip, "Spotify");
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        }
        let config = PlatformConfig {
            dbus_name: "spotify_win",
            display_name: "Spotify",
            hwnd: Some(hwnd as *mut std::ffi::c_void),
        };
        if let Ok(mut controls) = MediaControls::new(config) {
            let _ = controls.attach(|event: MediaControlEvent| match event {
                MediaControlEvent::Play => dispatch(MediaAction::Play),
                MediaControlEvent::Pause | MediaControlEvent::Stop => dispatch(MediaAction::Pause),
                MediaControlEvent::Toggle => dispatch(MediaAction::TogglePlay),
                MediaControlEvent::Next => dispatch(MediaAction::Next),
                MediaControlEvent::Previous => dispatch(MediaAction::Prev),
                _ => {}
            });
            SMTC.with(|cell| {
                *cell.borrow_mut() = Some(controls);
            });
        }
    }

    /// Aviso de Windows (globo/notificación del icono de la bandeja) con la canción que empieza.
    pub fn notify(title: &str, body: &str) {
        use windows_sys::Win32::UI::Shell::{NIF_INFO, NIIF_INFO, NIIF_NOSOUND};
        let h = MAIN_HWND.load(Ordering::Relaxed);
        if h == 0 {
            return;
        }
        unsafe {
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = h as HWND;
            nid.uID = TRAY_ICON_ID;
            nid.uFlags = NIF_INFO;
            nid.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
            for (i, ch) in title.encode_utf16().take(63).enumerate() {
                nid.szInfoTitle[i] = ch;
            }
            for (i, ch) in body.encode_utf16().take(255).enumerate() {
                nid.szInfo[i] = ch;
            }
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub fn update_smtc(
        title: &str,
        artist: &str,
        album: &str,
        cover_url: Option<&str>,
        playing: bool,
        duration_ms: i64,
    ) {
        SMTC.with(|cell| {
            let mut guard = cell.borrow_mut();
            let Some(controls) = guard.as_mut() else { return };
            let _ = controls.set_metadata(MediaMetadata {
                title: Some(title),
                artist: Some(artist),
                album: Some(album),
                cover_url,
                duration: (duration_ms > 0).then(|| Duration::from_millis(duration_ms as u64)),
            });
            let _ = controls.set_playback(if playing {
                MediaPlayback::Playing { progress: None }
            } else {
                MediaPlayback::Paused { progress: None }
            });
        });
        let tip = if title.is_empty() {
            "Spotify".to_owned()
        } else if artist.is_empty() {
            format!("{title} — Spotify")
        } else {
            format!("{title} · {artist}")
        };
        let h = MAIN_HWND.load(Ordering::Relaxed);
        if h != 0 {
            unsafe {
                let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
                nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                nid.hWnd = h as HWND;
                nid.uID = TRAY_ICON_ID;
                nid.uFlags = NIF_TIP;
                fill_tip(&mut nid.szTip, &tip);
                let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            }
        }
    }
}

#[cfg(not(windows))]
mod media_keys {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};

    static INSTALLED: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static CONTROLS: RefCell<Option<MediaControls>> = const { RefCell::new(None) };
    }

    #[derive(Clone, Copy)]
    enum MediaAction {
        TogglePlay,
        Play,
        Pause,
        Next,
        Prev,
    }

    fn dispatch(action: MediaAction) {
        let _ = slint::invoke_from_event_loop(move || {
            super::with_app(|ui, app| match action {
                MediaAction::TogglePlay => super::trigger_toggle_play(ui, app),
                MediaAction::Play => super::trigger_set_playing(ui, app, true),
                MediaAction::Pause => super::trigger_set_playing(ui, app, false),
                MediaAction::Next => super::trigger_next(app),
                MediaAction::Prev => super::trigger_previous(app),
            });
        });
    }

    pub fn install() {
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        let config = PlatformConfig {
            dbus_name: "spotifymac",
            display_name: "SpotifyMac",
            hwnd: None,
        };
        if let Ok(mut controls) = MediaControls::new(config) {
            let _ = controls.attach(|event: MediaControlEvent| match event {
                MediaControlEvent::Play => dispatch(MediaAction::Play),
                MediaControlEvent::Pause | MediaControlEvent::Stop => dispatch(MediaAction::Pause),
                MediaControlEvent::Toggle => dispatch(MediaAction::TogglePlay),
                MediaControlEvent::Next => dispatch(MediaAction::Next),
                MediaControlEvent::Previous => dispatch(MediaAction::Prev),
                _ => {}
            });
            CONTROLS.with(|cell| {
                *cell.borrow_mut() = Some(controls);
            });
        }
    }

    pub fn update_smtc(
        title: &str,
        artist: &str,
        album: &str,
        cover_url: Option<&str>,
        playing: bool,
        duration_ms: i64,
    ) {
        CONTROLS.with(|cell| {
            let mut guard = cell.borrow_mut();
            let Some(controls) = guard.as_mut() else { return };
            let _ = controls.set_metadata(MediaMetadata {
                title: Some(title),
                artist: Some(artist),
                album: Some(album),
                cover_url,
                duration: (duration_ms > 0).then(|| Duration::from_millis(duration_ms as u64)),
            });
            let _ = controls.set_playback(if playing {
                MediaPlayback::Playing { progress: None }
            } else {
                MediaPlayback::Paused { progress: None }
            });
        });
    }
}

fn trigger_toggle_play(ui: &MainWindow, app: &mut App) {
    ctl(app, Ctl::Toggle);
    let pos = app.player.position();
    app.player.playing = !app.player.playing;
    app.player.set_position(pos);
    render_player(ui, app);
}

fn trigger_set_playing(ui: &MainWindow, app: &mut App, playing: bool) {
    if app.player.playing != playing {
        trigger_toggle_play(ui, app);
    }
}

fn trigger_next(app: &mut App) {
    app.pending_track = None;
    app.play_epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    ctl(app, Ctl::Next);
}

fn trigger_previous(app: &mut App) {
    app.pending_track = None;
    app.play_epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    ctl(app, Ctl::Prev);
}

#[cfg(target_os = "macos")]
fn configure_macos_window(window: &slint::winit_030::winit::window::Window) {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};

    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }

    extern "C" {
        fn objc_getClass(name: *const std::ffi::c_char) -> *mut c_void;
        fn sel_registerName(name: *const std::ffi::c_char) -> *mut c_void;
        fn objc_msgSend();
    }

    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return };
    unsafe {
        let send_ptr: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let send_bool: unsafe extern "C" fn(*mut c_void, *mut c_void, i8) =
            std::mem::transmute(objc_msgSend as *const ());
        let send_obj: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend as *const ());
        let send_rgba: unsafe extern "C" fn(*mut c_void, *mut c_void, f64, f64, f64, f64) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let send_cstr: unsafe extern "C" fn(*mut c_void, *mut c_void, *const std::ffi::c_char) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let send_named: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());

        let ns_view = appkit.ns_view.as_ptr();
        let ns_window = send_ptr(ns_view, sel_registerName(c"window".as_ptr()));
        if ns_window.is_null() {
            return;
        }
        // Fondo sólido oscuro para que la barra de título nativa no sea transparente.
        send_bool(ns_window, sel_registerName(c"setOpaque:".as_ptr()), 1);
        let ns_color_cls = objc_getClass(c"NSColor".as_ptr());
        let bg = send_rgba(
            ns_color_cls,
            sel_registerName(c"colorWithSRGBRed:green:blue:alpha:".as_ptr()),
            0.067,
            0.067,
            0.086,
            1.0,
        );
        if !bg.is_null() {
            send_obj(ns_window, sel_registerName(c"setBackgroundColor:".as_ptr()), bg);
        }
        send_bool(ns_window, sel_registerName(c"setTitlebarAppearsTransparent:".as_ptr()), 1);

        let ns_str_cls = objc_getClass(c"NSString".as_ptr());
        let dark_name = send_cstr(
            ns_str_cls,
            sel_registerName(c"stringWithUTF8String:".as_ptr()),
            c"NSAppearanceNameDarkAqua".as_ptr(),
        );
        let ns_app_cls = objc_getClass(c"NSAppearance".as_ptr());
        let dark_app = send_named(ns_app_cls, sel_registerName(c"appearanceNamed:".as_ptr()), dark_name);
        if !dark_app.is_null() {
            send_obj(ns_window, sel_registerName(c"setAppearance:".as_ptr()), dark_app);
        }
    }
}

/// Aplica esquinas redondeadas nativas en Windows 11 y sincroniza el icono maximizar/restaurar.
fn sync_window_frame(ui: &MainWindow) {
    use slint::winit_030::WinitWindowAccessor;
    let mut maximized = false;
    let _ = ui.window().with_winit_window(|window| {
        maximized = window.is_maximized();
        #[cfg(windows)]
        {
            use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            use windows_sys::Win32::Foundation::HWND;
            use windows_sys::Win32::Graphics::Dwm::{
                DwmSetWindowAttribute, DWMWCP_DONOTROUND, DWMWCP_ROUND,
                DWMWA_WINDOW_CORNER_PREFERENCE,
            };
            if let Ok(handle) = window.window_handle() {
                if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                    let hwnd = win32.hwnd.get() as HWND;
                    media_keys::install(hwnd);
                    let pref = if maximized { DWMWCP_DONOTROUND } else { DWMWCP_ROUND };
                    unsafe {
                        let _ = DwmSetWindowAttribute(
                            hwnd,
                            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                            (&pref as *const i32).cast(),
                            std::mem::size_of::<i32>() as u32,
                        );
                    }
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            window.set_decorations(true);
            configure_macos_window(window);
            media_keys::install();
        }
        #[cfg(all(not(windows), not(target_os = "macos")))]
        {
            media_keys::install();
        }
    });
    ui.set_window_maximized(maximized);
}

fn wire_window_controls(ui: &MainWindow) {
    use slint::winit_030::WinitWindowAccessor;
    ui.on_window_minimize(|| {
        with_app(|ui, _| {
            let _ = ui.window().with_winit_window(|w| w.set_minimized(true));
        });
    });
    ui.on_window_toggle_maximize(|| {
        with_app(|ui, _| {
            let _ = ui
                .window()
                .with_winit_window(|w| w.set_maximized(!w.is_maximized()));
            let _ = slint::invoke_from_event_loop(|| {
                with_app(|ui, _| sync_window_frame(ui));
            });
        });
    });
    ui.on_window_close(|| {
        with_app(|ui, _| {
            if cfg!(windows) && ui.get_pref_tray() {
                #[cfg(windows)]
                media_keys::hide_to_tray();
            } else {
                let _ = ui.hide();
                let _ = slint::quit_event_loop();
            }
        });
    });
    ui.on_toggle_mini_player(|| {
        static MINI_RESTORE: std::sync::Mutex<Option<(i32, i32, u32, u32)>> =
            std::sync::Mutex::new(None);
        with_app(|ui, _| {
            let entering = !ui.get_mini_player_open();
            ui.set_mini_player_open(entering);
            let _ = ui.window().with_winit_window(|w| {
                use slint::winit_030::winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
                use slint::winit_030::winit::window::WindowLevel;
                if entering {
                    if w.is_maximized() {
                        w.set_maximized(false);
                    }
                    let pos = w.outer_position().ok().map(|p| (p.x, p.y)).unwrap_or((100, 100));
                    let sz = w.inner_size();
                    if let Ok(mut guard) = MINI_RESTORE.lock() {
                        *guard = Some((pos.0, pos.1, sz.width, sz.height));
                    }
                    let _ = w.request_inner_size(LogicalSize::new(380.0_f64, 168.0_f64));
                    w.set_window_level(WindowLevel::AlwaysOnTop);
                } else {
                    w.set_window_level(WindowLevel::Normal);
                    if let Ok(mut guard) = MINI_RESTORE.lock() {
                        if let Some((x, y, width, height)) = guard.take() {
                            let _ = w.request_inner_size(PhysicalSize::new(width, height));
                            w.set_outer_position(PhysicalPosition::new(x, y));
                        }
                    }
                }
            });
            sync_window_frame(ui);
        });
    });
    ui.on_window_drag(|| {
        thread_local! {
            static LAST_TITLE_DOWN: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
        }
        with_app(|ui, _| {
            let now = Instant::now();
            let double = LAST_TITLE_DOWN.with(|cell| {
                let hit = cell
                    .get()
                    .is_some_and(|t| now.duration_since(t) < Duration::from_millis(400));
                cell.set(if hit { None } else { Some(now) });
                hit
            });
            if double {
                let _ = ui.window().with_winit_window(|w| {
                    #[cfg(target_os = "macos")]
                    {
                        use slint::winit_030::winit::window::Fullscreen;
                        let next = if w.fullscreen().is_some() {
                            None
                        } else {
                            Some(Fullscreen::Borderless(None))
                        };
                        w.set_fullscreen(next);
                    }
                    #[cfg(not(target_os = "macos"))]
                    w.set_maximized(!w.is_maximized());
                });
                let _ = slint::invoke_from_event_loop(|| {
                    with_app(|ui, _| sync_window_frame(ui));
                });
            } else {
                let _ = ui.window().with_winit_window(|w| {
                    let _ = w.drag_window();
                });
                sync_window_frame(ui);
            }
        });
    });
    ui.on_window_resize(|dir| {
        if let Some(direction) = parse_resize_direction(dir) {
            with_app(|ui, _| {
                let _ = ui.window().with_winit_window(|w| {
                    let _ = w.drag_resize_window(direction);
                });
            });
        }
    });
}

/// Lo que ocurre al pulsar una tarjeta de Inicio.
#[derive(Clone)]
enum Target {
    Track(String),
    List(Playlist),
}

struct Player {
    track: Option<Track>,
    playing: bool,
    position_ms: i64,
    /// Instante en que se midió `position_ms` (mientras suena, el tiempo avanza desde aquí).
    anchor: Instant,
}

impl Player {
    fn position(&self) -> i64 {
        self.position_ms + if self.playing { self.anchor.elapsed().as_millis() as i64 } else { 0 }
    }
    fn set_position(&mut self, ms: i64) {
        self.position_ms = ms;
        self.anchor = Instant::now();
    }
    fn sync_solo(&mut self, live: &Live, measured: bool) -> bool {
        let changed = live.track.as_ref().map(|t| &t.uri) != self.track.as_ref().map(|t| &t.uri);
        let position = if measured { live.position_ms } else { self.position() };
        self.playing = live.is_playing();
        self.set_position(position);
        if live.track.is_some() { self.track = live.track.clone(); }
        changed
    }
}

/// Una pantalla del historial: Inicio/Búsqueda (por número de página) o una lista/álbum/artista.
#[derive(Clone)]
struct NavEntry {
    page: i32,
    source: Option<browse::Source>,
}
impl NavEntry {
    fn key(&self) -> String {
        match &self.source {
            None => format!("page{}", self.page),
            Some(browse::Source::Liked) => "liked".into(),
            Some(browse::Source::Playlist(p)) => p.uri.clone(),
            Some(browse::Source::Album(a)) => a.uri.clone(),
            Some(browse::Source::ArtistTop(a)) => format!("artist:{}", a.id),
        }
    }
}
/// Guarda la pantalla actual antes de navegar a otra (salvo que sea la misma, o se esté regresando).
fn push_history(ui: &MainWindow, app: &mut App) {
    if app.replaying { return; }
    let page = ui.get_page();
    let source = if matches!(page, 2 | 3) { app.current_source.clone() } else { None };
    if matches!(page, 2 | 3) && source.is_none() { return; }
    let entry = NavEntry { page, source };
    if app.history.last().is_some_and(|last| last.key() == entry.key()) { return; }
    app.history.push(entry);
    if app.history.len() > 60 { app.history.remove(0); }
    app.forward_history.clear();
    ui.set_can_go_back(true);
    ui.set_can_go_forward(false);
}
fn go_back(ui: &MainWindow, app: &mut App) {
    let current_page = ui.get_page();
    let current_source = if matches!(current_page, 2 | 3) { app.current_source.clone() } else { None };
    let Some(entry) = app.history.pop() else { return };
    app.forward_history.push(NavEntry { page: current_page, source: current_source });
    if app.forward_history.len() > 60 { app.forward_history.remove(0); }
    ui.set_can_go_back(!app.history.is_empty());
    ui.set_can_go_forward(true);
    app.replaying = true;
    match entry.source {
        Some(src) => browse::open_source(ui, app, src),
        None => {
            media::reset(ui, app);
            match entry.page {
                4 => settings::open_settings(ui, app),
                5 => settings::open_profile(ui, app),
                1 => { ui.set_page(1); ui.set_page_title("Buscar".into()); }
                _ => { ui.set_page(0); ui.set_page_title("Inicio".into()); }
            }
        }
    }
    app.replaying = false;
}

fn go_forward(ui: &MainWindow, app: &mut App) {
    let current_page = ui.get_page();
    let current_source = if matches!(current_page, 2 | 3) { app.current_source.clone() } else { None };
    let Some(entry) = app.forward_history.pop() else { return };
    app.history.push(NavEntry { page: current_page, source: current_source });
    if app.history.len() > 60 { app.history.remove(0); }
    ui.set_can_go_forward(!app.forward_history.is_empty());
    ui.set_can_go_back(true);
    app.replaying = true;
    match entry.source {
        Some(src) => browse::open_source(ui, app, src),
        None => {
            media::reset(ui, app);
            match entry.page {
                4 => settings::open_settings(ui, app),
                5 => settings::open_profile(ui, app),
                1 => { ui.set_page(1); ui.set_page_title("Buscar".into()); }
                _ => { ui.set_page(0); ui.set_page_title("Inicio".into()); }
            }
        }
    }
    app.replaying = false;
}

/// Estado de la app. Vive solo en el hilo de la interfaz; los hilos de trabajo lo tocan con `on_ui`.
struct App {
    ui: slint::Weak<MainWindow>,
    engine: Option<Arc<Engine>>,
    loader: Arc<Loader>,
    player: Player,
    playlists: Vec<Playlist>,
    /// «Tu biblioteca»: álbumes guardados y artistas seguidos (para los filtros).
    lib_albums: Vec<Playlist>,
    lib_artists: Vec<spotify_core::ArtistItem>,
    lib_shows: Vec<Playlist>,
    library: Rc<VecModel<LibraryEntry>>,
    cards: Rc<VecModel<CardEntry>>,
    card_targets: Vec<Target>,
    made: Rc<VecModel<CardEntry>>,
    mixes: Rc<VecModel<CardEntry>>,
    made_targets: Vec<Playlist>,
    mix_targets: Vec<Playlist>,
    tracks: Vec<Track>,
    visible_indices: Vec<usize>,
    current_source: Option<browse::Source>,
    /// Pantallas visitadas (para «regresar» y «avanzar») y marca de que se está repitiendo una.
    history: Vec<NavEntry>,
    forward_history: Vec<NavEntry>,
    replaying: bool,
    actions: actions::Actions,
    catalog: catalog::Catalog,
    media: media::Media,
    media_now: media::Media,
    lyrics: lyrics::State,
    solo_device_id: Option<String>,
    play_epoch: Arc<std::sync::atomic::AtomicU64>,
    play_dispatch: Arc<std::sync::Mutex<()>>,
    pending_track: Option<(String, Instant)>,
    username: Option<String>,
    rows: Rc<VecModel<TrackEntry>>,
    context: Option<String>,
    timer: Timer,
    notify: notify::NotifyState,
    sleep: notify::SleepState,
    connect: connect::State,
    settings: settings::State,
    cover_url: Option<String>,
    ready: bool,
    // ---- Soloist (respaldo cuando Spotify niega las claves de audio a librespot) ----
    soloist: Option<Arc<Soloist>>,
    solo_status: soloist::Status,
    /// ¿Es Soloist el dispositivo que suena ahora?
    solo_active: bool,
    /// Este equipo no obtiene audio de librespot: se reproduce con Soloist (se recuerda entre sesiones).
    prefer_soloist: bool,
    last_play: Option<PlayReq>,
    shuffle: bool,
    repeat_engine: &'static str,
    replay_when_ready: bool,
    last_key_fallback: Option<Instant>,
    /// El usuario está configurando Soloist: el asistente se abre y se cierra solo.
    solo_setup: bool,
    /// Número de la lista abierta: descarta páginas de una lista que ya se cerró.
    list_gen: u64,
    search: browse::SearchState,
    queue: queue_ui::QueueState,
    /// Oculta los controles del reproductor grande tras 2 s sin mover el ratón.
    np_timer: Timer,
}

#[derive(Clone)]
enum PlayReq {
    /// Playlist/álbum, opcionalmente empezando por una canción (`skip` = posición para Soloist).
    Context { uri: String, start: Option<String>, skip: usize },
    Track(String),
    /// Lista de canciones sin contexto (favoritas, resultados…), empezando por `start`.
    Tracks { uris: Vec<String>, start: String },
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&MainWindow, &mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        let app = guard.as_mut()?;
        let ui = app.ui.upgrade()?;
        Some(f(&ui, app))
    })
}

/// Ejecuta `f` en el hilo de la interfaz (se puede llamar desde cualquier hilo).
fn on_ui(f: impl FnOnce(&MainWindow, &mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(f);
    });
}

fn log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(engine::data_dir().join("app.log")) {
        let _ = writeln!(f, "{msg}");
    }
}

fn to_image(d: &Decoded) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&d.rgba, d.width, d.height))
}

/// Pide una portada y, al llegar, la aplica en el hilo de la interfaz.
fn load_cover(app: &App, url: &str, side: u32, apply: impl FnOnce(&MainWindow, &mut App, &Decoded) + Send + 'static) {
    // Cabecera abierta y resultados de búsqueda antes que las estanterías de fondo.
    let urgent = side >= 256 || app.ui.upgrade().is_some_and(|ui| ui.get_page() == 1);
    load_cover_with_priority(app, url, side, urgent, apply);
}

fn load_cover_with_priority(app: &App, url: &str, side: u32, urgent: bool, apply: impl FnOnce(&MainWindow, &mut App, &Decoded) + Send + 'static) {
    let scale = app.ui.upgrade().map(|ui| ui.window().scale_factor()).unwrap_or(1.0);
    let pixels = ((side as f32 * scale.max(1.0)).ceil() as u32).min(2048);
    app.loader.load(url, pixels, urgent, move |d| {
        if let Some(d) = d {
            on_ui(move |ui, app| apply(ui, app, &d));
        }
    });
}

/// Foto del artista para la portada del perfil y color de fondo tomado de ella.
fn apply_artist_photo(ui: &MainWindow, _app: &App, d: &Decoded) {
    ui.set_list_cover(to_image(d));
    if let Some(c) = d.color {
        ui.set_artist_accent(slint::Color::from_rgb_u8(c[0], c[1], c[2]));
        let [r, g, b] = color::deep(c);
        ui.set_list_tint(slint::Color::from_rgb_u8(r, g, b));
    }
}

// ---------- Reproductor ----------

fn track_rows(tracks: &[Track], playing_uri: Option<&str>) -> Vec<TrackEntry> {
    tracks
        .iter()
        .enumerate()
        .map(|(i, t)| TrackEntry {
            index: i as i32 + 1,
            uri: t.uri.clone().into(),
            cover: Image::default(),
            title: t.name.clone().into(),
            artist: t.artist_line().into(),
            album: t.album.as_ref().map(|a| a.name.clone()).unwrap_or_default().into(),
            duration: format_time(t.duration_ms).into(),
            playcount: "".into(),
            saved: false,
            like_busy: false,
            explicit: t.explicit == Some(true),
            playing: playing_uri == Some(t.uri.as_str()),
        })
        .collect()
}

fn refresh_rows(app: &mut App) {
    let uri = app.player.track.as_ref().map(|t| t.uri.clone());
    let existing: std::collections::HashMap<String, Image> = (0..app.rows.row_count())
        .filter_map(|i| app.rows.row_data(i)).map(|r| (r.uri.to_string(), r.cover)).collect();
    let ui = app.ui.upgrade();
    let (query, sort, ascending) = ui.as_ref().map(|ui| (ui.get_list_filter().to_string(), ui.get_list_sort() as usize, ui.get_list_ascending())).unwrap_or_default();
    app.visible_indices = spotify_core::playlist_ordering::indices(&app.tracks, &query,
        spotify_core::playlist_ordering::PlaylistSort::ALL.get(sort).copied().unwrap_or_default(), ascending);
    let visible: Vec<_> = app.visible_indices.iter().map(|&i| app.tracks[i].clone()).collect();
    let mut rows = track_rows(&visible, uri.as_deref());
    let mut jobs: std::collections::HashMap<String, Vec<(usize, String)>> = std::collections::HashMap::new();
    for (i, (row, track)) in rows.iter_mut().zip(&visible).enumerate() {
        row.saved = app.actions.saved.get(&track.uri).copied().unwrap_or(false);
        row.like_busy = app.actions.like_pending.contains(&track.uri);
        if matches!(app.current_source, Some(browse::Source::ArtistTop(_))) {
            if let Some(&n) = app.catalog.top_playcounts.get(&track.uri) {
                row.playcount = spotify_core::text::format_count(n).into();
            }
        }
        if let Some(cover) = existing.get(&track.uri).filter(|image| image.size().width > 0) {
            row.cover = cover.clone();
        } else if let Some(url) = track.image_url() {
            jobs.entry(url.to_owned()).or_default().push((i, track.uri.clone()));
        }
    }
    app.rows.set_vec(rows);
    if let Some(ui) = &ui {
        ui.set_list_matches(format!("{} de {} canciones", visible.len(), app.tracks.len()).into());
    }
    actions::request_saved(app, visible);
    let mut jobs: Vec<_> = jobs.into_iter().collect();
    jobs.sort_by_key(|(_, rows)| rows.first().map(|(index, _)| *index).unwrap_or(usize::MAX));
    for (url, wanted) in jobs {
        let urgent = wanted.first().is_some_and(|(index, _)| *index < 24);
        load_cover_with_priority(app, &url, 44, urgent, move |_, app, decoded| {
            let image = to_image(decoded);
            for (i, uri) in wanted {
                if let Some(mut row) = app.rows.row_data(i).filter(|r| r.uri.as_str() == uri) {
                    row.cover = image.clone();
                    app.rows.set_row_data(i, row);
                }
            }
        });
    }
}

fn render_progress(ui: &MainWindow, app: &App) {
    let Some(t) = &app.player.track else { return };
    let dur = t.duration_ms;
    // Sin duración conocida (Connect a veces la manda en 0) el tiempo sigue avanzando: nunca `min(v, 0)`.
    let pos = if dur > 0 { app.player.position().clamp(0, dur) } else { app.player.position().max(0) };
    ui.set_progress(if dur > 0 { pos as f32 / dur as f32 } else { 0.0 });
    ui.set_elapsed(format_time(pos).into());
    ui.set_remaining(if dur > 0 { format!("-{}", format_time(dur - pos)) } else { "--:--".to_owned() }.into());
    lyrics::update_active(ui, app);
}

fn render_player(ui: &MainWindow, app: &mut App) {
    ui.set_playing(app.player.playing);
    actions::render_likes(ui, app);
    if let Some(track) = app.player.track.clone() { actions::request_saved(app, vec![track]); }
    match app.player.track.clone() {
        Some(t) => {
            ui.set_track_title(t.name.clone().into());
            ui.set_track_artist(t.artist_line().into());
            ui.set_track_album(t.album.as_ref().map(|a| a.name.clone()).unwrap_or_default().into());
            ui.set_track_explicit(t.explicit.unwrap_or(false));
            render_progress(ui, app);
            let url = t.image_url().map(str::to_owned);
            if url != app.cover_url {
                app.cover_url = url.clone();
                ui.set_track_cover(Image::default());
                ui.set_tint(slint::Color::from_rgb_u8(35, 35, 35));
                ui.set_track_pill(slint::Color::from_rgb_u8(28, 30, 38));
                ui.set_track_accent(slint::Color::from_rgb_u8(235, 235, 235));
                if let Some(url) = url {
                    // Presenta una copia ligera mientras llega la de alta resolución.
                    // Una respuesta tardía de la copia pequeña nunca sustituye a la grande.
                    for side in [256, 1200] {
                        let wanted = url.clone();
                        load_cover(app, &url, side, move |ui, app, d| {
                            if app.cover_url.as_deref() != Some(wanted.as_str())
                                || ui.get_track_cover().size().width > d.width { return; }
                            ui.set_track_cover(to_image(d));
                            if let Some(c) = d.color {
                                ui.set_track_accent(slint::Color::from_rgb_u8(c[0], c[1], c[2]));
                                let [r, g, b] = color::deep(c);
                                ui.set_tint(slint::Color::from_rgb_u8(r, g, b));
                                let [pr, pg, pb] = color::pill(c);
                                ui.set_track_pill(slint::Color::from_rgb_u8(pr, pg, pb));
                            }
                        });
                    }
                }
            }
            notify::track_changed(ui, app);
            if ui.get_now_playing_open() {
                media::load_now(ui, app, &t);
            }
            if lyrics_visible(ui) {
                lyrics::request(ui, app, &t);
            }
            queue_ui::maybe_fill_infinite(ui, app);
            settings::update_crossfade_marker(ui, t.duration_ms as u32);
            #[cfg(windows)]
            media_keys::update_smtc(
                &t.name,
                &t.artist_line(),
                t.album.as_ref().map(|a| a.name.as_str()).unwrap_or(""),
                t.image_url(),
                app.player.playing,
                t.duration_ms as i64,
            );
        }
        None => {
            ui.set_track_title("Nada sonando".into());
            ui.set_track_artist("".into());
            ui.set_track_album("".into());
            #[cfg(windows)]
            media_keys::update_smtc("", "", "", None, false, 0);
        }
    }
    // El reloj de 1 s solo corre mientras suena: en pausa la app no despierta.
    if app.player.playing {
        if !app.timer.running() {
            // Tic de 50 ms para fluidez total; en modo ahorro de batería omite minimizado y espacia letras.
            let mut tick = 0u32;
            app.timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
                tick = tick.wrapping_add(1);
                with_app(|ui, app| {
                    // Cada ~2 s: el modo infinito comprueba si la canción está por acabar sin nada detrás.
                    if tick % 40 == 0 {
                        queue_ui::maybe_fill_infinite(ui, app);
                    }
                    if ui.get_pref_battery_saver() {
                        use slint::winit_030::WinitWindowAccessor;
                        let minimized = ui
                            .window()
                            .with_winit_window(|w| w.is_minimized().unwrap_or(false))
                            .unwrap_or(false);
                        if minimized {
                            return;
                        }
                        if tick % 20 == 0 {
                            render_progress(ui, app);
                        } else if tick % 3 == 0 && lyrics_visible(ui) {
                            lyrics::update_active(ui, app);
                        }
                    } else if tick % 20 == 0 {
                        render_progress(ui, app);
                    } else if lyrics_visible(ui) {
                        lyrics::update_active(ui, app);
                    }
                });
            });
        }
    } else {
        app.timer.stop();
    }
}

fn repeat_to_engine(mode: &str) -> &'static str {
    match mode {
        "context" | "all" => "context",
        "track" | "one" => "track",
        _ => "off",
    }
}

fn repeat_to_ui(engine_mode: &str) -> &'static str {
    match engine_mode {
        "context" => "all",
        "track" => "one",
        _ => "off",
    }
}

fn apply_state(ui: &MainWindow, app: &mut App, s: EngineState) {
    // Con Soloist mandan sus eventos: el motor sin audio seguiría diciendo que «suena».
    if use_soloist(app) {
        return;
    }
    // Mientras suena otro dispositivo, el motor local parado no debe pisar ese estado.
    let local_playing = s.playing && !s.stopped.unwrap_or(false);
    if app.connect.remote.is_some() {
        if !local_playing { return; }
        connect::clear_remote(ui, app);
    }
    let previous = app.player.track.as_ref().map(|t| t.uri.clone());
    app.player.playing = local_playing;
    app.player.set_position(s.position_ms);
    if s.track.is_some() {
        app.player.track = s.track;
    }
    ui.set_shuffle(s.shuffle);
    ui.set_repeat_mode(repeat_to_ui(&s.repeat).into());
    // Lo que muestra la interfaz es lo que se manda al cargar otra lista: si no se recordaba, al
    // abrir otra playlist se enviaba «sin repetir» aunque el botón dijera lo contrario (la cola
    // se veía en bucle y luego no se repetía).
    app.shuffle = s.shuffle;
    app.repeat_engine = repeat_to_engine(&s.repeat);
    ui.set_volume((s.volume as f32 / 100.0).clamp(0.0, 1.0));
    render_player(ui, app);
    if previous != app.player.track.as_ref().map(|t| t.uri.clone()) {
        refresh_rows(app);
    }
}

// ---------- Biblioteca e inicio ----------

fn load_library(engine: Arc<Engine>) {
    // Álbumes guardados y artistas seguidos, en paralelo a las playlists.
    let collection_engine = engine.clone();
    std::thread::spawn(move || match collection_engine.call("library_collection", json!({}), Duration::from_secs(60)) {
        Ok(v) => {
            let parse = |key: &str| v.get(key).cloned().unwrap_or(json!([]));
            let albums: Vec<serde_json::Value> = serde_json::from_value(parse("albums")).unwrap_or_default();
            let albums: Vec<Playlist> = albums.iter().filter_map(|a| {
                let id = a.get("id")?.as_str()?;
                let mut p = Playlist::new(id, a.get("uri")?.as_str()?, a.get("name")?.as_str()?);
                p.images = serde_json::from_value(a.get("images").cloned().unwrap_or(json!([]))).ok();
                let first = a.get("artists").and_then(|x| x.get(0));
                p.artist_name = first.and_then(|x| x.get("name")).and_then(|x| x.as_str()).map(str::to_owned);
                p.artist_id = first.and_then(|x| x.get("id")).and_then(|x| x.as_str()).map(str::to_owned);
                Some(p)
            }).collect();
            let artists: Vec<spotify_core::ArtistItem> = serde_json::from_value(parse("artists")).unwrap_or_default();
            let shows: Vec<serde_json::Value> = serde_json::from_value(parse("shows")).unwrap_or_default();
            let shows: Vec<Playlist> = shows.iter().filter_map(|sh| {
                let mut p = Playlist::new(sh.get("id")?.as_str()?, sh.get("uri")?.as_str()?, sh.get("name")?.as_str()?);
                p.images = serde_json::from_value(sh.get("images").cloned().unwrap_or(json!([]))).ok();
                let publisher = sh.get("publisher").and_then(|x| x.as_str()).unwrap_or_default();
                p.owner = Some(spotify_core::SpOwner { id: String::new(), display_name: Some(if publisher.is_empty() { "Podcast".into() } else { format!("Podcast · {publisher}") }) });
                Some(p)
            }).collect();
            on_ui(move |_ui, app| {
                app.lib_albums = albums;
                app.lib_artists = artists;
                app.lib_shows = shows;
                browse::rebuild_library(app);
            });
        }
        Err(e) => log(&format!("biblioteca (álbumes/artistas) falló: {e}")),
    });
    std::thread::spawn(move || match engine.call("rootlist", json!({}), Duration::from_secs(60)) {
        Ok(v) => match serde_json::from_value::<Page<Option<Playlist>>>(v) {
            Ok(page) => {
                let lists: Vec<Playlist> = page.items.into_iter().flatten().collect();
                on_ui(move |_ui, app| browse::show_playlists(app, lists));
            }
            Err(e) => log(&format!("rootlist: respuesta inesperada: {e}")),
        },
        Err(e) => log(&format!("rootlist falló: {e}")),
    });
}

/// Las mismas secciones personalizadas que consume Home.swift.
fn load_home(engine: Arc<Engine>) {
    let recent_engine = engine.clone();
    std::thread::spawn(move || {
        match recent_engine.call("recently_played", json!({}), Duration::from_secs(60)) {
            Ok(v) => {
                let mut seen = std::collections::HashSet::new();
                let tracks = v.get("items").and_then(Value::as_array).into_iter().flatten()
                    .filter_map(|it| serde_json::from_value::<Track>(it.get("track")?.clone()).ok())
                    .filter(|t| seen.insert(t.album.as_ref().and_then(|a| a.uri.clone()).unwrap_or_else(|| t.uri.clone())))
                    .take(12).collect();
                on_ui(move |ui, app| { show_home(app, tracks); ui.set_home_message("".into()); });
            }
            Err(e) => {
                log(&format!("recently_played: {e}"));
                on_ui(|ui, _| ui.set_home_message("No se pudieron cargar los recientes. Pulsa Actualizar para reintentar.".into()));
            }
        }
    });
    std::thread::spawn(move || {
        match engine.call("home_sections", json!({"time_zone": "America/Mexico_City"}), Duration::from_secs(60)) {
            Ok(v) => {
                if let Ok(sections) = serde_json::from_value::<spotify_core::HomeSections>(v) {
                    let (made, mixes) = (sections.made_for_you, sections.top_mixes);
                    on_ui(move |_, app| {
                        let ids: std::collections::HashSet<_> = made.iter().map(|p| p.uri.clone()).collect();
                        app.made_targets = made;
                        app.mix_targets = mixes.into_iter().filter(|p| !ids.contains(&p.uri)).collect();
                        for (kind, lists) in [(browse::Cards::Made, app.made_targets.clone()), (browse::Cards::Mixes, app.mix_targets.clone())] {
                            browse::fill_cards(app, kind, lists.iter().map(|p| (p.name.clone(), p.plain_description().unwrap_or_default(), p.image_url().map(str::to_owned))).collect());
                        }
                    });
                }
            }
            Err(e) => log(&format!("home_sections: {e}")),
        }
    });
}

fn show_home(app: &mut App, tracks: Vec<Track>) {
    app.card_targets = tracks.iter().map(|t| {
        match t.album.as_ref().filter(|a| a.uri.is_some()) {
            Some(a) => {
                let mut p = Playlist::new(a.id.as_deref().unwrap_or(""), a.uri.as_deref().unwrap(), &a.name);
                p.images = a.images.clone();
                p.artist_name = Some(spotify_core::catalog::primary_artist(&t.artists));
                Target::List(p)
            }
            None => Target::Track(t.uri.clone()),
        }
    }).collect();
    let items = tracks.iter().map(|t| {
        (t.album.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| t.name.clone()), t.artist_line(), t.image_url().map(str::to_owned))
    }).collect();
    browse::fill_cards(app, browse::Cards::Home, items);
}

// ---------- Conexión y login ----------

fn set_login(ui: &MainWindow, state: &str, message: &str) {
    ui.set_login_state(state.into());
    ui.set_login_message(message.into());
}

fn set_ready(ui: &MainWindow, app: &mut App) {
    if app.ready {
        return;
    }
    app.ready = true;
    set_login(ui, "ready", "");
    if let Some(engine) = app.engine.clone() {
        load_library(engine.clone());
        load_home(engine);
    }
    settings::preload(ui, app);
    settings::sync_engine_prefs(ui, app);
    // Inicia Soloist desde el arranque si ya hay clave; si aún no se configura, abre la bienvenida.
    if app.soloist.is_none() {
        if let Some(key) = secrets::get(soloist::KEY_SECRET) {
            if !app.prefer_soloist {
                app.prefer_soloist = true;
                let _ = std::fs::write(prefer_flag_path(), b"1");
            }
            app.solo_setup = true;
            start_soloist(app, key);
        } else {
            app.solo_setup = true;
            ui.set_soloist_step("key".into());
            ui.set_soloist_status(String::new().into());
            ui.set_soloist_open(true);
        }
    }
}

fn ensure_engine(app: &mut App) -> Result<Arc<Engine>, String> {
    if let Some(e) = app.engine.as_ref().filter(|e| e.is_alive()) {
        return Ok(e.clone());
    }
    let e = Engine::spawn(handle_event)?;
    app.engine = Some(e.clone());
    Ok(e)
}

fn save_credentials(result: &Value) {
    if let Some(c) = result.get("credentials").filter(|c| !c.is_null()) {
        if let Err(e) = secrets::set(secrets::ENGINE_CREDENTIALS, &c.to_string()) {
            log(&e);
        }
    }
}

/// Reconecta con la credencial guardada (hasta 5 intentos con espera creciente).
fn connect_saved(ui: &MainWindow, app: &mut App) {
    let Some(saved) = secrets::get(secrets::ENGINE_CREDENTIALS).and_then(|s| serde_json::from_str::<Value>(&s).ok()) else {
        return set_login(ui, "needs-login", "Inicia sesión en Spotify para reproducir música.");
    };
    let engine = match ensure_engine(app) {
        Ok(e) => e,
        Err(e) => return set_login(ui, "error", &e),
    };
    set_login(ui, "connecting", "Conectando con Spotify…");
    std::thread::spawn(move || {
        for attempt in 0..5u32 {
            match engine.call("connect", json!({"credentials": saved}), Duration::from_secs(90)) {
                Ok(result) => {
                    save_credentials(&result);
                    return on_ui(|ui, app| set_ready(ui, app));
                }
                Err(e) => {
                    log(&format!("conexión del motor falló: {e}"));
                    if !engine.is_alive() {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs((1u64 << attempt).min(8)));
                }
            }
        }
        on_ui(|ui, _| set_login(ui, "error", "No se pudo conectar a Spotify. Revisa tu conexión e inténtalo de nuevo."));
    });
}

fn start_login(ui: &MainWindow, app: &mut App) {
    let engine = match ensure_engine(app) {
        Ok(e) => e,
        Err(e) => return set_login(ui, "error", &e),
    };
    set_login(ui, "connecting", "Autoriza SpotifyMac en la ventana del navegador que se acaba de abrir…");
    std::thread::spawn(move || {
        match engine.call("login", json!({"scopes": login_scopes()}), Duration::from_secs(900)) {
            Ok(result) => {
                save_credentials(&result);
                on_ui(|ui, app| set_ready(ui, app));
            }
            Err(e) => {
                log(&format!("login falló: {e}"));
                on_ui(move |ui, _| set_login(ui, "error", &format!("No se pudo iniciar sesión: {e}")));
            }
        }
    });
}

/// librespot cierra el proceso si la cuenta no es Premium: se explica y no se reintenta.
fn free_account() -> bool {
    std::fs::read_to_string(engine::data_dir().join("engine.log")).is_ok_and(|l| l.contains("does not support"))
}

fn handle_event(event: EngineEvent) {
    match event {
        EngineEvent::State(s) => on_ui(move |ui, app| apply_state(ui, app, *s)),
        EngineEvent::Status { state: EngineStatus::Ready, username } => on_ui(move |ui, app| {
            app.username = username;
            if ui.get_user_name().is_empty() { if let Some(u) = &app.username { ui.set_user_name(u.clone().into()); } }
            set_ready(ui, app);
            catalog::load_pins(app);
            if let Some(browse::Source::Playlist(p))=&app.current_source { ui.set_list_can_edit(actions::can_edit(app,p)); }
        }),
        EngineEvent::Status { state: EngineStatus::Disconnected, .. } => on_ui(|ui, app| {
            let was_ready = std::mem::replace(&mut app.ready, false);
            app.player.playing = false;
            render_player(ui, app);
            if free_account() {
                secrets::remove(secrets::ENGINE_CREDENTIALS);
                return set_login(ui, "error", "Esta cuenta es Spotify Free. Para reproducir en SpotifyMac se necesita Spotify Premium.");
            }
            if was_ready || ui.get_login_state() == "connecting" {
                connect_saved(ui, app);
            }
        }),
        EngineEvent::Status { .. } => {}
        EngineEvent::Credentials(c) => {
            let _ = secrets::set(secrets::ENGINE_CREDENTIALS, &c.to_string());
        }
        EngineEvent::Queue(items) => on_ui(move |ui, app| queue_ui::set_engine_queue(ui, app, items)),
        EngineEvent::Remote(r) => on_ui(move |ui, app| connect::apply_remote(ui, app, *r)),
        EngineEvent::LoginAuthorized => on_ui(|ui, _| set_login(ui, "connecting", "Procesando…")),
        EngineEvent::LibraryChanged(kind) => on_ui(move |ui, app| {
            actions::library_changed(ui,app,&kind);
            if kind=="pins" {catalog::load_pins(app);}
            if let Some(e) = app.engine.clone() {
                load_library(e);
            }
        }),
        EngineEvent::AudioKeyError(unavailable) => {
            log(&format!("clave de audio rechazada (sin audio: {unavailable})"));
            on_ui(|ui, app| key_error(ui, app));
        }
        EngineEvent::BrowseTo(url) => log(&format!("login: el motor abrió el navegador en {url}")),
    }
}

// ---------- Soloist y despacho de controles ----------

fn prefer_flag_path() -> std::path::PathBuf {
    engine::data_dir().join("prefer_soloist")
}

/// ¿Se reproduce con Soloist ahora mismo?
fn use_soloist(app: &App) -> bool {
    app.prefer_soloist && app.solo_status == soloist::Status::Ready && app.soloist.is_some()
}

enum Ctl {
    Toggle,
    Next,
    Prev,
    Seek(i64),
    Volume(u32),
    Shuffle(bool),
    /// "off" | "context" | "track"
    Repeat(&'static str),
}

/// Manda una orden de reproducción al reproductor en uso (dispositivo remoto, Soloist o librespot).
fn ctl(app: &App, c: Ctl) {
    if let Some((to, _)) = &app.connect.remote {
        connect::remote_ctl(app, to, &c);
    } else if use_soloist(app) {
        let Some(s) = &app.soloist else { return };
        match c {
            Ctl::Toggle => s.command(if app.player.playing { "pause" } else { "play" }, json!({})),
            Ctl::Next => s.command("skip_next", json!({})),
            Ctl::Prev => s.command("skip_prev", json!({})),
            Ctl::Seek(ms) => s.command("seek", json!({"position_ms": ms.max(0)})),
            Ctl::Volume(v) => s.command("set_volume", json!({"volume": v.min(100)})),
            Ctl::Shuffle(on) => s.command("set_shuffle", json!({"enabled": on})),
            Ctl::Repeat(mode) => {
                s.command("set_repeat_track", json!({"enabled": false}));
                s.command("set_repeat_context", json!({"enabled": mode == "context"}));
                if mode == "track" {
                    s.command("set_repeat_track", json!({"enabled": true}));
                }
            }
        }
    } else if let Some(e) = &app.engine {
        match c {
            Ctl::Toggle => e.fire("toggle", json!({})),
            Ctl::Next => e.fire("next", json!({})),
            Ctl::Prev => e.fire("prev", json!({})),
            Ctl::Seek(ms) => e.fire("seek", json!({"ms": ms})),
            Ctl::Volume(v) => e.fire("volume", json!({"percent": v})),
            Ctl::Shuffle(on) => e.fire("shuffle", json!({"on": on})),
            Ctl::Repeat(mode) => e.fire("repeat", json!({"mode": mode})),
        }
    }
}

fn play_req(app: &mut App, req: PlayReq) {
    queue_ui::clear_autoplay_on_play(app);
    app.last_play = Some(req.clone());
    app.pending_track=None;
    let epoch=app.play_epoch.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
    // Si suena otro dispositivo (Alexa, teléfono…), la música nueva se carga allí, como en la app oficial.
    if let (Some((to, _)), Some(e)) = (app.connect.remote.clone(), app.engine.clone()) {
        let mut args = match &req {
            PlayReq::Context { uri, start: Some(start), skip } => json!({"uri": uri, "start_uri": start, "start_index": skip}),
            PlayReq::Context { uri, .. } => json!({"uri": uri}),
            PlayReq::Track(uri) => json!({"uri": uri}),
            PlayReq::Tracks { uris, start } => json!({"uris": uris, "start_uri": start}),
        };
        args["to"] = json!(to);
        std::thread::spawn(move || {
            if let Err(err) = e.call("remote_load", args, Duration::from_secs(8)) { log(&format!("carga remota: {err}")); }
        });
        return;
    }
    if use_soloist(app) {
        let Some(s) = app.soloist.clone() else { return };
        if !app.solo_active {
            s.command("activate", json!({}));
        }
        match &req {
            PlayReq::Context { uri, start: None, .. } => s.command("play",json!({"uri":uri})),
            PlayReq::Context { uri, start: Some(start), skip } => {
                app.pending_track=Some((start.clone(),Instant::now()));
                let (engine,device,gate,counter)=(app.engine.clone(),app.solo_device_id.clone(),app.play_dispatch.clone(),app.play_epoch.clone());
                let (uri,start,skip)=(uri.clone(),start.clone(),*skip);
                std::thread::spawn(move||{
                    let Ok(_guard)=gate.lock() else{return;};
                    if counter.load(std::sync::atomic::Ordering::SeqCst)!=epoch{return;}
                    let result=match(engine,device) {
                        (Some(e),Some(to))=>e.call("remote_load",json!({"to":to,"uri":uri,"start_uri":start,"start_index":skip}),Duration::from_secs(8)),
                        _=>Err("dispositivo Connect no anunciado".into()),
                    };
                    if result.is_err() && counter.load(std::sync::atomic::Ordering::SeqCst)==epoch {
                        // Soloist no admite offset en su WebSocket. El respaldo carga solo la canción,
                        // nunca una cascada de skip_next que atraviese la playlist.
                        s.command("play",json!({"uri":start}));
                        if let Err(e)=result {log(&format!("carga directa de canción: {e}"));}
                    }
                });
            }
            PlayReq::Track(uri) => {app.pending_track=Some((uri.clone(),Instant::now()));s.command("play",json!({"uri":uri}));}
            PlayReq::Tracks { start, .. } => {app.pending_track=Some((start.clone(),Instant::now()));s.command("play",json!({"uri":start}));}
        }
    } else if let Some(e) = &app.engine {
        match &req {
            PlayReq::Context { uri, start: Some(start), .. } => e.fire("play_context", json!({"uri": uri, "start_uri": start, "shuffle": app.shuffle, "repeat": app.repeat_engine})),
            PlayReq::Context { uri, .. } => e.fire("play_context", json!({"uri": uri, "shuffle": app.shuffle, "repeat": app.repeat_engine})),
            PlayReq::Track(uri) => e.fire("play_tracks", json!({"uris": [uri], "shuffle": app.shuffle, "repeat": app.repeat_engine})),
            PlayReq::Tracks { uris, start } => e.fire("play_tracks", json!({"uris": uris, "start_uri": start, "shuffle": app.shuffle, "repeat": app.repeat_engine})),
        }
    }
}

/// Paso del asistente y texto de estado para cada estado de Soloist.
fn solo_step(s: &soloist::Status) -> (&'static str, String) {
    use soloist::Status::*;
    match s {
        Off => ("key", "sin configurar".into()),
        Downloading => ("starting", "Descargando Soloist desde los servidores de Spotify…".into()),
        Starting => ("starting", "Abriendo Soloist y validando tu clave…".into()),
        NeedsPairing => ("pair", "esperando emparejamiento".into()),
        Ready => ("ready", "listo".into()),
        Failed(m) => ("error", m.clone()),
    }
}

fn start_soloist(app: &mut App, key: String) {
    if let Some(old) = app.soloist.take() {
        old.stop();
    }
    app.solo_status = soloist::Status::Starting;
    app.solo_active = false;
    app.soloist = Some(Soloist::start(key, |event| match event {
        soloist::Event::Status(s) => on_ui(move |ui, app| solo_status_changed(ui, app, s)),
        soloist::Event::Update(u, live) => on_ui(move |ui, app| solo_update(ui, app, u, *live)),
        soloist::Event::DeviceId(id) => on_ui(move|_,app|app.solo_device_id=Some(id)),
        soloist::Event::DaysLeft(d) => log(&format!("soloist: quedan {d} días")),
    }));
}

fn solo_status_changed(ui: &MainWindow, app: &mut App, s: soloist::Status) {
    let (step, text) = solo_step(&s);
    ui.set_soloist_status(text.into());
    log(&format!("soloist: estado {s:?}"));
    // El asistente solo se mueve solo mientras el usuario lo está configurando.
    if app.solo_setup {
        ui.set_soloist_step(step.into());
        match &s {
            soloist::Status::NeedsPairing | soloist::Status::Failed(_) => ui.set_soloist_open(true),
            // Clave validada y emparejado: el asistente se cierra solo.
            soloist::Status::Ready => {
                ui.set_soloist_open(false);
                app.solo_setup = false;
            }
            _ => {}
        }
    }
    if matches!(s, soloist::Status::Failed(_)) {
        app.replay_when_ready = false;
    }
    app.solo_status = s;
    if app.solo_status == soloist::Status::Ready {
        if app.replay_when_ready {
            app.replay_when_ready = false;
            if let Some(req) = app.last_play.clone() {
                play_req(app, req);
            }
        }
        render_player(ui, app);
        queue_ui::refresh(ui, app);
    }
}

fn solo_update(ui: &MainWindow, app: &mut App, u: Update, live: Live) {
    match u {
        Update::Auth { is_active, .. } | Update::Device { is_active } => app.solo_active = is_active,
        Update::Error(m) => log(&format!("soloist: error {m}")),
        Update::Live | Update::Details if use_soloist(app) => {
            if app.solo_active && live.is_playing() { connect::clear_remote(ui, app); }
            if app.connect.remote.is_some() { return; }
            let waiting=app.pending_track.as_ref().is_some_and(|(uri,at)|at.elapsed()<Duration::from_secs(12) && live.track.as_ref().is_none_or(|t|t.uri!=*uri));
            let changed=if waiting {false}else{app.pending_track=None;app.player.sync_solo(&live,u==Update::Live)};
            ui.set_shuffle(live.shuffle);
            ui.set_repeat_mode(repeat_to_ui(&live.repeat).into());
            app.shuffle = live.shuffle;
            app.repeat_engine = repeat_to_engine(&live.repeat);
            ui.set_volume((live.volume as f32 / 100.0).clamp(0.0, 1.0));
            render_player(ui, app);
            if changed {
                refresh_rows(app);
            }
            queue_ui::set_solo_queue(ui, app, live.queue);
        }
        _ => {}
    }
}

/// Spotify negó las claves de audio a librespot: se pasa a Soloist (y se recuerda para la próxima vez).
fn key_error(ui: &MainWindow, app: &mut App) {
    if use_soloist(app) || app.last_key_fallback.is_some_and(|t| t.elapsed() < Duration::from_secs(8)) {
        return;
    }
    app.last_key_fallback = Some(Instant::now());
    if !app.prefer_soloist {
        app.prefer_soloist = true;
        let _ = std::fs::write(prefer_flag_path(), b"1");
        log("librespot sin audio: se usará Soloist");
    }
    if let Some(e) = &app.engine {
        e.fire("pause", json!({})); // que el motor no se quede «sonando» sin audio
    }
    app.player.playing = false;
    render_player(ui, app);
    match (&app.solo_status, secrets::get(soloist::KEY_SECRET)) {
        (soloist::Status::Ready, _) => {
            if let Some(req) = app.last_play.clone() {
                play_req(app, req);
            }
        }
        (soloist::Status::Off | soloist::Status::Failed(_), Some(key)) => {
            // Ya hay clave: se arranca y, si falta emparejar o falla, el asistente se abre solo.
            app.replay_when_ready = true;
            app.solo_setup = true;
            ui.set_soloist_step("starting".into());
            ui.set_soloist_status("Abriendo Soloist…".into());
            start_soloist(app, key);
        }
        (soloist::Status::Off | soloist::Status::Failed(_), None) => {
            app.solo_setup = true;
            ui.set_soloist_step("key".into());
            ui.set_soloist_open(true);
        }
        (soloist::Status::NeedsPairing, _) => {
            app.replay_when_ready = true;
            app.solo_setup = true;
            ui.set_soloist_step("pair".into());
            ui.set_soloist_status("esperando emparejamiento".into());
            ui.set_soloist_open(true);
        }
        _ => app.replay_when_ready = true, // ya arrancando: se reproduce al estar listo
    }
}

fn open_in_browser(url: &str) {
    // Solo URLs https sin caracteres especiales del intérprete de comandos.
    if url.starts_with("https://") && !url.contains(['&', '^', '|', '"', ' ', '<', '>']) {
        #[cfg(windows)]
        let mut c = {
            use std::os::windows::process::CommandExt;
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "start", "", url]);
            c.creation_flags(0x0800_0000);
            c
        };
        #[cfg(target_os = "macos")]
        let mut c = {
            let mut c = std::process::Command::new("open");
            c.arg(url);
            c
        };
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let mut c = {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(url);
            c
        };
        let _ = c.spawn();
    }
}

// ---------- Arranque ----------

#[cfg(windows)]
struct WinTimerPeriod;
#[cfg(windows)]
impl WinTimerPeriod {
    fn acquire() -> Self {
        unsafe { windows_sys::Win32::Media::timeBeginPeriod(1); }
        Self
    }
}
#[cfg(windows)]
impl Drop for WinTimerPeriod {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Media::timeEndPeriod(1); }
    }
}

pub(crate) fn close_now_playing_view(ui: &MainWindow, app: &mut App, restore_page_media: bool) {
    ui.set_now_playing_open(false);
    app.np_timer.stop();
    ui.set_np_controls_visible(true);
    // Quita la cola al salir del reproductor grande para no reabrirla ni mostrar el panel flotante.
    ui.set_full_queue_open(false);
    ui.set_queue_open(false);
    ui.set_artist_picker_open(false);
    media::stop_now(ui, app);
    if restore_page_media {
        media::restore(ui, app);
    }
}

fn main() -> Result<(), slint::PlatformError> {
    updater::cleanup_old_binaries();
    // macOS: barra de título transparente integrada con el contenido (estilo apps nativas);
    // los botones de semáforo siempre visibles salvo en pantalla completa.
    #[cfg(target_os = "macos")]
    {
        use slint::winit_030::winit::platform::macos::WindowAttributesExtMacOS;
        let _ = slint::BackendSelector::new()
            .backend_name("winit".into())
            .with_winit_window_attributes_hook(|attrs| {
                attrs
                    .with_decorations(true)
                    .with_titlebar_transparent(true)
                    .with_title_hidden(true)
                    .with_fullsize_content_view(true)
            })
            .select();
    }
    #[cfg(windows)]
    let _timer_period = WinTimerPeriod::acquire();
    let ui = MainWindow::new()?;
    ui.set_is_macos(cfg!(target_os = "macos"));
    ui.set_app_version(env!("APP_BUILD_INFO").into());
    if let Err(error) = ui.window().set_rendering_notifier(|state, api| {
        if matches!(state, slint::RenderingState::RenderingSetup) {
            if matches!(api, slint::GraphicsAPI::NativeOpenGL { .. }) {
                log("interfaz: GPU/OpenGL, presentación sincronizada sin bloqueo VSync");
            }
            static FRAME_INIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !FRAME_INIT.swap(true, std::sync::atomic::Ordering::Relaxed) {
                let _ = slint::invoke_from_event_loop(|| { with_app(|ui, _| sync_window_frame(ui)); });
            }
        }
        media::on_render_state(state, api);
    }) {
        log(&format!("interfaz: notificador de GPU no disponible ({error})"));
    }

    let library = Rc::new(VecModel::<LibraryEntry>::default());
    let cards = Rc::new(VecModel::<CardEntry>::default());
    let rows = Rc::new(VecModel::<TrackEntry>::default());
    ui.set_library(ModelRc::from(library.clone()));
    ui.set_home_cards(ModelRc::from(cards.clone()));
    ui.set_tracks(ModelRc::from(rows.clone()));
    let made = Rc::new(VecModel::<CardEntry>::default());
    let mixes = Rc::new(VecModel::<CardEntry>::default());
    ui.set_made_cards(ModelRc::from(made.clone()));
    ui.set_mix_cards(ModelRc::from(mixes.clone()));
    let queue = queue_ui::QueueState::new();
    ui.set_queue_rows(ModelRc::from(queue.model.clone()));
    let search = browse::SearchState::new();
    ui.set_search_tracks(ModelRc::from(search.m_tracks.clone()));
    ui.set_search_artists(ModelRc::from(search.m_artists.clone()));
    ui.set_search_albums(ModelRc::from(search.m_albums.clone()));
    ui.set_search_playlists(ModelRc::from(search.m_playlists.clone()));
    ui.set_recent_searches(ModelRc::from(search.m_recents.clone()));
    apply_fonts(&ui);

    let catalog=catalog::Catalog::new(&ui);
    let media=media::Media::new(&ui, media::Output::Page);
    let media_now=media::Media::new(&ui, media::Output::Track);
    let lyrics=lyrics::State::new();
    ui.set_lyric_rows(ModelRc::from(lyrics.model.clone()));
    APP.with(|cell| {
        *cell.borrow_mut() = Some(App {
            ui: ui.as_weak(),
            engine: None,
            loader: Arc::new(Loader::new(8)),
            player: Player { track: None, playing: false, position_ms: 0, anchor: Instant::now() },
            playlists: Vec::new(),
            lib_albums: Vec::new(),
            lib_artists: Vec::new(),
            lib_shows: Vec::new(),
            library,
            cards,
            card_targets: Vec::new(),
            made, mixes, made_targets: vec![], mix_targets: vec![],
            tracks: Vec::new(),
            visible_indices: vec![], current_source: None, actions: actions::Actions::default(), username: None,
            catalog, media, media_now, lyrics, solo_device_id: None, play_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            play_dispatch: Arc::new(std::sync::Mutex::new(())), pending_track: None,
            rows,
            context: None,
            timer: Timer::default(),
            notify: notify::NotifyState::default(),
            sleep: notify::SleepState::default(),
            connect: connect::State::default(),
            settings: settings::State::default(),
            cover_url: None,
            ready: false,
            soloist: None,
            solo_status: soloist::Status::Off,
            solo_active: false,
            prefer_soloist: prefer_flag_path().is_file(),
            last_play: None,
            shuffle: false,
            repeat_engine: "off",
            replay_when_ready: false,
            last_key_fallback: None,
            history: Vec::new(),
            forward_history: Vec::new(),
            replaying: false,
            solo_setup: false,
            list_gen: 0,
            search,
            queue,
            np_timer: Timer::default(),
        });
    });

    // ---- Navegación ----
    wire_window_controls(&ui);
    ui.on_go_back(|| { with_app(|ui, app| go_back(ui, app)); });
    ui.on_go_forward(|| { with_app(|ui, app| go_forward(ui, app)); });
    ui.on_go(|page| {
        with_app(|ui, app| {
            if ui.get_page() != page { push_history(ui, app); }
            media::reset(ui, app);
            ui.set_page(page);
            ui.set_page_title(if page == 1 { "Buscar" } else { "Inicio" }.into());
        });
    });
    ui.on_set_sleep(|minutes| {
        with_app(|ui, app| notify::set_sleep(ui, app, minutes));
    });
    ui.on_open_library(|i| {
        with_app(|ui, app| browse::open_library_row(ui, app, i as usize));
    });
    ui.on_open_card(|i| {
        with_app(|ui, app| {
            match app.card_targets.get(i as usize).cloned() {
                Some(Target::List(p)) => browse::open_source(ui, app, browse::Source::Playlist(p)),
                Some(Target::Track(uri)) => play_req(app, PlayReq::Track(uri)),
                None => {}
            }
        });
    });
    ui.on_open_section(|kind, i| {
        with_app(|ui, app| {
            let lists = if kind == 0 { &app.made_targets } else { &app.mix_targets };
            if let Some(p) = lists.get(i as usize).cloned() { browse::open_source(ui, app, browse::Source::Playlist(p)); }
        });
    });
    ui.on_refresh_home(|| {
        with_app(|ui, app| {
            if let Some(e) = app.engine.clone() { ui.set_home_message("Actualizando…".into()); load_home(e); }
        });
    });
    ui.on_play_list(|| { with_app(|ui, app| browse::play_list_track(ui, app, 0)); });
    actions::wire(&ui);
    catalog::wire(&ui);
    media::wire(&ui);
    connect::wire(&ui);
    settings::wire(&ui);
    lyrics::wire(&ui);
    updater::wire(&ui);
    ui.on_open_now_playing(|| {
        with_app(|ui, app| {
            ui.set_now_playing_open(true);
            ui.set_queue_open(false);
            ui.set_full_queue_open(false);
            if ui.get_pref_lyrics() && !ui.get_lyrics_open() { ui.set_lyrics_open(true); }
            if let Some(track) = app.player.track.clone() {
                media::load_now(ui, app, &track);
                if ui.get_lyrics_open() {
                    lyrics::request(ui, app, &track);
                }
            }
            np_activity(ui, app);
        });
    });
    ui.on_np_activity(|| {
        with_app(np_activity);
    });
    ui.on_ensure_lyrics(|| {
        with_app(|ui, app| {
            if let Some(track) = app.player.track.clone() {
                lyrics::request(ui, app, &track);
            }
        });
    });
    ui.on_close_now_playing(|| {
        with_app(|ui, app| close_now_playing_view(ui, app, true));
    });
    ui.on_play_track(|i| {
        with_app(|ui, app| browse::play_list_track(ui, app, i as usize));
    });
    ui.on_search_edited(|text| {
        with_app(|ui, app| browse::search_edited(ui, app, text.to_string()));
    });
    ui.on_play_search_track(|i| {
        with_app(|ui, app| browse::play_search_track(ui, app, i as usize));
    });
    ui.on_open_search(|kind, i| {
        with_app(|ui, app| browse::open_search_result(ui, app, kind, i as usize));
    });
    ui.on_search_remember(|| { with_app(browse::remember_search); });
    ui.on_search_recent(|i| {
        with_app(|ui, app| {
            if let Some(q) = ui.get_recent_searches().row_data(i.max(0) as usize) {
                browse::search_for(ui, app, q.to_string(), 0);
            }
        });
    });
    ui.on_remove_recent(|i| { with_app(|_, app| browse::remove_recent(app, i.max(0) as usize)); });
    ui.on_clear_recents(|| { with_app(|_, app| browse::clear_recents(app)); });
    ui.on_search_genre(|q| { with_app(|ui, app| browse::search_for(ui, app, q.to_string(), 4)); });
    ui.on_search_load_more(|| { with_app(browse::load_more_tracks); });
    ui.on_open_search_top(|| { with_app(browse::open_top); });
    ui.on_play_search_top(|| { with_app(browse::play_top); });
    ui.on_play_search_artist(|i| { with_app(|ui, app| browse::play_search_artist(ui, app, i.max(0) as usize)); });
    ui.on_login(|| {
        with_app(start_login);
    });

    // ---- Cola ----
    ui.on_queue_remove(|i| {
        with_app(|ui, app| queue_ui::edit(ui, app, QueueOp::Remove(i as usize)));
    });
    ui.on_queue_top(|i| {
        with_app(|ui, app| queue_ui::edit(ui, app, QueueOp::ToTop(i as usize)));
    });
    ui.on_queue_play(|i| {
        with_app(|ui, app| queue_ui::play(ui, app, i.max(0) as usize));
    });
    ui.on_queue_move(|from, to| {
        with_app(|ui, app| queue_ui::edit(ui, app, QueueOp::Move { from: from.max(0) as usize, to: to.max(0) as usize }));
    });

    // ---- Soloist ----
    ui.on_soloist_open_page(|| open_in_browser(soloist::KEY_PAGE));
    ui.on_soloist_save(|key| {
        let key = key.trim().to_owned();
        if key.is_empty() {
            return;
        }
        with_app(|ui, app| {
            if let Err(e) = secrets::set(soloist::KEY_SECRET, &key) {
                log(&e);
                return ui.set_soloist_status("no se pudo guardar la clave".into());
            }
            app.prefer_soloist = true;
            let _ = std::fs::write(prefer_flag_path(), b"1");
            app.replay_when_ready = app.last_play.is_some();
            app.solo_setup = true;
            ui.set_soloist_step("starting".into());
            ui.set_soloist_status("Abriendo Soloist y validando tu clave…".into());
            start_soloist(app, key);
        });
    });
    // Engranaje: abre el asistente en el paso que corresponde al estado actual.
    ui.on_soloist_open_panel(|| {
        with_app(|ui, app| {
            let (step, text) = solo_step(&app.solo_status);
            ui.set_soloist_step(step.into());
            ui.set_soloist_status(text.into());
            app.solo_setup = step != "ready";
            ui.set_soloist_open(true);
        });
    });

    // ---- Controles ----
    ui.on_toggle_infinite_queue(|| {
        with_app(|ui, app| {
            let next = !ui.get_infinite_queue();
            ui.set_infinite_queue(next);
            if next {
                queue_ui::maybe_fill_infinite(ui, app);
                queue_ui::toast(ui, app, "Cola infinita activada");
            } else {
                queue_ui::toast(ui, app, "Cola infinita desactivada");
            }
        });
    });
    ui.on_toggle_play(|| {
        with_app(|ui, app| trigger_toggle_play(ui, app));
    });
    ui.on_next(|| {
        with_app(|_, app| trigger_next(app));
    });
    ui.on_previous(|| {
        with_app(|_, app| trigger_previous(app));
    });
    ui.on_seek(|v| {
        with_app(|ui, app| {
            let Some(dur) = app.player.track.as_ref().map(|t| t.duration_ms) else { return };
            let ms = (v as f64 * dur as f64) as i64;
            ctl(app, Ctl::Seek(ms));
            app.player.set_position(ms);
            render_progress(ui, app);
        });
    });
    // Barras de volumen (barra inferior y pantalla completa): controlan el dispositivo de Connect que suena.
    ui.on_set_volume(|v| {
        with_app(|ui, app| connect::set_volume(ui, app, v));
    });
    ui.on_toggle_shuffle(|| {
        with_app(|ui, app| {
            let on = !ui.get_shuffle();
            app.shuffle = on;
            ui.set_shuffle(on);
            ctl(app, Ctl::Shuffle(on));
        });
    });
    ui.on_cycle_repeat(|| {
        with_app(|ui, app| {
            let (ui_mode, engine_mode) = match ui.get_repeat_mode().as_str() {
                "off" => ("all", "context"),
                "all" => ("one", "track"),
                _ => ("off", "off"),
            };
            app.repeat_engine = engine_mode;
            ui.set_repeat_mode(ui_mode.into());
            ctl(app, Ctl::Repeat(engine_mode));
        });
    });

    // Pruebas: `--soloist-panel` abre el panel; `--soloist-test-key <clave>` arranca Soloist sin guardar nada.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--soloist-panel") {
        ui.set_soloist_open(true);
    }
    if let Some(key) = args.iter().position(|a| a == "--soloist-test-key").and_then(|i| args.get(i + 1)).cloned() {
        ui.set_soloist_open(true);
        with_app(|ui, app| {
            app.solo_setup = true;
            ui.set_soloist_step("starting".into());
            start_soloist(app, key);
        });
    }

    // `--now-playing` abre directamente la pantalla completa (útil para capturas y pruebas).
    if std::env::args().any(|a| a == "--now-playing") {
        ui.set_now_playing_open(true);
    }

    // Conexión: con credencial guardada se reconecta sola; si no, se pide iniciar sesión.
    with_app(|ui, app| {
        if engine::find_binary().is_none() {
            return set_login(ui, "error", "No se encontró el motor de reproducción. Descarga la última versión de SpotifyMac.");
        }
        match ensure_engine(app) {
            Ok(_) => connect_saved(ui, app),
            Err(e) => set_login(ui, "error", &e),
        }
    });

    let result = ui.run();
    // Al salir se cierra el motor (si no, quedaría un proceso huérfano hasta que Windows cierre su stdin).
    APP.with(|cell| cell.borrow_mut().take());
    result
}

#[cfg(test)]
mod player_tests {
    use super::*;

    #[test]
    fn home_sections_match_engine_response() {
        let sections: spotify_core::HomeSections = serde_json::from_value(json!({
            "made_for_you":[{"id":"p", "uri":"spotify:playlist:p", "name":"Para ti"}],
            "top_mixes":[{"id":"m", "uri":"spotify:playlist:m", "name":"Tu mix"}]
        })).unwrap();
        assert_eq!(sections.made_for_you[0].name, "Para ti");
        assert_eq!(sections.top_mixes[0].uri, "spotify:playlist:m");
    }

    #[test]
    fn volume_queue_and_options_do_not_rewind_progress() {
        let mut player = Player { track: None, playing: true, position_ms: 80_000, anchor: Instant::now() - Duration::from_secs(12) };
        let mut live = Live { status: "playing".into(), position_ms: 0, ..Live::default() };
        for event in [json!({"type":"volume_changed", "volume":20}), json!({"type":"queue_changed", "upcoming":[]}), json!({"type":"options_changed", "options":{"shuffle":true}})] {
            for update in spotify_core::soloist_protocol::handle(&mut live, &event, 0) {
                player.sync_solo(&live, update == Update::Live);
            }
            assert!(player.position() >= 92_000);
        }
        assert!(player.playing);
        assert_eq!(live.volume, 20);
    }

    #[test]
    fn pause_freezes_current_position_and_position_sync_allows_backward_seek() {
        let mut player = Player { track: None, playing: true, position_ms: 60_000, anchor: Instant::now() - Duration::from_secs(5) };
        let mut live = Live { status: "paused".into(), position_ms: 0, ..Live::default() };
        player.sync_solo(&live, false);
        assert!(!player.playing);
        assert!(player.position() >= 65_000);
        live.position_ms = 10_000;
        player.sync_solo(&live, true);
        assert_eq!(player.position(), 10_000);
    }

    #[test]
    fn parse_resize_direction_maps_all_eight_edges() {
        use slint::winit_030::winit::window::ResizeDirection as R;
        assert_eq!(parse_resize_direction(0), Some(R::North));
        assert_eq!(parse_resize_direction(3), Some(R::East));
        assert_eq!(parse_resize_direction(7), Some(R::SouthEast));
        assert_eq!(parse_resize_direction(-1), None);
        assert_eq!(parse_resize_direction(8), None);
    }
}
