//! Resultado principal de la búsqueda: siempre un artista (portado de `SearchRanking.swift`).
//!
//! 1) El artista cuyo nombre mejor coincide con lo escrito (exacto > contenido en la consulta > prefijo > palabra).
//!    Entre empates manda el orden de relevancia de Spotify.
//! 2) Si ninguno coincide bien (p. ej. se escribió el título de una canción), el artista principal de la
//!    primera canción: «blinding lights» → The Weeknd.
//! 3) Si no hay canciones, el artista del primer álbum o, en último caso, el primer artista.

use crate::models::{ArtistItem, SearchResults, SpArtist, Track};
use crate::text::words;

/// Título sin «feat.», «remaster», «version», «edit»… (portado de `MusicVideoFinder.cleanTitle`).
pub fn clean_title(s: &str) -> String {
    let mut t = match s.find(" - ") {
        Some(i) => s[..i].to_owned(),
        None => s.to_owned(),
    };
    while let Some(open) = t.find(|c| c == '(' || c == '[') {
        let Some(rel) = t[open..].find(|c| c == ')' || c == ']') else { break };
        let close = open + rel;
        let inner = t[open + 1..close].to_lowercase();
        let removable = inner.starts_with("feat")
            || inner.starts_with("ft.")
            || inner.starts_with("with ")
            || inner.starts_with("con ")
            || inner.contains("remaster")
            || inner.contains("version")
            || inner.contains("edit");
        if !removable {
            break;
        }
        t.replace_range(open..=close, "");
    }
    t.trim_matches(|c| c == ' ' || c == '\t').to_owned()
}

/// 0 = no coincide. Mayor = mejor.
pub fn match_score(name: &str, query: &str) -> u32 {
    let (nw, qw) = (words(name), words(query));
    let (n, q) = (nw.concat(), qw.concat());
    if q.is_empty() || n.is_empty() {
        return 0;
    }
    if n == q {
        return 100;
    }
    // La consulta contiene el nombre completo como palabras: «bad bunny dtmf» → Bad Bunny.
    if nw.len() <= qw.len()
        && n.chars().count() >= 3
        && format!(" {} ", qw.join(" ")).contains(&format!(" {} ", nw.join(" ")))
    {
        return 90;
    }
    if n.starts_with(&q) {
        return 80;
    }
    if nw.iter().any(|w| w.starts_with(&q)) {
        return 60; // «weeknd» → The Weeknd
    }
    if q.chars().count() >= 3 && n.contains(&q) {
        return 40;
    }
    0
}

fn from_ref(r: &SearchResults, reference: &SpArtist) -> Option<ArtistItem> {
    let id = reference.id.as_deref()?;
    Some(r.artists.iter().find(|a| a.id == id).cloned().unwrap_or_else(|| ArtistItem::new(id, &reference.name)))
}

pub fn top_artist(query: &str, r: &SearchResults) -> Option<ArtistItem> {
    let mut best: Option<(&ArtistItem, u32)> = None;
    for a in &r.artists {
        let s = match_score(&a.name, query);
        if s == 0 {
            continue;
        }
        if best.map_or(true, |(_, bs)| s > bs) {
            best = Some((a, s));
        }
    }

    // Si lo escrito es el título de una canción («blinding lights»), un artista que solo comparte una palabra
    // con el título («Blinding») no debe ganar: manda el artista de esa canción.
    if let Some((b, 90)) = best {
        if let Some(reference) = r.tracks.first().and_then(|t| t.artists.first()) {
            if reference.id.as_deref().is_some_and(|id| id != b.id) {
                let title = words(&clean_title(&r.tracks[0].name)).concat();
                let q = words(query).concat();
                if title.chars().count() >= 3 && q.contains(&title) {
                    return from_ref(r, reference);
                }
            }
        }
    }
    if let Some((b, s)) = best {
        if s >= 60 {
            return Some(b.clone());
        }
    }
    // El artista de la primera canción (o álbum) que coincide con lo escrito.
    if let Some(a) = r.tracks.first().and_then(|t| t.artists.first()).and_then(|a| from_ref(r, a)) {
        return Some(a);
    }
    if let Some(a) = r
        .albums
        .first()
        .and_then(|al| al.artists.as_ref())
        .and_then(|v| v.first())
        .and_then(|a| from_ref(r, a))
    {
        return Some(a);
    }
    best.map(|(a, _)| a.clone()).or_else(|| r.artists.first().cloned())
}

