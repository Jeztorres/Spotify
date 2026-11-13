//! Créditos de MusicBrainz, con coincidencia estricta de canción/álbum/artista como en Swift.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

fn normalize(s: &str) -> String {
    spotify_core::text::words(s).join(" ")
}
fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}
fn matching_id(track: &Track, search: &Value) -> Option<String> {
    let artist = track.artists.first()?.name.as_str();
    let album = &track.album.as_ref()?.name;
    search
        .get("recordings")?
        .as_array()?
        .iter()
        .find_map(|hit| {
            if normalize(hit.get("title")?.as_str()?) != normalize(&track.name) {
                return None;
            }
            let artists = hit.get("artist-credit")?.as_array()?;
            if !artists.iter().any(|a| {
                a.get("name")
                    .or_else(|| a.pointer("/artist/name"))
                    .and_then(Value::as_str)
                    .is_some_and(|s| normalize(s) == normalize(artist))
            }) {
                return None;
            }
            if !hit.get("releases")?.as_array()?.iter().any(|r| {
                r.get("title")
                    .and_then(Value::as_str)
                    .is_some_and(|s| normalize(s) == normalize(album))
            }) {
                return None;
            }
            if hit
                .get("length")
                .and_then(Value::as_i64)
                .is_some_and(|length| {
                    track.duration_ms > 0 && (length - track.duration_ms).abs() > 5000
                })
            {
                return None;
            }
            hit.get("id")?
                .as_str()
                .filter(|id| valid_id(id))
                .map(str::to_owned)
        })
}
fn role(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "producer" => "Producción",
        "executive producer" => "Producción ejecutiva",
        "co-producer" => "Coproducción",
        "engineer" => "Ingeniería",
        "mix" | "mix engineer" => "Mezcla",
        "mastering" | "mastering engineer" => "Masterización",
        "recording" => "Grabación",
        "instrument" => "Instrumentos",
        "vocal" => "Voz",
        "performer" => "Interpretación",
        "composer" => "Composición",
        "lyricist" => "Letra",
        "writer" => "Autoría",
        "arranger" => "Arreglos",
        "orchestrator" => "Orquestación",
        "conductor" => "Dirección musical",
        "programming" => "Programación",
        _ => return None,
    })
}
fn format_detail(detail: &Value) -> String {
    fn add(v: &Value, grouped: &mut BTreeMap<String, BTreeSet<String>>) {
        for rel in v
            .get("relations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(kind), Some(person)) = (
                rel.get("type").and_then(Value::as_str).and_then(role),
                rel.get("target-credit")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| rel.pointer("/artist/name").and_then(Value::as_str)),
            ) {
                grouped
                    .entry(kind.into())
                    .or_default()
                    .insert(person.into());
            }
        }
    }
    let mut grouped = BTreeMap::new();
    add(detail, &mut grouped);
    for rel in detail
        .get("relations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(work) = rel.get("work") {
            add(work, &mut grouped);
        }
    }
    grouped
        .into_iter()
        .map(|(role, people)| {
            format!(
                "{role}\n{}",
                people.into_iter().collect::<Vec<_>>().join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
pub(crate) fn get(path: &str, query: &[(&str, &str)]) -> Result<Value, String> {
    static GATE: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    let mut last = GATE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|e| e.to_string())?;
    if let Some(instant) = *last {
        std::thread::sleep(Duration::from_millis(1100).saturating_sub(instant.elapsed()));
    }
    *last = Some(Instant::now());
    let mut request = ureq::get(&format!("https://musicbrainz.org/ws/2/{path}"))
        .timeout(Duration::from_secs(20))
        .set(
            "User-Agent",
            "SpotifyMac-Windows/0.1 (desktop music player)",
        )
        .query("fmt", "json");
    for (key, value) in query {
        request = request.query(key, value);
    }
    let response = request.call().map_err(|e| e.to_string())?;
    serde_json::from_reader(response.into_reader()).map_err(|e| e.to_string())
}
pub fn load(track: &Track) -> Result<(String, String), String> {
    let Some(artist) = track.artists.first() else {
        return Ok((
            "No hay datos suficientes para buscar los créditos.".into(),
            String::new(),
        ));
    };
    let Some(album) = track.album.as_ref() else {
        return Ok((
            "No hay datos suficientes para buscar los créditos.".into(),
            String::new(),
        ));
    };
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let query = format!(
        "recording:\"{}\" AND artist:\"{}\" AND release:\"{}\"",
        quote(&track.name),
        quote(&artist.name),
        quote(&album.name)
    );
    let search = get("recording/", &[("query", &query), ("limit", "25")])?;
    let Some(id) = matching_id(track, &search) else {
        return Ok(("No se encontró una coincidencia exacta en MusicBrainz para esta versión de la canción.".into(), String::new()));
    };
    let detail = get(
        &format!("recording/{id}"),
        &[("inc", "artist-rels+work-rels+work-level-rels")],
    )?;
    let text = format_detail(&detail);
    Ok((
        if text.is_empty() {
            "La grabación existe, pero MusicBrainz todavía no tiene créditos detallados.".into()
        } else {
            format!("{text}\n\nFuente: MusicBrainz")
        },
        format!("https://musicbrainz.org/recording/{id}"),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credits_reject_different_album_or_duration_and_deduplicate_people() {
        let track: Track = serde_json::from_value(json!({"uri":"spotify:track:a","name":"Song","artists":[{"name":"Artist"}],"album":{"name":"Album"},"duration_ms":180000})).unwrap();
        let mut hit = json!({"id":"12345678-1234-1234-1234-123456789abc","title":"Song","artist-credit":[{"name":"Artist"}],"releases":[{"title":"Other"}],"length":180000});
        assert!(matching_id(&track, &json!({"recordings":[hit.clone()]})).is_none());
        hit["releases"] = json!([{"title":"Album"}]);
        hit["length"] = json!(200000);
        assert!(matching_id(&track, &json!({"recordings":[hit.clone()]})).is_none());
        hit["length"] = json!(181000);
        assert!(matching_id(&track, &json!({"recordings":[hit]})).is_some());
        let detail = json!({"relations":[{"type":"producer","artist":{"name":"A"}},{"type":"producer","artist":{"name":"A"}},{"work":{"relations":[{"type":"composer","artist":{"name":"B"}}]}}]});
        assert_eq!(format_detail(&detail), "Composición\nB\n\nProducción\nA");
    }
}
