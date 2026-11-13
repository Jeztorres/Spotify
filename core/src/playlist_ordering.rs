//! Filtro y orden de las canciones de una playlist (portado de `PlaylistOrdering.swift`).

use crate::models::Track;
use crate::text::{standard_compare, standard_contains};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaylistSort {
    #[default]
    Original,
    Title,
    Artist,
    Album,
    Duration,
}

impl PlaylistSort {
    pub const ALL: [PlaylistSort; 5] =
        [PlaylistSort::Original, PlaylistSort::Title, PlaylistSort::Artist, PlaylistSort::Album, PlaylistSort::Duration];

    pub fn label(self) -> &'static str {
        match self {
            PlaylistSort::Original => "Orden original",
            PlaylistSort::Title => "Título",
            PlaylistSort::Artist => "Artista",
            PlaylistSort::Album => "Álbum",
            PlaylistSort::Duration => "Duración",
        }
    }
}

/// Devuelve una copia filtrada y ordenada; la lista original no cambia. Los empates conservan el orden original.
pub fn apply(tracks: &[Track], query: &str, sort: PlaylistSort, ascending: bool) -> Vec<Track> {
    let query = query.trim();
    let mut filtered: Vec<(usize, &Track)> = tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            query.is_empty()
                || [t.name.as_str(), &t.artist_line(), t.album.as_ref().map_or("", |a| a.name.as_str())]
                    .iter()
                    .any(|field| standard_contains(field, query))
        })
        .collect();

    if sort == PlaylistSort::Original {
        if !ascending {
            filtered.reverse();
        }
        return filtered.into_iter().map(|(_, t)| t.clone()).collect();
    }

    let album = |t: &Track| t.album.as_ref().map(|a| a.name.clone()).unwrap_or_default();
    filtered.sort_by(|(li, l), (ri, r)| {
        let result = match sort {
            PlaylistSort::Title => standard_compare(&l.name, &r.name),
            PlaylistSort::Artist => standard_compare(&l.artist_line(), &r.artist_line()),
            PlaylistSort::Album => standard_compare(&album(l), &album(r)),
            PlaylistSort::Duration => l.duration_ms.cmp(&r.duration_ms),
            PlaylistSort::Original => Ordering::Equal,
        };
        if result == Ordering::Equal {
            return li.cmp(ri); // estable: los empates mantienen su posición original
        }
        if ascending { result } else { result.reverse() }
    });
    filtered.into_iter().map(|(_, t)| t.clone()).collect()
}

/// Posiciones originales de las filas visibles, incluso si hay canciones duplicadas.
pub fn indices(tracks: &[Track], query: &str, sort: PlaylistSort, ascending: bool) -> Vec<usize> {
    use std::collections::{HashMap, VecDeque};
    if sort == PlaylistSort::Original {
        let query = query.trim();
        let mut positions: Vec<_> = tracks.iter().enumerate().filter(|(_, t)| query.is_empty()
            || [t.name.as_str(), &t.artist_line(), t.album.as_ref().map_or("", |a| a.name.as_str())].iter()
                .any(|field| standard_contains(field, query))).map(|(i,_)| i).collect();
        if !ascending { positions.reverse(); }
        return positions;
    }
    let mut positions: HashMap<&Track, VecDeque<usize>> = HashMap::new();
    for (i, track) in tracks.iter().enumerate() { positions.entry(track).or_default().push_back(i); }
    apply(tracks, query, sort, ascending).iter().filter_map(|track| positions.get_mut(track)?.pop_front()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SpAlbum, SpArtist};

    fn track(title: &str, artist: &str, album: &str, duration: i64) -> Track {
        Track {
            id: Some(title.into()),
            uri: format!("spotify:track:{title}"),
            name: title.into(),
            artists: vec![SpArtist { name: artist.into(), id: None }],
            album: Some(SpAlbum { name: album.into(), images: None, id: None, uri: None }),
            duration_ms: duration,
            explicit: None,
        }
    }

    fn names(v: &[Track]) -> Vec<&str> {
        v.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn search_and_ordering_preserve_original_list() {
        let source = vec![
            track("Canción", "Zoé", "Segundo", 180),
            track("Otra", "Banda", "Primero", 240),
            track("Alma", "Zoé", "Primero", 120),
        ];
        assert_eq!(names(&apply(&source, "zoe", PlaylistSort::Title, true)), ["Alma", "Canción"]);
        assert_eq!(names(&apply(&source, "", PlaylistSort::Duration, false)), ["Otra", "Canción", "Alma"]);
        assert_eq!(names(&apply(&source, "primero", PlaylistSort::Original, true)), ["Otra", "Alma"]);
        assert_eq!(names(&source), ["Canción", "Otra", "Alma"]);
    }

    #[test]
    fn original_descending_reverses() {
        let source = vec![track("a", "x", "y", 1), track("b", "x", "y", 2)];
        assert_eq!(names(&apply(&source, "", PlaylistSort::Original, false)), ["b", "a"]);
    }

    #[test]
    fn ties_keep_original_order_even_descending() {
        let source = vec![track("a", "x", "y", 5), track("b", "x", "y", 5)];
        assert_eq!(names(&apply(&source, "", PlaylistSort::Duration, false)), ["a", "b"]);
    }
    #[test]
    fn visible_indices_keep_original_positions_for_playback_and_actions() {
        let a = track("Zulu", "Zoé", "Uno", 200);
        let b = track("Alma", "Banda", "Dos", 100);
        let source = vec![a.clone(), b.clone(), a.clone()];
        assert_eq!(indices(&source, "", PlaylistSort::Title, true), [1, 0, 2]);
        assert_eq!(indices(&source, "zoe", PlaylistSort::Original, true), [0, 2]);
        assert!(indices(&source, "missing", PlaylistSort::Original, true).is_empty());
        assert_eq!(indices(&source, "", PlaylistSort::Original, false), [2,1,0]);
    }

}
