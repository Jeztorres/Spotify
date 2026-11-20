//! Avisos del sistema al cambiar de canción y temporizador de apagado.

use super::*;

/// Notificación nativa con la canción que empieza (solo si la ventana no está a la vista).
pub fn track_changed(ui: &MainWindow, app: &mut App) {
    let Some(t) = app.player.track.clone() else { return };
    if !app.player.playing || app.notify.last_uri.as_deref() == Some(t.uri.as_str()) {
        return;
    }
    let first = app.notify.last_uri.is_none();
    app.notify.last_uri = Some(t.uri.clone());
    // La primera canción al abrir la app no se anuncia: el usuario ya la está viendo.
    if first || !ui.get_pref_notify() || window_in_front(ui) {
        return;
    }
    let album = t.album.as_ref().map(|a| a.name.clone()).unwrap_or_default();
    let body = if album.is_empty() { t.artist_line() } else { format!("{} · {}", t.artist_line(), album) };
    show(&t.name, &body);
}

fn window_in_front(ui: &MainWindow) -> bool {
    use slint::winit_030::WinitWindowAccessor;
    ui.window()
        .with_winit_window(|w| w.has_focus() && !w.is_minimized().unwrap_or(false) && w.is_visible().unwrap_or(true))
        .unwrap_or(true)
}

/// Muestra el aviso con lo que ofrece cada sistema, sin bloquear la interfaz.
fn show(title: &str, body: &str) {
    #[cfg(windows)]
    crate::media_keys::notify(title, body);
    #[cfg(not(windows))]
    {
        let (title, body) = (title.to_owned(), body.to_owned());
        std::thread::spawn(move || {
            #[cfg(target_os = "macos")]
            {
                let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
                let script = format!("display notification \"{}\" with title \"{}\"", esc(&body), esc(&title));
                let _ = std::process::Command::new("osascript").arg("-e").arg(script).output();
            }
            #[cfg(all(not(windows), not(target_os = "macos")))]
            {
                let _ = std::process::Command::new("notify-send").args(["-a", "SpotifyMac", &title, &body]).output();
            }
        });
    }
}

#[derive(Default)]
pub struct NotifyState {
    last_uri: Option<String>,
}

// ---------- Temporizador de apagado ----------

#[derive(Default)]
pub struct SleepState {
    deadline: Option<Instant>,
    end_of_track: bool,
    timer: Timer,
}

/// `minutes`: 0 apaga el temporizador, -1 pausa al terminar la canción actual, >0 minutos.
pub fn set_sleep(ui: &MainWindow, app: &mut App, minutes: i32) {
    app.sleep.deadline = (minutes > 0).then(|| Instant::now() + Duration::from_secs(minutes as u64 * 60));
    app.sleep.end_of_track = minutes == -1;
    ui.set_sleep_choice(minutes);
    if minutes == 0 {
        app.sleep.timer.stop();
        ui.set_sleep_label("".into());
        return;
    }
    render_sleep(ui, app);
    app.sleep.timer.start(TimerMode::Repeated, Duration::from_millis(400), || {
        with_app(|ui, app| tick(ui, app));
    });
}

fn tick(ui: &MainWindow, app: &mut App) {
    let due = if let Some(deadline) = app.sleep.deadline {
        Instant::now() >= deadline
    } else if app.sleep.end_of_track {
        // Se pausa justo antes del final, para que no llegue a empezar la siguiente.
        app.player.track.as_ref().is_some_and(|t| t.duration_ms > 0 && t.duration_ms as i64 - app.player.position() <= 900)
    } else {
        false
    };
    if due {
        if app.player.playing {
            trigger_set_playing(ui, app, false);
        }
        set_sleep(ui, app, 0);
        queue_ui::toast(ui, app, if ui.get_pref_lang() == "en" { "Sleep timer: playback paused" } else { "Temporizador: música en pausa" });
        return;
    }
    render_sleep(ui, app);
}

fn render_sleep(ui: &MainWindow, app: &App) {
    let en = ui.get_pref_lang() == "en";
    let label = if let Some(deadline) = app.sleep.deadline {
        let left = deadline.saturating_duration_since(Instant::now()).as_secs();
        if left >= 60 { format!("{} min", left.div_ceil(60)) } else { format!("0:{left:02}") }
    } else if app.sleep.end_of_track {
        if en { "End of track".into() } else { "Fin de la canción".into() }
    } else {
        String::new()
    };
    ui.set_sleep_label(label.into());
}
