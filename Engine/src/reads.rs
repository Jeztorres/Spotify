//! Lecturas de catálogo por la sesión de streaming, con la forma que ya decodifica la app
//! (la de Web API). Adaptado de `session_reads.rs` de Spotifast/Fastpotify (MIT).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use librespot_core::{session::Session, spotify_id::SpotifyId, SpotifyUri};
use librespot_metadata::image::Images;
use librespot_metadata::playlist::attribute::PlaylistAttributes;
use librespot_metadata::{Album, Artist, Episode, Metadata, Playlist, Show, Track};
use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
use librespot_protocol::extension_kind::ExtensionKind;
use librespot_protocol::playlist4_external::SelectedListContent;
use protobuf::{EnumOrUnknown, Message};
use serde_json::{json, Value};

const IMAGE_HOST: &str = "https://i.scdn.co/image/";
/// Hash de la consulta persistida que usa el propio cliente de Spotify (no es una credencial).
const RELATED_ARTISTS_HASH: &str = "3d031d6cb22a2aa7c8d203d49b49df731f58b1e2799cc38d9876d58771aa66f3";
/// Consulta persistida de la página del artista del reproductor web (`queryArtistOverview`).
// Secciones personalizadas del Inicio de Spotify (consulta `homeSection`, como Psst y Fastpotify, MIT).
const HOME_SECTION_HASH: &str = "eb3fba2d388cf4fc4d696b1757a58584e9538a3b515ea742e9cc9465807340be";
const MADE_FOR_YOU_SECTION: &str = "spotify:section:0JQ5DAUnp4wcj0bCb3wh3S";
const TOP_MIXES_SECTION: &str = "spotify:section:0JQ5DAnM3wGh0gz1MXnu89";
/// Spotify DJ no se puede reproducir como playlist.
const DJ_PLAYLIST: &str = "spotify:playlist:37i9dQZF1EYkqdzj48dyYq";
const ARTIST_OVERVIEW_HASH: &str = "9f8134ef565e78621f1e1793555bd6633c5ac144ae0f89604ed3ae3f80b3c8e6";

fn uri_of(u: &SpotifyUri) -> String { u.to_uri() }
fn id_of(u: &SpotifyUri) -> Option<String> { Some(u.to_id()) }

fn images_json(images: &Images) -> Vec<Value> {
    let mut list: Vec<_> = images.iter().collect();
    list.sort_by_key(|i| -(i.width as i64));
    list.iter().map(|i| json!({"url": format!("{IMAGE_HOST}{}", i.id)})).collect()
}

fn artist_ref(a: &Artist) -> Value { json!({"name": a.name, "id": id_of(&a.id)}) }

fn album_json(a: &Album) -> Value {
    json!({"name": a.name, "id": id_of(&a.id), "uri": uri_of(&a.id), "images": images_json(&a.covers)})
}

fn track_json(t: &Track) -> Value {
    json!({
        "id": id_of(&t.id), "uri": uri_of(&t.id), "name": t.name,
        "artists": t.artists.iter().map(artist_ref).collect::<Vec<_>>(),
        "album": album_json(&t.album),
        "duration_ms": t.duration.max(0),
        "explicit": t.is_explicit,
    })
}

/// Cliente HTTP compartido (mantiene conexiones TLS/HTTP2 calientes hacia api-partner).
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

async fn batch_chunk<T>(session: Session, chunk: Vec<String>, kind: ExtensionKind) -> Result<Vec<(String, T)>>
where
    T: Metadata,
    for<'a> T: TryFrom<&'a T::Message>,
    T::Message: Message,
{
    let mut request = BatchedEntityRequest::new();
    let mut seen = BTreeSet::new();
    for uri in &chunk {
        if !seen.insert(uri.clone()) { continue; }
        request.entity_request.push(EntityRequest {
            entity_uri: uri.clone(),
            query: vec![ExtensionQuery { extension_kind: EnumOrUnknown::new(kind), ..Default::default() }],
            ..Default::default()
        });
    }
    if request.entity_request.is_empty() { return Ok(Vec::new()); }
    let response = session.spclient().get_extended_metadata(request).await?;
    let mut out = Vec::new();
    for array in response.extended_metadata {
        for data in array.extension_data {
            if !matches!(data.header.status_code, 0 | 200) { continue; }
            let Some(any) = data.extension_data.as_ref() else { continue };
            let Ok(message) = T::Message::parse_from_bytes(&any.value) else { continue };
            let parsed = T::try_from(&message).ok();
            if let Some(item) = parsed {
                out.push((data.entity_uri.clone(), item));
            }
        }
    }
    Ok(out)
}

/// Detalles de muchas entidades en lotes concurrentes `extended-metadata`.
async fn batch<T>(session: &Session, uris: &[String], kind: ExtensionKind) -> Result<HashMap<String, T>>
where
    T: Metadata + Send + 'static,
    for<'a> T: TryFrom<&'a T::Message>,
    T::Message: Message,
{
    let chunks: Vec<Vec<String>> = uris.chunks(100).map(|c| c.to_vec()).collect();
    if chunks.is_empty() { return Ok(HashMap::new()); }
    if chunks.len() == 1 {
        return Ok(batch_chunk(session.clone(), chunks.into_iter().next().unwrap(), kind).await?.into_iter().collect());
    }
    let mut set = tokio::task::JoinSet::new();
    for chunk in chunks {
        let session = session.clone();
        set.spawn(async move { batch_chunk::<T>(session, chunk, kind).await });
    }
    let mut out = HashMap::new();
    let mut first_err = None;
    while let Some(r) = set.join_next().await {
        match r {
            Ok(Ok(items)) => out.extend(items),
            Ok(Err(e)) => { first_err.get_or_insert(e); }
            Err(e) => { first_err.get_or_insert(anyhow!(e)); }
        }
    }
    if out.is_empty() { if let Some(e) = first_err { return Err(e); } }
    Ok(out)
}

/// Fichas de canciones ya pedidas (JSON listo), para no volver a pedirlas al abrir otra vez una lista.
static TRACK_CACHE: Mutex<Option<HashMap<String, Value>>> = Mutex::new(None);
const TRACK_CACHE_MAX: usize = 30_000;

/// Fichas de canciones por URI: primero la memoria; lo que falte, en lotes de 100 pedidos a la vez.
async fn track_values(session: &Session, uris: &[String]) -> Result<HashMap<String, Value>> {
    let mut out = HashMap::new();
    let mut missing: Vec<String> = Vec::new();
    {
        let guard = TRACK_CACHE.lock().unwrap();
        let cache = guard.as_ref();
        let mut seen = BTreeSet::new();
        for u in uris {
            if !seen.insert(u.as_str()) { continue; }
            match cache.and_then(|c| c.get(u)) {
                Some(v) => { out.insert(u.clone(), v.clone()); }
                None if u.starts_with("spotify:track:") => missing.push(u.clone()),
                None => {}
            }
        }
    }
    if missing.is_empty() { return Ok(out); }
    let mut set = tokio::task::JoinSet::new();
    for chunk in missing.chunks(100) {
        let session = session.clone();
        let chunk = chunk.to_vec();
        set.spawn(async move {
            let found: Result<HashMap<String, Track>> = batch(&session, &chunk, ExtensionKind::TRACK_V4).await;
            found.map(|m| m.iter().map(|(u, t)| (u.clone(), track_json(t))).collect::<Vec<_>>())
        });
    }
    let mut fetched = Vec::new();
    let mut first_err = None;
    while let Some(r) = set.join_next().await {
        match r {
            Ok(Ok(list)) => fetched.extend(list),
            Ok(Err(e)) => { first_err.get_or_insert(e); }
            Err(e) => { first_err.get_or_insert(anyhow!(e)); }
        }
    }
    if fetched.is_empty() { if let Some(e) = first_err { return Err(e); } }
    let mut guard = TRACK_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if cache.len() + fetched.len() > TRACK_CACHE_MAX { cache.clear(); }
    for (u, v) in fetched { cache.insert(u.clone(), v.clone()); out.insert(u, v); }
    Ok(out)
}

async fn tracks(session: &Session, uris: &[String]) -> Result<Vec<Value>> {
    let found = track_values(session, uris).await?;
    Ok(uris.iter().filter_map(|u| found.get(u).cloned()).collect())
}

pub async fn queue_tracks(session: &Session, uris: &[String]) -> Result<Vec<Value>> {
    if uris.is_empty() { return Ok(Vec::new()); }
    tracks(session, uris).await
}

// ---- Biblioteca de playlists (rootlist) ----

