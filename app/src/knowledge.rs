//! Información pública del catálogo; nunca se atribuye una biografía a una coincidencia no musical.
use super::*;
pub struct Knowledge {
    pub text: String,
    pub url: String,
    #[allow(dead_code)]
    pub genres: Vec<String>,
}
fn norm(s: &str) -> String {
    spotify_core::text::words(s).join(" ")
}
fn musical(text: &str) -> bool {
    let text = spotify_core::text::fold(text);
    [
        "cantante",
        "musico",
        "banda",
        "rapero",
        "compositor",
        "grupo",
        "singer",
        "musician",
        "band",
        "rapper",
        "songwriter",
        "producer",
        "album",
        "disco",
        "extended play",
    ]
    .iter()
    .any(|word| text.contains(word))
}
fn wiki_agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(8))
            .max_idle_connections_per_host(4)
            .user_agent("SpotifyMac-Windows/0.1 (catalogue information)")
            .build()
    })
}
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
fn wiki_cache_path(name: &str, artist: Option<&str>) -> std::path::PathBuf {
    let key = format!("wiki-v1|{}|{}", name.to_lowercase(), artist.unwrap_or("").to_lowercase());
    let hash = key.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let dir = engine::data_dir().join("profile-cache");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("wiki-{hash:016x}.json"))
}
fn wiki(name: &str, artist: Option<&str>) -> Option<(String, String)> {
    let path = wiki_cache_path(name, artist);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            let age = unix_now().saturating_sub(v["at"].as_u64().unwrap_or(0));
            let text = v.get("text").and_then(Value::as_str);
            let ttl = if text.is_some() { 7 * 86400 } else { 86400 };
            if age < ttl {
                return text.map(|t| (t.to_owned(), v["url"].as_str().unwrap_or_default().to_owned()));
            }
        }
    }
    let agent = wiki_agent();
    let mut any_ok = false;
    for lang in ["es", "en"] {
        let query = match artist {
            Some(a) => format!("\"{name}\" \"{a}\" album"),
            None => format!("\"{name}\" musician band singer"),
        };
        let mut fetch = |params: &[(&str, &str)]| -> Option<Value> {
            let mut req = agent
                .get(&format!("https://{lang}.wikipedia.org/w/api.php"))
                .query("format", "json")
                .query("action", "query");
            for (k, v) in params {
                req = req.query(k, v);
            }
            let val: Value = serde_json::from_reader(req.call().ok()?.into_reader()).ok()?;
            any_ok = true;
            Some(val)
        };
        let Some(search) = fetch(&[("list", "search"), ("srsearch", &query), ("srlimit", "2")])
        else {
            continue;
        };
        for hit in search
            .pointer("/query/search")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(title) = hit.get("title").and_then(Value::as_str) else {
                continue;
            };
            let Some(data) = fetch(&[
                ("prop", "extracts|info"),
                ("exintro", "1"),
                ("explaintext", "1"),
                ("inprop", "url"),
                ("redirects", "1"),
                ("titles", title),
            ]) else {
                continue;
            };
            for page in data
                .pointer("/query/pages")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|m| m.values())
            {
                let Some(text) = page
                    .get("extract")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    continue;
                };
                if !musical(text) {
                    continue;
                }
                if let Some(a) = artist {
                    if !norm(text).contains(&norm(a)) {
                        continue;
                    }
                } else if !norm(title).contains(&norm(name)) {
                    continue;
                }
                let url = page
                    .get("fullurl")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if url.starts_with(&format!("https://{lang}.wikipedia.org/wiki/")) {
                    if let Ok(bytes) = serde_json::to_vec(&json!({ "at": unix_now(), "text": text, "url": url })) {
                        let _ = std::fs::write(&path, bytes);
                    }
                    return Some((text.to_owned(), url.to_owned()));
                }
            }
        }
    }
    if any_ok {
        if let Ok(bytes) = serde_json::to_vec(&json!({ "at": unix_now(), "text": null })) {
            let _ = std::fs::write(&path, bytes);
        }
    }
    None
}
pub fn artist(name: &str) -> Knowledge {
    let (text, url) = wiki(name, None).unwrap_or_else(|| {
        (
            "No hay una biografía disponible para este artista.".into(),
            String::new(),
        )
    });
    Knowledge {
        text,
        url,
        genres: vec![],
    }
}
#[allow(dead_code)]
pub fn album(name: &str, artist: &str) -> Knowledge {
    let (text, url) = wiki(name, Some(artist)).unwrap_or_else(|| {
        (
            "No hay una reseña disponible para este álbum.".into(),
            String::new(),
        )
    });
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let query = format!(
        "releasegroup:\"{}\" AND artist:\"{}\"",
        quote(name),
        quote(artist)
    );
    let genres = (|| -> Option<Vec<String>> {
        let search = credits::get("release-group/", &[("query", &query), ("limit", "5")]).ok()?;
        let hit = search
            .get("release-groups")?
            .as_array()?
            .iter()
            .find(|hit| {
                hit.get("title")
                    .and_then(Value::as_str)
                    .is_some_and(|s| norm(s) == norm(name))
                    && hit
                        .get("artist-credit")
                        .and_then(Value::as_array)
                        .is_some_and(|a| {
                            a.iter().any(|a| {
                                a.get("name")
                                    .or_else(|| a.pointer("/artist/name"))
                                    .and_then(Value::as_str)
                                    .is_some_and(|s| norm(s) == norm(artist))
                            })
                        })
            })?;
        let id = hit.get("id")?.as_str()?;
        if id.len() != 36 || !id.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') {
            return None;
        }
        let detail = credits::get(&format!("release-group/{id}"), &[("inc", "genres")]).ok()?;
        let mut genres = detail.get("genres")?.as_array()?.clone();
        genres.sort_by_key(|g| -g.get("count").and_then(Value::as_i64).unwrap_or(0));
        Some(
            genres
                .iter()
                .take(5)
                .filter_map(|g| g.get("name")?.as_str().map(str::to_owned))
                .collect(),
        )
    })()
    .unwrap_or_default();
    Knowledge { text, url, genres }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn non_musical_articles_are_not_artist_biographies() {
        assert!(!musical("A city located near a river."));
        assert!(musical("Es una banda de rock."));
    }
}
