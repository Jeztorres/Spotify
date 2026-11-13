//! Traducción de letras al español con el endpoint público de Google Translate
//! (`clients5.google.com/translate_a/t`, sin clave): detecta el idioma solo (`sl=auto`).
//!
//! Las líneas se envían en lotes de hasta 1800 caracteres unidas por saltos de línea y se
//! reparten de vuelta por posición. Caché en dos niveles: RAM y un JSON en `%APPDATA%\SpotifyMac`,
//! así que una canción ya traducida se ve al instante y sin internet.

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const ENDPOINT: &str = "https://clients5.google.com/translate_a/t";
const TARGET: &str = "es";
const CHUNK_CHARS: usize = 1800;
const MAX_SONGS_ON_DISK: usize = 400;

/// Resultado de traducir una letra: idioma detectado y texto traducido por número de línea.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Translation {
    pub lang: String,
    pub lines: HashMap<usize, String>,
}

impl Translation {
    /// La letra ya estaba en español: no hay nada que mostrar debajo.
    pub fn is_target_language(&self) -> bool {
        self.lang.eq_ignore_ascii_case(TARGET)
    }
}

/// Identidad de la canción (artista + título) más una huella del texto: si cambia la letra
/// (otro proveedor), no se reutiliza una traducción con las líneas desalineadas.
pub fn song_key(artist: &str, title: &str, lines: &[String]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in lines.join("\n").bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{}|{}|{hash:016x}", artist.trim().to_lowercase(), title.trim().to_lowercase())
}

fn memory() -> &'static Mutex<HashMap<String, Translation>> {
    static MEM: OnceLock<Mutex<HashMap<String, Translation>>> = OnceLock::new();
    MEM.get_or_init(|| Mutex::new(load_disk()))
}

fn disk_path() -> std::path::PathBuf {
    crate::engine::data_dir().join("lyrics_translations.json")
}

fn load_disk() -> HashMap<String, Translation> {
    let Some(Value::Object(map)) = std::fs::read(disk_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()) else {
        return HashMap::new();
    };
    map.into_iter()
        .filter_map(|(key, v)| {
            let lang = v.get("lang")?.as_str()?.to_owned();
            let lines = v.get("lines")?.as_object()?.iter()
                .filter_map(|(i, t)| Some((i.parse().ok()?, t.as_str()?.to_owned())))
                .collect();
            Some((key, Translation { lang, lines }))
        })
        .collect()
}

fn save_disk(all: &HashMap<String, Translation>) {
    let mut out = Map::new();
    for (key, t) in all.iter().take(MAX_SONGS_ON_DISK) {
        let lines: Map<String, Value> = t.lines.iter().map(|(i, s)| (i.to_string(), json!(s))).collect();
        out.insert(key.clone(), json!({"lang": t.lang, "lines": lines}));
    }
    let _ = std::fs::write(disk_path(), Value::Object(out).to_string());
}

/// Traducción ya guardada (RAM o disco), sin red.
pub fn cached(key: &str) -> Option<Translation> {
    memory().lock().ok()?.get(key).cloned()
}

/// Traduce las líneas (bloquea: llamar desde un hilo de trabajo). Guarda el resultado en caché.
pub fn translate(key: &str, lines: &[String]) -> Result<Translation, String> {
    if let Some(hit) = cached(key) {
        return Ok(hit);
    }
    let wanted: Vec<(usize, &str)> = lines.iter().enumerate()
        .map(|(i, l)| (i, l.trim()))
        .filter(|(_, l)| worth_translating(l))
        .collect();
    let mut result = Translation::default();
    for chunk in chunks(&wanted, CHUNK_CHARS) {
        let joined = chunk.iter().map(|(_, l)| *l).collect::<Vec<_>>().join("\n");
        let (text, lang) = request(&joined)?;
        if result.lang.is_empty() {
            result.lang = lang;
        }
        let parts: Vec<&str> = text.split('\n').collect();
        if parts.len() == chunk.len() {
            for ((i, _), t) in chunk.iter().zip(parts) {
                result.lines.insert(*i, t.trim().to_owned());
            }
        } else {
            // El traductor fusionó o partió líneas: ese bloque se traduce línea a línea.
            for (i, line) in chunk {
                if let Ok((t, _)) = request(line) {
                    result.lines.insert(i, t.trim().to_owned());
                }
            }
        }
    }
    if let Ok(mut all) = memory().lock() {
        if all.len() >= MAX_SONGS_ON_DISK {
            if let Some(old) = all.keys().next().cloned() {
                all.remove(&old);
            }
        }
        all.insert(key.to_owned(), result.clone());
        save_disk(&all);
    }
    Ok(result)
}