pub async fn library(session: &Session) -> Result<Value> {
    let mut playlists = Vec::new();
    let mut from = 0usize;
    loop {
        let bytes = session.spclient().get_rootlist(from, Some(500)).await?;
        let response = SelectedListContent::parse_from_bytes(&bytes)?;
        let Some(contents) = response.contents.into_option() else { break };
        let count = contents.items.len();
        for (item, meta) in contents.items.iter().zip(contents.meta_items.iter()) {
            let Some(id) = item.uri().strip_prefix("spotify:playlist:") else { continue };
            if !matches!(meta.status_code(), 0 | 200) { continue; }
            let Ok(attrs) = PlaylistAttributes::try_from(&*meta.attributes) else { continue };
            let owner = meta.owner_username().to_string();
            playlists.push(json!({
                "id": id, "uri": item.uri(), "name": attrs.name,
                "description": (!attrs.description.is_empty()).then_some(attrs.description.clone()),
                "images": playlist_images(&attrs),
                "owner": {"id": owner, "display_name": Value::Null},
            }));
        }
        if !contents.truncated() || count == 0 { break; }
        from += count;
    }
    // Nombres de los dueños y portadas de mosaico, a la vez y recordados entre llamadas
    // (antes eran hasta 5 s + 12 s seguidos cada vez que se cargaba la biblioteca).
    let owners: BTreeSet<String> = playlists.iter()
        .filter_map(|p| p["owner"]["id"].as_str().map(String::from))
        .filter(|o| !o.is_empty() && o != "spotify").take(80).collect();
    let missing: Vec<(usize, String)> = playlists.iter().enumerate()
        .filter(|(_, p)| p["images"].as_array().map_or(true, |a| a.is_empty()))
        .filter_map(|(i, p)| p["id"].as_str().map(|id| (i, id.to_string()))).take(120).collect();

    let mut owner_set = tokio::task::JoinSet::new();
    for owner in owners {
        if let Some(name) = OWNER_NAMES.lock().unwrap().get_or_insert_with(HashMap::new).get(&owner).cloned() {
            owner_set.spawn(async move { (owner, Some(name)) });
            continue;
        }
        let session = session.clone();
        owner_set.spawn(async move {
            let name = session.spclient().get_user_profile(&owner, Some(0), Some(0)).await.ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|v| v.get("name").and_then(Value::as_str).map(String::from));
            (owner, name)
        });
    }
    let mut cover_set = tokio::task::JoinSet::new();
    for (i, id) in missing {
        if let Some(url) = COVERS.lock().unwrap().get_or_insert_with(HashMap::new).get(&id).cloned() {
            playlists[i]["images"] = json!([{"url": url}]);
            continue;
        }
        let session = session.clone();
        cover_set.spawn(async move { (i, id.clone(), playlist_cover(&session, &id).await) });
    }
    let mut names = HashMap::new();
    names.insert("spotify".to_string(), "Spotify".to_string());
    let _ = tokio::time::timeout(Duration::from_secs(6), async {
        let owners_done = async {
            while let Some(Ok((owner, name))) = owner_set.join_next().await {
                if let Some(name) = name {
                    OWNER_NAMES.lock().unwrap().get_or_insert_with(HashMap::new).insert(owner.clone(), name.clone());
                    names.insert(owner, name);
                }
            }
        };
        let covers_done = async {
            while let Some(Ok((i, id, cover))) = cover_set.join_next().await {
                if let Some(url) = cover {
                    COVERS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, url.clone());
                    playlists[i]["images"] = json!([{"url": url}]);
                }
            }
        };
        tokio::join!(owners_done, covers_done);
    }).await;
    for p in &mut playlists {
        if let Some(name) = p["owner"]["id"].as_str().and_then(|o| names.get(o)).cloned() {
            p["owner"]["display_name"] = json!(name);
        }
    }
    Ok(json!({"items": playlists, "total": playlists.len(), "next": Value::Null}))
}

static OWNER_NAMES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static COVERS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Precarga al conectar: «Tus me gusta» y sus primeras fichas, para que abrir la lista sea inmediato.
pub async fn warmup(session: &Session) -> Result<Value> {
    let s = session.clone();
    tokio::spawn(async move {
        if let Ok(all) = liked_uris(&s, false).await {
            let first: Vec<String> = all.into_iter().take(200).collect();
            let _ = track_values(&s, &first).await;
        }
    });
    Ok(Value::Null)
}

/// Portada de una playlist sin imagen propia: mosaico de 4 álbumes distintos, o la portada del primero.
async fn playlist_cover(session: &Session, id: &str) -> Option<String> {
    let page = playlist_items(session, id, 0, 30).await.ok()?;
    let covers: Vec<String> = page["items"].as_array()?.iter()
        .filter_map(|i| i["track"]["album"]["images"][0]["url"].as_str().map(String::from))
        .collect();
    mosaic_url(&covers)
}

fn mosaic_url(covers: &[String]) -> Option<String> {
    let mut seen = BTreeSet::new();
    let distinct: Vec<&String> = covers.iter().filter(|u| seen.insert(u.as_str())).collect();
    if distinct.len() >= 4 {
        let ids: Option<Vec<&str>> = distinct[..4].iter().map(|u| u.strip_prefix(IMAGE_HOST)).collect();
        if let Some(ids) = ids { return Some(format!("https://mosaic.scdn.co/640/{}", ids.concat())); }
    }
    distinct.first().map(|u| u.to_string())
}

fn playlist_images(attrs: &PlaylistAttributes) -> Vec<Value> {
    let mut sized: Vec<(u32, String)> = attrs.picture_sizes.iter().filter_map(|p| {
        let url = if p.url.starts_with("http") { p.url.clone() }
            else { format!("{IMAGE_HOST}{}", p.url.strip_prefix("spotify:image:")?) };
        let w = match p.target_name.as_str() { "large" => 640, "default" => 300, "small" => 60, _ => 0 };
        Some((w, url))
    }).collect();
    sized.sort_by_key(|(w, _)| std::cmp::Reverse(*w));
    if !sized.is_empty() { return sized.into_iter().map(|(_, url)| json!({"url": url})).collect(); }
    if attrs.picture.is_empty() { return vec![]; }
    let hex: String = attrs.picture.iter().map(|b| format!("{b:02x}")).collect();
    vec![json!({"url": format!("{IMAGE_HOST}{hex}")})]
}

// ---- Canciones de una playlist ----

/// La lista completa de URIs se guarda 5 min en memoria para paginar sin volver a pedirla.
static PLAYLISTS: Mutex<Option<HashMap<String, (Instant, Vec<String>)>>> = Mutex::new(None);

/// Descarta únicamente la lista modificada; la siguiente lectura obtiene la revisión nueva.
pub fn forget_playlist(id: &str) {
    if let Some(cache) = PLAYLISTS.lock().unwrap().as_mut() { cache.remove(id); }
}

pub async fn playlist_items(session: &Session, id: &str, offset: usize, limit: usize) -> Result<Value> {
    let cached = PLAYLISTS.lock().unwrap().as_ref()
        .and_then(|m| m.get(id).filter(|(t, _)| t.elapsed() < Duration::from_secs(300)).map(|(_, l)| l.clone()));
    let all = match cached {
        Some(list) => list,
        None => {
            let uri = SpotifyUri::Playlist { id: SpotifyId::from_base62(id)?, user: None };
            let list = Playlist::get(session, &uri).await?;
            let uris: Vec<String> = list.contents.items.iter().map(|i| uri_of(&i.id)).collect();
            PLAYLISTS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id.to_string(), (Instant::now(), uris.clone()));
            uris
        }
    };
    let page: Vec<String> = all.iter().skip(offset).take(limit).cloned().collect();
    let found = track_values(session, &page).await?;
    // Las filas se conservan aunque falte una canción, como hace Web API.
    let items: Vec<Value> = page.iter().map(|u| json!({"track": found.get(u).cloned()})).collect();
    let more = offset + page.len() < all.len();
    Ok(json!({"items": items, "total": all.len(), "next": if more { json!("session") } else { Value::Null }}))
}

// ---- Podcasts ----

static SHOWS: Mutex<Option<HashMap<String, (Instant, Arc<Show>)>>> = Mutex::new(None);

async fn show_cached(session: &Session, id: &str) -> Result<Arc<Show>> {
    if let Some(sh) = SHOWS.lock().unwrap().as_ref().and_then(|m| m.get(id))
        .filter(|(t, _)| t.elapsed() < Duration::from_secs(300)).map(|(_, s)| s.clone()) {
        return Ok(sh);
    }
    let uri = SpotifyUri::Show { id: SpotifyId::from_base62(id)? };
    let sh = Arc::new(Show::get(session, &uri).await?);
    let mut guard = SHOWS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() > 200 { map.clear(); }
    map.insert(id.to_string(), (Instant::now(), sh.clone()));
    Ok(sh)
}

/// Un episodio con la forma de una canción, para reutilizar las listas de la app
/// (el «álbum» es el programa y el «artista», quien lo publica).
fn episode_json(e: &Episode, show: &Show) -> Value {
    let date = format!("{:04}-{:02}-{:02}", e.publish_time.year(), u8::from(e.publish_time.month()), e.publish_time.day());
    let covers = if e.covers.is_empty() { &show.covers } else { &e.covers };
    json!({
        "id": id_of(&e.id), "uri": uri_of(&e.id), "name": e.name,
        "artists": [{"name": if show.publisher.is_empty() { show.name.clone() } else { show.publisher.clone() }, "id": Value::Null}],
        "album": {"name": show.name, "id": id_of(&show.id), "uri": uri_of(&show.id), "images": images_json(covers)},
        "duration_ms": e.duration.max(0),
        "explicit": e.is_explicit,
        "release_date": date,
    })
}

/// Episodios de un podcast (lo más reciente primero, como lo entrega Spotify), por páginas.
pub async fn show_episodes(session: &Session, id: &str, offset: usize, limit: usize) -> Result<Value> {
    let show = show_cached(session, id).await?;
    let all: Vec<String> = show.episodes.iter().map(uri_of).collect();
    let page: Vec<String> = all.iter().skip(offset).take(limit).cloned().collect();
    let found: HashMap<String, Episode> = if page.is_empty() { HashMap::new() } else { batch(session, &page, ExtensionKind::EPISODE_V4).await? };
    let items: Vec<Value> = page.iter().map(|u| json!({"track": found.get(u).map(|e| episode_json(e, &show))})).collect();
    let more = offset + page.len() < all.len();
    Ok(json!({
        "items": items, "total": all.len(), "next": if more { json!("session") } else { Value::Null },
        "show": {"name": show.name, "publisher": show.publisher, "description": show.description, "images": images_json(&show.covers)},
    }))
}

