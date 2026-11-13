//! Protocolo de Spotify Soloist (WebSocket local + registro). Portado de `Soloist.swift`.

use crate::models::{SpAlbum, SpArtist, SpImage, Track};
use serde_json::Value;

/// Estado en vivo de lo que suena en Soloist (empujado por su WebSocket).
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    /// idle, playing, paused, buffering
    pub status: String,
    pub track: Option<Track>,
    pub next: Option<Track>,
    /// Lo que viene después (canciones y episodios), según el último `queue_changed`.
    pub queue: Vec<Track>,
    pub context_uri: Option<String>,
    pub position_ms: i64,
    /// Velocidad de avance de `position_ms` (0 = no avanza).
    pub speed: f64,
    pub volume: i32,
    pub shuffle: bool,
    pub repeat: String,
}

impl Default for Live {
    fn default() -> Self {
        Live {
            status: "idle".into(),
            track: None,
            next: None,
            queue: Vec::new(),
            context_uri: None,
            position_ms: 0,
            speed: 0.0,
            volume: 70,
            shuffle: false,
            repeat: "off".into(),
        }
    }
}

impl Live {
    pub fn is_playing(&self) -> bool {
        self.status == "playing" || (self.status == "buffering" && self.speed > 0.0)
    }
}

/// Lo que cambió tras un mensaje.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// Sesión de Soloist: `logged_in` = emparejado; `is_active` = es el dispositivo que suena.
    Auth { logged_in: bool, is_active: bool },
    Device { is_active: bool },
    /// `live` cambió (la posición se mide ahora).
    Live,
    /// Cambiaron controles o metadatos; la posición anterior NO se ha vuelto a medir.
    Details,
    Error(String),
    /// Hay que pedir más datos (`get_state`, `get_queue`).
    Ask(&'static str),
}

fn name_of(e: Option<&Value>) -> Option<String> {
    let e = e?;
    let entity = e.get("entity").unwrap_or(e);
    entity.pointer("/decorations/identity/name")?.as_str().map(str::to_owned)
}

