//! Protocolo JSON (una línea por mensaje) con el proceso `spotifymac-engine`.

use crate::models::{QueueItem, Track};
use serde::Deserialize;
use serde_json::Value;

/// Estado que empuja el motor en cada cambio de reproducción.
#[derive(Debug, Clone, Deserialize)]
pub struct EngineState {
    pub device_id: String,
    pub playing: bool,
    pub position_ms: i64,
    #[serde(default)]
    pub track: Option<Track>,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default, rename = "repeat")]
    pub repeat: String,
    #[serde(default)]
    pub volume: i32,
    /// true cuando librespot se detuvo (la reproducción pasó a otro dispositivo Connect).
    #[serde(default)]
    pub stopped: Option<bool>,
    #[serde(default)]
    pub loading: Option<bool>,
}

/// Lo que suena en otro dispositivo Spotify Connect (Alexa, teléfono…). `device_id: None` = ya no suena nada fuera.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RemoteState {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub volume_percent: Option<i64>,
    #[serde(default)]
    pub playing: bool,
    #[serde(default)]
    pub position_ms: i64,
    /// Instante (ms Unix) en que se calculó `position_ms`.
    #[serde(default)]
    pub measured_at: i64,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default)]
    pub repeat: String,
    #[serde(default)]
    pub track: Option<Track>,
}