// ---- Biblioteca: álbumes guardados y artistas seguidos (colección de la sesión) ----

/// URIs de un conjunto de la colección (`collection` = canciones y álbumes, `artist` = artistas
/// seguidos) que empiezan por `prefix`, lo más reciente primero.
async fn collection_uris(session: &Session, set: &str, prefix: &str, max: usize) -> Result<Vec<String>> {
    let mut items: Vec<(i64, String)> = Vec::new();
    let mut token = String::new();
    for _ in 0..40 {
        let page = session.spclient().get_collection_page(set, &token, 300).await?;
        for it in &page.items {
            if !it.is_removed && it.uri.starts_with(prefix) { items.push((it.added_at, it.uri.clone())); }
        }
        if page.next_page_token.is_empty() { break; }
        token = page.next_page_token.clone();
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen = BTreeSet::new();
    Ok(items.into_iter().map(|(_, u)| u).filter(|u| seen.insert(u.clone())).take(max).collect())
}

/// Álbumes guardados y artistas seguidos para los filtros de «Tu biblioteca».
pub async fn library_collection(session: &Session) -> Result<Value> {
    let (album_uris, artist_uris, show_uris) = tokio::join!(
        collection_uris(session, "collection", "spotify:album:", 400),
        collection_uris(session, "artist", "spotify:artist:", 400),
        collection_uris(session, "show", "spotify:show:", 200),
    );
    let show_uris = show_uris.unwrap_or_default();
    let shows: HashMap<String, Show> = if show_uris.is_empty() { HashMap::new() } else {
        batch(session, &show_uris, ExtensionKind::SHOW_V4).await.unwrap_or_default()
    };
    let shows: Vec<Value> = show_uris.iter().filter_map(|u| shows.get(u)).map(|sh| json!({
        "id": id_of(&sh.id), "uri": uri_of(&sh.id), "name": sh.name, "publisher": sh.publisher,
        "images": images_json(&sh.covers),
    })).collect();
    let album_uris = album_uris.unwrap_or_default();
    let artist_uris = artist_uris.unwrap_or_default();
    let albums = if album_uris.is_empty() { HashMap::new() } else { albums_cached(session, &album_uris).await.unwrap_or_default() };
    let artists: HashMap<String, Artist> = if artist_uris.is_empty() { HashMap::new() } else {
        batch(session, &artist_uris, ExtensionKind::ARTIST_V4).await.unwrap_or_default()
    };
    let albums: Vec<Value> = album_uris.iter().filter_map(|u| albums.get(u)).map(|al| json!({
        "id": id_of(&al.id), "uri": uri_of(&al.id), "name": al.name,
        "images": images_json(&al.covers),
        "artists": al.artists.iter().map(artist_ref).collect::<Vec<_>>(),
    })).collect();
    let artists: Vec<Value> = artist_uris.iter().filter_map(|u| artists.get(u)).map(|a| json!({
        "id": id_of(&a.id), "uri": uri_of(&a.id), "name": a.name,
        "images": images_json(if a.portraits.is_empty() { &a.portrait_group } else { &a.portraits }),
    })).collect();
    Ok(json!({"albums": albums, "artists": artists, "shows": shows}))
}

// ---- Canciones que te gustan (colección), sin Web API ----

static LIKED: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);

/// «Tus me gusta» por la colección de la sesión (`collection/v2/paging`), lo más reciente primero.
/// Todas las canciones de «Tus me gusta» (URIs, la más reciente primero), con 5 min de caché.
async fn liked_uris(session: &Session, fresh: bool) -> Result<Vec<String>> {
    let cached = LIKED.lock().unwrap().as_ref()
        .filter(|(t, _)| t.elapsed() < Duration::from_secs(1800)).map(|(_, l)| l.clone());
    if let Some(list) = cached.filter(|_| !fresh) { return Ok(list); }
    let mut items: Vec<(i64, String)> = Vec::new();
    let mut token = String::new();
    for _ in 0..60 {
        let page = session.spclient().get_collection_page("collection", &token, 300).await?;
        for it in &page.items {
            if !it.is_removed && it.uri.starts_with("spotify:track:") { items.push((it.added_at, it.uri.clone())); }
        }
        if page.next_page_token.is_empty() { break; }
        token = page.next_page_token.clone();
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));
    let uris: Vec<String> = items.into_iter().map(|(_, u)| u).collect();
    *LIKED.lock().unwrap() = Some((Instant::now(), uris.clone()));
    Ok(uris)
}

/// ¿Está cada canción en «Tus me gusta»? Por la sesión (sin cuota de Web API).
pub async fn liked_contains(session: &Session, uris: &[String]) -> Result<Value> {
    let all: std::collections::HashSet<String> = liked_uris(session, false).await?.into_iter().collect();
    Ok(json!(uris.iter().map(|u| all.contains(u)).collect::<Vec<_>>()))
}

/// Guarda o quita canciones de «Tus me gusta», como la app oficial (colección `collection`).
pub async fn set_liked(session: &Session, uris: &[String], saved: bool) -> Result<Value> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let items = uris.iter().map(|u| librespot_protocol::collection2v2::CollectionItem {
        uri: u.clone(), added_at: now, is_removed: !saved, ..Default::default()
    }).collect();
    session.spclient().write_collection("collection", items).await?;
    if let Some((_, list)) = LIKED.lock().unwrap().as_mut() {
        list.retain(|u| !uris.contains(u));
        if saved { for u in uris.iter().rev() { list.insert(0, u.clone()); } }
    }
    Ok(Value::Null)
}

/// Añade canciones al final de una playlist (cambios de playlist4, como la app oficial).
pub async fn playlist_add(session: &Session, id: &str, uris: &[String]) -> Result<Value> {
    use librespot_protocol::playlist4_external::{op::Kind, Add, Delta, Item, ListChanges, Op};
    let sid = SpotifyId::from_base62(id)?;
    let mut last: Option<anyhow::Error> = None;
    for _ in 0..2 {   // si la playlist cambió entre medias (409), una vez más con la revisión nueva
        let bytes = session.spclient().get_playlist(&sid).await?;
        let current = SelectedListContent::parse_from_bytes(&bytes)?;
        let mut add = Add::new();
        add.items = uris.iter().map(|u| { let mut i = Item::new(); i.set_uri(u.clone()); i }).collect();
        add.set_add_last(true);
        let mut op = Op::new();
        op.set_kind(Kind::ADD);
        op.add = protobuf::MessageField::some(add);
        let mut delta = Delta::new();
        delta.ops.push(op);
        let mut changes = ListChanges::new();
        changes.set_base_revision(current.revision().to_vec());
        changes.deltas.push(delta);
        match session.spclient().post_playlist_changes(&sid, &changes).await {
            Ok(_) => { forget_playlist(id); return Ok(Value::Null); }
            Err(e) => last = Some(e.into()),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("no se pudo añadir a la playlist")))
}

/// Mueve una canción dentro de la playlist. `to_index` sigue la convención de Spotify: posición
/// (en la lista original) delante de la que se inserta.
pub async fn playlist_move(session: &Session, id: &str, from: usize, to_index: usize) -> Result<Value> {
    use librespot_protocol::playlist4_external::{op::Kind, Delta, ListChanges, Mov, Op};
    let sid = SpotifyId::from_base62(id)?;
    let mut last: Option<anyhow::Error> = None;
    for _ in 0..2 {
        let bytes = session.spclient().get_playlist(&sid).await?;
        let current = SelectedListContent::parse_from_bytes(&bytes)?;
        let mut mov = Mov::new();
        mov.set_from_index(from as i32);
        mov.set_length(1);
        mov.set_to_index(to_index as i32);
        let mut op = Op::new();
        op.set_kind(Kind::MOV);
        op.mov = protobuf::MessageField::some(mov);
        let mut delta = Delta::new();
        delta.ops.push(op);
        let mut changes = ListChanges::new();
        changes.set_base_revision(current.revision().to_vec());
        changes.deltas.push(delta);
        match session.spclient().post_playlist_changes(&sid, &changes).await {
            Ok(_) => { forget_playlist(id); return Ok(Value::Null); }
            Err(e) => last = Some(e.into()),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("no se pudo mover la canción")))
}

// ---- Fijados de «Tu biblioteca» (colección `ylpin`, la misma que usa la app oficial) ----

/// Elementos fijados (playlists, álbumes, artistas, «Tus me gusta»…), el más reciente primero.
pub async fn pins(session: &Session) -> Result<Value> {
    let mut items: Vec<(i64, String)> = Vec::new();
    let mut token = String::new();
    for _ in 0..5 {
        let page = session.spclient().get_collection_page("ylpin", &token, 100).await?;
        for it in &page.items {
            if !it.is_removed { items.push((it.added_at, it.uri.clone())); }
        }
        if page.next_page_token.is_empty() { break; }
        token = page.next_page_token.clone();
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(json!(items.into_iter().map(|(_, u)| u).collect::<Vec<_>>()))
}

/// Fija o desfija elementos en «Tu biblioteca» (se sincroniza con todos los dispositivos).
pub async fn set_pinned(session: &Session, uris: &[String], pinned: bool) -> Result<Value> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let items = uris.iter().map(|u| librespot_protocol::collection2v2::CollectionItem {
        uri: u.clone(), added_at: now, is_removed: !pinned, ..Default::default()
    }).collect();
    session.spclient().write_collection("ylpin", items).await?;
    Ok(Value::Null)
}

// ---- Crear playlist (como la app oficial: playlist nueva y luego al principio de la rootlist) ----

