//! Cola de reproducción: lista, edición (quitar, subir) y «añadir a la cola».

use super::*;
use spotify_core::queue::{self as q, QueueOp};
use spotify_core::QueueItem;

pub struct QueueState {
    /// Cola del motor (librespot), con uid para poder reordenarla.
    items: Vec<QueueItem>,
    /// Cola de Soloist (solo lectura).
    solo: Vec<Track>,
    pub model: Rc<VecModel<QueueRow>>,
    toast_timer: Timer,
    autoplay_inflight: bool,
    autoplay_history: Vec<String>,
    autoplay_last_empty_seed: Option<String>,
}

impl QueueState {
    pub fn new() -> Self {
        QueueState {
            items: vec![],
            solo: vec![],
            model: Rc::default(),
            toast_timer: Timer::default(),
            autoplay_inflight: false,
            autoplay_history: vec![],
            autoplay_last_empty_seed: None,
        }
    }
}

fn row(t: &Track, tag: &str) -> QueueRow {
    QueueRow { title: t.name.clone().into(), artist: t.artist_line().into(), tag: tag.into(), duration: format_time(t.duration_ms).into(), cover: Image::default() }
}

/// Dispositivo cuya cola se edita: el remoto que suena, Soloist o (None) este motor.
fn target(app: &App) -> Option<String> {
    if let Some((id, _)) = &app.connect.remote {
        return Some(id.clone());
    }
    if use_soloist(app) { app.solo_device_id.clone() } else { None }
}

/// Con Soloist se usa la cola que ve Connect (con uid, editable) si el motor la tiene; si no, la de Soloist.
fn solo_only(app: &App) -> bool {
    use_soloist(app) && app.queue.items.is_empty()
}

/// Reconstruye las filas según el reproductor en uso.
pub fn refresh(ui: &MainWindow, app: &mut App) {
    let soloist = solo_only(app);
    let tracks: Vec<Track> = if soloist {
        app.queue.solo.clone()
    } else {
        app.queue.items.iter().map(|i| i.track.clone()).collect()
    };
    let rows: Vec<QueueRow> = if soloist {
        tracks.iter().map(|t| row(t, "")).collect()
    } else {
        app.queue.items.iter().map(|i| row(&i.track, q::tag(i))).collect()
    };
    app.queue.model.set_vec(rows);
    // La siguiente canción: su letra (y traducción) se piden ya, para que aparezcan al cambiar.
    if crate::lyrics_visible(ui) {
        if let Some(next) = tracks.first() {
            lyrics::prefetch(ui, app, next);
        }
    }
    ui.set_queue_editable(!soloist && app.engine.is_some() && (!use_soloist(app) || app.solo_device_id.is_some()));
    let gen = app.list_gen;
    let jobs: Vec<_> = tracks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| t.image_url().map(|url| (i, url.to_owned())))
        .collect();
    for (i, url) in jobs {
        load_cover(app, &url, 72, move |_, app, d| {
            if app.list_gen != gen {
                return;
            }
            if let Some(mut row) = app.queue.model.row_data(i) {
                row.cover = to_image(d);
                app.queue.model.set_row_data(i, row);
            }
        });
    }
}

fn clean_engine_items(app: &App, items: Vec<QueueItem>) -> Vec<QueueItem> {
    let current_uri = app.player.track.as_ref().map(|t| t.uri.as_str());
    let non_queued_count = items.iter().filter(|i| !i.queued).count();
    let mut out = Vec::with_capacity(items.len());
    let mut last_non_queued: Option<String> = None;
    for item in items {
        if item.queued {
            out.push(item);
            continue;
        }
        // Evita el bucle sintético de librespot cuando repite la canción actual 40 veces en `next_tracks`.
        if non_queued_count > 2 && current_uri == Some(item.track.uri.as_str()) {
            continue;
        }
        if last_non_queued.as_deref() == Some(item.track.uri.as_str()) {
            continue;
        }
        last_non_queued = Some(item.track.uri.clone());
        out.push(item);
    }
    out
}

fn clean_solo_tracks(app: &App, tracks: Vec<Track>) -> Vec<Track> {
    let current_uri = app.player.track.as_ref().map(|t| t.uri.as_str());
    let total = tracks.len();
    let mut out = Vec::with_capacity(total);
    let mut last: Option<String> = None;
    for t in tracks {
        if total > 2 && current_uri == Some(t.uri.as_str()) {
            continue;
        }
        if last.as_deref() == Some(t.uri.as_str()) {
            continue;
        }
        last = Some(t.uri.clone());
        out.push(t);
    }
    out
}

pub fn set_engine_queue(ui: &MainWindow, app: &mut App, items: Vec<QueueItem>) {
    app.queue.items = clean_engine_items(app, items);
    refresh(ui, app);
    maybe_fill_infinite(ui, app);
}

pub fn set_solo_queue(ui: &MainWindow, app: &mut App, tracks: Vec<Track>) {
    app.queue.solo = clean_solo_tracks(app, tracks);
    refresh(ui, app);
    maybe_fill_infinite(ui, app);
}

fn enqueue_silent(app: &App, uri: &str) {
    if use_soloist(app) {
        if let Some(s) = &app.soloist {
            s.command("add_to_queue", json!({"uri": uri}));
        }
    } else if let Some(e) = &app.engine {
        let mut args = json!({"uri": uri});
        if let Some(to) = target(app) {
            args["to"] = json!(to);
        }
        e.fire("queue_add", args);
    }
}

