//! Motor de SpotifyMac: reproducción con librespot (Spotify Connect) y lecturas
//! de catálogo por la sesión de streaming, sin cuota de Web API.
//!
//! Protocolo: una orden JSON por línea en stdin (`{"id":1,"cmd":"…",…}`); por stdout
//! salen respuestas (`{"id":1,"ok":true,"result":…}`) y eventos (`{"event":"…",…}`).
//! Los registros van a stderr y nunca incluyen tokens ni credenciales.
//!
//! Ideas de arquitectura tomadas de Spotifast/Fastpotify (MIT, github.com/crmne/fastpotify):
//! lecturas de rootlist, metadatos por lotes y artistas relacionados por Pathfinder.

mod reads;

use std::sync::{atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering}, Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use librespot_connect::{ConnectConfig, LoadContextOptions, LoadRequest, LoadRequestOptions, Options, PlayingTrack, Spirc};
use librespot_core::{authentication::Credentials, cache::Cache, config::{DeviceType, SessionConfig}, session::Session};
use librespot_metadata::audio::UniqueFields;
use librespot_playback::{
    audio_backend::{self, Sink, SinkResult},
    config::{AudioFormat, Bitrate, NormalisationType, PlayerConfig, VolumeCtrl},
    convert::Converter,
    decoder::AudioPacket,
    mixer::{self, Mixer, MixerConfig, VolumeGetter},
    player::{Player, PlayerEvent},
};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

const DEVICE_NAME: &str = "SpotifyMac";
/// Callback local de la aprobación de reproducción (distinto del de Web API, 8888).
const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";

type Out = mpsc::UnboundedSender<Value>;

/// Canal de eventos visible para el registro: así un rechazo de clave de audio se avisa a la app
/// en cuanto ocurre (unos segundos), sin esperar a que librespot agote sus reintentos (~40 s).
static EVENTS: OnceLock<Out> = OnceLock::new();

#[derive(Default, Clone)]
struct LocalState {
    playing: bool,
    position_ms: u32,
    updated: Option<Instant>,
    track: Option<Value>,
    shuffle: bool,
    repeat: &'static str,
    volume: u16,
    /// librespot dejó de ser el dispositivo activo (otro dispositivo Connect tomó la reproducción).
    stopped: bool,
    /// La canción anterior terminó sola: la siguiente entra con fundido (crossfade).
    pending_fade_in: bool,
    fade_in: bool,
    /// Cargando una canción (aún sin audio): la app lo usa para relevar al reproductor web si tarda.
    loading: bool,
}

impl LocalState {
    /// Posición estimada ahora mismo (la última conocida más lo transcurrido sonando).
    fn position_now(&self) -> u64 {
        let elapsed = if self.playing { self.updated.map_or(0, |t| t.elapsed().as_millis() as u64) } else { 0 };
        self.position_ms as u64 + elapsed
    }
    fn duration_ms(&self) -> u64 {
        self.track.as_ref().and_then(|t| t.get("duration_ms")).and_then(Value::as_u64).unwrap_or(0)
    }
}

/// Preferencias de reproducción que manda la app. El crossfade se aplica al momento;
/// el resto (calidad, normalización, sin pausas, autoplay) al crear el reproductor (al conectar).
#[derive(Clone)]
struct Prefs {
    bitrate: Bitrate,
    gapless: bool,
    normalise: bool,
    pregain_db: f64,
    autoplay: bool,
}

impl Default for Prefs {
    fn default() -> Self { Self { bitrate: Bitrate::Bitrate320, gapless: true, normalise: true, pregain_db: 0.0, autoplay: true } }
}

/// Volumen del mezclador multiplicado por la envolvente del crossfade.
/// librespot reproduce una sola pista a la vez, así que el crossfade es un fundido:
/// la canción que termina baja en los últimos N segundos y la siguiente sube en sus primeros N
/// (curva de igual potencia). Solo en transiciones automáticas; al saltar o buscar no se aplica.
/// Lo llama el hilo del reproductor en cada paquete, justo antes de escribirlo en la salida.
struct FadeVolume {
    inner: Box<dyn VolumeGetter + Send>,
    state: Arc<Mutex<LocalState>>,
    crossfade_ms: Arc<AtomicU32>,
    last: AtomicU64,
}

impl FadeVolume {
    fn envelope(st: &LocalState, cf: u64) -> f64 {
        let pos = st.position_now();
        let dur = st.duration_ms();
        let mut g: f64 = 1.0;
        // Pistas muy cortas: el fundido no puede ocupar más de un tercio.
        let cf = if dur > 0 { cf.min(dur / 3) } else { cf };
        if cf == 0 { return 1.0; }
        if dur > 0 && pos + cf > dur {
            let remaining = dur as i64 - pos as i64;
            // Estado viejo (la siguiente aún no se anunció): no se silencia más de 1,5 s.
            if remaining > -1500 { g = g.min((remaining.max(0) as f64) / cf as f64); }
        }
        if st.fade_in && pos < cf { g = g.min(pos as f64 / cf as f64); }
        (g.clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2).sin()
    }
}

impl VolumeGetter for FadeVolume {
    fn attenuation_factor(&self) -> f64 {
        let base = self.inner.attenuation_factor();
        let cf = self.crossfade_ms.load(Ordering::Relaxed) as u64;
        if cf == 0 { return base; }
        // Nunca bloquea el hilo de audio: si el estado está ocupado, repite el último factor.
        let g = match self.state.try_lock() {
            Ok(st) => { let g = Self::envelope(&st, cf); self.last.store(g.to_bits(), Ordering::Relaxed); g }
            Err(_) => f64::from_bits(self.last.load(Ordering::Relaxed)),
        };
        base * g
    }
}

impl LocalState {
    fn json(&self, device_id: &str) -> Value {
        let elapsed = if self.playing { self.updated.map_or(0, |t| t.elapsed().as_millis() as u32) } else { 0 };
        json!({
            "event": "state",
            "device_id": device_id,
            "playing": self.playing,
            "position_ms": self.position_ms.saturating_add(elapsed),
            "track": self.track,
            "shuffle": self.shuffle,
            "repeat": if self.repeat.is_empty() { "off" } else { self.repeat },
            "volume": (self.volume as u32 * 100 / u16::MAX as u32),
            "stopped": self.stopped,
            "loading": self.loading,
        })
    }
}

struct Engine {
    session: Session,
    spirc: Arc<Spirc>,
    state: Arc<Mutex<LocalState>>,
    /// Spotify Connect apagado a propósito (la app reproduce con el Web Playback SDK).
    /// La sesión sigue viva para las lecturas de catálogo.
    connect_off: AtomicBool,
}

const EQ_BANDS: usize = 9;
const EQ_FREQS: [f64; EQ_BANDS] = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];

fn peaking_coeffs(freq: f64, gain_db: f64) -> biquad::Coefficients<f64> {
    use biquad::{Coefficients, ToHertz, Type};
    Coefficients::<f64>::from_params(Type::PeakingEQ(gain_db), 44100.hz(), freq.hz(), 1.414)
        .unwrap_or_else(|_| Coefficients::<f64>::from_params(Type::PeakingEQ(0.0), 44100.hz(), freq.hz(), 1.414).unwrap())
}

struct EqSink {
    inner: Box<dyn Sink>,
    enabled: Arc<AtomicBool>,
    bands: Arc<[AtomicI32; EQ_BANDS]>,
    last_db: [i32; EQ_BANDS],
    filters: [[biquad::DirectForm2Transposed<f64>; 2]; EQ_BANDS],
}

impl EqSink {
    fn new(inner: Box<dyn Sink>, enabled: Arc<AtomicBool>, bands: Arc<[AtomicI32; EQ_BANDS]>) -> Self {
        let last_db = std::array::from_fn(|i| bands[i].load(Ordering::Relaxed));
        let filters = std::array::from_fn(|i| {
            let coeffs = peaking_coeffs(EQ_FREQS[i], last_db[i] as f64 / 100.0);
            [
                biquad::DirectForm2Transposed::<f64>::new(coeffs),
                biquad::DirectForm2Transposed::<f64>::new(coeffs),
            ]
        });
        Self { inner, enabled, bands, last_db, filters }
    }
}