pub async fn playlist_create(session: &Session, name: &str, description: &str) -> Result<Value> {
    use librespot_protocol::playlist4_external::{
        op::Kind, Add, CreateListReply, Delta, Item, ItemAttributes, ListAttributes,
        ListAttributesPartialState, ListChanges, Op, UpdateListAttributes,
    };
    use http::header::{HeaderMap, HeaderValue, ACCEPT};
    let name = name.trim();
    if name.is_empty() { return Err(anyhow!("falta el nombre")); }

    // 1) La playlist nueva: una operación que pone sus atributos (nombre y descripción).
    let mut attrs = ListAttributes::new();
    attrs.set_name(name.to_string());
    if !description.trim().is_empty() { attrs.set_description(description.trim().to_string()); }
    let mut partial = ListAttributesPartialState::new();
    partial.values = protobuf::MessageField::some(attrs);
    let mut upd = UpdateListAttributes::new();
    upd.new_attributes = protobuf::MessageField::some(partial);
    let mut op = Op::new();
    op.set_kind(Kind::UPDATE_LIST_ATTRIBUTES);
    op.update_list_attributes = protobuf::MessageField::some(upd);
    let mut delta = Delta::new();
    delta.ops.push(op);
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/x-protobuf"));
    let bytes = session.spclient()
        .request_with_protobuf(&http::Method::POST, "/playlist/v2/playlist", Some(headers), &delta).await?;
    let uri = match CreateListReply::parse_from_bytes(&bytes) {
        Ok(r) if r.uri().starts_with("spotify:playlist:") => r.uri().to_string(),
        _ => serde_json::from_slice::<Value>(&bytes).ok()
            .and_then(|v| v.get("uri").and_then(Value::as_str).map(String::from))
            .ok_or_else(|| anyhow!("Spotify no devolvió la playlist nueva"))?,
    };

    // 2) Añadirla al principio de «Tu biblioteca» (rootlist), con la revisión actual.
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let user = session.username();
    let mut last: Option<anyhow::Error> = None;
    for _ in 0..2 {
        let root = session.spclient().get_rootlist(0, Some(1)).await?;
        let current = SelectedListContent::parse_from_bytes(&root)?;
        let mut item = Item::new();
        item.set_uri(uri.clone());
        let mut ia = ItemAttributes::new();
        ia.set_timestamp(now_ms);
        item.attributes = protobuf::MessageField::some(ia);
        let mut add = Add::new();
        add.items.push(item);
        add.set_add_first(true);
        let mut op = Op::new();
        op.set_kind(Kind::ADD);
        op.add = protobuf::MessageField::some(add);
        let mut delta = Delta::new();
        delta.ops.push(op);
        let mut changes = ListChanges::new();
        changes.set_base_revision(current.revision().to_vec());
        changes.deltas.push(delta);
        let endpoint = format!("/playlist/v2/user/{user}/rootlist/changes");
        match session.spclient().request_with_protobuf(&http::Method::POST, &endpoint, None, &changes).await {
            Ok(_) => {
                let id = uri.trim_start_matches("spotify:playlist:").to_string();
                return Ok(json!({"id": id, "uri": uri, "name": name,
                                 "description": (!description.trim().is_empty()).then(|| description.trim().to_string()),
                                 "images": [], "owner": {"id": user, "display_name": Value::Null}}));
            }
            Err(e) => last = Some(e.into()),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("no se pudo añadir la playlist a tu biblioteca")))
}

// ---- Editar playlist: nombre, descripción y portada (como la app oficial) ----

/// Aplica un cambio de atributos (nombre, descripción, portada) a una playlist propia.
async fn update_attributes(session: &Session, id: &str,
                           attrs: librespot_protocol::playlist4_external::ListAttributes) -> Result<()> {
    use librespot_protocol::playlist4_external::{op::Kind, Delta, ListAttributesPartialState, ListChanges, Op, UpdateListAttributes};
    let sid = SpotifyId::from_base62(id)?;
    let mut last: Option<anyhow::Error> = None;
    for _ in 0..2 {
        let bytes = session.spclient().get_playlist(&sid).await?;
        let current = SelectedListContent::parse_from_bytes(&bytes)?;
        let mut partial = ListAttributesPartialState::new();
        partial.values = protobuf::MessageField::some(attrs.clone());
        let mut upd = UpdateListAttributes::new();
        upd.new_attributes = protobuf::MessageField::some(partial);
        let mut op = Op::new();
        op.set_kind(Kind::UPDATE_LIST_ATTRIBUTES);
        op.update_list_attributes = protobuf::MessageField::some(upd);
        let mut delta = Delta::new();
        delta.ops.push(op);
        let mut changes = ListChanges::new();
        changes.set_base_revision(current.revision().to_vec());
        changes.deltas.push(delta);
        match session.spclient().post_playlist_changes(&sid, &changes).await {
            Ok(_) => { PLAYLISTS.lock().unwrap().as_mut().map(|m| m.remove(id)); return Ok(()); }
            Err(e) => last = Some(e.into()),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("no se pudo editar la playlist")))
}

pub async fn playlist_edit(session: &Session, id: &str, name: Option<&str>, description: Option<&str>) -> Result<Value> {
    let mut attrs = librespot_protocol::playlist4_external::ListAttributes::new();
    if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) { attrs.set_name(n.to_string()); }
    if let Some(d) = description { attrs.set_description(d.trim().to_string()); }
    update_attributes(session, id, attrs).await?;
    Ok(Value::Null)
}

/// Sube una portada (JPEG) a una playlist propia. Camino de la app oficial: subida a
/// image-upload.spotify.com → registro en la playlist → atributo `picture`. Si falla, Web API.
pub async fn playlist_set_cover(session: &Session, id: &str, path: &str) -> Result<Value> {
    use librespot_protocol::playlist4_external::{RegisterPlaylistImageRequest, RegisterPlaylistImageResponse};
    use http::header::{HeaderMap, HeaderValue, ACCEPT};
    let jpeg: Vec<u8> = std::fs::read(path)?;
    if jpeg.len() > 256 * 1024 { return Err(anyhow!("la portada supera 256 KB")); }
    let login = session.login5().auth_token().await?;
    let client = http_client();

    let official: Result<()> = async {
        let client_token = session.spclient().client_token().await?;
        let r = client.post("https://image-upload.spotify.com/v4/playlist")
            .bearer_auth(&login.access_token).header("client-token", client_token)
            .header("content-type", "image/jpeg").body(jpeg.clone())
            .timeout(Duration::from_secs(30)).send().await?;
        if !r.status().is_success() { return Err(anyhow!("subida de imagen: {}", r.status().as_u16())); }
        let body: Value = r.json().await?;
        let token = body.get("uploadToken").and_then(Value::as_str).ok_or_else(|| anyhow!("sin uploadToken"))?;
        let mut req = RegisterPlaylistImageRequest::new();
        req.set_upload_token(token.to_string());
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/x-protobuf"));
        let bytes = session.spclient().request_with_protobuf(&http::Method::POST,
            &format!("/playlist/v2/playlist/{id}/register-image"), Some(headers), &req).await?;
        let picture = RegisterPlaylistImageResponse::parse_from_bytes(&bytes).ok()
            .map(|r| r.picture().to_vec()).filter(|p| !p.is_empty())
            .ok_or_else(|| anyhow!("registro de imagen sin respuesta"))?;
        let mut attrs = librespot_protocol::playlist4_external::ListAttributes::new();
        attrs.set_picture(picture);
        update_attributes(session, id, attrs).await
    }.await;
    if official.is_ok() { return Ok(cover_result(session, id).await); }

    // Respaldo: Web API (una sola llamada; necesita el permiso ugc-image-upload).
    let b64 = base64_encode(&jpeg);
    let r = client.put(format!("https://api.spotify.com/v1/playlists/{id}/images"))
        .bearer_auth(&login.access_token).header("content-type", "image/jpeg").body(b64)
        .timeout(Duration::from_secs(30)).send().await?;
    if !r.status().is_success() {
        return Err(anyhow!("No se pudo subir la portada ({}; {})", official.err().map(|e| e.to_string()).unwrap_or_default(), r.status().as_u16()));
    }
    PLAYLISTS.lock().unwrap().as_mut().map(|m| m.remove(id));
    Ok(cover_result(session, id).await)
}