/// Si la cola está por acabarse y el modo infinito está activo, añade recomendaciones basadas en lo que suena.
pub fn maybe_fill_infinite(ui: &MainWindow, app: &mut App) {
    if !ui.get_infinite_queue() || ui.get_repeat_mode() == "one" || app.queue.autoplay_inflight {
        return;
    }
    let Some(current) = app.player.track.clone() else {
        return;
    };
    if !current.uri.starts_with("spotify:track:") {
        return;
    }
    if app.queue.autoplay_history.last().map(String::as_str) != Some(current.uri.as_str()) {
        app.queue.autoplay_history.push(current.uri.clone());
        if app.queue.autoplay_history.len() > 60 {
            let excess = app.queue.autoplay_history.len() - 60;
            app.queue.autoplay_history.drain(..excess);
        }
        app.queue.autoplay_last_empty_seed = None;
    }
    let upcoming = if solo_only(app) { app.queue.solo.len() } else { app.queue.items.len() };
    if upcoming > 2 {
        return;
    }
    if app.queue.autoplay_last_empty_seed.as_deref() == Some(current.uri.as_str()) {
        return;
    }
    let Some(engine) = app.engine.clone() else {
        return;
    };
    app.queue.autoplay_inflight = true;

    let mut seeds = vec![current.uri.clone()];
    for u in app.queue.autoplay_history.iter().rev() {
        if u.starts_with("spotify:track:") && !seeds.contains(u) {
            seeds.push(u.clone());
            if seeds.len() >= 3 {
                break;
            }
        }
    }
    let mut exclude: Vec<String> = app.queue.autoplay_history.clone();
    for item in &app.queue.items {
        exclude.push(item.track.uri.clone());
    }
    for t in &app.queue.solo {
        exclude.push(t.uri.clone());
    }
    let seed_uri = current.uri;

    std::thread::spawn(move || {
        let res = engine
            .call(
                "recommend",
                json!({"seeds": seeds, "exclude": exclude, "limit": 8}),
                Duration::from_secs(25),
            )
            .and_then(|v| {
                serde_json::from_value::<spotify_core::TopTracksResponse>(v)
                    .map_err(|e| e.to_string())
            });
        on_ui(move |ui, app| {
            app.queue.autoplay_inflight = false;
            if !ui.get_infinite_queue() || ui.get_repeat_mode() == "one" {
                return;
            }
            let Ok(resp) = res else {
                return;
            };
            let mut existing: std::collections::HashSet<String> =
                app.queue.autoplay_history.iter().cloned().collect();
            if let Some(t) = &app.player.track {
                existing.insert(t.uri.clone());
            }
            for item in &app.queue.items {
                existing.insert(item.track.uri.clone());
            }
            for t in &app.queue.solo {
                existing.insert(t.uri.clone());
            }
            let picked: Vec<Track> = resp
                .tracks
                .into_iter()
                .filter(|t| t.uri.starts_with("spotify:track:") && existing.insert(t.uri.clone()))
                .take(5)
                .collect();
            if picked.is_empty() {
                app.queue.autoplay_last_empty_seed = Some(seed_uri);
                return;
            }
            for t in picked {
                app.queue.autoplay_history.push(t.uri.clone());
                enqueue_silent(app, &t.uri);
            }
            if app.queue.autoplay_history.len() > 60 {
                let excess = app.queue.autoplay_history.len() - 60;
                app.queue.autoplay_history.drain(..excess);
            }
        });
    });
}

/// Quita, mueve o pone la primera una canción, en el dispositivo que suene (por connect-state).
pub fn edit(ui: &MainWindow, app: &mut App, op: QueueOp) {
    if solo_only(app) {
        return;
    }
    let new = q::apply(&app.queue.items, op);
    if q::uids(&new) == q::uids(&app.queue.items) {
        return;
    }
    app.queue.items = new;
    refresh(ui, app);
    if let Some(e) = &app.engine {
        let mut args = json!({"uids": q::uids(&app.queue.items)});
        if let Some(to) = target(app) { args["to"] = json!(to); }
        e.fire("queue_set", args);
    }
}

/// Clic en una canción de la cola: suena ya; las anteriores salen de la cola, como en Spotify.
pub fn play(ui: &MainWindow, app: &mut App, i: usize) {
    let (uri, uid, track) = if solo_only(app) {
        let Some(t) = app.queue.solo.get(i).cloned() else { return };
        app.queue.solo.drain(..=i);
        (t.uri.clone(), String::new(), t)
    } else {
        let Some(item) = app.queue.items.get(i).cloned() else { return };
        app.queue.items = q::apply(&app.queue.items, QueueOp::JumpTo(i));
        (item.track.uri.clone(), item.uid.clone(), item.track)
    };
    let Some(e) = app.engine.clone() else { return };
    let mut args = json!({"uri": uri, "uid": uid});
    if let Some(to) = target(app) { args["to"] = json!(to); }
    e.fire("queue_jump", args);
    // Respuesta inmediata; el estado real llega del reproductor en un momento.
    app.player.track = Some(track);
    app.player.playing = true;
    app.player.set_position(0);
    render_player(ui, app);
    refresh(ui, app);
}

/// Clic derecho en una canción: la añade a la cola del reproductor en uso.
pub fn add_track(ui: &MainWindow, app: &mut App, track: Track) {
    if use_soloist(app) {
        if let Some(s) = &app.soloist {
            s.command("add_to_queue", json!({"uri": track.uri}));
        }
    } else if let Some(e) = &app.engine {
        e.fire("queue_add", json!({"uri": track.uri}));
    } else {
        return;
    }
    toast(ui, app, &format!("Añadida a la cola: {}", track.name));
}

pub fn toast(ui: &MainWindow, app: &mut App, msg: &str) {
    ui.set_toast(msg.into());
    app.queue.toast_timer.start(TimerMode::SingleShot, Duration::from_secs(2), || {
        with_app(|ui, _| ui.set_toast("".into()));
    });
}
