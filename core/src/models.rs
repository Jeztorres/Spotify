//! Modelos de datos (portados de `Models.swift`). El JSON de Spotify usa `snake_case`.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpImage {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpArtist {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpAlbum {
    pub name: String,
    #[serde(default)]
    pub images: Option<Vec<SpImage>>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
}

fn first_image(images: &Option<Vec<SpImage>>) -> Option<&str> {
    images.as_ref()?.first().map(|i| i.url.as_str())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Track {
    #[serde(default)]
    pub id: Option<String>,
    pub uri: String,
    pub name: String,
    pub artists: Vec<SpArtist>,
    #[serde(default)]
    pub album: Option<SpAlbum>,
    pub duration_ms: i64,
    /// Contenido explícito (lo marca Spotify).
    #[serde(default)]
    pub explicit: Option<bool>,
}

impl Track {
    pub fn identity(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.uri)
    }
    pub fn artist_line(&self) -> String {
        self.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
    }
    pub fn image_url(&self) -> Option<&str> {
        first_image(&self.album.as_ref()?.images)
    }
}

/// Una canción de la cola. `uid` la identifica dentro de la cola (puede repetirse la misma canción).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QueueItem {
    pub uid: String,
    /// Añadida a mano a la cola.
    pub queued: bool,
    /// La puso el Autoplay de Spotify.
    #[serde(default)]
    pub autoplay: Option<bool>,
    pub track: Track,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpOwner {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Profile {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub images: Option<Vec<SpImage>>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub owner: Option<SpOwner>,
    /// Nombre visible del creador si el perfil no da su identificador.
    #[serde(default)]
    pub creator_display_name: Option<String>,
    /// Para álbumes: artista principal.
    #[serde(default)]
    pub artist_id: Option<String>,
    #[serde(default)]
    pub artist_name: Option<String>,
}

impl Playlist {
    pub fn new(id: &str, uri: &str, name: &str) -> Self {
        Playlist {
            id: id.into(),
            uri: uri.into(),
            name: name.into(),
            images: None,
            description: None,
            owner: None,
            creator_display_name: None,
            artist_id: None,
            artist_name: None,
        }
    }

    pub fn image_url(&self) -> Option<&str> {
        first_image(&self.images)
    }

    /// Descripción sin etiquetas HTML ni entidades.
    pub fn plain_description(&self) -> Option<String> {
        static TAGS: OnceLock<Regex> = OnceLock::new();
        let d = self.description.as_deref().filter(|d| !d.is_empty())?;
        let tags = TAGS.get_or_init(|| Regex::new("<[^>]+>").unwrap());
        let mut d = tags.replace_all(d, "").into_owned();
        for (a, b) in [("&amp;", "&"), ("&#x27;", "'"), ("&quot;", "\""), ("&lt;", "<"), ("&gt;", ">")] {
            d = d.replace(a, b);
        }
        static ENTITIES: OnceLock<Regex> = OnceLock::new();
        let entities = ENTITIES.get_or_init(|| Regex::new(r"&#(x[0-9a-fA-F]+|[0-9]+);").unwrap());
        d = entities.replace_all(&d, |c: &regex::Captures<'_>| {
            let value = &c[1];
            let number = if let Some(hex) = value.strip_prefix('x') { u32::from_str_radix(hex, 16) } else { value.parse::<u32>() };
            number.ok().and_then(char::from_u32).map(|c| c.to_string()).unwrap_or_else(|| c[0].to_owned())
        }).into_owned();
        (!d.is_empty()).then_some(d)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(default)]
    pub total: Option<i64>,
    #[serde(default)]
    pub next: Option<String>,
}

/// Respuesta de `home_sections` del motor (claves snake_case).
#[derive(Debug, Clone, Deserialize)]
pub struct HomeSections {
    pub made_for_you: Vec<Playlist>,
    pub top_mixes: Vec<Playlist>,
}

/// Datos de formato de una playlist. Las editoriales («This Is»…) traen una imagen ancha de cabecera.
/// Las claves se comparan sin guiones bajos ni mayúsculas.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaylistHeader {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub attributes: HashMap<String, String>,
    #[serde(default)]
    pub owner: Option<SpOwner>,
}