/// URL de la portada nueva (si Spotify ya la tiene) para mostrarla sin esperar.
async fn cover_result(session: &Session, id: &str) -> Value {
    COVERS.lock().unwrap().as_mut().map(|m| m.remove(id));
    let images = async {
        let sid = SpotifyId::from_base62(id).ok()?;
        let bytes = session.spclient().get_playlist(&sid).await.ok()?;
        let list = SelectedListContent::parse_from_bytes(&bytes).ok()?;
        let attrs = list.attributes.as_ref().and_then(|a| PlaylistAttributes::try_from(a).ok())?;
        Some(playlist_images(&attrs))
    }.await.unwrap_or_default();
    json!({"images": images})
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Olvida la copia de «Tus me gusta» (cambió en otro dispositivo).
pub fn forget_liked() { *LIKED.lock().unwrap() = None; }

/// Quita una playlist de «Tu biblioteca» (rootlist). Si es tuya, es lo que Spotify llama «Eliminar».
pub async fn playlist_remove(session: &Session, id: &str) -> Result<Value> {
    use librespot_protocol::playlist4_external::{op::Kind, Delta, Item, ListChanges, Op, Rem};
    let target = format!("spotify:playlist:{id}");
    let user = session.username();
    let mut last: Option<anyhow::Error> = None;
    for _ in 0..2 {
        // Posición en la rootlist (las carpetas también cuentan).
        let mut uris: Vec<String> = Vec::new();
        let mut revision = Vec::new();
        let mut from = 0usize;
        loop {
            let bytes = session.spclient().get_rootlist(from, Some(500)).await?;
            let list = SelectedListContent::parse_from_bytes(&bytes)?;
            if revision.is_empty() { revision = list.revision().to_vec(); }
            let Some(contents) = list.contents.into_option() else { break };
            let n = contents.items.len();
            uris.extend(contents.items.iter().map(|i| i.uri().to_string()));
            if !contents.truncated() || n == 0 { break; }
            from += n;
        }
        let Some(index) = uris.iter().position(|u| *u == target) else { return Ok(Value::Null) };
        let mut item = Item::new();
        item.set_uri(target.clone());
        let mut rem = Rem::new();
        rem.set_from_index(index as i32);
        rem.set_length(1);
        rem.items.push(item);
        let mut op = Op::new();
        op.set_kind(Kind::REM);
        op.rem = protobuf::MessageField::some(rem);
        let mut delta = Delta::new();
        delta.ops.push(op);
        let mut changes = ListChanges::new();
        changes.set_base_revision(revision);
        changes.deltas.push(delta);
        let endpoint = format!("/playlist/v2/user/{user}/rootlist/changes");
        match session.spclient().request_with_protobuf(&http::Method::POST, &endpoint, None, &changes).await {
            Ok(_) => { COVERS.lock().unwrap().as_mut().map(|m| m.remove(id)); return Ok(Value::Null); }
            Err(e) => last = Some(e.into()),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("no se pudo eliminar la playlist")))
}

/// Recomendaciones personalizadas a partir de canciones semilla: la mezcla «inspirada en» que hace
/// Spotify para cada canción (la misma de su radio). Se intercalan y se quitan las ya presentes.
pub async fn recommend(session: &Session, seeds: &[String], exclude: &[String], limit: usize) -> Result<Value> {
    let mut set = tokio::task::JoinSet::new();
    for seed in seeds.iter().filter(|s| s.starts_with("spotify:track:")).take(3) {
        let session = session.clone();
        let seed = seed.clone();
        set.spawn(async move {
            let uri = SpotifyUri::from_uri(&seed).ok()?;
            let bytes = session.spclient().get_radio_for_track(&uri).await.ok()?;
            let v: Value = serde_json::from_slice(&bytes).ok()?;
            let mix = v.pointer("/mediaItems/0/uri").and_then(Value::as_str)?.strip_prefix("spotify:playlist:")?.to_string();
            let page = playlist_items(&session, &mix, 0, 40).await.ok()?;
            Some(page["items"].as_array()?.iter().filter_map(|i| i.get("track").filter(|t| !t.is_null()).cloned()).collect::<Vec<_>>())
        });
    }
    let mut lists: Vec<Vec<Value>> = Vec::new();
    while let Some(r) = set.join_next().await { if let Ok(Some(l)) = r { lists.push(l); } }
    let mut seen: std::collections::HashSet<String> = exclude.iter().chain(seeds.iter()).cloned().collect();
    let mut out = Vec::new();
    let longest = lists.iter().map(Vec::len).max().unwrap_or(0);
    'outer: for i in 0..longest {
        for l in &lists {
            if let Some(t) = l.get(i) {
                let Some(u) = t["uri"].as_str() else { continue };
                if seen.insert(u.to_string()) { out.push(t.clone()); }
                if out.len() >= limit { break 'outer; }
            }
        }
    }
    Ok(json!({"tracks": out}))
}

pub async fn liked_items(session: &Session, offset: usize, limit: usize) -> Result<Value> {
    // Antes la primera página volvía a descargar la colección entera (hasta 60 peticiones seguidas).
    // Ahora, si hay copia, se responde con ella al instante y se actualiza detrás.
    if offset == 0 && LIKED.lock().unwrap().is_some() {
        let s = session.clone();
        tokio::spawn(async move { let _ = liked_uris(&s, true).await; });
    }
    let all = liked_uris(session, false).await?;
    let page: Vec<String> = all.iter().skip(offset).take(limit).cloned().collect();
    let found = track_values(session, &page).await?;
    let items: Vec<Value> = page.iter().filter_map(|u| found.get(u).map(|t| json!({"track": t}))).collect();
    let more = offset + page.len() < all.len();
    Ok(json!({"items": items, "total": all.len(), "next": if more { json!("session") } else { Value::Null }}))
}

/// Datos de formato de la playlist (las editoriales de Spotify traen, p. ej., la imagen ancha de cabecera).
/// Aprovecha la misma descarga para dejar en caché la lista de canciones que pedirá `playlist_items`.
pub async fn playlist_header(session: &Session, id: &str) -> Result<Value> {
    let uri = SpotifyUri::Playlist { id: SpotifyId::from_base62(id)?, user: None };
    let list = Playlist::get(session, &uri).await?;
    let uris: Vec<String> = list.contents.items.iter().map(|i| uri_of(&i.id)).collect();
    PLAYLISTS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id.to_string(), (Instant::now(), uris));
    let attrs: serde_json::Map<String, Value> = list.attributes.format_attributes.iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
    let owner_id = match &list.id {
        SpotifyUri::Playlist { user: Some(user), .. } if !user.is_empty() => Some(user.clone()),
        _ => None,
    };
    let owner_name = if let Some(id) = &owner_id {
        if id == "spotify" { Some("Spotify".to_string()) }
        else {
            tokio::time::timeout(Duration::from_secs(3), session.spclient().get_user_profile(id, Some(0), Some(0)))
                .await.ok().and_then(Result::ok)
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|v| v.get("name").and_then(Value::as_str).map(String::from))
        }
    } else { None };
    Ok(json!({"format": list.attributes.format, "attributes": attrs,
              "owner": owner_id.map(|id| json!({"id": id, "display_name": owner_name}))}))
}

// ---- Álbumes y artistas ----

static ARTIST_CACHE: Mutex<Option<HashMap<String, (Instant, Arc<Artist>)>>> = Mutex::new(None);
static ALBUM_CACHE: Mutex<Option<HashMap<String, (Instant, Arc<Album>)>>> = Mutex::new(None);
static ARTIST_LOCKS: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
const ARTIST_CACHE_TTL: Duration = Duration::from_secs(300);
const ALBUM_CACHE_TTL: Duration = Duration::from_secs(600);
const ARTIST_CACHE_MAX: usize = 256;
const ALBUM_CACHE_MAX: usize = 1000;

async fn album_cached(session: &Session, id: &str) -> Result<Arc<Album>> {
    let uri = format!("spotify:album:{id}");
    if let Some(hit) = ALBUM_CACHE.lock().unwrap().as_ref()
        .and_then(|m| m.get(&uri).filter(|(t, _)| t.elapsed() < ALBUM_CACHE_TTL).map(|(_, a)| a.clone()))
    {
        return Ok(hit);
    }
    let album = Arc::new(Album::get(session, &SpotifyUri::Album { id: SpotifyId::from_base62(id)? }).await?);
    let mut guard = ALBUM_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if cache.len() >= ALBUM_CACHE_MAX { cache.clear(); }
    cache.insert(uri, (Instant::now(), album.clone()));
    Ok(album)
}

pub async fn album_tracks(session: &Session, id: &str) -> Result<Value> {
    let album = album_cached(session, id).await?;
    let uris: Vec<String> = album.tracks().map(uri_of).collect();
    let list = tracks(session, &uris).await?;
    Ok(json!({"items": list, "total": list.len(), "next": Value::Null}))
}

pub async fn album_details(session: &Session, id: &str) -> Result<Value> {
    let album = album_cached(session, id).await?;
    let date = format!("{:04}-{:02}-{:02}", album.date.year(), u8::from(album.date.month()), album.date.day());
    Ok(json!({"name": album.name, "artists": album.artists.iter().map(artist_ref).collect::<Vec<_>>(),
        "images": images_json(&album.covers), "label": album.label, "release_date": date,
        "total_tracks": album.tracks().count(), "album_type": album.type_str,
        "copyrights": album.copyrights.iter().map(|c| json!({"text":c.text})).collect::<Vec<_>>() }))
}