/// Las canciones del artista principal primero (conservando el orden de Spotify dentro de cada grupo).
pub fn tracks_preferring(tracks: &[Track], top: Option<&ArtistItem>) -> Vec<Track> {
    let Some(top) = top else { return tracks.to_vec() };
    let is_mine = |t: &&Track| t.artists.first().and_then(|a| a.id.as_deref()) == Some(top.id.as_str());
    tracks.iter().filter(is_mine).chain(tracks.iter().filter(|t| !is_mine(t))).cloned().collect()
}

/// El artista principal el primero de la lista, sin duplicados.
pub fn artists_with_top(list: &[ArtistItem], top: Option<&ArtistItem>) -> Vec<ArtistItem> {
    let Some(top) = top else { return list.to_vec() };
    let head = list.iter().find(|a| a.id == top.id).unwrap_or(top).clone();
    std::iter::once(head).chain(list.iter().filter(|a| a.id != top.id).cloned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist(id: &str, name: &str) -> ArtistItem {
        ArtistItem::new(id, name)
    }

    fn track(name: &str, by: &ArtistItem) -> Track {
        Track {
            id: Some(format!("t{name}")),
            uri: format!("spotify:track:{name}"),
            name: name.into(),
            artists: vec![SpArtist { name: by.name.clone(), id: Some(by.id.clone()) }],
            album: None,
            duration_ms: 1000,
            explicit: None,
        }
    }

    fn top(q: &str, r: &SearchResults) -> Option<String> {
        top_artist(q, r).map(|a| a.name)
    }

    #[test]
    fn exact_and_prefix_and_word_matches() {
        let r = SearchResults {
            artists: vec![artist("1", "Bad Religion"), artist("2", "Bad Bunny"), artist("3", "The Weeknd")],
            ..Default::default()
        };
        assert_eq!(top("bad bunny", &r).as_deref(), Some("Bad Bunny")); // exacto, aunque no sea el primero
        assert_eq!(top("bad bu", &r).as_deref(), Some("Bad Bunny")); // prefijo mientras escribe
        assert_eq!(top("weeknd", &r).as_deref(), Some("The Weeknd")); // palabra suelta
        assert_eq!(top("  BAD BUNNY ", &r).as_deref(), Some("Bad Bunny"));
    }

    #[test]
    fn query_containing_artist_name_wins() {
        let r = SearchResults { artists: vec![artist("1", "Dtmf Band"), artist("2", "Bad Bunny")], ..Default::default() };
        assert_eq!(top("bad bunny dtmf", &r).as_deref(), Some("Bad Bunny"));
    }

    #[test]
    fn song_title_falls_back_to_track_artist() {
        let weeknd = artist("w", "The Weeknd");
        let r = SearchResults {
            tracks: vec![track("Blinding Lights", &weeknd)],
            artists: vec![artist("x", "Blinding")],
            ..Default::default()
        };
        assert_eq!(top("blinding lights", &r).as_deref(), Some("The Weeknd")); // aunque no esté en la lista de artistas
    }

    #[test]
    fn tracks_and_artists_order_put_top_first() {
        let (a, b) = (artist("1", "A"), artist("2", "B"));
        let ts = vec![track("b1", &b), track("a1", &a), track("b2", &b), track("a2", &a)];
        let names: Vec<_> = tracks_preferring(&ts, Some(&a)).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["a1", "a2", "b1", "b2"]);
        let n = |v: Vec<ArtistItem>| v.into_iter().map(|a| a.name).collect::<Vec<_>>();
        assert_eq!(n(artists_with_top(&[b.clone(), a.clone()], Some(&a))), ["A", "B"]);
        assert_eq!(n(artists_with_top(&[b.clone()], Some(&a))), ["A", "B"]);
    }

    #[test]
    fn no_results() {
        assert_eq!(top("x", &SearchResults::default()), None);
    }

    #[test]
    fn clean_title_strips_feat_and_remaster() {
        assert_eq!(clean_title("Song (feat. X) - 2011 Remaster"), "Song");
        assert_eq!(clean_title("Song [Remastered 2011]"), "Song");
        assert_eq!(clean_title("Song (Live)"), "Song (Live)");
    }
}