impl Sink for EqSink {
    fn start(&mut self) -> SinkResult<()> {
        use biquad::Biquad as _;
        for ch_pair in &mut self.filters {
            ch_pair[0].reset_state();
            ch_pair[1].reset_state();
        }
        self.inner.start()
    }
    fn stop(&mut self) -> SinkResult<()> { self.inner.stop() }
    fn write(&mut self, mut packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        use biquad::Biquad as _;
        if !self.enabled.load(Ordering::Relaxed) {
            return self.inner.write(packet, converter);
        }
        let mut active = false;
        for i in 0..EQ_BANDS {
            let cur = self.bands[i].load(Ordering::Relaxed);
            if cur != self.last_db[i] {
                self.last_db[i] = cur;
                let coeffs = peaking_coeffs(EQ_FREQS[i], cur as f64 / 100.0);
                self.filters[i][0].update_coefficients(coeffs);
                self.filters[i][1].update_coefficients(coeffs);
            }
            if cur.abs() >= 5 { active = true; }
        }
        if active {
            if let AudioPacket::Samples(ref mut samples) = packet {
                for (idx, s) in samples.iter_mut().enumerate() {
                    let ch = idx & 1;
                    let mut v = *s;
                    for (i, ch_pair) in self.filters.iter_mut().enumerate() {
                        if self.last_db[i].abs() >= 5 { v = ch_pair[ch].run(v); }
                    }
                    *s = v.clamp(-1.0, 1.0);
                }
            }
        }
        self.inner.write(packet, converter)
    }
}

struct App {
    out: Out,
    engine: tokio::sync::Mutex<Option<Arc<Engine>>>,
    device_id: String,
    prefs: Mutex<Prefs>,
    crossfade_ms: Arc<AtomicU32>,
    eq_enabled: Arc<AtomicBool>,
    eq_bands: Arc<[AtomicI32; EQ_BANDS]>,
}

fn device_id() -> String {
    let mut h = Sha1::new();
    // `USER` en macOS/Linux, `USERNAME` en Windows.
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
    h.update(format!("{DEVICE_NAME}-{user}"));
    hex::encode(h.finalize())
}

#[tokio::main]
async fn main() -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let _ = EVENTS.set(tx.clone());
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .format(|buf, record| {
            use std::io::Write;
            if record.level() == log::Level::Error && record.target().starts_with("librespot_core::audio_key") {
                if let Some(out) = EVENTS.get() { let _ = out.send(json!({"event": "audio_key_error"})); }
            }
            // Sin clave, librespot sigue «sin descifrar» y solo saca ruido: para la app es una canción sin audio.
            if record.target().starts_with("librespot_playback::player") && format!("{}", record.args()).contains("without decryption") {
                if let Some(out) = EVENTS.get() { let _ = out.send(json!({"event": "audio_key_unavailable"})); }
            }
            writeln!(buf, "[{} {} {}] {}", buf.timestamp(), record.level(), record.target(), record.args())
        })
        .init();
    tokio::spawn(async move {
        use std::io::Write;
        let stdout = std::io::stdout();
        while let Some(v) = rx.recv().await {
            let mut lock = stdout.lock();
            let _ = writeln!(lock, "{v}");
            let _ = lock.flush();
        }
    });
    let app = Arc::new(App { out: tx.clone(), engine: tokio::sync::Mutex::new(None), device_id: device_id(),
                            prefs: Mutex::new(Prefs::default()), crossfade_ms: Arc::new(AtomicU32::new(0)),
                            eq_enabled: Arc::new(AtomicBool::new(true)),
                            eq_bands: Arc::new(std::array::from_fn(|_| AtomicI32::new(0))) });
    let _ = tx.send(json!({"event": "status", "state": "needs_credentials", "device_id": app.device_id}));

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let app = app.clone();
        tokio::spawn(async move {
            let id = msg.get("id").cloned().unwrap_or(Value::Null);
            let reply = match handle(&app, &msg).await {
                Ok(result) => json!({"id": id, "ok": true, "result": result}),
                Err(e) => {
                    let cmd = msg.get("cmd").and_then(Value::as_str).unwrap_or("?");
                    eprintln!("orden {cmd} falló: {e:#}");
                    json!({"id": id, "ok": false, "error": format!("{e:#}")})
                }
            };
            let _ = app.out.send(reply);
        });
    }
    // stdin cerrado: la app terminó.
    if let Some(engine) = app.engine.lock().await.take() {
        let _ = engine.spirc.shutdown();
    }
    Ok(())
}

fn s<'a>(msg: &'a Value, key: &str) -> Result<&'a str> {
    msg.get(key).and_then(Value::as_str).ok_or_else(|| anyhow!("falta «{key}»"))
}

// Construye una orden Connect con contexto y licencia de reproducción bajo demanda.
fn remote_play_body(msg: &Value) -> Result<Value> {
    let context = if let Some(uri) = msg.get("uri").and_then(Value::as_str) {
        if uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:") {
            json!({"uri": "", "url": "", "metadata": {}, "pages": [{"tracks": [{"uri": uri}]}]})
        } else {
            json!({"uri": uri, "url": format!("context://{uri}"), "metadata": {}})
        }
    } else {
        let tracks: Vec<Value> = msg.get("uris").and_then(Value::as_array)
            .ok_or_else(|| anyhow!("faltan uris"))?.iter()
            .filter_map(Value::as_str).map(|uri| json!({"uri": uri})).collect();
        if tracks.is_empty() { return Err(anyhow!("no hay canciones para reproducir")); }
        json!({"uri": "", "url": "", "metadata": {}, "pages": [{"tracks": tracks}]})
    };
    let mut options = json!({"license": "on-demand", "skip_to": {}, "player_options_override": {}});
    if let Some(shuffle) = msg.get("shuffle").and_then(Value::as_bool) {
        options["player_options_override"]["shuffling_context"] = json!(shuffle);
    }
    if let Some(mode) = msg.get("repeat").and_then(Value::as_str) {
        options["player_options_override"]["repeating_context"] = json!(mode == "context");
        options["player_options_override"]["repeating_track"] = json!(mode == "track");
    }
    if let Some(uri) = msg.get("start_uri").and_then(Value::as_str) {
        options["skip_to"] = json!({"track_uri": uri});
    }
    if let Some(index) = msg.get("start_index").and_then(Value::as_u64) {
        options["skip_to"]["track_index"] = json!(index);
    }
    Ok(json!({"command": {"endpoint": "play", "context": context,
        "play_origin": {"feature_identifier": "spotifymac"}, "options": options}}))
}

#[cfg(test)]
mod remote_play_tests {
    use super::*;

    #[test]
    fn playlist_command_keeps_context_and_start_track() {
        let body = remote_play_body(&json!({"uri":"spotify:playlist:abc", "start_uri":"spotify:track:one"})).unwrap();
        assert_eq!(body["command"]["context"]["url"], "context://spotify:playlist:abc");
        assert_eq!(body["command"]["options"]["license"], "on-demand");
        assert_eq!(body["command"]["options"]["skip_to"]["track_uri"], "spotify:track:one");
        let last=remote_play_body(&json!({"uri":"spotify:playlist:abc","start_uri":"spotify:track:last","start_index":999})).unwrap();
        assert_eq!(last["command"]["options"]["skip_to"]["track_index"],999);
    }

    #[test]
    fn track_list_is_a_context_page() {
        let body = remote_play_body(&json!({"uris":["spotify:track:a", "spotify:track:b"]})).unwrap();
        assert_eq!(body["command"]["context"]["pages"][0]["tracks"][1]["uri"], "spotify:track:b");
        assert!(remote_play_body(&json!({"uris":[]})).is_err());
    }
}

// ---- Canvas (protobuf a mano: la versión de librespot no trae los mensajes de petición) ----
// EntityCanvazRequest { repeated Entity entities = 1 } · Entity { string entity_uri = 1 }
// EntityCanvazResponse { repeated Canvaz canvases = 1 } · Canvaz { string url = 2; Type type = 4 }