/// Comparte una única lectura `ARTIST_V4` entre llamadas simultáneas (`artist_top`, `artist_albums`, `artists`).
async fn artist(session: &Session, id: &str) -> Result<Arc<Artist>> {
    if let Some(hit) = ARTIST_CACHE.lock().unwrap().as_ref()
        .and_then(|m| m.get(id).filter(|(t, _)| t.elapsed() < ARTIST_CACHE_TTL).map(|(_, a)| a.clone()))
    {
        return Ok(hit);
    }
    let lock = {
        let locks = ARTIST_LOCKS.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
        let mut map = locks.lock().await;
        if map.len() > ARTIST_CACHE_MAX { map.clear(); }
        map.entry(id.to_owned()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
    };
    let _inflight = lock.lock().await;
    if let Some(hit) = ARTIST_CACHE.lock().unwrap().as_ref()
        .and_then(|m| m.get(id).filter(|(t, _)| t.elapsed() < ARTIST_CACHE_TTL).map(|(_, a)| a.clone()))
    {
        return Ok(hit);
    }
    let fetched = Arc::new(Artist::get(session, &SpotifyUri::Artist { id: SpotifyId::from_base62(id)? }).await?);
    let mut guard = ARTIST_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if cache.len() >= ARTIST_CACHE_MAX { cache.clear(); }
    cache.insert(id.to_owned(), (Instant::now(), fetched.clone()));
    Ok(fetched)
}

pub async fn artist_top(session: &Session, id: &str) -> Result<Value> {
    let a = artist(session, id).await?;
    let country = session.country();
    let top = a.top_tracks.iter().find(|t| t.country == country).or_else(|| a.top_tracks.iter().find(|t| t.country == "ZZ"));
    let uris: Vec<String> = top.map(|t| t.tracks.iter().take(10).map(uri_of).collect()).unwrap_or_default();
    Ok(json!({"tracks": tracks(session, &uris).await?}))
}

async fn albums_cached(session: &Session, uris: &[String]) -> Result<HashMap<String, Arc<Album>>> {
    let mut out = HashMap::new();
    let mut missing = Vec::new();
    {
        let guard = ALBUM_CACHE.lock().unwrap();
        let cache = guard.as_ref();
        let mut seen = BTreeSet::new();
        for u in uris {
            if !seen.insert(u.as_str()) { continue; }
            if let Some(al) = cache.and_then(|c| c.get(u)).filter(|(t, _)| t.elapsed() < ALBUM_CACHE_TTL).map(|(_, a)| a.clone()) {
                out.insert(u.clone(), al);
            } else {
                missing.push(u.clone());
            }
        }
    }
    if !missing.is_empty() {
        let fetched: HashMap<String, Album> = batch(session, &missing, ExtensionKind::ALBUM_V4).await?;
        let mut guard = ALBUM_CACHE.lock().unwrap();
        let cache = guard.get_or_insert_with(HashMap::new);
        if cache.len() + fetched.len() > ALBUM_CACHE_MAX { cache.clear(); }
        let now = Instant::now();
        for (u, al) in fetched {
            let al = Arc::new(al);
            cache.insert(u.clone(), (now, al.clone()));
            out.insert(u, al);
        }
    }
    Ok(out)
}

pub async fn artist_albums(session: &Session, id: &str) -> Result<Value> {
    let a = artist(session, id).await?;
    let mut wanted: Vec<(String, &str)> = Vec::new();
    for (groups, name, limit) in [
        (&a.albums, "album", usize::MAX),
        (&a.singles, "single", usize::MAX),
        (&a.compilations, "compilation", 24),
        (&a.appears_on_albums, "appears_on", 40),
    ] {
        for group in groups.iter().take(limit) {
            if let Some(first) = group.first() { wanted.push((uri_of(first), name)); }
        }
    }
    wanted.truncate(160);
    let uris: Vec<String> = wanted.iter().map(|(u, _)| u.clone()).collect();
    let found = albums_cached(session, &uris).await?;
    let items: Vec<Value> = wanted.iter().filter_map(|(u, group)| {
        let al = found.get(u)?;
        let date = format!("{:04}-{:02}-{:02}", al.date.year(), u8::from(al.date.month()), al.date.day());
        Some(json!({
            "id": id_of(&al.id), "uri": uri_of(&al.id), "name": al.name,
            "images": images_json(&al.covers),
            "artists": al.artists.iter().map(artist_ref).collect::<Vec<_>>(),
            "release_date": date, "album_group": group,
            "album_type": if *group == "appears_on" { "album" } else { group },
        }))
    }).collect();
    Ok(json!({"items": items}))
}

fn artist_detail_json(a: &Artist) -> Value {
    json!({
        "id": id_of(&a.id), "uri": uri_of(&a.id), "name": a.name,
        "images": images_json(if a.portraits.is_empty() { &a.portrait_group } else { &a.portraits }),
        "popularity": a.popularity,
        "biography": a.biographies.first().map(|b| &b.text),
        "related_artists": a.related.iter().take(16).map(|r|json!({"id":id_of(&r.id),"uri":uri_of(&r.id),"name":r.name,
            "images":images_json(if r.portraits.is_empty(){&r.portrait_group}else{&r.portraits})})).collect::<Vec<_>>(),
    })
}

pub async fn artists(session: &Session, ids: &[String]) -> Result<Value> {
    if let [id] = ids {
        let a = artist(session, id).await?;
        return Ok(json!({"artists": [artist_detail_json(&a)]}));
    }
    let uris: Vec<String> = ids.iter().map(|i| format!("spotify:artist:{i}")).collect();
    let found: HashMap<String, Artist> = batch(session, &uris, ExtensionKind::ARTIST_V4).await?;
    let items: Vec<Value> = uris.iter().filter_map(|u| found.get(u)).map(artist_detail_json).collect();
    Ok(json!({"artists": items}))
}

pub async fn track_credits(session: &Session, id: &str) -> Result<Value> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(anyhow!("id de canción inválido"));
    }
    let login = session.login5().auth_token().await?;
    let client_token = session.spclient().client_token().await?;
    let url = format!("https://spclient.wg.spotify.com/track-credits-view/v0/experimental/{id}/credits");
    let response = http_client()
        .get(&url)
        .bearer_auth(&login.access_token)
        .header("client-token", client_token)
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(anyhow!("track-credits-view respondió {}", response.status().as_u16()));
    }
    Ok(response.json::<Value>().await?)
}

/// Artistas relacionados por Pathfinder con el token de la sesión (login5 + client token).
pub async fn artist_related(session: &Session, id: &str) -> Result<Value> {
    let login = session.login5().auth_token().await?;
    let client_token = session.spclient().client_token().await?;
    let query = json!({
        "operationName": "queryArtistRelated",
        "variables": {"uri": format!("spotify:artist:{id}")},
        "extensions": {"persistedQuery": {"version": 1, "sha256Hash": RELATED_ARTISTS_HASH}}
    });
    let response = http_client()
        .post("https://api-partner.spotify.com/pathfinder/v2/query")
        .bearer_auth(&login.access_token)
        .header("client-token", client_token)
        .json(&query)
        .timeout(Duration::from_secs(20))
        .send().await?;
    if !response.status().is_success() {
        return Err(anyhow!("Pathfinder respondió {}", response.status().as_u16()));
    }
    let body: Value = response.json().await?;
    let entries = body.pointer("/data/artistUnion/relatedContent/relatedArtists/items")
        .and_then(Value::as_array).ok_or_else(|| anyhow!("respuesta sin artistas relacionados"))?;
    let artists: Vec<Value> = entries.iter().filter_map(|d| {
        let uri = d.get("uri")?.as_str()?;
        let name = d.pointer("/profile/name")?.as_str()?;
        let mut sources: Vec<&Value> = d.pointer("/visuals/avatarImage/sources").and_then(Value::as_array)?.iter().collect();
        sources.sort_by_key(|s| -(s.get("width").and_then(Value::as_i64).unwrap_or(0)));
        Some(json!({
            "id": uri.rsplit(':').next(), "uri": uri, "name": name,
            "images": sources.iter().filter_map(|s| s.get("url")).map(|u| json!({"url": u})).collect::<Vec<_>>(),
        }))
    }).collect();
    Ok(json!({"artists": artists}))
}

/// Primera URL de imagen de un objeto de Pathfinder (`sources` o `images.items[0].sources`), la más grande.
fn pf_image(v: &Value) -> Vec<Value> {
    let sources = v.get("sources").and_then(Value::as_array)
        .or_else(|| v.pointer("/images/items/0/sources").and_then(Value::as_array))
        .or_else(|| v.pointer("/coverArt/sources").and_then(Value::as_array));
    let mut list: Vec<&Value> = sources.into_iter().flatten().collect();
    list.sort_by_key(|s| -(s.get("width").and_then(Value::as_i64).unwrap_or(0)));
    list.first().and_then(|s| s.get("url")).map(|u| vec![json!({"url": u})]).unwrap_or_default()
}

/// Playlist de Pathfinder → forma de la Web API (la que decodifica la app).
fn pf_playlist(item: &Value) -> Option<Value> {
    let d = item.get("data").unwrap_or(item);
    let uri = d.get("uri")?.as_str()?;
    if !uri.starts_with("spotify:playlist:") { return None; }
    let owner = d.pointer("/ownerV2/data");
    Some(json!({
        "id": id_from(uri), "uri": uri,
        "name": d.get("name")?.as_str()?,
        "images": pf_image(d),
        "description": d.get("description").and_then(Value::as_str).unwrap_or(""),
        "owner": {
            "id": owner.and_then(|o| o.get("username").or_else(|| o.get("uri"))).and_then(Value::as_str).map(id_from).unwrap_or("spotify"),
            "display_name": owner.and_then(|o| o.get("name")).and_then(Value::as_str),
        },
    }))
}

fn pf_playlists(body: &Value, path: &str) -> Vec<Value> {
    body.pointer(path).and_then(Value::as_array).into_iter().flatten().filter_map(pf_playlist).collect()
}

fn pf_top_playcounts(root: &Value) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    let items = root
        .pointer("/discography/topTracks/items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for item in items {
        let track = item.get("track").unwrap_or(item);
        let track = track.get("data").unwrap_or(track);
        let uri = track
            .get("uri")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                track
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| format!("spotify:track:{id}"))
            });
        let Some(uri) = uri else { continue };
        let count = track
            .get("playcount")
            .or_else(|| track.get("playCount"))
            .or_else(|| item.get("playcount"))
            .or_else(|| item.get("playCount"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            });
        if let Some(n) = count {
            out.insert(uri, Value::from(n));
        }
    }
    out
}

