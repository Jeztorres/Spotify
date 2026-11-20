//! Créditos de canciones: Spotify (Engine) -> Credits.fm -> MusicBrainz.
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
fn translate_role_title(title: &str) -> &str {
    match title.to_lowercase().as_str() {
        "performed by" | "performers" => "Interpretación",
        "written by" => "Composición y letra",
        "produced by" | "production" => "Producción",
        "engineered by" | "engineering" => "Ingeniería",
        "mixed by" => "Mezcla",
        "mastered by" => "Masterización",
        _ => title,
    }
}

pub fn parse_spotify_credits(v: &Value) -> Option<String> {
    let roles = v.get("roleCredits")?.as_array()?;
    let mut blocks = Vec::new();
    for role in roles {
        let title = role.get("roleTitle").and_then(Value::as_str).unwrap_or("").trim();
        let artists = role.get("artists").and_then(Value::as_array);
        let mut lines = Vec::new();
        if let Some(artists) = artists {
            for a in artists {
                let name = a.get("name").and_then(Value::as_str).unwrap_or("").trim();
                if name.is_empty() {
                    continue;
                }
                let subroles: Vec<&str> = a
                    .get("subroles")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                if subroles.is_empty() {
                    lines.push(format!("• {name}"));
                } else {
                    lines.push(format!("• {name} ({})", subroles.join(", ")));
                }
            }
        }
        if !title.is_empty() && !lines.is_empty() {
            blocks.push(format!("{}\n{}", translate_role_title(title), lines.join("\n")));
        }
    }
    if blocks.is_empty() {
        return None;
    }
    let sources: Vec<&str> = v
        .get("sourceNames")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let footer = if sources.is_empty() {
        "Fuente: Spotify".to_owned()
    } else {
        format!("Fuente: {}", sources.join(" · "))
    };
    Some(format!("{}\n\n{footer}", blocks.join("\n\n")))
}

pub fn load_with_engine(track: &Track, engine: Option<&crate::engine::Engine>) -> Result<(String, String), String> {
    if let (Some(e), Some(id)) = (engine, track.uri.strip_prefix("spotify:track:")) {
        if !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            if let Ok(val) = e.call("track_credits", serde_json::json!({ "id": id }), std::time::Duration::from_secs(10)) {
                if let Some(text) = parse_spotify_credits(&val) {
                    return Ok((text, format!("https://open.spotify.com/track/{id}")));
                }
            }
        }
    }
    load(track)
}

fn title_case_if_upper(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty()
        || !trimmed.chars().any(|c| c.is_alphabetic())
        || trimmed.chars().any(|c| c.is_lowercase())
    {
        return trimmed.to_owned();
    }
    let mut out = String::with_capacity(trimmed.len());
    let mut cap_next = true;
    for ch in trimmed.chars() {
        if cap_next && ch.is_alphabetic() {
            out.extend(ch.to_uppercase());
            cap_next = false;
        } else {
            out.extend(ch.to_lowercase());
            cap_next = ch.is_whitespace() || ch == '-' || ch == '\'';
        }
    }
    out
}

fn credits_fm_role(role: &str) -> &str {
    match role.trim().to_ascii_lowercase().as_str() {
        "composer" => "Composición",
        "lyricist" | "author" => "Letra",
        "composer/lyricist" | "writer" | "songwriter" | "" => "Composición y letra",
        "producer" => "Producción",
        "executive producer" => "Producción ejecutiva",
        "co-producer" => "Coproducción",
        "arranger" => "Arreglos",
        "adapter" => "Adaptación",
        "translator" => "Traducción",
        "performer" | "main artist" | "primary artist" => "Interpretación",
        "featured artist" | "featuring" => "Artista invitado",
        _ => role.trim(),
    }
}

fn matching_credits_fm_isrcs(track: &Track, search: &Value) -> Vec<String> {
    let Some(items) = search.pointer("/recordings/items").and_then(Value::as_array) else {
        return Vec::new();
    };
    let target_title = normalize(&track.name);
    let target_artists: Vec<String> = track.artists.iter().map(|a| normalize(&a.name)).collect();
    let mut out = Vec::new();
    for item in items {
        let Some(title) = item.get("title").and_then(Value::as_str) else {
            continue;
        };
        if normalize(title) != target_title {
            continue;
        }
        let Some(artists) = item.get("artist_names").and_then(Value::as_array) else {
            continue;
        };
        let artist_match = artists.iter().filter_map(Value::as_str).any(|a| {
            let norm = normalize(a);
            target_artists.iter().any(|t| *t == norm)
        });
        if !artist_match {
            continue;
        }
        if let Some(isrc) = item.get("isrc").and_then(Value::as_str) {
            if isrc.len() >= 10
                && isrc.bytes().all(|b| b.is_ascii_alphanumeric())
                && !out.iter().any(|existing| existing == isrc)
            {
                out.push(isrc.to_owned());
            }
        }
    }
    out
}