impl PlaylistHeader {
    pub fn image_url(&self) -> Option<String> {
        let norm = |k: &str| k.to_lowercase().replace('_', "");
        let mut by_key: HashMap<String, &String> = HashMap::new();
        for (k, v) in &self.attributes {
            by_key.entry(norm(k)).or_insert(v);
        }
        let value = ["headerimageurldesktop", "headerimageurl", "headerimage"]
            .iter()
            .find_map(|k| by_key.get(*k).copied())
            .or_else(|| {
                by_key
                    .iter()
                    .find(|(k, v)| k.contains("headerimage") && v.starts_with("http"))
                    .map(|(_, v)| *v)
            })?;
        (value.starts_with("https://") || value.starts_with("http://")).then(|| value.clone())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtistOverview {
    #[serde(default)]
    pub featuring: Vec<Playlist>,
    #[serde(default)]
    pub discovered_on: Vec<Playlist>,
    #[serde(default)]
    pub artist_playlists: Vec<Playlist>,
    #[serde(default)]
    pub appears_on: Vec<AlbumItem>,
    #[serde(default)]
    pub top_playcounts: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistItem {
    #[serde(default)]
    pub track: Option<Track>,
    #[serde(default)]
    pub item: Option<Track>,
}

impl PlaylistItem {
    pub fn resolved(&self) -> Option<&Track> {
        self.track.as_ref().or(self.item.as_ref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedTrack {
    pub track: Track,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtistItem {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub images: Option<Vec<SpImage>>,
    #[serde(default)]
    pub popularity: Option<i32>,
}

impl ArtistItem {
    pub fn new(id: &str, name: &str) -> Self {
        ArtistItem { id: id.into(), uri: format!("spotify:artist:{id}"), name: name.into(), images: None, popularity: None }
    }
    pub fn image_url(&self) -> Option<&str> {
        first_image(&self.images)
    }
    pub fn as_playlist(&self) -> Playlist {
        Playlist {
            images: self.images.clone(),
            description: Some("Canciones populares".into()),
            ..Playlist::new(&self.id, &self.uri, &self.name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AlbumItem {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub images: Option<Vec<SpImage>>,
    #[serde(default)]
    pub artists: Option<Vec<SpArtist>>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub album_group: Option<String>,
    #[serde(default)]
    pub album_type: Option<String>,
}

impl AlbumItem {
    pub fn image_url(&self) -> Option<&str> {
        first_image(&self.images)
    }
    pub fn artist_line(&self) -> String {
        self.artists.iter().flatten().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
    }
    pub fn as_playlist(&self) -> Playlist {
        let year: String = self.release_date.as_deref().unwrap_or("").chars().take(4).collect();
        let desc = [self.artist_line(), year].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        let first = self.artists.as_ref().and_then(|a| a.first());
        Playlist {
            images: self.images.clone(),
            description: Some(desc),
            artist_id: first.and_then(|a| a.id.clone()),
            artist_name: first.map(|a| a.name.clone()),
            ..Playlist::new(&self.id, &self.uri, &self.name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Followers {
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistDetail {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub followers: Option<Followers>,
    #[serde(default)]
    pub genres: Option<Vec<String>>,
    #[serde(default)]
    pub images: Option<Vec<SpImage>>,
    #[serde(default)]
    pub popularity: Option<i32>,
}

impl ArtistDetail {
    pub fn image_url(&self) -> Option<&str> {
        first_image(&self.images)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopTracksResponse {
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub artists: Vec<ArtistItem>,
    pub albums: Vec<AlbumItem>,
    pub playlists: Vec<Playlist>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.artists.is_empty() && self.albums.is_empty() && self.playlists.is_empty()
    }

    /// Spotify no permite buscar usuarios: los perfiles salen de los dueños de las playlists encontradas.
    pub fn profiles(&self) -> Vec<Profile> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for owner in self.playlists.iter().filter_map(|p| p.owner.as_ref()) {
            if seen.insert(owner.id.clone()) {
                out.push(Profile { id: owner.id.clone(), name: owner.display_name.clone().unwrap_or_else(|| owner.id.clone()) });
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub tracks: Option<Page<Option<Track>>>,
    #[serde(default)]
    pub artists: Option<Page<Option<ArtistItem>>>,
    #[serde(default)]
    pub albums: Option<Page<Option<AlbumItem>>>,
    #[serde(default)]
    pub playlists: Option<Page<Option<Playlist>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Copyright {
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlbumDetail {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub total_tracks: Option<i32>,
    #[serde(default)]
    pub album_type: Option<String>,
    #[serde(default)]
    pub copyrights: Option<Vec<Copyright>>,
    #[serde(default)]
    pub genres: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDetail {
    #[serde(default)]
    pub images: Option<Vec<SpImage>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub volume_percent: Option<i32>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub is_restricted: Option<bool>,
    #[serde(default)]
    pub supports_volume: Option<bool>,
}

/// Tipo de icono para el dispositivo (la UI lo asigna a su propio icono, sin SF Symbols).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceIcon {
    Computer,
    Smartphone,
    Tablet,
    Speaker,
    SmartSpeaker,
    Tv,
    Receiver,
    Cast,
    GameConsole,
    Car,
    Watch,
    Generic,
}

impl Device {
    pub fn identity(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.name)
    }

    pub fn icon(&self) -> DeviceIcon {
        match self.kind.as_deref().unwrap_or("").to_lowercase().as_str() {
            "computer" => DeviceIcon::Computer,
            "smartphone" => DeviceIcon::Smartphone,
            "tablet" => DeviceIcon::Tablet,
            "speaker" => {
                let n = self.name.to_lowercase();
                if n.contains("echo") || n.contains("alexa") { DeviceIcon::SmartSpeaker } else { DeviceIcon::Speaker }
            }
            "tv" => DeviceIcon::Tv,
            "avr" | "stb" => DeviceIcon::Receiver,
            "audiodongle" | "castaudio" | "castvideo" => DeviceIcon::Cast,
            "gameconsole" => DeviceIcon::GameConsole,
            "automobile" => DeviceIcon::Car,
            "smartwatch" => DeviceIcon::Watch,
            _ => DeviceIcon::Generic,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicesResponse {
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackState {
    pub is_playing: bool,
    #[serde(default)]
    pub progress_ms: Option<i64>,
    #[serde(default)]
    pub shuffle_state: Option<bool>,
    #[serde(default)]
    pub repeat_state: Option<String>,
    #[serde(default)]
    pub item: Option<Track>,
    #[serde(default)]
    pub device: Option<Device>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_decodes_spotify_json() {
        let t: Track = serde_json::from_str(
            r#"{"id":"1","uri":"spotify:track:1","name":"N","artists":[{"name":"A","id":"a"}],
                "album":{"name":"Al","images":[{"url":"https://i/1"}]},"duration_ms":183000}"#,
        )
        .unwrap();
        assert_eq!(t.image_url(), Some("https://i/1"));
        assert_eq!(t.artist_line(), "A");
        assert_eq!(t.identity(), "1");
    }

    #[test]
    fn plain_description_strips_html() {
        let mut p = Playlist::new("1", "u", "n");
        p.description = Some("<a href=\"x\">Rock &amp; Roll</a> &quot;hits&quot; &#34;M&#xfa;sica&#34;".into());
        assert_eq!(p.plain_description().as_deref(), Some("Rock & Roll \"hits\" \"Música\""));
    }

    #[test]
    fn header_image_prefers_desktop_key_and_requires_http() {
        let mut h = PlaylistHeader::default();
        h.attributes.insert("header_image_url".into(), "https://a/b".into());
        h.attributes.insert("header_image_url_desktop".into(), "https://a/desktop".into());
        assert_eq!(h.image_url().as_deref(), Some("https://a/desktop"));

        let mut bad = PlaylistHeader::default();
        bad.attributes.insert("headerimage".into(), "ftp://x".into());
        assert_eq!(bad.image_url(), None);
    }

    #[test]
    fn profiles_come_from_unique_playlist_owners() {
        let mut p1 = Playlist::new("1", "u1", "a");
        p1.owner = Some(SpOwner { id: "o".into(), display_name: Some("Ana".into()) });
        let mut p2 = Playlist::new("2", "u2", "b");
        p2.owner = Some(SpOwner { id: "o".into(), display_name: None });
        let r = SearchResults { playlists: vec![p1, p2], ..Default::default() };
        assert_eq!(r.profiles(), vec![Profile { id: "o".into(), name: "Ana".into() }]);
    }

    #[test]
    fn album_as_playlist_builds_description() {
        let a = AlbumItem {
            id: "1".into(),
            uri: "u".into(),
            name: "Al".into(),
            images: None,
            artists: Some(vec![SpArtist { name: "Zoé".into(), id: Some("z".into()) }]),
            release_date: Some("2013-05-01".into()),
            album_group: None,
            album_type: None,
        };
        let p = a.as_playlist();
        assert_eq!(p.description.as_deref(), Some("Zoé · 2013"));
        assert_eq!(p.artist_id.as_deref(), Some("z"));
    }

    #[test]
    fn device_icon_by_type() {
        let d = |kind: &str, name: &str| Device {
            id: None,
            name: name.into(),
            volume_percent: None,
            kind: Some(kind.into()),
            is_active: None,
            is_restricted: None,
            supports_volume: None,
        };
        assert_eq!(d("Speaker", "Echo Dot").icon(), DeviceIcon::SmartSpeaker);
        assert_eq!(d("Speaker", "Sonos").icon(), DeviceIcon::Speaker);
        assert_eq!(d("???", "x").icon(), DeviceIcon::Generic);
    }
}