/// Secciones de la página del artista que la Web API no ofrece: playlists de Spotify con el artista
/// («Featuring»), playlists donde aparece («Discovered on»), playlists del propio artista y «Aparece en».
pub async fn artist_overview(session: &Session, id: &str) -> Result<Value> {
    let login = session.login5().auth_token().await?;
    let client_token = session.spclient().client_token().await?;
    let query = json!({
        "operationName": "queryArtistOverview",
        "variables": {"uri": format!("spotify:artist:{id}"), "locale": "", "includePrerelease": true},
        "extensions": {"persistedQuery": {"version": 1, "sha256Hash": ARTIST_OVERVIEW_HASH}}
    });
    let response = http_client()
        .post("https://api-partner.spotify.com/pathfinder/v2/query")
        .bearer_auth(&login.access_token)
        .header("client-token", client_token)
        .json(&query)
        .timeout(Duration::from_secs(20))
        .send().await?;
    if !response.status().is_success() {
        return Err(anyhow!("Pathfinder respondió {}", response.status().as_u16()));
    }
    let body: Value = response.json().await?;
    let root = body.pointer("/data/artistUnion").ok_or_else(|| anyhow!("respuesta sin artista"))?;

    let appears_on: Vec<Value> = root.pointer("/relatedContent/appearsOn/items").and_then(Value::as_array).into_iter().flatten()
        .flat_map(|g| g.pointer("/releases/items").and_then(Value::as_array).cloned().unwrap_or_default())
        .filter_map(|r| {
            let uri = r.get("uri")?.as_str()?;
            let artists: Vec<Value> = r.pointer("/artists/items").and_then(Value::as_array).into_iter().flatten()
                .filter_map(|a| Some(json!({"name": a.pointer("/profile/name")?.as_str()?, "id": a.get("uri").and_then(Value::as_str).map(id_from)})))
                .collect();
            Some(json!({
                "id": id_from(uri), "uri": uri, "name": r.get("name")?.as_str()?,
                "images": pf_image(&r), "artists": artists,
                "release_date": r.pointer("/date/year").and_then(Value::as_i64).map(|y| y.to_string()),
                "album_group": "appears_on",
            }))
        }).collect();

    Ok(json!({
        "biography": root.pointer("/profile/biography/text").and_then(Value::as_str),
        "monthly_listeners": root.pointer("/stats/monthlyListeners").and_then(Value::as_u64),
        "world_rank": root.pointer("/stats/worldRank").and_then(Value::as_u64),
        "followers": root.pointer("/stats/followers").and_then(Value::as_u64),
        "featuring": pf_playlists(root, "/relatedContent/featuringV2/items"),
        "discovered_on": pf_playlists(root, "/relatedContent/discoveredOnV2/items"),
        "artist_playlists": pf_playlists(root, "/profile/playlistsV2/items"),
        "appears_on": appears_on,
        "top_playcounts": pf_top_playcounts(root),
    }))
}

// ---- Búsqueda por la sesión (servicio searchview de los clientes de Spotify) ----

fn encode(q: &str) -> String {
    q.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

fn id_from(uri: &str) -> &str { uri.rsplit(':').next().unwrap_or_default() }

fn search_artists(v: &Value) -> Vec<Value> {
    v.pointer("/artists/items").and_then(Value::as_array).into_iter().flatten().filter_map(|a| {
        let uri = a.get("uri")?.as_str()?;
        Some(json!({"id": id_from(uri), "name": a.pointer("/profile/name")?.as_str()?}))
    }).collect()
}

fn search_items<'a>(root: &'a Value, section: &str) -> impl Iterator<Item = &'a Value> {
    root.pointer(&format!("/{section}/items")).and_then(Value::as_array).into_iter().flatten()
}

fn search_response(body: &Value, types: &str) -> Result<Value> {
    let root = body.pointer("/data/searchV2").ok_or_else(|| anyhow!("búsqueda web sin resultados válidos"))?;
    let want = |k: &str| types.split(',').any(|t| t.trim() == k);
    let mut out = serde_json::Map::new();
    if want("track") {
        let items: Vec<Value> = search_items(root, "tracksV2").filter_map(|h| {
            let wrapper = h.get("item")?;
            let d = wrapper.get("data")?;
            let uri = wrapper.get("uri").or_else(|| wrapper.get("_uri")).or_else(|| d.get("uri"))?.as_str()?;
            let album = d.get("albumOfTrack");
            Some(json!({"id": id_from(uri), "uri": uri, "name": d.get("name")?.as_str()?,
                "artists": search_artists(d), "duration_ms": d.pointer("/duration/totalMilliseconds").and_then(Value::as_i64).unwrap_or(0),
                "album": album.map(|a| json!({"name": a.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "uri": a.get("uri"), "id": a.get("uri").and_then(Value::as_str).map(id_from), "images": pf_image(a)}))}))
        }).collect();
        out.insert("tracks".into(), json!({"items": items}));
    }
    if want("artist") {
        let items: Vec<Value> = search_items(root, "artists").filter_map(|h| {
            let d = h.get("data")?; let uri = d.get("uri")?.as_str()?;
            Some(json!({"id": id_from(uri), "uri": uri, "name": d.pointer("/profile/name")?.as_str()?,
                "images": d.pointer("/visuals/avatarImage").map(pf_image).unwrap_or_default()}))
        }).collect();
        out.insert("artists".into(), json!({"items": items}));
    }
    if want("album") {
        let items: Vec<Value> = search_items(root, "albumsV2").filter_map(|h| {
            let d = h.get("data")?; let uri = d.get("uri")?.as_str()?;
            Some(json!({"id": id_from(uri), "uri": uri, "name": d.get("name")?.as_str()?,
                "images": pf_image(d), "artists": search_artists(d),
                "release_date": d.pointer("/date/year").and_then(Value::as_i64).map(|y| y.to_string())}))
        }).collect();
        out.insert("albums".into(), json!({"items": items}));
    }
    if want("playlist") {
        let items: Vec<Value> = search_items(root, "playlists").filter_map(pf_playlist).collect();
        out.insert("playlists".into(), json!({"items": items}));
    }
    if want("show") {
        let items: Vec<Value> = search_items(root, "podcasts").filter_map(|h| {
            let d = h.get("data")?; let uri = d.get("uri")?.as_str()?;
            if !uri.starts_with("spotify:show:") { return None; }
            let publisher = d.pointer("/publisher/name").and_then(Value::as_str).unwrap_or_default();
            Some(json!({"id": id_from(uri), "uri": uri, "name": d.get("name")?.as_str()?, "images": pf_image(d),
                "owner": {"id": "", "display_name": if publisher.is_empty() { "Podcast".to_string() } else { format!("Podcast · {publisher}") }}}))
        }).collect();
        out.insert("shows".into(), json!({"items": items}));
    }
    Ok(Value::Object(out))
}

/// Búsqueda por la sesión del reproductor web; no usa la cuota de la Web API.
pub async fn search(session: &Session, query: &str, types: &str, limit: u32, offset: u32) -> Result<Value> {
    let login = session.login5().auth_token().await?;
    let client_token = session.spclient().client_token().await?;
    let client = http_client();
    let hashes = [
        "db61238974d27839a136c9dc02bfdbe3fab7635f21cf85976ebff9a1ee281345",
        "fcad5a3e0d5af727fb76966f06971c19cfa2275e6ff7671196753e008611873c",
        "d3f7547835dc86a4fdf3997e0f79314e7580eaf4aaf2f4cb1e71e189c5dfcb1f",
    ];
    let mut last = String::new();
    for hash in hashes {
        let payload = json!({"operationName": "searchDesktop", "variables": {
            "searchTerm": query, "offset": offset, "limit": limit, "numberOfTopResults": 5,
            "includeAudiobooks": true, "includeArtistHasConcertsField": false,
            "includePreReleases": false, "includeLocalConcertsField": false, "includeAuthors": false
        }, "extensions": {"persistedQuery": {"version": 1, "sha256Hash": hash}}});
        let response = client.post("https://api-partner.spotify.com/pathfinder/v2/query")
            .bearer_auth(&login.access_token).header("client-token", &client_token)
            .json(&payload).timeout(Duration::from_secs(15)).send().await?;
        if !response.status().is_success() { last = format!("HTTP {}", response.status()); continue; }
        let body: Value = response.json().await?;
        if body.pointer("/data/searchV2").is_some() { return search_response(&body, types); }
        last = body.pointer("/errors/0/message").and_then(Value::as_str).unwrap_or("respuesta inesperada").to_string();
    }
    Err(anyhow!("búsqueda web no disponible: {last}"))
}

#[cfg(test)]
mod search_tests {
    use super::*;
    #[test]
    fn converts_web_search_results() {
        let body = json!({"data":{"searchV2":{
            "tracksV2":{"items":[{"item":{"uri":"spotify:track:t","data":{"name":"Song","duration":{"totalMilliseconds":123},"artists":{"items":[{"uri":"spotify:artist:a","profile":{"name":"BTS"}}]},"albumOfTrack":{"name":"Album","uri":"spotify:album:b","coverArt":{"sources":[{"url":"https://example.com/a"}]}}}}}]},
            "artists":{"items":[{"data":{"uri":"spotify:artist:a","profile":{"name":"BTS"},"visuals":{"avatarImage":{"sources":[{"url":"https://example.com/b"}]}}}}]},
            "albumsV2":{"items":[{"data":{"uri":"spotify:album:b","name":"Album"}}]},
            "playlists":{"items":[{"data":{"uri":"spotify:playlist:p","name":"List"}}]}
        }}});
        let r = search_response(&body, "track,artist,album,playlist").unwrap();
        assert_eq!(r["tracks"]["items"][0]["artists"][0]["name"], "BTS");
        assert_eq!(r["artists"]["items"][0]["name"], "BTS");
        assert_eq!(r["albums"]["items"][0]["id"], "b");
        assert_eq!(r["playlists"]["items"][0]["id"], "p");
    }
}

#[cfg(test)]
mod cover_tests {
    use super::*;
    #[test]
    fn mosaic_needs_four_distinct_albums() {
        let u = |x: &str| format!("{IMAGE_HOST}{x}");
        assert_eq!(mosaic_url(&[u("a"), u("a"), u("b")]), Some(u("a")));
        assert_eq!(mosaic_url(&[u("a"), u("b"), u("a"), u("c"), u("d")]).as_deref(), Some("https://mosaic.scdn.co/640/abcd"));
        assert_eq!(mosaic_url(&[]), None);
    }
}