fn pb_len(out: &mut Vec<u8>, mut n: usize) {
    loop {
        let b = (n & 0x7f) as u8; n >>= 7;
        if n == 0 { out.push(b); break } else { out.push(b | 0x80) }
    }
}

fn canvas_request(uri: &str) -> Vec<u8> {
    let mut entity = vec![0x0a];
    pb_len(&mut entity, uri.len());
    entity.extend_from_slice(uri.as_bytes());
    let mut out = vec![0x0a];
    pb_len(&mut out, entity.len());
    out.extend(entity);
    out
}

fn pb_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let byte = *b.get(*i)?; *i += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 { return Some(v); }
        shift += 7;
        if shift > 63 { return None; }
    }
}

/// Recorre un mensaje y llama a `f(campo, tipo, valor varint, bytes)` por cada campo.
fn pb_fields<'a>(b: &'a [u8], mut f: impl FnMut(u64, u64, u64, &'a [u8])) -> Option<()> {
    let mut i = 0;
    while i < b.len() {
        let key = pb_varint(b, &mut i)?;
        let (field, wire) = (key >> 3, key & 7);
        match wire {
            0 => { let v = pb_varint(b, &mut i)?; f(field, wire, v, &[]); }
            1 => { i += 8; }
            2 => { let n = pb_varint(b, &mut i)? as usize; let d = b.get(i..i + n)?; i += n; f(field, wire, 0, d); }
            5 => { i += 4; }
            _ => return None,
        }
    }
    Some(())
}

/// Primer canvas con video: (url, tipo). Las imágenes fijas (tipo 0) no cuentan.
fn parse_canvas(b: &[u8]) -> Option<(String, u64)> {
    let mut found = None;
    pb_fields(b, |field, wire, _, data| {
        if found.is_some() || field != 1 || wire != 2 { return; }
        let (mut url, mut kind) = (String::new(), 0u64);
        pb_fields(data, |f, w, v, d| {
            if f == 2 && w == 2 { url = String::from_utf8_lossy(d).into_owned(); }
            if f == 4 && w == 0 { kind = v; }
        });
        if !url.is_empty() && kind != 0 && !url.ends_with(".jpg") { found = Some((url, kind)); }
    });
    found
}

#[cfg(test)]
mod canvas_tests {
    use super::*;
    #[test]
    fn builds_request_and_reads_response() {
        let req = canvas_request("spotify:track:abc");
        assert_eq!(req[0], 0x0a);
        assert_eq!(&req[4..], b"spotify:track:abc");
        // Canvaz { id:"x"(1), url:"https://c/v.mp4"(2), type:2 (4) } dentro de canvases (1)
        let url = b"https://c/v.mp4";
        let mut inner = vec![0x0a, 1, b'x', 0x12, url.len() as u8];
        inner.extend_from_slice(url);
        inner.extend_from_slice(&[0x20, 2]);
        let mut resp = vec![0x0a, inner.len() as u8];
        resp.extend(inner);
        resp.extend_from_slice(&[0x10, 0x3c]); // ttl
        assert_eq!(parse_canvas(&resp), Some(("https://c/v.mp4".into(), 2)));
        assert_eq!(parse_canvas(&[]), None);
    }

    #[test]
    fn reorder_queue_moves_removes_and_keeps_hidden() {
        let t = |uri: &str, uid: &str, provider: &str| {
            let mut p = super::ProvidedTrack::new(); p.uri = uri.into(); p.uid = uid.into(); p.provider = provider.into(); p
        };
        let next = vec![t("spotify:track:a", "q0", "queue"), t("spotify:track:b", "c1", "context"),
                        t("spotify:track:c", "c2", "context"), t("spotify:delimiter", "", "context")];
        let out = super::reorder_next_tracks(&next, &["c2".into(), "q0".into()]);
        let uris: Vec<&str> = out.iter().map(|p| p.uri.as_str()).collect();
        assert_eq!(uris, ["spotify:track:c", "spotify:track:a", "spotify:delimiter"]);
        assert_eq!(out[1].provider, "context");   // la de la cola, detrás del contexto, deja de ser «cola»
    }
}

type ProvidedTrack = librespot_protocol::player::ProvidedTrack;

fn uris_arg(msg: &Value) -> Result<Vec<String>> {
    let list: Vec<String> = msg.get("uris").and_then(Value::as_array).ok_or_else(|| anyhow!("faltan uris"))?
        .iter().filter_map(Value::as_str).map(String::from).collect();
    if list.is_empty() { return Err(anyhow!("faltan uris")); }
    Ok(list)
}

/// Pistas de la cola que se muestran: canciones y episodios reales (sin separadores ni no disponibles).
fn is_visible_next(t: &ProvidedTrack) -> bool {
    (t.uri.starts_with("spotify:track:") || t.uri.starts_with("spotify:episode:")) && t.provider != "unavailable"
}

fn visible_next_tracks(ps: &librespot_protocol::player::PlayerState) -> Vec<ProvidedTrack> {
    ps.next_tracks.iter().filter(|t| is_visible_next(t)).take(80).cloned().collect()
}

/// La cola con las pistas visibles en el orden pedido (las no nombradas se quitan) y,
/// detrás, lo que no se muestra (separadores, pistas más allá de las visibles) tal cual.
fn reorder_next_tracks(next: &[ProvidedTrack], order: &[String]) -> Vec<ProvidedTrack> {
    let shown: Vec<usize> = (0..next.len()).filter(|&i| is_visible_next(&next[i])).take(80).collect();
    let mut result: Vec<ProvidedTrack> = order.iter()
        .filter_map(|uid| shown.iter().find(|&&i| !uid.is_empty() && &next[i].uid == uid).map(|&i| next[i].clone()))
        .collect();
    // Lo añadido a la cola va primero, como en Spotify: una pista de la cola movida detrás de una
    // del contexto pasa a contar como del contexto para que Spotify no la vuelva a subir.
    if let Some(first_ctx) = result.iter().position(|t| t.provider != "queue") {
        for t in result.iter_mut().skip(first_ctx) {
            if t.provider == "queue" { t.provider = "context".into(); }
        }
    }
    result.extend((0..next.len()).filter(|i| !shown.contains(i)).map(|i| next[i].clone()));
    result
}

fn provided_json(t: &ProvidedTrack) -> Value {
    json!({"uri": t.uri, "uid": t.uid, "provider": t.provider, "metadata": t.metadata})
}