fn format_credits_fm(detail: &Value) -> Option<String> {
    let mut grouped: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut publishers: BTreeSet<String> = BTreeSet::new();
    let mut has_songwriters = false;

    for sw in detail
        .get("songwriters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(raw_name) = sw.get("name").and_then(Value::as_str) else {
            continue;
        };
        let name = title_case_if_upper(raw_name);
        if name.is_empty() {
            continue;
        }
        has_songwriters = true;
        let role_raw = sw.get("role").and_then(Value::as_str).unwrap_or("");
        let role_label = credits_fm_role(role_raw);
        grouped.entry(role_label.into()).or_default().insert(name);
        for pub_item in sw
            .get("publishers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let pub_name = pub_item
                .as_str()
                .or_else(|| pub_item.get("name").and_then(Value::as_str));
            if let Some(p) = pub_name.map(title_case_if_upper).filter(|s| !s.is_empty()) {
                publishers.insert(p);
            }
        }
    }

    if !has_songwriters {
        return None;
    }

    for perf in detail
        .get("performers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(raw_name) = perf.get("name").and_then(Value::as_str) else {
            continue;
        };
        let name = title_case_if_upper(raw_name);
        if name.is_empty() {
            continue;
        }
        let role_raw = perf
            .get("role")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .or_else(|| perf.get("credit_type").and_then(Value::as_str))
            .unwrap_or("performer");
        let role_label = credits_fm_role(role_raw);
        grouped.entry(role_label.into()).or_default().insert(name);
    }

    if !publishers.is_empty() {
        grouped.insert("Editoriales".into(), publishers);
    }

    if grouped.is_empty() {
        return None;
    }

    let body = grouped
        .into_iter()
        .map(|(role, people)| {
            format!(
                "{role}\n{}",
                people.into_iter().collect::<Vec<_>>().join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(format!("{body}\n\nFuente: Credits.fm"))
}

fn credits_fm_get(path: &str, query: &[(&str, &str)]) -> Result<Value, String> {
    let mut request = ureq::get(&format!("https://api.credits.fm/v1/{path}"))
        .timeout(Duration::from_secs(12))
        .set("User-Agent", "SpotifyMac-Windows/0.1 (desktop music player)");
    for (key, value) in query {
        request = request.query(key, value);
    }
    let response = request.call().map_err(|e| e.to_string())?;
    serde_json::from_reader(response.into_reader()).map_err(|e| e.to_string())
}

fn load_credits_fm(track: &Track) -> Option<(String, String)> {
    let artist = track.artists.first()?;
    let q = format!("{} {}", track.name, artist.name).to_lowercase();
    let search = credits_fm_get("search", &[("q", &q)]).ok()?;
    for isrc in matching_credits_fm_isrcs(track, &search).into_iter().take(3) {
        if let Ok(detail) = credits_fm_get(&format!("isrc/{isrc}"), &[]) {
            if let Some(text) = format_credits_fm(&detail) {
                return Some((text, format!("https://credits.fm/isrc/{isrc}")));
            }
        }
    }
    None
}

pub fn load(track: &Track) -> Result<(String, String), String> {
    let Some(artist) = track.artists.first() else {
        return Ok((
            "No hay datos suficientes para buscar los créditos.".into(),
            String::new(),
        ));
    };
    if let Some(hit) = load_credits_fm(track) {
        return Ok(hit);
    }
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
        return Ok(("No se encontró una coincidencia exacta para esta canción en Credits.fm ni en MusicBrainz.".into(), String::new()));
    };
    let detail = get(
        &format!("recording/{id}"),
        &[("inc", "artist-rels+work-rels+work-level-rels")],
    )?;
    let text = format_detail(&detail);
    Ok((
        if text.is_empty() {
            "La grabación existe, pero todavía no tiene créditos detallados en Credits.fm ni en MusicBrainz.".into()
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

    #[test]
    fn credits_fm_matches_isrc_and_formats_songwriters_and_performers() {
        let track: Track = serde_json::from_value(json!({
            "uri": "spotify:track:a",
            "name": "Hipnotízame",
            "artists": [{"name": "Wisin & Yandel"}],
            "album": {"name": "Líderes"},
            "duration_ms": 220000
        }))
        .unwrap();
        let search = json!({
            "recordings": {
                "items": [
                    {"isrc": "XX0000000001", "title": "Hipnotízame", "artist_names": ["Otro Artista"]},
                    {"isrc": "USUM71206368", "title": "Hipnotízame", "artist_names": ["Wisin & Yandel"]}
                ]
            }
        });
        assert_eq!(
            matching_credits_fm_isrcs(&track, &search),
            vec!["USUM71206368".to_string()]
        );

        let detail = json!({
            "songwriters": [
                {"name": "MARCOS EFRAIN MASIS", "role": "Composer", "publishers": []},
                {"name": "JUAN LUIS MORERA LUNA", "role": "Composer", "publishers": ["Sony Music"]}
            ],
            "performers": [
                {"name": "Wisin & Yandel", "role": "performer"}
            ]
        });
        let formatted = format_credits_fm(&detail).unwrap();
        assert!(formatted.contains("Composición\nJuan Luis Morera Luna, Marcos Efrain Masis"));
        assert!(formatted.contains("Interpretación\nWisin & Yandel"));
        assert!(formatted.contains("Editoriales\nSony Music"));
        assert!(formatted.ends_with("Fuente: Credits.fm"));
    }
}