// ---- Inicio: «Hecho para ti» (daylist, Daily Mix, Descubrimiento semanal…) y «Tus mixes principales» ----

/// Playlists de una sección del Inicio, en el orden de Spotify, sin repetir.
fn home_section_page(body: &Value) -> Result<(Vec<Value>, usize, Option<u64>)> {
    if body.get("errors").is_some_and(|e| e.as_array().is_some_and(|a| !a.is_empty())) {
        return Err(anyhow!("Spotify no devolvió la sección del Inicio"));
    }
    let sections = body.pointer("/data/homeSections/sections").and_then(Value::as_array)
        .filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("respuesta sin secciones del Inicio"))?;
    let mut lists = Vec::new();
    let mut count = 0usize;
    let mut total: Option<u64> = Some(0);
    for section in sections {
        let items = section.pointer("/sectionItems/items").and_then(Value::as_array).cloned().unwrap_or_default();
        count += items.len();
        total = total.and_then(|t| section.pointer("/sectionItems/totalCount").and_then(Value::as_u64).map(|c| t + c));
        for item in &items {
            let Some(d) = item.pointer("/content/data") else { continue };
            if d.get("__typename").and_then(Value::as_str) != Some("Playlist") { continue; }
            if d.get("uri").and_then(Value::as_str) == Some(DJ_PLAYLIST) { continue; }
            if let Some(p) = pf_playlist(d) { lists.push(p); }
        }
    }
    Ok((lists, count, total))
}

async fn home_section(client: &reqwest::Client, token: &str, client_token: &str, country: &str, time_zone: &str, section: &str) -> Result<Vec<Value>> {
    const PAGE: usize = 20;
    let mut out: Vec<Value> = Vec::new();
    let mut seen = BTreeSet::new();
    for page in 0..4 {
        let offset = page * PAGE;
        let query = json!({
            "operationName": "homeSection",
            "variables": {"uri": section, "country": country, "timeZone": time_zone, "sp_t": token,
                          "sectionItemsLimit": PAGE, "sectionItemsOffset": offset},
            "extensions": {"persistedQuery": {"version": 1, "sha256Hash": HOME_SECTION_HASH}}
        });
        let response = client.post("https://api-partner.spotify.com/pathfinder/v2/query")
            .bearer_auth(token).header("client-token", client_token)
            .json(&query).timeout(Duration::from_secs(20)).send().await?;
        if !response.status().is_success() {
            return Err(anyhow!("Pathfinder respondió {}", response.status().as_u16()));
        }
        let (lists, count, total) = home_section_page(&response.json::<Value>().await?)?;
        for p in lists {
            if let Some(id) = p.get("id").and_then(Value::as_str) { if seen.insert(id.to_string()) { out.push(p); } }
        }
        let more = count > 0 && total.map_or(count == PAGE, |t| (offset + count) as u64 > 0 && ((offset + count) as u64) < t);
        if !more { break; }
    }
    Ok(out)
}

pub async fn home_sections(session: &Session, time_zone: &str) -> Result<Value> {
    let login = session.login5().auth_token().await?;
    let client_token = session.spclient().client_token().await?;
    let country = session.country();
    let client = http_client();
    let (made, mixes) = tokio::join!(
        home_section(&client, &login.access_token, &client_token, &country, time_zone, MADE_FOR_YOU_SECTION),
        home_section(&client, &login.access_token, &client_token, &country, time_zone, TOP_MIXES_SECTION),
    );
    if made.is_err() && mixes.is_err() { return Err(made.unwrap_err()); }
    Ok(json!({"made_for_you": made.unwrap_or_default(), "top_mixes": mixes.unwrap_or_default()}))
}

#[cfg(test)]
mod home_tests {
    use super::*;

    #[test]
    fn home_section_keeps_playlists_in_order_and_skips_dj() {
        let body = json!({"data": {"homeSections": {"sections": [{"sectionItems": {"totalCount": 3, "items": [
            {"content": {"data": {"__typename": "Playlist", "uri": "spotify:playlist:37i9dQZF1EP6YuccBxUcC1", "name": "daylist",
                                  "images": {"items": [{"sources": [{"url": "https://i.scdn.co/a", "width": 300}]}]},
                                  "ownerV2": {"data": {"name": "Spotify", "username": "spotify"}}}}},
            {"content": {"data": {"__typename": "Playlist", "uri": DJ_PLAYLIST, "name": "DJ"}}},
            {"content": {"data": {"__typename": "Album", "uri": "spotify:album:x", "name": "Álbum"}}}
        ]}}]}}});
        let (lists, count, total) = home_section_page(&body).unwrap();
        assert_eq!(count, 3); assert_eq!(total, Some(3));
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0]["name"], "daylist");
        assert_eq!(lists[0]["owner"]["id"], "spotify");
        assert_eq!(lists[0]["images"][0]["url"], "https://i.scdn.co/a");
        assert!(home_section_page(&json!({"errors": [{"message": "x"}]})).is_err());
    }
}

// ---- Escuchado recientemente (servicio recently-played de Spotify, por la sesión) ----

/// Contextos escuchados hace poco (playlist, álbum, artista…) con la última canción de cada uno,
/// en la forma de `/me/player/recently-played` de la Web API: `{items: [{track, played_at, context}]}`.
pub async fn recently_played(session: &Session) -> Result<Value> {
    let user = session.username();
    let endpoint = format!("/recently-played/v3/user/{user}/recently-played?format=json&offset=0&limit=50&filter=default,collection-new-episodes&market=from_token");
    let bytes = session.spclient().request(&http::Method::GET, &endpoint, None, None).await?;
    let body: Value = serde_json::from_slice(&bytes)?;
    let contexts = recent_contexts(&body);
    let uris: Vec<String> = contexts.iter().filter_map(|c| c.2.clone()).collect::<BTreeSet<_>>().into_iter().collect();
    let found: HashMap<String, Track> = if uris.is_empty() { HashMap::new() } else { batch(session, &uris, ExtensionKind::TRACK_V4).await? };
    let items: Vec<Value> = contexts.iter().filter_map(|(ctx, ms, track)| {
        let t = found.get(track.as_ref()?)?;
        Some(json!({"track": track_json(t), "played_at": iso_time(*ms), "context": {"uri": ctx}}))
    }).collect();
    Ok(json!({"items": items}))
}

/// (contexto, ms de la última escucha, última canción) de la respuesta de recently-played.
fn recent_contexts(body: &Value) -> Vec<(String, i64, Option<String>)> {
    body.get("playContexts").and_then(Value::as_array).into_iter().flatten().filter_map(|c| {
        let uri = c.get("uri")?.as_str()?.to_string();
        let ms = c.get("lastPlayedTime").and_then(Value::as_i64).unwrap_or(0);
        let track = c.get("lastPlayedTrackUri").and_then(Value::as_str)
            .filter(|u| u.starts_with("spotify:track:")).map(String::from);
        Some((uri, ms, track))
    }).collect()
}

/// Milisegundos Unix → «2026-11-06T16:42:57Z».
fn iso_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // Fecha civil desde días (algoritmo de Howard Hinnant).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod recent_tests {
    use super::*;

    #[test]
    fn parses_recent_contexts_and_formats_time() {
        let body = json!({"playContexts": [
            {"uri": "spotify:playlist:abc", "lastPlayedTime": 1762447377000i64, "lastPlayedTrackUri": "spotify:track:t1"},
            {"uri": "spotify:album:x", "lastPlayedTime": 1762440000000i64}
        ]});
        let c = recent_contexts(&body);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].2.as_deref(), Some("spotify:track:t1"));
        assert_eq!(c[1].2, None);
        assert_eq!(iso_time(1762447377000), "2025-11-06T16:42:57Z");
        assert_eq!(iso_time(0), "1970-01-01T00:00:00Z");
    }
}

#[cfg(test)]
mod playlist_refresh_tests {
    use super::*;
    #[test]
    fn a_modified_playlist_does_not_reuse_the_previous_track_snapshot() {
        let mut cache=HashMap::new();
        cache.insert("modified".to_owned(),(Instant::now(),vec!["spotify:track:old".to_owned()]));
        cache.insert("other".to_owned(),(Instant::now(),vec!["spotify:track:keep".to_owned()]));
        *PLAYLISTS.lock().unwrap()=Some(cache);
        forget_playlist("modified");
        let guard=PLAYLISTS.lock().unwrap();let cache=guard.as_ref().unwrap();
        assert!(!cache.contains_key("modified"));assert!(cache.contains_key("other"));
    }

    #[test]
    fn extracts_top_track_playcounts_from_artist_overview() {
        let root = json!({
            "discography": {
                "topTracks": {
                    "items": [
                        {"track": {"uri": "spotify:track:t1", "playcount": "1254896231"}},
                        {"track": {"data": {"uri": "spotify:track:t2", "playCount": 987654}}},
                        {"track": {"uri": "spotify:track:t3", "playcount": "not-a-number"}},
                        {"track": {"id": "t4"}, "playcount": "42"}
                    ]
                }
            }
        });
        let counts = pf_top_playcounts(&root);
        assert_eq!(counts.get("spotify:track:t1").and_then(Value::as_u64), Some(1_254_896_231));
        assert_eq!(counts.get("spotify:track:t2").and_then(Value::as_u64), Some(987_654));
        assert!(!counts.contains_key("spotify:track:t3"));
        assert_eq!(counts.get("spotify:track:t4").and_then(Value::as_u64), Some(42));
    }
}