/// Líneas con texto real: se omiten «♪», vacías y las que son solo signos.
fn worth_translating(line: &str) -> bool {
    line.chars().any(char::is_alphabetic)
}

/// Agrupa líneas en bloques que no pasen de `max` caracteres (una línea larga va sola).
fn chunks<'a>(lines: &[(usize, &'a str)], max: usize) -> Vec<Vec<(usize, &'a str)>> {
    let mut out: Vec<Vec<(usize, &str)>> = Vec::new();
    let mut size = 0;
    for &(i, line) in lines {
        let len = line.chars().count() + 1;
        if out.is_empty() || size + len > max {
            out.push(Vec::new());
            size = 0;
        }
        out.last_mut().unwrap().push((i, line));
        size += len;
    }
    out
}

/// Cliente HTTP propio: prueba primero las direcciones IPv4. En redes sin IPv6 la conexión a Google
/// fallaba a veces con «red no accesible» (os error 10051) y la traducción no llegaba.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            .resolver(|addr: &str| -> std::io::Result<Vec<std::net::SocketAddr>> {
                use std::net::ToSocketAddrs;
                let mut all: Vec<_> = addr.to_socket_addrs()?.collect();
                all.sort_by_key(|a| !a.is_ipv4());
                Ok(all)
            })
            .build()
    })
}

/// Hasta tres intentos con una espera corta (cortes de red momentáneos).
fn request(text: &str) -> Result<(String, String), String> {
    let mut last = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(400 * attempt));
        }
        match request_once(text) {
            Ok(r) => return Ok(r),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn request_once(text: &str) -> Result<(String, String), String> {
    let reply = agent().get(ENDPOINT)
        .query("client", "dict-chrome-ex")
        .query("sl", "auto")
        .query("tl", TARGET)
        .query("q", text)
        .call()
        // Sin la URL (llevaría la letra entera) en el mensaje de error.
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            ureq::Error::Transport(t) => t.kind().to_string() + &t.message().map(|m| format!(": {m}")).unwrap_or_default(),
        })?;
    let value: Value = serde_json::from_reader(reply.into_reader()).map_err(|e| e.to_string())?;
    parse_reply(&value).ok_or_else(|| "respuesta de traducción inesperada".to_owned())
}

/// `[["texto traducido","en"]]` → (texto, idioma). También acepta `["texto"]`.
fn parse_reply(v: &Value) -> Option<(String, String)> {
    let first = v.as_array()?.first()?;
    match first {
        Value::String(s) => Some((s.clone(), String::new())),
        Value::Array(pair) => Some((
            pair.first()?.as_str()?.to_owned(),
            pair.get(1).and_then(Value::as_str).unwrap_or("").to_owned(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_google_reply_shapes() {
        assert_eq!(parse_reply(&json!([["hola\nmundo", "en"]])), Some(("hola\nmundo".into(), "en".into())));
        assert_eq!(parse_reply(&json!(["hola"])), Some(("hola".into(), "".into())));
        assert_eq!(parse_reply(&json!({})), None);
    }

    #[test]
    fn chunks_respect_limit_and_keep_order() {
        let lines = vec![(0, "aaaa"), (1, "bbbb"), (2, "cccc")];
        let c = chunks(&lines, 10);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0], vec![(0, "aaaa"), (1, "bbbb")]);
        assert_eq!(c[1], vec![(2, "cccc")]);
    }

    #[test]
    fn skips_symbols_and_music_notes() {
        assert!(!worth_translating("♪"));
        assert!(!worth_translating("  "));
        assert!(worth_translating("Hello"));
    }

    #[test]
    fn key_changes_with_lyrics_text() {
        let a = song_key("Artist", "Song", &["one".into()]);
        let b = song_key("artist ", "song", &["one".into()]);
        let c = song_key("Artist", "Song", &["two".into()]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