/// Origen de una transferencia: el dispositivo que suena ahora o, si no suena ninguno, este cliente.
fn transfer_source(active: Option<&str>, me: &str) -> String {
    active.filter(|a| !a.is_empty()).unwrap_or(me).to_owned()
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

static REMOTE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Estado de otro dispositivo (Alexa, teléfono…) solo con lo que trae el paquete push de Connect.
/// `measured_at` (ms Unix) permite a la app interpolar la posición aunque el evento llegue tarde.
fn remote_state_json(ps: &librespot_protocol::player::PlayerState, active: &str, name: &str, volume: Option<u32>) -> Value {
    let playing = ps.is_playing && !ps.is_paused;
    let now = unix_ms();
    let mut pos = ps.position_as_of_timestamp;
    if playing && ps.timestamp > 0 { pos += (now - ps.timestamp).max(0); }
    let t = ps.track.as_ref();
    let md = t.map(|t| t.metadata.clone()).unwrap_or_default();
    let uri = t.map(|t| t.uri.clone()).unwrap_or_default();
    let image = ["image_xlarge_url", "image_large_url", "image_url"].iter()
        .find_map(|k| md.get(*k)).map(|u| u.replace("spotify:image:", "https://i.scdn.co/image/"));
    let opts = ps.options.as_ref();
    json!({
        "event": "remote_state", "seq": REMOTE_SEQ.fetch_add(1, Ordering::Relaxed) + 1,
        "device_id": active, "device_name": name, "volume_percent": volume,
        "playing": playing, "position_ms": pos.max(0), "measured_at": now,
        "shuffle": opts.map(|o| o.shuffling_context).unwrap_or(false),
        "repeat": opts.map(|o| if o.repeating_track { "track" } else if o.repeating_context { "context" } else { "off" }).unwrap_or("off"),
        "track": if uri.is_empty() { Value::Null } else { json!({
            "id": uri.rsplit(':').next(), "uri": uri,
            "name": md.get("title").cloned().unwrap_or_default(),
            "artists": md.get("artist_name").map(|a| vec![json!({"name": a})]).unwrap_or_default(),
            "album": {"name": md.get("album_title").cloned().unwrap_or_default(),
                      "images": image.map(|u| vec![json!({"url": u})]).unwrap_or_default()},
            "duration_ms": ps.duration,
        })},
    })
}

/// Sustituye la canción del push por la del catálogo, sin perder una duración conocida.
fn remote_merge_track(event: &mut Value, mut track: Value) {
    let pushed = event.pointer("/track/duration_ms").and_then(Value::as_i64).unwrap_or(0);
    if track.get("duration_ms").and_then(Value::as_i64).unwrap_or(0) <= 0 && pushed > 0 {
        track["duration_ms"] = json!(pushed);
    }
    event["track"] = track;
}

#[cfg(test)]
mod remote_tests {
    use super::*;

    #[test]
    fn transfer_uses_active_device_as_source() {
        assert_eq!(transfer_source(Some("alexa"), "me"), "alexa");
        assert_eq!(transfer_source(None, "me"), "me");
        assert_eq!(transfer_source(Some(""), "me"), "me");
    }

    #[test]
    fn merge_keeps_pushed_duration_when_catalog_lacks_it() {
        let mut ev = json!({"track": {"uri": "spotify:track:1", "duration_ms": 1000}});
        remote_merge_track(&mut ev, json!({"uri": "spotify:track:1", "name": "N"}));
        assert_eq!(ev["track"]["duration_ms"], 1000);
        assert_eq!(ev["track"]["name"], "N");
    }
}

async fn engine(app: &App) -> Result<Arc<Engine>> {
    app.engine.lock().await.clone().ok_or_else(|| anyhow!("reproductor no conectado"))
}

async fn handle(app: &Arc<App>, msg: &Value) -> Result<Value> {
    let cmd = s(msg, "cmd")?;
    match cmd {
        "login" => {
            // Un único inicio de sesión en el navegador. La app pide aquí todos sus permisos ("scopes") y recibe
            // el token para la Web API además de la credencial del motor: así solo se abre una página.
            let client_id = SessionConfig::default().client_id;
            let scopes: Vec<String> = msg.get("scopes").and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .filter(|v: &Vec<String>| !v.is_empty())
                .unwrap_or_else(|| vec!["streaming".to_string()]);
            let page = msg.get("page").and_then(Value::as_str).unwrap_or("<h2>Listo. Ya puedes volver a SpotifyMac.</h2>").to_string();
            let token = tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
                librespot_oauth::OAuthClientBuilder::new(&client_id, REDIRECT_URI, refs)
                    .open_in_browser()
                    .with_custom_message(&page)
                    .build()?
                    .get_access_token()
            })
            .await??;
            // El usuario ya aceptó en el navegador: la app muestra «Procesando…» mientras se conecta.
            let _ = app.out.send(json!({"event": "login_authorized"}));
            let expires_in = token.expires_at.saturating_duration_since(std::time::Instant::now()).as_secs();
            let access = token.access_token.clone();
            let mut result = connect(app, Credentials::with_access_token(token.access_token)).await?;
            result["token"] = json!({
                "access_token": access, "refresh_token": token.refresh_token,
                "expires_in": expires_in, "scopes": token.scopes, "client_id": SessionConfig::default().client_id,
            });
            Ok(result)
        }
        "connect" => {
            let creds: Credentials = serde_json::from_value(msg.get("credentials").cloned().ok_or_else(|| anyhow!("faltan credenciales"))?)?;
            connect(app, creds).await
        }
        "logout" => {
            if let Some(e) = app.engine.lock().await.take() {
                let _ = e.spirc.shutdown();
            }
            let _ = app.out.send(json!({"event": "status", "state": "needs_credentials", "device_id": app.device_id}));
            Ok(Value::Null)
        }
        "state" => Ok(engine(app).await?.state.lock().unwrap().json(&app.device_id)),
        // Pasa la reproducción de este dispositivo a otro por Spotify Connect (connect-state), sin Web API.
        // La app lo usa para entregar la música al reproductor web cuando Spotify niega el audio a librespot.
        "transfer_to" => {
            let e = engine(app).await?;
            let to = s(msg, "to")?;
            e.session.spclient().transfer(&app.device_id, to, None).await?;
            Ok(Value::Null)
        }
        // Dispositivos Spotify Connect de la cuenta (el mismo listado que ve el teléfono), sin Web API.
        "devices" => {
            let e = engine(app).await?;
            let st = e.spirc.watch_cluster_state().borrow().clone();
            let list: Vec<Value> = st.devices.values().map(|d| json!({
                "id": d.device_id, "name": d.device_alias,
                "type": format!("{:?}", d.device_type).to_lowercase(),
                "is_active": d.is_active || st.active_device_id.as_deref() == Some(d.device_id.as_str()),
                "volume_percent": (d.volume.min(65535) * 100 / 65535),
            })).collect();
            Ok(json!({"devices": list, "self_id": app.device_id}))
        }
        // Carga música directamente en el dispositivo activo sin transferirla al Mac.
        "remote_load" => {
            let e = engine(app).await?;
            let to = s(msg, "to")?;
            let body = remote_play_body(msg)?.to_string();
            let mut headers = http::HeaderMap::new();
            headers.insert("content-type", "application/json".parse().unwrap());
            e.session.spclient().request(&http::Method::POST,
                &format!("/connect-state/v1/player/command/from/{}/to/{to}", app.device_id),
                Some(headers), Some(body.as_bytes())).await?;
            Ok(Value::Null)
        }
        // Controla otro dispositivo (Alexa, teléfono…) por connect-state, sin Web API.
        // endpoint: pause, resume, skip_next, skip_prev, seek_to (value = ms),
        // set_shuffling_context / set_repeating_context / set_repeating_track (value = bool).
        "remote" => {
            let e = engine(app).await?;
            let to = s(msg, "to")?;
            let mut command = json!({"endpoint": s(msg, "endpoint")?});
            if let Some(v) = msg.get("value") { command["value"] = v.clone(); }
            let body = json!({"command": command}).to_string();
            let mut headers = http::HeaderMap::new();
            headers.insert("content-type", "application/json".parse().unwrap());
            e.session.spclient().request(&http::Method::POST,
                &format!("/connect-state/v1/player/command/from/{}/to/{to}", app.device_id),
                Some(headers), Some(body.as_bytes())).await?;
            Ok(Value::Null)
        }
        // Reordena o quita canciones de la cola del dispositivo que suene (este Mac, el reproductor web u otro).
        // `uids`: las pistas visibles de la cola en el nuevo orden; las que falten se quitan.
        // Va por connect-state con `set_queue`, como cuando se arrastra la cola en la app oficial.
        "queue_set" => {
            let e = engine(app).await?;
            let to = msg.get("to").and_then(Value::as_str).unwrap_or(&app.device_id).to_string();
            let order: Vec<String> = msg.get("uids").and_then(Value::as_array).ok_or_else(|| anyhow!("faltan uids"))?
                .iter().filter_map(Value::as_str).map(String::from).collect();
            let ps = e.spirc.watch_player_state().borrow().clone().ok_or_else(|| anyhow!("no hay cola"))?;
            let body = json!({"command": {
                "endpoint": "set_queue",
                "next_tracks": reorder_next_tracks(&ps.next_tracks, &order).iter().map(provided_json).collect::<Vec<_>>(),
                "prev_tracks": ps.prev_tracks.iter().map(provided_json).collect::<Vec<_>>(),
                "queue_revision": ps.queue_revision,
            }}).to_string();
            let mut headers = http::HeaderMap::new();
            headers.insert("content-type", "application/json".parse().unwrap());
            e.session.spclient().request(&http::Method::POST,
                &format!("/connect-state/v1/player/command/from/{}/to/{to}", app.device_id),
                Some(headers), Some(body.as_bytes())).await?;
            Ok(Value::Null)
        }
        // Pulsar una canción de la cola: salta directamente a ella (en este equipo o en el dispositivo activo).
        "queue_jump" => {
            let e = engine(app).await?;
            let to = msg.get("to").and_then(Value::as_str).unwrap_or(&app.device_id).to_string();
            let body = json!({"command": {"endpoint": "skip_next",
                "track": {"uri": s(msg, "uri")?, "uid": msg.get("uid").and_then(Value::as_str).unwrap_or("")}}}).to_string();
            let mut headers = http::HeaderMap::new();
            headers.insert("content-type", "application/json".parse().unwrap());
            e.session.spclient().request(&http::Method::POST,
                &format!("/connect-state/v1/player/command/from/{}/to/{to}", app.device_id),
                Some(headers), Some(body.as_bytes())).await?;
            Ok(Value::Null)
        }
        "remote_volume" => {
            let e = engine(app).await?;
            let to = s(msg, "to")?;
            let pct = msg.get("percent").and_then(Value::as_u64).unwrap_or(50).min(100);
            let body = json!({"volume": pct * 65535 / 100}).to_string();
            let mut headers = http::HeaderMap::new();
            headers.insert("content-type", "application/json".parse().unwrap());
            e.session.spclient().request(&http::Method::PUT,
                &format!("/connect-state/v1/connect/volume/from/{}/to/{to}", app.device_id),
                Some(headers), Some(body.as_bytes())).await?;
            Ok(Value::Null)
        }
        // Pasa la música (desde el dispositivo que suene ahora) a `to`, por connect-state.
        // El origen es el dispositivo que suena según el clúster de Connect; si no suena ninguno, este cliente.
        // (Antes se pedía «de `to` a `to`» y Spotify no movía nada.)
        "transfer_any" => {
            let e = engine(app).await?;
            let to = s(msg, "to")?;
            let active = e.spirc.watch_cluster_state().borrow().active_device_id.clone();
            let from = transfer_source(active.as_deref(), &app.device_id);
            if from == to { return Ok(Value::Null); }
            e.session.spclient().transfer(&from, to, None).await?;
            Ok(Value::Null)
        }
        "liked_items" => {
            let e = engine(app).await?;
            let offset = msg.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            let limit = msg.get("limit").and_then(Value::as_u64).unwrap_or(100).clamp(1, 200) as usize;
            reads::liked_items(&e.session, offset, limit).await
        }
        // Letras como en la app oficial (servicio color-lyrics de Spotify; proveedor habitual: Musixmatch).
        "lyrics" => {
            let e = engine(app).await?;
            let id = librespot_core::spotify_id::SpotifyId::from_base62(s(msg, "target")?)?;
            let bytes = e.session.spclient().get_lyrics(&id).await?;
            Ok(serde_json::from_slice::<Value>(&bytes)?)
        }
        // Token de Web API sacado de la sesión ya iniciada (login5): renueva el acceso sin abrir el navegador.
        "web_token" => {
            let e = engine(app).await?;
            let t = e.session.login5().auth_token().await?;
            Ok(json!({"access_token": t.access_token, "expires_in": t.expires_in.as_secs()}))
        }
        // ---- Reproducción (Spotify Connect, sin Web API) ----
        "play_context" | "play_tracks" => {
            let e = engine(app).await?;
            let shuffle = msg.get("shuffle").and_then(Value::as_bool).unwrap_or(false);
            let repeat = msg.get("repeat").and_then(Value::as_str).unwrap_or("off");
            let options = LoadRequestOptions {
                start_playing: true,
                seek_to: 0,
                context_options: Some(LoadContextOptions::Options(Options { shuffle, repeat: repeat == "context", repeat_track: repeat == "track" })),
                playing_track: msg.get("start_uri").and_then(Value::as_str).map(|u| PlayingTrack::Uri(u.to_string())),
            };
            let request = if cmd == "play_context" {
                LoadRequest::from_context_uri(s(msg, "uri")?.to_string(), options)
            } else {
                let uris = msg.get("uris").and_then(Value::as_array).ok_or_else(|| anyhow!("faltan uris"))?
                    .iter().filter_map(Value::as_str).map(String::from).collect();
                LoadRequest::from_tracks(uris, options)
            };
            e.spirc.activate()?;
            e.spirc.load(request)?;
            Ok(Value::Null)
        }
        "activate" => { engine(app).await?.spirc.activate()?; Ok(Value::Null) }
        // Retira este Mac de Spotify Connect (deja de aparecer como «SpotifyMac» del motor) y conserva
        // la sesión para las lecturas. La app lo usa cuando Spotify rechaza las claves de audio.
        "connect_off" => {
            let e = engine(app).await?;
            if !e.connect_off.swap(true, Ordering::SeqCst) { let _ = e.spirc.shutdown(); }
            Ok(Value::Null)
        }
        // Añade una canción a la cola. Sin `to` (o con este Mac): por el motor. Con otro dispositivo
        // (reproductor web, teléfono…): orden `add_to_queue` por connect-state, como la app oficial.
        "queue_add" => {
            let e = engine(app).await?;
            let uri = s(msg, "uri")?;
            match msg.get("to").and_then(Value::as_str).filter(|to| *to != app.device_id) {
                None => { e.spirc.add_to_queue(librespot_core::SpotifyUri::from_uri(uri)?)?; }
                Some(to) => {
                    let body = json!({"command": {"endpoint": "add_to_queue",
                        "track": {"uri": uri, "metadata": {"is_queued": "true"}, "provider": "queue"}}}).to_string();
                    let mut headers = http::HeaderMap::new();
                    headers.insert("content-type", "application/json".parse().unwrap());
                    e.session.spclient().request(&http::Method::POST,
                        &format!("/connect-state/v1/player/command/from/{}/to/{to}", app.device_id),
                        Some(headers), Some(body.as_bytes())).await?;
                }
            }
            Ok(Value::Null)
        }
        // «Tus me gusta» y playlists por la sesión: sin cuota de Web API.
        "liked_contains" => reads::liked_contains(&engine(app).await?.session, &uris_arg(msg)?).await,
        "set_liked" => reads::set_liked(&engine(app).await?.session, &uris_arg(msg)?,
            msg.get("saved").and_then(Value::as_bool).unwrap_or(true)).await,
        "playlist_add" => reads::playlist_add(&engine(app).await?.session, s(msg, "target")?, &uris_arg(msg)?).await,
        "pins" => reads::pins(&engine(app).await?.session).await,
        "warmup" => reads::warmup(&engine(app).await?.session).await,
        "playlist_remove" => reads::playlist_remove(&engine(app).await?.session, s(msg, "target")?).await,
        "recommend" => {
            let seeds: Vec<String> = msg.get("seeds").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
            let exclude: Vec<String> = msg.get("exclude").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
            reads::recommend(&engine(app).await?.session, &seeds, &exclude,
                msg.get("limit").and_then(Value::as_u64).unwrap_or(30) as usize).await
        }
        "playlist_set_cover" => reads::playlist_set_cover(&engine(app).await?.session, s(msg, "target")?, s(msg, "path")?).await,
        "playlist_edit" => reads::playlist_edit(&engine(app).await?.session, s(msg, "target")?,
            msg.get("name").and_then(Value::as_str), msg.get("description").and_then(Value::as_str)).await,
        "set_pinned" => reads::set_pinned(&engine(app).await?.session, &uris_arg(msg)?,
            msg.get("pinned").and_then(Value::as_bool).unwrap_or(true)).await,
        "playlist_create" => reads::playlist_create(&engine(app).await?.session, s(msg, "name")?,
            msg.get("description").and_then(Value::as_str).unwrap_or("")).await,
        "play" => { engine(app).await?.spirc.play()?; Ok(Value::Null) }
        "pause" => { engine(app).await?.spirc.pause()?; Ok(Value::Null) }
        "toggle" => { engine(app).await?.spirc.play_pause()?; Ok(Value::Null) }
        "next" => { engine(app).await?.spirc.next()?; Ok(Value::Null) }
        "prev" => { engine(app).await?.spirc.prev()?; Ok(Value::Null) }
        "seek" => {
            let ms = msg.get("ms").and_then(Value::as_u64).unwrap_or(0) as u32;
            engine(app).await?.spirc.set_position_ms(ms)?;
            Ok(Value::Null)
        }
        "volume" => {
            let pct = msg.get("percent").and_then(Value::as_u64).unwrap_or(50).min(100) as u32;
            engine(app).await?.spirc.set_volume((pct * u16::MAX as u32 / 100) as u16)?;
            Ok(Value::Null)
        }
        "shuffle" => { engine(app).await?.spirc.shuffle(msg.get("on").and_then(Value::as_bool).unwrap_or(false))?; Ok(Value::Null) }
        "repeat" => {
            let e = engine(app).await?;
            match s(msg, "mode")? {
                "track" => { e.spirc.repeat(true)?; e.spirc.repeat_track(true)?; }
                "context" => { e.spirc.repeat_track(false)?; e.spirc.repeat(true)?; }
                _ => { e.spirc.repeat_track(false)?; e.spirc.repeat(false)?; }
            }
            Ok(Value::Null)
        }
        // ---- Lecturas por la sesión (sin cuota de Web API) ----
        "rootlist" => reads::library(&engine(app).await?.session).await,
        "library_collection" => reads::library_collection(&engine(app).await?.session).await,
        "show_episodes" => {
            let e = engine(app).await?;
            let offset = msg.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            let limit = msg.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 100) as usize;
            reads::show_episodes(&e.session, s(msg, "target")?, offset, limit).await
        }
        "playlist_items" => {
            let e = engine(app).await?;
            if msg.get("fresh").and_then(Value::as_bool)==Some(true) { reads::forget_playlist(s(msg,"target")?); }
            let offset = msg.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            let limit = msg.get("limit").and_then(Value::as_u64).unwrap_or(100).clamp(1, 200) as usize;
            reads::playlist_items(&e.session, s(msg, "target")?, offset, limit).await
        }
        "playlist_header" => reads::playlist_header(&engine(app).await?.session, s(msg, "target")?).await,
        "album_details" => reads::album_details(&engine(app).await?.session, s(msg, "target")?).await,
        "album_tracks" => reads::album_tracks(&engine(app).await?.session, s(msg, "target")?).await,
        "artist_top" => reads::artist_top(&engine(app).await?.session, s(msg, "target")?).await,
        "artist_albums" => reads::artist_albums(&engine(app).await?.session, s(msg, "target")?).await,
        "artist_related" => reads::artist_related(&engine(app).await?.session, s(msg, "target")?).await,
        "recently_played" => reads::recently_played(&engine(app).await?.session).await,
        "home_sections" => reads::home_sections(&engine(app).await?.session,
            msg.get("time_zone").and_then(Value::as_str).unwrap_or("UTC")).await,
        "artist_overview" => reads::artist_overview(&engine(app).await?.session, s(msg, "target")?).await,
        "search" => {
            let e = engine(app).await?;
            let limit = msg.get("limit").and_then(Value::as_u64).unwrap_or(10).clamp(1, 50) as u32;
            let offset = msg.get("offset").and_then(Value::as_u64).unwrap_or(0) as u32;
            let types = msg.get("types").and_then(Value::as_str).unwrap_or("track,artist,album,playlist");
            reads::search(&e.session, s(msg, "q")?, types, limit, offset).await
        }
        "artists" => {
            let ids: Vec<String> = msg.get("ids").and_then(Value::as_array).into_iter().flatten()
                .filter_map(Value::as_str).map(String::from).collect();
            reads::artists(&engine(app).await?.session, &ids).await
        }
        "track_credits" => {
            reads::track_credits(&engine(app).await?.session, s(msg, "id")?).await
        }
        // Canvas: el video corto en bucle que Spotify muestra detrás de la canción (MP4 sin cifrar).
        "canvas" => {
            let e = engine(app).await?;
            let uri = s(msg, "uri")?;
            let mut headers = http::HeaderMap::new();
            headers.insert("content-type", "application/x-protobuf".parse().unwrap());
            headers.insert("accept", "application/protobuf".parse().unwrap());
            let bytes = e.session.spclient().request(&http::Method::POST, "/canvaz-cache/v0/canvases",
                Some(headers), Some(&canvas_request(uri))).await?;
            Ok(parse_canvas(&bytes).map_or(Value::Null, |(url, kind)| json!({"url": url, "type": kind})))
        }
        // Preferencias de reproducción (Configuración de la app).
        "settings" => {
            if let Some(cf) = msg.get("crossfade_secs").and_then(Value::as_f64) {
                app.crossfade_ms.store((cf.clamp(0.0, 12.0) * 1000.0) as u32, Ordering::Relaxed);
            }
            if let Some(en) = msg.get("eq_enabled").and_then(Value::as_bool) {
                app.eq_enabled.store(en, Ordering::Relaxed);
            }
            if let Some(arr) = msg.get("eq_bands").and_then(Value::as_array) {
                for (i, val) in arr.iter().take(EQ_BANDS).enumerate() {
                    if let Some(db) = val.as_f64() {
                        app.eq_bands[i].store((db.clamp(-12.0, 12.0) * 100.0).round() as i32, Ordering::Relaxed);
                    }
                }
            }
            let mut p = app.prefs.lock().unwrap();
            if let Some(q) = msg.get("bitrate").and_then(Value::as_u64) {
                p.bitrate = match q { 96 => Bitrate::Bitrate96, 160 => Bitrate::Bitrate160, _ => Bitrate::Bitrate320 };
            }
            if let Some(v) = msg.get("gapless").and_then(Value::as_bool) { p.gapless = v; }
            if let Some(v) = msg.get("normalise").and_then(Value::as_bool) { p.normalise = v; }
            if let Some(v) = msg.get("pregain_db").and_then(Value::as_f64) { p.pregain_db = v.clamp(-10.0, 10.0); }
            if let Some(v) = msg.get("autoplay").and_then(Value::as_bool) { p.autoplay = v; }
            Ok(Value::Null)
        }
        // Perfil público (el propio si no se indica usuario): seguidores, seguidos, playlists públicas
        // y artistas escuchados recientemente, como en la app oficial.
        "user_profile" => {
            let e = engine(app).await?;
            let user = msg.get("user").and_then(Value::as_str).map(String::from).unwrap_or_else(|| e.session.username());
            let bytes = e.session.spclient().get_user_profile(&user, Some(50), Some(50)).await?;
            let mut v = serde_json::from_slice::<Value>(&bytes)?;
            if v.get("username").is_none() { v["username"] = json!(user); }
            Ok(v)
        }
        "user_followers" | "user_following" => {
            let e = engine(app).await?;
            let user = msg.get("user").and_then(Value::as_str).map(String::from).unwrap_or_else(|| e.session.username());
            let bytes = if cmd == "user_followers" { e.session.spclient().get_user_followers(&user).await? }
                        else { e.session.spclient().get_user_following(&user).await? };
            Ok(serde_json::from_slice::<Value>(&bytes)?)
        }
        "is_following" => {
            let e = engine(app).await?;
            let uri = s(msg, "uri")?;
            let me = e.session.username();
            let bytes = e.session.spclient().get_user_following(&me).await?;
            let v: Value = serde_json::from_slice(&bytes)?;
            let found = v.get("profiles").or_else(|| v.get("items")).and_then(Value::as_array)
                .into_iter().flatten()
                .any(|p| p.get("uri").and_then(Value::as_str) == Some(uri));
            Ok(json!({"following": found}))
        }
        "set_following" => {
            let e = engine(app).await?;
            let uri = s(msg, "uri")?;
            let follow = msg.get("follow").and_then(Value::as_bool).unwrap_or(true);
            let body = json!({"target_uris": [uri]}).to_string();
            let mut headers = http::HeaderMap::new();
            headers.insert("content-type", "application/json".parse().unwrap());
            let method = if follow { http::Method::POST } else { http::Method::DELETE };
            e.session.spclient().request(&method, "/socialgraph/v2/following?format=json", Some(headers), Some(body.as_bytes())).await?;
            Ok(json!({"following": follow}))
        }
        other => Err(anyhow!("orden desconocida: {other}")),
    }
}

