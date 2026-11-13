//! Spotify Connect: elegir dispositivo y controlar su volumen. El motor habla `connect-state`
//! (el mismo listado que ve el teléfono), sin Web API.
use super::*;

const COMPUTER: &str = "M4 5 H20 V16 H4 Z M2 20 H22";
const PHONE: &str = "M8 3 H16 C16.6 3 17 3.4 17 4 V20 C17 20.6 16.6 21 16 21 H8 C7.4 21 7 20.6 7 20 V4 C7 3.4 7.4 3 8 3 Z M11 18 H13";
const SPEAKER: &str = "M7 3 H17 C17.6 3 18 3.4 18 4 V20 C18 20.6 17.6 21 17 21 H7 C6.4 21 6 20.6 6 20 V4 C6 3.4 6.4 3 7 3 Z M12 12 A3 3 0 1 0 12 18 A3 3 0 1 0 12 12 Z M12 6.5 A0.5 0.5 0 1 0 12 7.5 A0.5 0.5 0 1 0 12 6.5 Z";
const TV: &str = "M3 5 H21 V16 H3 Z M8 20 H16";

#[derive(Default)]
pub struct State {
    pub model: Rc<VecModel<ConnectDevice>>,
    /// Ids de los dispositivos del modelo (mismo orden), para pasar la música al elegido.
    ids: Vec<String>,
    /// Dispositivo activo: (id, es este equipo).
    active: Option<(String, bool)>,
    volume_timer: Timer,
    /// Dispositivo de fuera que suena ahora (id, nombre): los controles van a él.
    pub remote: Option<(String, String)>,
    /// Número del último `remote_state` aplicado (descarta los que llegan tarde).
    remote_seq: u64,
    /// Último movimiento del volumen remoto hecho aquí: evita que un evento viejo haga saltar la barra.
    volume_dragged: Option<std::time::Instant>,
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Estado de otro dispositivo (Alexa, teléfono…): se muestra en el reproductor y la posición se interpola.
pub fn apply_remote(ui: &MainWindow, app: &mut App, r: spotify_core::engine_protocol::RemoteState) {
    if r.seq != 0 && r.seq < app.connect.remote_seq {
        return;
    }
    app.connect.remote_seq = app.connect.remote_seq.max(r.seq);
    let Some(id) = r.device_id.clone() else { return clear_remote(ui, app) };
    // Soloist es este equipo: sus propios eventos ya mandan.
    if app.solo_device_id.as_deref() == Some(id.as_str()) || r.device_name == soloist::DEVICE_NAME {
        return clear_remote(ui, app);
    }
    let previous = app.player.track.as_ref().map(|t| t.uri.clone());
    ui.set_device_label(format!("Reproduciendo en {}", r.device_name).into());
    app.connect.remote = Some((id, r.device_name.clone()));
    app.player.playing = r.playing;
    app.player.set_position(r.position_at(unix_ms()));
    if r.track.is_some() {
        app.player.track = r.track;
    }
    ui.set_shuffle(r.shuffle);
    ui.set_repeat_mode(repeat_to_ui(&r.repeat).into());
    // La barra de volumen muestra el del dispositivo que suena (Alexa, teléfono…), salvo mientras se arrastra.
    if let Some(v) = r.volume_percent {
        let v = (v as f32 / 100.0).clamp(0.0, 1.0);
        ui.set_connect_volume(v);
        if app.connect.volume_dragged.is_none_or(|t| t.elapsed() > Duration::from_millis(1500)) {
            ui.set_volume(v);
        }
    }
    ui.set_connect_remote(true);
    render_player(ui, app);
    if previous != app.player.track.as_ref().map(|t| t.uri.clone()) {
        refresh_rows(app);
    }
}

pub fn clear_remote(ui: &MainWindow, app: &mut App) {
    if app.connect.remote.take().is_some() {
        // La música volvió a este equipo: el volumen vuelve a ser el local.
        if app.connect.active.as_ref().is_some_and(|(_, local)| !local) { app.connect.active = None; }
        ui.set_device_label("".into());
        ui.set_connect_remote(false);
    }
}

/// Orden de reproducción para el dispositivo remoto, por connect-state.
pub fn remote_ctl(app: &App, to: &str, c: &Ctl) {
    let Some(e) = &app.engine else { return };
    let send = |endpoint: &str, value: Option<Value>| {
        let mut args = json!({"to": to, "endpoint": endpoint});
        if let Some(v) = value { args["value"] = v; }
        e.fire("remote", args);
    };
    match c {
        Ctl::Toggle => send(if app.player.playing { "pause" } else { "resume" }, None),
        Ctl::Next => send("skip_next", None),
        Ctl::Prev => send("skip_prev", None),
        Ctl::Seek(ms) => send("seek_to", Some(json!((*ms).max(0)))),
        Ctl::Volume(v) => e.fire("remote_volume", json!({"to": to, "percent": (*v).min(100)})),
        Ctl::Shuffle(on) => send("set_shuffling_context", Some(json!(on))),
        Ctl::Repeat(mode) => {
            send("set_repeating_context", Some(json!(*mode != "off")));
            send("set_repeating_track", Some(json!(*mode == "track")));
        }
    }
}

fn icon(kind: &str, name: &str) -> &'static str {
    let name = name.to_lowercase();
    match kind {
        "computer" => COMPUTER,
        "smartphone" | "tablet" => PHONE,
        "tv" | "castvideo" | "avr" | "stb" => TV,
        _ if name.contains("soloist") || name.contains("spotifymac") => COMPUTER,
        _ => SPEAKER,
    }
}
fn kind_label(kind: &str, local: bool) -> &'static str {
    if local {
        return "SpotifyMac · Spotify Connect";
    }
    match kind {
        "smartphone" => "Teléfono",
        "computer" => "Ordenador",
        "tablet" => "Tableta",
        "speaker" => "Altavoz",
        "tv" => "Televisor",
        "avr" | "stb" => "Receptor",
        "castaudio" | "castvideo" | "audiodongle" => "Google Cast",
        "gameconsole" => "Consola",
        "automobile" => "Coche",
        _ => "Dispositivo",
    }
}