impl RemoteState {
    /// Posición interpolada: Connect no avisa cada segundo, solo al cambiar de canción, pausar o buscar.
    /// `progresoActual = progresoBase + (ahora − últimaSincronización)` mientras suena.
    pub fn position_at(&self, now_unix_ms: i64) -> i64 {
        let elapsed = if self.playing && self.measured_at > 0 { (now_unix_ms - self.measured_at).max(0) } else { 0 };
        self.position_ms.max(0) + elapsed
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineStatus {
    NeedsLogin,
    Connecting,
    Ready,
    Disconnected,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Status { state: EngineStatus, username: Option<String> },
    State(Box<EngineState>),
    /// Credencial reutilizable renovada: hay que guardarla.
    Credentials(Value),
    LoginAuthorized,
    /// `true` = la canción no se pudo reproducir.
    AudioKeyError(bool),
    /// El navegador debe abrir esta URL (login).
    BrowseTo(String),
    LibraryChanged(String),
    /// Cola completa de lo que viene después, con el uid de cada canción.
    Queue(Vec<QueueItem>),
    Remote(Box<RemoteState>),
}

/// Interpreta una línea del motor que NO es respuesta a una orden.
pub fn parse_line(line: &str) -> Option<EngineEvent> {
    if let Some(url) = line.strip_prefix("Browse to: ") {
        return Some(EngineEvent::BrowseTo(url.trim().to_owned()));
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let event = v.get("event")?.as_str()?;
    Some(match event {
        "status" => EngineEvent::Status {
            state: match v.get("state").and_then(Value::as_str) {
                Some("ready") => EngineStatus::Ready,
                Some("connecting") => EngineStatus::Connecting,
                Some("disconnected") => EngineStatus::Disconnected,
                _ => EngineStatus::NeedsLogin,
            },
            username: v.get("username").and_then(Value::as_str).filter(|u| !u.is_empty()).map(str::to_owned),
        },
        "state" => EngineEvent::State(Box::new(serde_json::from_value(v).ok()?)),
        "credentials" => EngineEvent::Credentials(v.get("credentials").cloned().filter(|c| !c.is_null())?),
        "login_authorized" => EngineEvent::LoginAuthorized,
        "audio_key_error" => EngineEvent::AudioKeyError(false),
        "audio_key_unavailable" => EngineEvent::AudioKeyError(true),
        "library_changed" => EngineEvent::LibraryChanged(v.get("kind").and_then(Value::as_str).unwrap_or("other").to_owned()),
        "queue" => EngineEvent::Queue(
            v.get("items").cloned().and_then(|i| serde_json::from_value(i).ok()).unwrap_or_default(),
        ),
        "remote_state" => EngineEvent::Remote(Box::new(serde_json::from_value(v).ok()?)),
        _ => return None,
    })
}

/// Una línea de respuesta: `{"id":1,"ok":true,"result":…}` o `{"id":1,"ok":false,"error":"…"}`.
pub fn parse_reply(line: &str) -> Option<(u64, Result<Value, String>)> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("event").is_some() {
        return None;
    }
    let id = v.get("id")?.as_u64()?;
    let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(false);
    Some((
        id,
        if ok {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(v.get("error").and_then(Value::as_str).unwrap_or("Error del motor").to_owned())
        },
    ))
}

/// Permisos que pide la app al iniciar sesión (los mismos que `Auth.swift`).
pub const LOGIN_SCOPES: &str = "streaming user-read-email user-read-private user-read-playback-state \
user-modify-playback-state user-read-currently-playing playlist-read-private playlist-read-collaborative \
user-library-read user-library-modify playlist-modify-public playlist-modify-private user-read-recently-played \
user-top-read";

pub fn login_scopes() -> Vec<&'static str> {
    LOGIN_SCOPES.split_whitespace().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_event() {
        let line = r#"{"event":"state","device_id":"d","playing":true,"position_ms":1234,
            "track":{"id":"1","uri":"spotify:track:1","name":"N","artists":[{"name":"A"}],
                     "album":{"name":"Al","images":[{"url":"https://i/1"}]},"duration_ms":200000},
            "shuffle":false,"repeat":"off","volume":50,"stopped":false,"loading":false}"#;
        match parse_line(&line.replace('\n', " ")) {
            Some(EngineEvent::State(s)) => {
                assert!(s.playing);
                assert_eq!(s.position_ms, 1234);
                assert_eq!(s.track.unwrap().name, "N");
                assert_eq!(s.volume, 50);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn state_with_null_track() {
        let line = r#"{"event":"state","device_id":"d","playing":false,"position_ms":0,"track":null,"shuffle":false,"repeat":"off","volume":10}"#;
        assert!(matches!(parse_line(line), Some(EngineEvent::State(s)) if s.track.is_none()));
    }

    #[test]
    fn parses_status_and_browse() {
        assert!(matches!(
            parse_line(r#"{"event":"status","state":"ready","username":"ana"}"#),
            Some(EngineEvent::Status { state: EngineStatus::Ready, username: Some(u) }) if u == "ana"
        ));
        assert!(matches!(
            parse_line(r#"{"event":"status","state":"needs_credentials","device_id":"x"}"#),
            Some(EngineEvent::Status { state: EngineStatus::NeedsLogin, username: None })
        ));
        assert!(matches!(parse_line("Browse to: https://accounts.spotify.com/x "), Some(EngineEvent::BrowseTo(u)) if u == "https://accounts.spotify.com/x"));
        assert!(parse_line("basura").is_none());
    }

    #[test]
    fn replies_are_not_events() {
        assert!(parse_line(r#"{"id":3,"ok":true,"result":null}"#).is_none());
        assert!(matches!(parse_reply(r#"{"id":3,"ok":true,"result":{"a":1}}"#), Some((3, Ok(_)))));
        assert!(matches!(parse_reply(r#"{"id":4,"ok":false,"error":"nope"}"#), Some((4, Err(e))) if e == "nope"));
        assert!(parse_reply(r#"{"event":"state"}"#).is_none());
    }

    #[test]
    fn parses_queue_event() {
        let line = r#"{"event":"queue","next":null,"items":[
            {"uid":"u1","queued":true,"autoplay":false,"track":{"id":"1","uri":"spotify:track:1","name":"N","artists":[{"name":"A"}],"duration_ms":1000}},
            {"uid":"u2","queued":false,"autoplay":true,"track":{"id":"2","uri":"spotify:track:2","name":"M","artists":[],"duration_ms":2000}}]}"#;
        match parse_line(&line.replace('\n', " ")) {
            Some(EngineEvent::Queue(items)) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].uid, "u1");
                assert!(items[0].queued);
                assert_eq!(items[1].autoplay, Some(true));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(parse_line(r#"{"event":"queue","items":[]}"#), Some(EngineEvent::Queue(v)) if v.is_empty()));
    }

    #[test]
    fn parses_remote_state_and_interpolates() {
        let line = r#"{"event":"remote_state","seq":3,"device_id":"alexa","device_name":"Echo","playing":true,"position_ms":1000,"measured_at":5000,
            "track":{"id":"1","uri":"spotify:track:1","name":"N","artists":[{"name":"A"}],"duration_ms":0}}"#;
        match parse_line(&line.replace('\n', " ")) {
            Some(EngineEvent::Remote(r)) => {
                assert_eq!(r.device_id.as_deref(), Some("alexa"));
                assert_eq!(r.position_at(7500), 3500);
                assert_eq!(r.seq, 3);
            }
            other => panic!("{other:?}"),
        }
        match parse_line(r#"{"event":"remote_state","device_id":null}"#) {
            Some(EngineEvent::Remote(r)) => assert!(r.device_id.is_none()),
            other => panic!("{other:?}"),
        }
        let paused = RemoteState { playing: false, position_ms: 900, measured_at: 1, ..Default::default() };
        assert_eq!(paused.position_at(99_999), 900);
    }

    #[test]
    fn scopes_include_streaming() {
        let s = login_scopes();
        assert_eq!(s[0], "streaming");
        assert!(s.contains(&"user-top-read"));
        assert_eq!(s.len(), 14);
    }
}