/// Abre la sesión, registra el dispositivo Connect y devuelve la credencial reutilizable
/// para que la app la guarde en el llavero (el motor no la deja en disco).
async fn connect(app: &Arc<App>, credentials: Credentials) -> Result<Value> {
    if let Some(old) = app.engine.lock().await.take() {
        let _ = old.spirc.shutdown();
    }
    let _ = app.out.send(json!({"event": "status", "state": "connecting", "device_id": app.device_id}));
    // La credencial reutilizable se queda solo en memoria (nunca en disco): la app la guarda en el llavero.
    // Antes iba a una carpeta temporal que se borraba al conectar, y cuando librespot la renovaba después
    // (reconexiones) no podía guardarla: la app se quedaba con una caducada y había que volver a iniciar sesión.
    let cache = Cache::new(None::<std::path::PathBuf>, None, None, None)?.with_memory_credentials();
    let prefs = app.prefs.lock().unwrap().clone();
    let session_config = SessionConfig { device_id: app.device_id.clone(), autoplay: Some(prefs.autoplay), ..SessionConfig::default() };
    let session = Session::new(session_config, Some(cache));

    let mixer_fn = mixer::find(Some("softvol")).ok_or_else(|| anyhow!("falta el mezclador"))?;
    let mixer: Arc<dyn Mixer> = mixer_fn(MixerConfig { volume_ctrl: VolumeCtrl::Cubic(VolumeCtrl::DEFAULT_DB_RANGE), ..MixerConfig::default() })?;
    let backend = audio_backend::find(Some("rodio".into())).ok_or_else(|| anyhow!("falta la salida de audio"))?;
    let player_config = PlayerConfig {
        position_update_interval: Some(std::time::Duration::from_secs(1)),
        bitrate: prefs.bitrate,
        gapless: prefs.gapless,
        normalisation: prefs.normalise,
        normalisation_type: NormalisationType::Auto,
        normalisation_pregain_db: prefs.pregain_db,
        ..PlayerConfig::default()
    };
    let state = Arc::new(Mutex::new(LocalState { volume: u16::MAX / 2, repeat: "off", ..Default::default() }));
    let volume = FadeVolume { inner: mixer.get_soft_volume(), state: state.clone(), crossfade_ms: app.crossfade_ms.clone(), last: AtomicU64::new(1f64.to_bits()) };
    let eq_enabled = app.eq_enabled.clone();
    let eq_bands = app.eq_bands.clone();
    let player = Player::new(player_config, session.clone(), Box::new(volume), move || {
        Box::new(EqSink::new(backend(None, AudioFormat::default()), eq_enabled.clone(), eq_bands.clone()))
    });

    tokio::spawn(run_events(player.get_player_event_channel(), state.clone(), app.out.clone(), app.device_id.clone()));

    let connect_config = ConnectConfig { name: DEVICE_NAME.into(), device_type: DeviceType::Computer, initial_volume: u16::MAX / 2, ..ConnectConfig::default() };
    let result = Spirc::new(connect_config, session.clone(), credentials, player, mixer).await;
    let reusable = session.cache().and_then(|c| c.credentials());
    let (spirc, task) = result.context("no se pudo conectar con Spotify")?;
    let spirc = Arc::new(spirc);
    let engine = Arc::new(Engine { session: session.clone(), spirc, state, connect_off: AtomicBool::new(false) });
    *app.engine.lock().await = Some(engine.clone());

    let players_rx = engine.spirc.watch_player_state();
    let clusters_rx = engine.spirc.watch_cluster_state();
    {
        // Cola completa (lo que viene después) del dispositivo que suene, con uid y proveedor de cada pista
        // para poder reordenarla. Los metadatos se recuerdan por URI: al avanzar solo se piden los nuevos.
        let mut queue = engine.spirc.watch_queue_list();
        let players = engine.spirc.watch_player_state();
        let out = app.out.clone();
        let session = session.clone();
        tokio::spawn(async move {
            let mut known: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
            loop {
                queue.borrow_and_update();
                let visible = players.borrow().as_ref().map(visible_next_tracks).unwrap_or_default();
                let missing: Vec<String> = visible.iter().map(|t| t.uri.clone())
                    .filter(|u| !known.contains_key(u)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                if !missing.is_empty() {
                    for t in reads::queue_tracks(&session, &missing).await.unwrap_or_default() {
                        if let Some(u) = t.get("uri").and_then(Value::as_str) { known.insert(u.to_string(), t.clone()); }
                    }
                }
                if known.len() > 2000 { known.clear(); }
                let items: Vec<Value> = visible.iter().filter_map(|t| known.get(&t.uri).map(|track| json!({
                    "uid": t.uid, "queued": t.provider == "queue", "autoplay": t.provider == "autoplay", "track": track,
                }))).collect();
                let next = items.first().and_then(|i| i.get("track")).cloned();
                let _ = out.send(json!({"event": "queue", "next": next, "items": items}));
                if queue.changed().await.is_err() { break; }
            }
        });
    }
    let out = app.out.clone();
    let device = app.device_id.clone();
    let weak = Arc::downgrade(app);
    tokio::spawn(async move {
        task.await;
        // Connect apagado a propósito: la sesión sigue sirviendo lecturas.
        if engine.connect_off.load(Ordering::SeqCst) { return; }
        // Solo avisa si esta sesión sigue siendo la actual (no un reemplazo).
        if let Some(app) = weak.upgrade() {
            let mut slot = app.engine.lock().await;
            if slot.as_ref().is_some_and(|e| Arc::ptr_eq(e, &engine)) {
                *slot = None;
                let _ = out.send(json!({"event": "status", "state": "disconnected", "device_id": device}));
            }
        }
    });
    // Si librespot renueva la credencial (reconexión de la sesión), se avisa a la app para que la guarde.
    {
        let out = app.out.clone();
        let session = session.clone();
        let mut last = serde_json::to_value(&reusable).unwrap_or(Value::Null);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                if session.is_invalid() { break; }
                let Some(cur) = session.cache().and_then(|c| c.credentials()) else { continue };
                let v = serde_json::to_value(&cur).unwrap_or(Value::Null);
                if v != last && !v.is_null() {
                    last = v.clone();
                    let _ = out.send(json!({"event": "credentials", "credentials": v}));
                }
            }
        });
    }
    // Lo que suena en otro dispositivo (Alexa, teléfono…) llega por Spotify Connect: se reenvía a la app.
    {
        let out = app.out.clone();
        let me = app.device_id.clone();
        let mut players = players_rx;
        let mut clusters = clusters_rx;
        let session = session.clone();
        tokio::spawn(async move {
            // Canción del catálogo (nombre, artistas, portada) por URI: el teléfono no siempre anuncia los metadatos.
            // Se rellena en segundo plano: el evento sale al instante con lo que trae el paquete push.
            let catalog: Arc<Mutex<Option<(String, Value)>>> = Arc::default();
            let mut fetching = String::new();
            let mut announced = false;
            loop {
                // El dispositivo activo y el estado del reproductor pueden llegar por separado.
                tokio::select! {
                    changed = players.changed() => if changed.is_err() { break },
                    changed = clusters.changed() => if changed.is_err() { break },
                }
                let ps = players.borrow_and_update().clone();
                let cluster = clusters.borrow_and_update().clone();
                let active = cluster.active_device_id.clone().filter(|a| *a != me);
                let (Some(ps), Some(active)) = (ps, active) else {
                    // Ya no suena nada fuera: la app deja de mostrar el dispositivo remoto.
                    if announced {
                        announced = false;
                        let _ = out.send(json!({"event": "remote_state", "device_id": Value::Null}));
                    }
                    continue;
                };
                let name = cluster.devices.get(&active).map(|d| d.device_alias.clone()).unwrap_or_default();
                let volume = cluster.devices.get(&active).map(|d| d.volume.min(65535) * 100 / 65535);
                let mut event = remote_state_json(&ps, &active, &name, volume);
                let uri = event.pointer("/track/uri").and_then(Value::as_str).unwrap_or("").to_owned();
                let cached = catalog.lock().unwrap().as_ref().filter(|c| c.0 == uri).map(|c| c.1.clone());
                if let Some(track) = cached {
                    remote_merge_track(&mut event, track);
                } else if uri.starts_with("spotify:track:") && fetching != uri {
                    fetching = uri.clone();
                    let (session, catalog, out, base) = (session.clone(), catalog.clone(), out.clone(), event.clone());
                    tokio::spawn(async move {
                        let Some(track) = reads::queue_tracks(&session, &[uri.clone()]).await.ok().and_then(|v| v.into_iter().next()) else { return };
                        *catalog.lock().unwrap() = Some((uri, track.clone()));
                        // Mismo estado, con los metadatos completos (el `measured_at` original corrige la posición).
                        let mut full = base;
                        remote_merge_track(&mut full, track);
                        let _ = out.send(full);
                    });
                }
                announced = true;
                let _ = out.send(event);
            }
        });
    }
    // Cambios de la biblioteca hechos en otro dispositivo (teléfono, web…): Spotify los empuja por el
    // dealer al instante (fijados, «Tus me gusta», playlists de la biblioteca). Se avisa a la app en segundos.
    {
        use futures_util::StreamExt;
        let dealer = session.dealer();
        let collection = dealer.listen_for("hm://collection/", |m| Ok(m.uri));
        let rootlist = dealer.listen_for("hm://playlist/v2/user/", |m| Ok(m.uri));
        match (collection, rootlist) {
            (Ok(mut collection), Ok(mut rootlist)) => {
                let out = app.out.clone();
                tokio::spawn(async move {
                    loop {
                        let uri = tokio::select! {
                            m = collection.next() => match m { Some(Ok(u)) => u, Some(Err(_)) => continue, None => break },
                            m = rootlist.next() => match m { Some(Ok(u)) => u, Some(Err(_)) => continue, None => break },
                        };
                        let kind = if uri.contains("/ylpin/") { "pins" }
                            else if uri.contains("rootlist") { "rootlist" }
                            else if uri.contains("/collection/collection/") || uri.ends_with("/collection") { "liked" }
                            else { "other" };
                        if kind == "liked" { reads::forget_liked(); }
                        log::info!("biblioteca cambió: {uri}");
                        let _ = out.send(json!({"event": "library_changed", "kind": kind}));
                    }
                });
            }
            (a, b) => log::warn!("sin avisos de biblioteca: {:?} {:?}", a.err(), b.err()),
        }
    }
    let username = session.username();
    let _ = app.out.send(json!({"event": "status", "state": "ready", "device_id": app.device_id, "username": username, "country": session.country()}));
    Ok(json!({"username": username, "credentials": reusable}))
}