pub fn open(ui: &MainWindow, app: &mut App) {
    ui.set_connect_message("".into());
    refresh(ui, app);
}

pub fn refresh(ui: &MainWindow, app: &mut App) {
    let Some(engine) = app.engine.clone() else {
        ui.set_connect_message("Spotify Connect todavía no está conectado.".into());
        return;
    };
    ui.set_connect_loading(true);
    std::thread::spawn(move || {
        let reply = engine.call("devices", json!({}), Duration::from_secs(10));
        on_ui(move |ui, app| apply(ui, app, reply));
    });
}

struct Dev {
    id: String,
    name: String,
    kind: String,
    active: bool,
    volume: i32,
    local: bool,
}

fn apply(ui: &MainWindow, app: &mut App, reply: Result<Value, String>) {
    ui.set_connect_loading(false);
    let value = match reply {
        Ok(v) => v,
        Err(e) => {
            ui.set_connect_message(format!("No se pudo leer la lista de dispositivos: {e}").into());
            return;
        }
    };
    let engine_self = value.get("self_id").and_then(Value::as_str).unwrap_or("").to_owned();
    let local_id = app.solo_device_id.clone();
    let mut devices: Vec<Dev> = value
        .get("devices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let id = d.get("id").and_then(Value::as_str)?.to_owned();
            // El motor sin audio no es un lugar donde escuchar: se oculta para no tener dos «SpotifyMac».
            if id == engine_self {
                return None;
            }
            let name = d.get("name").and_then(Value::as_str).unwrap_or("Dispositivo").to_owned();
            let local = local_id.as_deref() == Some(id.as_str()) || (local_id.is_none() && name == soloist::DEVICE_NAME);
            Some(Dev {
                kind: d.get("type").and_then(Value::as_str).unwrap_or("").to_owned(),
                active: d.get("is_active").and_then(Value::as_bool).unwrap_or(false),
                volume: d.get("volume_percent").and_then(Value::as_i64).unwrap_or(50).clamp(0, 100) as i32,
                id,
                name,
                local,
            })
        })
        .collect();
    devices.sort_by(|a, b| {
        b.local
            .cmp(&a.local)
            .then(b.active.cmp(&a.active))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let entry = |d: &Dev| ConnectDevice {
        id: d.id.clone().into(),
        name: if d.local { "Este equipo".to_owned() } else { d.name.clone() }.into(),
        kind: kind_label(&d.kind, d.local).into(),
        icon: icon(&d.kind, &d.name).into(),
        active: d.active,
        local: d.local,
    };
    let active = devices.iter().find(|d| d.active);
    app.connect.active = active.map(|d| (d.id.clone(), d.local));
    ui.set_has_connect_active(active.is_some());
    if let Some(a) = active {
        ui.set_connect_active(entry(a));
        ui.set_connect_remote(!a.local);
        ui.set_connect_volume(if a.local { ui.get_volume() } else { a.volume as f32 / 100.0 });
    } else {
        ui.set_connect_remote(false);
    }
    let others: Vec<&Dev> = devices.iter().filter(|d| !d.active).collect();
    app.connect.ids = others.iter().map(|d| d.id.clone()).collect();
    app.connect.model.set_vec(others.iter().map(|d| entry(d)).collect::<Vec<_>>());
    ui.set_connect_message(
        if devices.is_empty() {
            "No hay dispositivos. Abre Spotify en el teléfono o di «Alexa, abre Spotify» y pulsa actualizar."
        } else {
            ""
        }
        .into(),
    );
}

pub fn transfer(ui: &MainWindow, app: &mut App, index: usize) {
    let (Some(id), Some(engine)) = (app.connect.ids.get(index).cloned(), app.engine.clone()) else { return };
    if !ui.get_connect_busy().is_empty() {
        return;
    }
    ui.set_connect_busy(id.clone().into());
    ui.set_connect_message("".into());
    std::thread::spawn(move || {
        let result = engine.call("transfer_any", json!({"to": id}), Duration::from_secs(15));
        if result.is_ok() {
            // El estado nuevo ya llega por `remote_state`; esto solo refresca la lista del panel.
            std::thread::sleep(Duration::from_millis(600));
        }
        on_ui(move |ui, app| {
            ui.set_connect_busy("".into());
            match result {
                Ok(_) => refresh(ui, app),
                Err(e) => ui.set_connect_message(format!("No se pudo pasar la música a ese dispositivo: {e}").into()),
            }
        });
    });
}

/// Volumen del dispositivo activo: el de este equipo va por el reproductor; el remoto, por connect-state.
pub fn set_volume(ui: &MainWindow, app: &mut App, v: f32) {
    let percent = (v.clamp(0.0, 1.0) * 100.0).round() as u32;
    // El dispositivo que suena según Connect manda; la lista del panel puede estar desactualizada.
    let target = app.connect.remote.clone().map(|(id, _)| (id, false)).or_else(|| app.connect.active.clone());
    match target {
        Some((id, false)) => {
            let Some(engine) = app.engine.clone() else { return };
            app.connect.volume_dragged = Some(std::time::Instant::now());
            ui.set_volume(v);
            ui.set_connect_volume(v);
            // Un envío cada 120 ms como mucho: el último valor siempre llega.
            app.connect.volume_timer.start(TimerMode::SingleShot, Duration::from_millis(120), move || {
                engine.fire("remote_volume", json!({"to": id, "percent": percent}));
            });
        }
        _ => {
            ui.set_volume(v);
            ctl(app, Ctl::Volume(percent));
        }
    }
}

pub fn wire(ui: &MainWindow) {
    if let Some(model) = with_app(|_, app| app.connect.model.clone()) {
        ui.set_connect_devices(ModelRc::from(model));
    }
    ui.on_open_connect(|| {
        with_app(|ui, app| open(ui, app));
    });
    ui.on_refresh_connect(|| {
        with_app(|ui, app| refresh(ui, app));
    });
    ui.on_transfer_device(|i| {
        with_app(|ui, app| transfer(ui, app, i.max(0) as usize));
    });
    ui.on_connect_set_volume(|v| {
        with_app(|ui, app| set_volume(ui, app, v));
    });
}
