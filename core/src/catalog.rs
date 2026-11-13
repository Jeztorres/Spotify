//! Selección de catálogo: autores reales del álbum, fijados y recomendaciones sin duplicados.
use crate::{AlbumItem, SpArtist, Track};
use std::collections::HashSet;

pub fn primary_artist(artists: &[SpArtist]) -> String {
    artists.first().map(|a| a.name.clone()).unwrap_or_default()
}
pub fn is_collection(uri: &str) -> bool {
    uri.ends_with(":collection") || uri == "spotify:collection:tracks"
}
pub fn pin_position(pins: &[String], uri: &str) -> Option<usize> {
    pins.iter()
        .position(|p| p == uri || is_collection(p) && is_collection(uri))
}
pub fn recommendations(candidates: &[Track], existing: &[Track]) -> Vec<Track> {
    let mut seen: HashSet<&str> = existing.iter().map(|t| t.uri.as_str()).collect();
    candidates
        .iter()
        .filter(|t| seen.insert(t.uri.as_str()))
        .cloned()
        .collect()
}
pub fn releases(items: &[AlbumItem], group: &str, excluded: Option<&str>) -> Vec<AlbumItem> {
    let mut seen = HashSet::new();
    let mut out: Vec<_> = items
        .iter()
        .filter(|a| {
            Some(a.id.as_str()) != excluded
                && seen.insert(a.id.clone())
                && match group {
                    "album" => a.album_group.as_deref().unwrap_or("album") == "album",
                    "single" => a.album_group.as_deref() == Some("single"),
                    "appears_on" => {
                        matches!(a.album_group.as_deref(), Some("appears_on" | "compilation"))
                    }
                    _ => true,
                }
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| b.release_date.cmp(&a.release_date));
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn album_header_uses_the_album_author_not_the_track_guest_list() {
        let album = vec![SpArtist {
            name: "Album artist".into(),
            id: Some("main".into()),
        }];
        let track = vec![
            album[0].clone(),
            SpArtist {
                name: "Guest".into(),
                id: None,
            },
        ];
        assert_eq!(primary_artist(&album), "Album artist");
        assert_eq!(primary_artist(&track), "Album artist");
        assert_eq!(primary_artist(&[]), "");
    }
    #[test]
    fn pinned_liked_uris_are_equivalent() {
        let pins = vec![
            "spotify:playlist:first".into(),
            "spotify:user:me:collection".into(),
        ];
        assert_eq!(pin_position(&pins, "spotify:collection:tracks"), Some(1));
        assert_eq!(pin_position(&pins, "spotify:playlist:first"), Some(0));
        assert_eq!(pin_position(&pins, "spotify:playlist:other"), None);
    }
    #[test]
    fn added_songs_disappear_from_recommendations() {
        let t = |id: &str| Track {
            id: Some(id.into()),
            uri: format!("spotify:track:{id}"),
            name: id.into(),
            artists: vec![],
            album: None,
            duration_ms: 1,
            explicit: None,
        };
        assert_eq!(
            recommendations(&[t("old"), t("new"), t("new")], &[t("old")]).len(),
            1
        );
        assert_eq!(
            recommendations(&[t("old"), t("new")], &[t("old"), t("new")]).len(),
            0
        );
    }
}