async fn run_events(
    mut events: librespot_playback::player::PlayerEventChannel,
    state: Arc<Mutex<LocalState>>,
    out: Out,
    device_id: String,
) {
    while let Some(event) = events.recv().await {
        let changed = {
            let mut st = state.lock().unwrap();
            let now = Some(Instant::now());
            match event {
                PlayerEvent::Loading { .. } => { st.loading = true; true }
                PlayerEvent::Playing { position_ms, .. } => {
                    st.loading = false;
                    if st.pending_fade_in && position_ms < 1500 { st.fade_in = true; st.pending_fade_in = false; }
                    st.playing = true; st.stopped = false; st.position_ms = position_ms; st.updated = now; true
                }
                PlayerEvent::EndOfTrack { .. } => { st.pending_fade_in = true; false }
                PlayerEvent::Paused { position_ms, .. } => { st.loading = false; st.playing = false; st.stopped = false; st.position_ms = position_ms; st.updated = now; true }
                PlayerEvent::Seeked { position_ms, .. } => {
                    st.fade_in = false; st.position_ms = position_ms; st.updated = now; true
                }
                PlayerEvent::PositionCorrection { position_ms, .. } => {
                    st.position_ms = position_ms; st.updated = now; true
                }
                PlayerEvent::PositionChanged { position_ms, .. } => { st.position_ms = position_ms; st.updated = now; false }
                PlayerEvent::AudioKeyUnavailable { .. } => {
                    let _ = out.send(json!({"event": "audio_key_unavailable"}));
                    false
                }
                PlayerEvent::Stopped { .. } => { st.loading = false; st.playing = false; st.stopped = true; true }
                PlayerEvent::TrackChanged { audio_item } => {
                    // Solo entra con fundido si la anterior terminó sola (no al saltar o elegir otra).
                    st.fade_in = st.pending_fade_in; st.pending_fade_in = false;
                    st.position_ms = 0; st.updated = now; st.stopped = false;
                    st.track = Some(audio_item_json(&audio_item));
                    true
                }
                PlayerEvent::VolumeChanged { volume } => { st.volume = volume; true }
                PlayerEvent::ShuffleChanged { shuffle } => { st.shuffle = shuffle; true }
                PlayerEvent::RepeatChanged { context, track } => {
                    st.repeat = if track { "track" } else if context { "context" } else { "off" }; true
                }
                _ => false,
            }
        };
        if changed {
            let snapshot = state.lock().unwrap().json(&device_id);
            let _ = out.send(snapshot);
        }
    }
}

