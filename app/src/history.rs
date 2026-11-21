//! Historial de escucha guardado en este equipo (las últimas canciones que sonaron).

use super::*;

const MAX: usize = 300;

#[derive(Default)]
pub struct HistoryState {
    last_uri: Option<String>,
    items: Option<Vec<(u64, Track)>>,
}

fn path() -> std::path::PathBuf {
    engine::data_dir().join("history.json")
}

fn load(state: &mut HistoryState) -> &mut Vec<(u64, Track)> {
    state.items.get_or_insert_with(|| {
        std::fs::read(path()).ok()
            .and_then(|b| serde_json::from_slice::<Vec<(u64, Track)>>(&b).ok())
            .unwrap_or_default()
    })
}

/// Apunta la canción que acaba de empezar (una vez por canción, la más reciente primero).
pub fn record(app: &mut App) {
    let Some(t) = app.player.track.clone() else { return };
    if !app.player.playing || !t.uri.starts_with("spotify:track:") || app.listened.last_uri.as_deref() == Some(t.uri.as_str()) {
        return;
    }
    app.listened.last_uri = Some(t.uri.clone());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let items = load(&mut app.listened);
    if items.first().is_some_and(|(_, first)| first.uri == t.uri) {
        return;
    }
    items.insert(0, (now, t));
    items.truncate(MAX);
    if let Ok(json) = serde_json::to_vec(items) {
        let _ = std::fs::write(path(), json);
    }
}

/// Canciones escuchadas, la más reciente primero (sin repetir la misma canción).
pub fn tracks(app: &mut App) -> Vec<Track> {
    let mut seen = std::collections::HashSet::new();
    load(&mut app.listened).iter().filter(|(_, t)| seen.insert(t.uri.clone())).map(|(_, t)| t.clone()).collect()
}