fn last_segment(uri: &str) -> Option<String> {
    uri.rsplit(':').next().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Canción a partir de un `item` de Soloist.
pub fn track_from_item(item: Option<&Value>) -> Option<Track> {
    let item = item?;
    let uri = item.get("uri")?.as_str().filter(|u| !u.is_empty())?;
    let deco = item.get("decorations").cloned().unwrap_or(Value::Null);
    let artists = deco
        .get("creators")
        .and_then(Value::as_array)
        .map(|cs| {
            cs.iter()
                .filter_map(|c| {
                    let n = name_of(Some(c))?;
                    let entity = c.get("entity").unwrap_or(c);
                    Some(SpArtist { name: n, id: entity.get("uri").and_then(Value::as_str).and_then(last_segment) })
                })
                .collect()
        })
        .unwrap_or_default();
    let parent = deco.get("parent");
    let parent_entity = parent.and_then(|p| p.get("entity")).or(parent);
    let order = ["xlarge", "large", "default", "small"];
    let mut covers: Vec<&Value> =
        deco.pointer("/visual_identity/cover").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    covers.sort_by_key(|c| order.iter().position(|o| Some(*o) == c.get("size").and_then(Value::as_str)).unwrap_or(9));
    let images: Vec<SpImage> =
        covers.iter().filter_map(|c| c.get("url").and_then(Value::as_str).map(|u| SpImage { url: u.to_owned() })).collect();
    let parent_uri = parent_entity.and_then(|p| p.get("uri")).and_then(Value::as_str);
    let album = SpAlbum {
        name: name_of(parent_entity).unwrap_or_default(),
        images: (!images.is_empty()).then_some(images),
        id: parent_uri.and_then(last_segment),
        uri: parent_uri.map(str::to_owned),
    };
    let duration = deco.pointer("/playback/duration_ms").and_then(Value::as_i64).unwrap_or(0);
    let explicit = deco
        .pointer("/playback/content_ratings")
        .and_then(Value::as_array)
        .is_some_and(|r| r.iter().any(|x| x.as_str() == Some("explicit")));
    Some(Track {
        id: last_segment(uri),
        uri: uri.to_owned(),
        name: name_of(Some(item)).unwrap_or_default(),
        artists,
        album: Some(album),
        duration_ms: duration,
        explicit: Some(explicit),
    })
}

fn apply_position(p: Option<&Value>, l: &mut Live, now_ms: i64) {
    let Some(p) = p else { return };
    let Some(ms) = p.get("position_ms").and_then(Value::as_i64) else { return };
    l.speed = p.get("speed").and_then(Value::as_f64).unwrap_or(0.0);
    l.position_ms = ms;
    // `timestamp_ms` es la hora del servidor: si el reloj local coincide (±5 s) se corrige el retraso.
    if let Some(ts) = p.get("timestamp_ms").and_then(Value::as_f64) {
        let drift = now_ms as f64 - ts;
        if drift > 0.0 && drift < 5000.0 && l.speed > 0.0 {
            l.position_ms = ms + (drift * l.speed) as i64;
        }
    }
}

fn apply_options(o: Option<&Value>, l: &mut Live) {
    let Some(o) = o else { return };
    if let Some(sh) = o.get("shuffle").and_then(Value::as_bool) {
        l.shuffle = sh;
    }
    if let Some(r) = o.get("repeat").and_then(Value::as_str) {
        l.repeat = r.to_owned();
    }
}

/// Interpreta un mensaje del WebSocket y actualiza `live`. `now_ms` = hora actual (ms desde 1970).
pub fn handle(live: &mut Live, v: &Value, now_ms: i64) -> Vec<Update> {
    let Some(kind) = v.get("type").and_then(Value::as_str) else { return vec![] };
    let b = |k: &str| v.get(k).and_then(Value::as_bool);
    match kind {
        "auth_state" => {
            let logged_in = b("logged_in").unwrap_or(false);
            let mut out = vec![Update::Auth { logged_in, is_active: b("is_active").unwrap_or(false) }];
            if logged_in {
                out.push(Update::Ask("get_state"));
            }
            out
        }
        "device_changed" => {
            let is_active = b("is_active").unwrap_or(false);
            let mut out = vec![Update::Device { is_active }];
            if is_active {
                out.push(Update::Ask("get_state"));
            }
            out
        }
        "playback_state" => {
            if let Some(s) = v.get("status").and_then(Value::as_str) {
                live.status = s.to_owned();
            }
            if v.get("item").is_some() {
                live.track = track_from_item(v.get("item"));
            }
            if v.get("context").is_some() {
                live.context_uri = v.pointer("/context/uri").and_then(Value::as_str).map(str::to_owned);
            }
            apply_position(v.get("position"), live, now_ms);
            if let Some(vol) = v.get("volume").and_then(Value::as_i64) {
                live.volume = vol as i32;
            }
            apply_options(v.get("options"), live);
            let measured = v.pointer("/position/position_ms").and_then(Value::as_i64).is_some();
            let mut out = vec![if measured { Update::Live } else { Update::Details }];
            if let Some(active) = b("is_active") {
                out.push(Update::Device { is_active: active });
            }
            out
        }
        "track_changed" => {
            live.track = track_from_item(v.get("item"));
            live.position_ms = 0;
            vec![Update::Live, Update::Ask("get_state"), Update::Ask("get_queue")]
        }
        "playback_changed" => {
            if let Some(s) = v.get("status").and_then(Value::as_str) {
                live.status = s.to_owned();
            }
            vec![Update::Details]
        }
        "position_sync" => {
            apply_position(v.get("position"), live, now_ms);
            vec![Update::Live]
        }
        "volume_changed" => {
            if let Some(vol) = v.get("volume").and_then(Value::as_i64) {
                live.volume = vol as i32;
            }
            vec![Update::Details]
        }
        "options_changed" => {
            apply_options(v.get("options"), live);
            vec![Update::Details]
        }
        "queue_changed" => {
            live.queue = v
                .get("upcoming")
                .and_then(Value::as_array)
                .map(|u| {
                    u.iter()
                        .filter_map(|i| track_from_item(i.get("item")))
                        .filter(|t| t.uri.starts_with("spotify:track:") || t.uri.starts_with("spotify:episode:"))
                        .collect()
                })
                .unwrap_or_default();
            live.next = live.queue.first().cloned();
            vec![Update::Details]
        }
        "context_changed" => {
            live.context_uri = v.pointer("/context/uri").and_then(Value::as_str).map(str::to_owned);
            vec![Update::Details]
        }
        "error" => vec![Update::Error(v.get("message").and_then(Value::as_str).unwrap_or("").to_owned())],
        _ => vec![],
    }
}

/// Datos que Soloist escribe en su registro al arrancar.
#[derive(Debug, Default, PartialEq)]
pub struct LogInfo {
    pub device_id: Option<String>,
    pub days_left: Option<i32>,
}

pub fn parse_log_line(line: &str) -> LogInfo {
    let mut info = LogInfo::default();
    if let Some(i) = line.find("device_id=") {
        let id: String = line[i + 10..].chars().take_while(|c| !c.is_whitespace() && *c != '\u{1b}').collect();
        if !id.is_empty() {
            info.device_id = Some(id);
        }
    }
    if let Some(i) = line.find("expires in ") {
        let n: String = line[i + 11..].chars().take_while(char::is_ascii_digit).collect();
        info.days_left = n.parse().ok();
    }
    info
}

/// Calidad de streaming de Soloist (`audio.play_bitrate_enumeration`): 160 → 3, 320 → 4, sin pérdida → 6.
pub fn quality_prefs(existing: &str, level: u32) -> String {
    const OURS: [&str; 5] = [
        "audio.play_bitrate_enumeration",
        "audio.play_bitrate_non_metered_enumeration",
        "audio.play_bitrate_non_metered_migrated",
        "audio.hifi_upgraded",
        "audio.allow_downgrade",
    ];
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| !OURS.iter().any(|k| l.starts_with(&format!("{k}="))))
        .map(str::to_owned)
        .collect();
    lines.push(format!("audio.play_bitrate_enumeration={level}"));
    lines.push(format!("audio.play_bitrate_non_metered_enumeration={level}"));
    lines.push("audio.play_bitrate_non_metered_migrated=true".into());
    if level >= 5 {
        lines.push("audio.hifi_upgraded=true".into());
        lines.push("audio.allow_downgrade=false".into());
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item() -> Value {
        json!({
            "uri": "spotify:track:abc",
            "decorations": {
                "identity": {"name": "Blinding Lights"},
                "creators": [{"entity": {"uri": "spotify:artist:w", "decorations": {"identity": {"name": "The Weeknd"}}}}],
                "parent": {"entity": {"uri": "spotify:album:al", "decorations": {"identity": {"name": "After Hours"}}}},
                "visual_identity": {"cover": [{"size": "small", "url": "https://s"}, {"size": "xlarge", "url": "https://x"}]},
                "playback": {"duration_ms": 200000, "content_ratings": ["explicit"]}
            }
        })
    }

    #[test]
    fn track_parsing() {
        let t = track_from_item(Some(&item())).unwrap();
        assert_eq!(t.name, "Blinding Lights");
        assert_eq!(t.artist_line(), "The Weeknd");
        assert_eq!(t.artists[0].id.as_deref(), Some("w"));
        assert_eq!(t.image_url(), Some("https://x")); // la más grande primero
        assert_eq!(t.album.as_ref().unwrap().name, "After Hours");
        assert_eq!(t.duration_ms, 200_000);
        assert_eq!(t.explicit, Some(true));
        assert_eq!(t.id.as_deref(), Some("abc"));
        assert!(track_from_item(Some(&json!({"uri": ""}))).is_none());
        assert!(track_from_item(None).is_none());
    }

    #[test]
    fn playback_state_updates_live() {
        let mut l = Live::default();
        let msg = json!({"type": "playback_state", "status": "playing", "item": item(),
            "context": {"uri": "spotify:album:al"}, "position": {"position_ms": 1000, "speed": 1.0},
            "volume": 55, "options": {"shuffle": true, "repeat": "context"}, "is_active": true});
        let out = handle(&mut l, &msg, 0);
        assert!(l.is_playing());
        assert_eq!((l.position_ms, l.volume, l.shuffle), (1000, 55, true));
        assert_eq!(l.repeat, "context");
        assert_eq!(l.context_uri.as_deref(), Some("spotify:album:al"));
        assert!(out.contains(&Update::Device { is_active: true }));
    }

    #[test]
    fn position_is_corrected_by_server_drift() {
        let mut l = Live::default();
        let msg = json!({"type": "position_sync", "position": {"position_ms": 1000, "speed": 1.0, "timestamp_ms": 10_000.0}});
        handle(&mut l, &msg, 10_800);
        assert_eq!(l.position_ms, 1800);
        // Reloj muy desviado: no se corrige.
        handle(&mut l, &msg, 99_999_999);
        assert_eq!(l.position_ms, 1000);
    }

    #[test]
    fn auth_and_errors() {
        let mut l = Live::default();
        let out = handle(&mut l, &json!({"type": "auth_state", "logged_in": true, "is_active": false}), 0);
        assert_eq!(out[0], Update::Auth { logged_in: true, is_active: false });
        assert!(out.contains(&Update::Ask("get_state")));
        let out = handle(&mut l, &json!({"type": "auth_state", "logged_in": false}), 0);
        assert_eq!(out, vec![Update::Auth { logged_in: false, is_active: false }]);
        assert_eq!(handle(&mut l, &json!({"type": "error", "message": "x"}), 0), vec![Update::Error("x".into())]);
        assert!(handle(&mut l, &json!({"type": "nada"}), 0).is_empty());
    }

    #[test]
    fn queue_picks_first_track() {
        let mut l = Live::default();
        let msg = json!({"type": "queue_changed", "upcoming": [{"item": {"uri": "spotify:delimiter:x"}}, {"item": item()}]});
        handle(&mut l, &msg, 0);
        assert_eq!(l.next.as_ref().unwrap().uri, "spotify:track:abc");
        assert_eq!(l.queue.len(), 1); // el separador no cuenta
    }

    #[test]
    fn partial_playback_state_does_not_claim_a_fresh_position() {
        let mut live = Live { status: "playing".into(), track: track_from_item(Some(&item())), position_ms: 70_000, ..Live::default() };
        let out = handle(&mut live, &json!({"type":"playback_state", "volume":25}), 0);
        assert_eq!(out, vec![Update::Details]);
        assert_eq!(live.position_ms, 70_000);
        assert!(live.track.is_some());
    }

    #[test]
    fn new_track_resets_position_until_server_sync() {
        let mut live = Live { position_ms: 70_000, ..Live::default() };
        let out = handle(&mut live, &json!({"type":"track_changed", "item":item()}), 0);
        assert!(out.contains(&Update::Live));
        assert_eq!(live.position_ms, 0);
    }

    #[test]
    fn log_lines() {
        let i = parse_log_line("ready, device_name=X device_id=abc123\u{1b}[0m rest");
        assert_eq!(i.device_id.as_deref(), Some("abc123"));
        assert_eq!(parse_log_line("client expires in 89 days").days_left, Some(89));
        assert_eq!(parse_log_line("nada"), LogInfo::default());
    }

    #[test]
    fn prefs_keep_other_lines() {
        let out = quality_prefs("foo=1\naudio.play_bitrate_enumeration=2\n", 4);
        assert!(out.contains("foo=1\n"));
        assert_eq!(out.matches("audio.play_bitrate_enumeration=").count(), 1);
        assert!(out.contains("audio.play_bitrate_enumeration=4"));
        assert!(!out.contains("hifi"));
        assert!(quality_prefs("", 6).contains("audio.allow_downgrade=false"));
    }
}