fn audio_item_json(item: &librespot_metadata::audio::AudioItem) -> Value {
    let images: Vec<Value> = {
        let mut covers = item.covers.clone();
        covers.sort_by_key(|c| -(c.width as i64));
        covers.iter().map(|c| json!({"url": c.url})).collect()
    };
    let (artists, album) = match &item.unique_fields {
        UniqueFields::Track { artists, album, .. } => (
            artists.iter().map(|a| json!({"name": a.name, "id": a.id.to_id()})).collect::<Vec<_>>(),
            album.clone(),
        ),
        UniqueFields::Episode { show_name, .. } => (vec![json!({"name": show_name})], show_name.clone()),
        UniqueFields::Local { artists, album, .. } => (
            artists.iter().map(|a| json!({"name": a})).collect(),
            album.clone().unwrap_or_default(),
        ),
    };
    json!({
        "id": item.track_id.to_id(),
        "uri": item.uri,
        "name": item.name,
        "artists": artists,
        "album": {"name": album, "images": images},
        "duration_ms": item.duration_ms,
    })
}

#[cfg(test)]
mod crossfade_tests {
    use super::*;

    fn st(pos: u32, dur: u64, fade_in: bool) -> LocalState {
        LocalState { position_ms: pos, track: Some(json!({"duration_ms": dur})), fade_in, ..Default::default() }
    }

    #[test]
    fn fades_out_at_the_end_and_in_at_the_start() {
        let half = std::f64::consts::FRAC_PI_4.sin();
        assert!((FadeVolume::envelope(&st(100_000, 200_000, false), 6000) - 1.0).abs() < 1e-9);
        assert!((FadeVolume::envelope(&st(197_000, 200_000, false), 6000) - half).abs() < 1e-9);
        assert!(FadeVolume::envelope(&st(200_000, 200_000, false), 6000) < 1e-9);
        assert!((FadeVolume::envelope(&st(3000, 200_000, true), 6000) - half).abs() < 1e-9);
        // Sin transición automática no hay fundido de entrada.
        assert!((FadeVolume::envelope(&st(0, 200_000, false), 6000) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn never_stays_silent_on_stale_state() {
        assert!((FadeVolume::envelope(&st(210_000, 200_000, false), 6000) - 1.0).abs() < 1e-9);
    }
}
