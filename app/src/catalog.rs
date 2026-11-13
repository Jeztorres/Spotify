//! Páginas de catálogo, recomendaciones y fijados. Toda respuesta queda ligada a la página que la pidió.
use super::*;
use spotify_core::{AlbumItem, ArtistItem, ArtistOverview, SpArtist, TopTracksResponse};
use std::collections::HashSet;

#[derive(Clone)]
pub enum Destination {
    Album(AlbumItem),
    Artist(ArtistItem),
    Playlist(Playlist),
}
impl Destination {
    fn uri(&self) -> &str {
        match self {
            Self::Album(a) => &a.uri,
            Self::Artist(a) => &a.uri,
            Self::Playlist(p) => &p.uri,
        }
    }
    fn card(&self) -> (String, String, Option<String>) {
        match self {
            Self::Album(a) => (
                a.name.clone(),
                [
                    a.release_date
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(4)
                        .collect::<String>(),
                    a.artist_line(),
                ]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
                a.image_url().map(str::to_owned),
            ),
            Self::Artist(a) => (
                a.name.clone(),
                "Artista".into(),
                a.image_url().map(str::to_owned),
            ),
            Self::Playlist(p) => (
                p.name.clone(),
                p.owner
                    .as_ref()
                    .and_then(|o| o.display_name.clone())
                    .unwrap_or_else(|| "Playlist".into()),
                p.image_url().map(str::to_owned),
            ),
        }
    }
}
pub struct Catalog {
    groups: [Rc<VecModel<CardEntry>>; 6],
    targets: [Vec<Destination>; 6],
    pub pins: Vec<String>,
    pin_pending: HashSet<String>,
    pins_epoch: u64,
    limits: [usize; 6],
    more: Rc<VecModel<bool>>,
    pub suggestions: Vec<Track>,
    pub suggestion_model: Rc<VecModel<TrackEntry>>,
    suggestion_gen: Option<u64>,
    releases: Vec<AlbumItem>,
    collaborations: Vec<AlbumItem>,
    pub album_artists: Vec<SpArtist>,
    pub list_artists: Vec<ArtistItem>,
    pub list_artists_model: Rc<VecModel<CardEntry>>,
    pub top_playcounts: std::collections::HashMap<String, u64>,
}
impl Catalog {
    pub fn new(ui: &MainWindow) -> Self {
        let groups: [Rc<VecModel<CardEntry>>; 6] = std::array::from_fn(|_| Rc::default());
        ui.set_catalog_albums(ModelRc::from(groups[0].clone()));
        ui.set_catalog_singles(ModelRc::from(groups[1].clone()));
        ui.set_catalog_collabs(ModelRc::from(groups[2].clone()));
        ui.set_catalog_artists(ModelRc::from(groups[3].clone()));
        ui.set_catalog_playlists(ModelRc::from(groups[4].clone()));
        ui.set_catalog_latest(ModelRc::from(groups[5].clone()));
        let list_artists_model: Rc<VecModel<CardEntry>> = Rc::default();
        ui.set_list_artists(ModelRc::from(list_artists_model.clone()));
        let more = Rc::new(VecModel::from(vec![false; 6]));
        ui.set_catalog_more(ModelRc::from(more.clone()));
        let suggestion_model: Rc<VecModel<TrackEntry>> = Rc::default();
        ui.set_suggestion_rows(ModelRc::from(suggestion_model.clone()));
        Self {
            groups,
            targets: std::array::from_fn(|_| vec![]),
            pins: vec![],
            pin_pending: HashSet::new(),
            pins_epoch: 0,
            limits: [12; 6],
            more,
            suggestions: vec![],
            suggestion_model,
            suggestion_gen: None,
            releases: vec![],
            collaborations: vec![],
            album_artists: vec![],
            list_artists: vec![],
            list_artists_model,
            top_playcounts: std::collections::HashMap::new(),
        }
    }
}
/// Biografía del artista; las largas se muestran recogidas con «Leer más».
fn set_bio(ui: &MainWindow, text: &str) {
    ui.set_artist_bio(text.into());
    ui.set_bio_long(text.chars().count() > 420);
    ui.set_bio_expanded(false);
}

pub fn reset(ui: &MainWindow, app: &mut App) {
    media::reset(ui, app);
    ui.set_motion_paused(!ui.get_pref_motion());
    app.catalog.limits = [12; 6];
    app.catalog.more.set_vec(vec![false; 6]);
    for model in &app.catalog.groups {
        model.set_vec(vec![]);
    }
    for targets in &mut app.catalog.targets {
        targets.clear();
    }
    app.catalog.suggestions.clear();
    app.catalog.suggestion_model.set_vec(vec![]);
    app.catalog.suggestion_gen = None;
    app.catalog.releases.clear();
    app.catalog.collaborations.clear();
    app.catalog.album_artists.clear();
    app.catalog.list_artists.clear();
    app.catalog.list_artists_model.set_vec(vec![]);
    app.catalog.top_playcounts.clear();
    ui.set_detail_panel(0);
    ui.set_album_copyright("".into());
    ui.set_album_artist("".into());
    ui.set_list_artist_avatar(Image::default());
    ui.set_list_header_artist("".into());
    ui.set_list_header_artist_clickable(false);
    ui.set_list_tracks_chip("".into());
    ui.set_list_duration_chip("".into());
    set_bio(ui, "");
    ui.set_artist_bio_source("".into());
    ui.set_artist_stats("".into());
    ui.set_catalog_notice("".into());
    ui.set_suggestion_message("Cargando recomendaciones…".into());
    ui.set_suggestions_loading(false);
}
pub fn set_list_artists(app: &mut App, artists: Vec<ArtistItem>) {
    let mut seen = HashSet::new();
    let unique: Vec<ArtistItem> = artists
        .into_iter()
        .filter(|a| !a.id.is_empty() && !a.name.is_empty() && seen.insert(a.id.clone()))
        .take(20)
        .collect();
    if unique.iter().map(|a| &a.id).eq(app.catalog.list_artists.iter().map(|a| &a.id)) {
        return;
    }
    app.catalog.list_artists = unique.clone();
    app.catalog.list_artists_model.set_vec(
        unique
            .iter()
            .map(|a| CardEntry {
                title: a.name.clone().into(),
                subtitle: "Artista".into(),
                cover: Image::default(),
            })
            .collect::<Vec<_>>(),
    );
    let Some(engine) = app.engine.clone() else { return };
    let gen = app.list_gen;
    let ids: Vec<String> = unique.iter().map(|a| a.id.clone()).collect();
    if ids.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        cached_call(&engine, "artists", json!({"ids": ids}), |result| {
            let Ok(value) = result else { return };
            let found = artist_items(&value, "artists");
            on_ui(move |_, app| {
                if !current(app, gen) {
                    return;
                }
                let by_id: std::collections::HashMap<String, String> = found
                    .into_iter()
                    .filter_map(|a| {
                        let url = a.image_url()?.to_owned();
                        Some((a.id, url))
                    })
                    .collect();
                for (i, artist) in app.catalog.list_artists.clone().into_iter().enumerate() {
                    let Some(url) = by_id.get(&artist.id).cloned() else { continue };
                    load_cover(app, &url, 160, move |_, app, d| {
                        if !current(app, gen) {
                            return;
                        }
                        if let Some(mut row) = app.catalog.list_artists_model.row_data(i) {
                            row.cover = to_image(&images::circle(d));
                            app.catalog.list_artists_model.set_row_data(i, row);
                        }
                    });
                }
            });
        });
    });
}

pub fn open_list_artist(ui: &MainWindow, app: &mut App, i: usize) {
    let Some(artist) = app.catalog.list_artists.get(i).cloned() else {
        return;
    };
    browse::open_source(ui, app, browse::Source::ArtistTop(artist));
}

fn current(app: &App, gen: u64) -> bool {
    app.list_gen == gen
}
fn plain(s: &str) -> String {
    let mut p = Playlist::new("", "", "");
    p.description = Some(s.replace("<br>", "\n").replace("<br/>", "\n"));
    p.plain_description().unwrap_or_default()
}
fn set_group(app: &mut App, kind: usize, items: Vec<Destination>) {
    let mut seen = HashSet::new();
    let items: Vec<_> = items
        .into_iter()
        .filter(|item| seen.insert(item.uri().to_owned()))
        .collect();
    let model = app.catalog.groups[kind].clone();
    let limit = app.catalog.limits[kind];
    app.catalog.more.set_row_data(kind, items.len() > limit);
    model.set_vec(
        items
            .iter()
            .take(limit)
            .map(|item| {
                let (t, s, _) = item.card();
                CardEntry {
                    title: t.into(),
                    subtitle: s.into(),
                    cover: Image::default(),
                }
            })
            .collect::<Vec<_>>(),
    );
    let jobs: Vec<_> = items
        .iter()
        .take(limit)
        .enumerate()
        .filter_map(|(i, item)| item.card().2.map(|url| (i, item.uri().to_owned(), url)))
        .collect();
    app.catalog.targets[kind] = items;
    let gen = app.list_gen;
    for (i, uri, url) in jobs {
        load_cover(app, &url, 160, move |_, app, d| {
            if !current(app, gen)
                || app.catalog.targets[kind]
                    .get(i)
                    .is_none_or(|target| target.uri() != uri)
            {
                return;
            }
            if let Some(mut row) = app.catalog.groups[kind].row_data(i) {
                row.cover = if kind == 3 { to_image(&images::circle(d)) } else { to_image(d) };
                app.catalog.groups[kind].set_row_data(i, row);
            }
        });
    }
}
fn rebuild_releases(app: &mut App, excluded: Option<&str>) {
    let albums = spotify_core::catalog::releases(&app.catalog.releases, "album", excluded);
    let singles = spotify_core::catalog::releases(&app.catalog.releases, "single", excluded);
    let mut collabs =
        spotify_core::catalog::releases(&app.catalog.releases, "appears_on", excluded);
    collabs.extend(app.catalog.collaborations.clone());
    let mut own = albums.clone();
    own.extend(singles.clone());
    own.sort_by(|a, b| b.release_date.cmp(&a.release_date));
    set_group(app, 0, albums.into_iter().map(Destination::Album).collect());
    set_group(
        app,
        1,
        singles.into_iter().map(Destination::Album).collect(),
    );
    set_group(
        app,
        2,
        collabs.into_iter().map(Destination::Album).collect(),
    );
    set_group(
        app,
        5,
        own.into_iter().take(1).map(Destination::Album).collect(),
    );
}
fn call(engine: &Engine, cmd: &str, args: Value) -> Result<Value, String> {
    engine.call(cmd, args, Duration::from_secs(60))
}
fn profile_cache_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = engine::data_dir().join("profile-cache");
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
}
fn profile_cache(key: &str) -> std::path::PathBuf {
    let hash = key.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    profile_cache_dir().join(format!("{hash:016x}.json"))
}
fn mem_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, (Instant, Value)>> {
    static MEM: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (Instant, Value)>>> = std::sync::OnceLock::new();
    MEM.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}
fn mem_get(key: &str) -> Option<(Value, bool)> {
    let guard = mem_cache().lock().ok()?;
    let (at, val) = guard.get(key)?;
    Some((val.clone(), at.elapsed() < Duration::from_secs(600)))
}
fn mem_put(key: String, val: Value) {
    let Ok(mut guard) = mem_cache().lock() else { return };
    if guard.len() >= 48 && !guard.contains_key(&key) {
        if let Some(oldest) = guard.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone()) {
            guard.remove(&oldest);
        }
    }
    guard.insert(key, (Instant::now(), val));
}
/// Datos del perfil en RAM y disco: lo último conocido se publica al instante y se actualiza
/// en segundo plano solo si tiene más de diez minutos. Un fallo de red nunca borra lo guardado.
pub(crate) fn cached_call(engine: &Engine, cmd: &str, args: Value, publish: impl Fn(Result<Value, String>)) {
    let key = format!("{cmd}|{args}");
    let path = profile_cache(&key);
    let mut cached = if let Some((v, fresh)) = mem_get(&key) {
        publish(Ok(v.clone()));
        if fresh {
            return;
        }
        Some(v)
    } else {
        None
    };
    if cached.is_none() {
        if let Some(v) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            publish(Ok(v.clone()));
            mem_put(key.clone(), v.clone());
            // Una visita reciente ya tiene datos frescos; evita repetir todas las peticiones del perfil.
            if std::fs::metadata(&path).and_then(|m| m.modified()).ok()
                .and_then(|t| t.elapsed().ok()).is_some_and(|age| age < Duration::from_secs(600)) {
                return;
            }
            cached = Some(v);
        }
    }
    match call(engine, cmd, args) {
        Ok(fresh) => {
            mem_put(key, fresh.clone());
            if cached.as_ref() != Some(&fresh) {
                if let Ok(bytes) = serde_json::to_vec(&fresh) {
                    let _ = std::fs::write(&path, bytes);
                }
                publish(Ok(fresh));
            }
        }
        Err(e) if cached.is_none() => publish(Err(e)),
        Err(e) => log(&format!("{cmd}: se mantiene la copia guardada ({e})")),
    }
}
fn artist_items(value: &Value, key: &str) -> Vec<ArtistItem> {
    value
        .get(key)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

pub fn apply_top_playcounts(app: &mut App) {
    if !matches!(app.current_source, Some(browse::Source::ArtistTop(_)))
        || app.catalog.top_playcounts.is_empty()
    {
        return;
    }
    for i in 0..app.rows.row_count() {
        if let Some(mut row) = app.rows.row_data(i) {
            let count = app
                .catalog
                .top_playcounts
                .get(row.uri.as_str())
                .copied()
                .map(spotify_core::text::format_count)
                .unwrap_or_default();
            if row.playcount.as_str() != count {
                row.playcount = count.into();
                app.rows.set_row_data(i, row);
            }
        }
    }
}

pub fn load_artist(app: &mut App, artist: ArtistItem) {
    media::load(app, artist.name.clone(), None);
    media::videos(app, artist.name.clone());
    let Some(engine) = app.engine.clone() else {
        return;
    };
    let gen = app.list_gen;
    if let Some(ui) = app.ui.upgrade() {
        ui.set_catalog_notice("Cargando perfil y discografía…".into());
    }
    let id = artist.id.clone();
    let e = engine.clone();
    let who = artist.clone();
    std::thread::spawn(move || cached_call(&e, "artists", json!({"ids":[id]}), |value| {
        let artist = who.clone();
        on_ui(move |ui, app| {
            if !current(app, gen) {
                return;
            }
            let Ok(value) = value else {
                ui.set_catalog_notice("No se pudo cargar la información del artista.".into());
                return;
            };
            let Some(detail) = value
                .get("artists")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
            else {
                return;
            };
            // La foto ya llegó desde la tarjeta que se pulsó: no se descarga ni se difumina dos veces.
            let missing = ui.get_list_cover().size().width == 0;
            if let Some(url) = detail.pointer("/images/0/url").and_then(Value::as_str).filter(|_| missing) {
                let url = url.to_owned();
                load_cover(app, &url, 400, move |ui, app, d| {
                    if current(app, gen) {
                        apply_artist_photo(ui, app, d);
                    }
                });
            }
            let bio = detail
                .get("biography")
                .and_then(Value::as_str)
                .map(plain)
                .unwrap_or_default();
            if !bio.is_empty() {
                set_bio(ui, &bio);
                ui.set_artist_bio_source("".into());
            } else {
                let name = artist.name.clone();
                std::thread::spawn(move || {
                    let info = knowledge::artist(&name);
                    on_ui(move |ui, app| {
                        if current(app, gen) && ui.get_artist_bio().is_empty() {
                            set_bio(ui, &info.text);
                            ui.set_artist_bio_source(info.url.into());
                        }
                    });
                });
            }
            if app.catalog.targets[3].is_empty() {
                set_group(
                    app,
                    3,
                    artist_items(detail, "related_artists")
                        .into_iter()
                        .map(Destination::Artist)
                        .collect(),
                );
            }
        });
    }));
    let e = engine.clone();
    let id = artist.id.clone();
    std::thread::spawn(move || cached_call(&e, "artist_albums", json!({"target":id}), |result| {
        let result = result
            .and_then(|v| serde_json::from_value::<Page<AlbumItem>>(v).map_err(|e| e.to_string()));
        on_ui(move |ui, app| {
            if !current(app, gen) {
                return;
            }
            match result {
                Ok(page) => {
                    app.catalog.releases = page.items;
                    rebuild_releases(app, None);
                    ui.set_catalog_notice("".into());
                }
                Err(e) => {
                    log(&e);
                    ui.set_catalog_notice("No se pudo cargar la discografía. Puedes volver a abrir el artista para reintentar.".into());
                }
            }
        });
    }));
    let e = engine.clone();
    let id = artist.id.clone();
    std::thread::spawn(move || cached_call(&e, "artist_related", json!({"target":id}), |result| {
        on_ui(move |_, app| {
            if current(app, gen) {
                if let Ok(value) = result {
                    let related = artist_items(&value, "artists");
                    if !related.is_empty() {
                        set_group(
                            app,
                            3,
                            related.into_iter().map(Destination::Artist).collect(),
                        );
                    }
                }
            }
        });
    }));
    let id = artist.id.clone();
    std::thread::spawn(move || cached_call(&engine, "artist_overview", json!({"target":id, "v":2}), |result| {
        on_ui(move |ui, app| {
            if !current(app, gen) {
                return;
            }
            let Ok(value) = result else {
                return;
            };
            let format_number = |n: u64| -> String {
                let s = n.to_string();
                let mut out = String::with_capacity(s.len() + s.len() / 3);
                for (i, c) in s.chars().enumerate() {
                    if i > 0 && (s.len() - i) % 3 == 0 {
                        out.push('.');
                    }
                    out.push(c);
                }
                out
            };
            let mut stats = vec![];
            if let Some(n) = value.get("monthly_listeners").and_then(Value::as_u64) {
                stats.push(format!("{} oyentes mensuales", format_number(n)));
            }
            if let Some(n) = value.get("followers").and_then(Value::as_u64) {
                stats.push(format!("{} seguidores", format_number(n)));
            }
            ui.set_artist_stats(stats.join(" · ").into());
            if let Some(rank) = value.get("world_rank").and_then(Value::as_u64) {
                ui.set_artist_rank(format!("#{rank}").into());
            } else {
                ui.set_artist_rank("".into());
            }
            if let Some(bio) = value
                .get("biography")
                .and_then(Value::as_str)
                .map(plain)
                .filter(|s| !s.is_empty())
            {
                set_bio(ui, &bio);
                ui.set_artist_bio_source("".into());
            }
            if let Ok(overview) = serde_json::from_value::<ArtistOverview>(value) {
                if !overview.top_playcounts.is_empty() {
                    app.catalog.top_playcounts = overview.top_playcounts;
                    apply_top_playcounts(app);
                }
                app.catalog.collaborations = overview.appears_on;
                rebuild_releases(app, None);
                set_group(
                    app,
                    4,
                    overview
                        .artist_playlists
                        .into_iter()
                        .chain(overview.featuring)
                        .chain(overview.discovered_on)
                        .map(Destination::Playlist)
                        .collect(),
                );
            }
        });
    }));
}

pub fn load_album(app: &mut App, album: AlbumItem) {
    let Some(engine) = app.engine.clone() else {
        return;
    };
    let gen = app.list_gen;
    let extras_started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    std::thread::spawn(move || {
        cached_call(&engine, "album_details", json!({"target":album.id}), |result| {
            let album = album.clone();
            let extras_started = extras_started.clone();
            on_ui(move |ui, app| {
                if !current(app, gen) {
                    return;
                }
                let value = result.unwrap_or_else(|e| {
                    log(&e);
                    json!({})
                });
                let artists: Vec<SpArtist> = value
                    .get("artists")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .filter(|a: &Vec<SpArtist>| !a.is_empty())
                    .unwrap_or_else(|| album.artists.clone().unwrap_or_default());
                let primary = artists.first().cloned();
                let primary_name = spotify_core::catalog::primary_artist(&artists);
                let all_names: Vec<&str> = artists.iter().map(|a| a.name.as_str()).filter(|n| !n.is_empty()).collect();
                ui.set_list_description(primary_name.clone().into());
                ui.set_album_artist(primary_name.clone().into());
                ui.set_list_header_artist(all_names.join(", ").into());
                ui.set_list_header_artist_clickable(artists.iter().any(|a| a.id.as_deref().is_some_and(|s| !s.is_empty())));
                app.catalog.album_artists = artists.clone();
                if let Some(browse::Source::Album(a)) = &mut app.current_source {
                    a.artists = Some(artists);
                }
                if let Some(name) = value.get("name").and_then(Value::as_str) {
                    ui.set_page_title(name.into());
                }
                match value.get("album_type").and_then(Value::as_str) {
                    Some("single") => ui.set_list_kind("SENCILLO / EP".into()),
                    Some("compilation") => ui.set_list_kind("RECOPILATORIO".into()),
                    Some("album") => ui.set_list_kind("ÁLBUM".into()),
                    _ => {}
                }
                if app.tracks.is_empty() {
                    if let Some(n) = value.get("total_tracks").and_then(Value::as_u64).filter(|n| *n > 0) {
                        ui.set_list_tracks_chip(format!("{n} {}", if n == 1 { "CANCIÓN" } else { "CANCIONES" }).into());
                    }
                }
                if !extras_started.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    if let Some(url) = value.pointer("/images/0/url").and_then(Value::as_str) {
                        let url = url.to_owned();
                        load_cover(app, &url, 400, move |ui, app, d| {
                            if current(app, gen) {
                                ui.set_list_cover(to_image(d));
                                browse::apply_list_colors(ui, d.color);
                            }
                        });
                    }
                    if let Some(artist) = primary.filter(|a| a.id.is_some()) {
                        load_album_related(app, artist.id.unwrap(), album.id.clone());
                    }
                    if !primary_name.is_empty() {
                        media::load(app, primary_name, Some(album.name.clone()));
                    }
                }
                let copyrights: Vec<String> = value
                    .get("copyrights")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c.get("text").and_then(Value::as_str).map(str::to_owned))
                    .filter(|t| !t.is_empty())
                    .collect();
                ui.set_album_copyright(copyrights.join("\n").into());
            });
        });
    });
}
fn load_album_related(app: &mut App, artist_id: String, album_id: String) {
    let Some(engine) = app.engine.clone() else {
        return;
    };
    let gen = app.list_gen;
    let e_avatar = engine.clone();
    let id_avatar = artist_id.clone();
    std::thread::spawn(move || {
        cached_call(&e_avatar, "artists", json!({"ids": [id_avatar]}), |result| {
            let url = result.ok().and_then(|v| {
                let imgs = v.pointer("/artists/0/images")?.as_array()?;
                imgs.last().or_else(|| imgs.first())?.get("url")?.as_str().map(str::to_owned)
            });
            let Some(url) = url else { return };
            on_ui(move |_, app| {
                if current(app, gen) {
                    load_cover(app, &url, 64, move |ui, app, d| {
                        if current(app, gen) {
                            ui.set_list_artist_avatar(to_image(&images::circle(d)));
                        }
                    });
                }
            });
        });
    });
    let e = engine.clone();
    let id = artist_id.clone();
    std::thread::spawn(move || {
        cached_call(&e, "artist_albums", json!({"target":id}), |result| {
            let result = result
                .and_then(|v| serde_json::from_value::<Page<AlbumItem>>(v).map_err(|e| e.to_string()));
            let album_id = album_id.clone();
            on_ui(move |_, app| {
                if current(app, gen) {
                    if let Ok(page) = result {
                        app.catalog.releases = page.items;
                        rebuild_releases(app, Some(&album_id));
                    }
                }
            });
        });
    });
    let id = artist_id.clone();
    std::thread::spawn(move || {
        cached_call(&engine, "artist_related", json!({"target":id}), |result| {
            on_ui(move |_, app| {
                if current(app, gen) {
                    if let Ok(v) = result {
                        set_group(
                            app,
                            3,
                            artist_items(&v, "artists")
                                .into_iter()
                                .map(Destination::Artist)
                                .collect(),
                        );
                    }
                }
            });
        });
    });
}
pub fn refresh_suggestion_rows(app: &mut App) {
    app.catalog.suggestions =
        spotify_core::catalog::recommendations(&app.catalog.suggestions, &app.tracks);
    let playing = app.player.track.as_ref().map(|t| t.uri.as_str());
    let mut rows = track_rows(&app.catalog.suggestions, playing);
    for row in &mut rows {
        row.saved = app
            .actions
            .saved
            .get(row.uri.as_str())
            .copied()
            .unwrap_or(false);
    }
    app.catalog.suggestion_model.set_vec(rows);
    let gen = app.list_gen;
    let jobs: Vec<_> = app
        .catalog
        .suggestions
        .iter()
        .enumerate()
        .filter_map(|(i, t)| t.image_url().map(|url| (i, t.uri.clone(), url.to_owned())))
        .collect();
    for (i, uri, url) in jobs {
        load_cover(app, &url, 44, move |_, app, d| {
            if current(app, gen) {
                if let Some(mut row) = app
                    .catalog
                    .suggestion_model
                    .row_data(i)
                    .filter(|r| r.uri.as_str() == uri)
                {
                    row.cover = to_image(d);
                    app.catalog.suggestion_model.set_row_data(i, row);
                }
            }
        });
    }
    actions::request_saved(app, app.catalog.suggestions.clone());
}
pub fn load_suggestions(ui: &MainWindow, app: &mut App, force: bool) {
    if !matches!(app.current_source, Some(browse::Source::Playlist(_))) {
        return;
    }
    if app.catalog.suggestion_gen == Some(app.list_gen) && (!force || ui.get_suggestions_loading())
    {
        return;
    }
    let Some(engine) = app.engine.clone() else {
        return;
    };
    let gen = app.list_gen;
    app.catalog.suggestion_gen = Some(gen);
    let exclude: Vec<_> = app.tracks.iter().map(|t| t.uri.clone()).collect();
    let mut seeds = vec![];
    if !exclude.is_empty() {
        for i in [0, exclude.len() / 2, exclude.len() - 1] {
            if !seeds.contains(&exclude[i]) {
                seeds.push(exclude[i].clone());
            }
        }
    }
    ui.set_suggestions_loading(true);
    ui.set_suggestion_message("Buscando canciones para esta playlist…".into());
    std::thread::spawn(move || {
        if seeds.is_empty() {
            if let Ok(v) = call(&engine, "liked_items", json!({"offset":0,"limit":5})) {
                seeds = v
                    .get("items")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|t| {
                        t.pointer("/track/uri")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .take(3)
                    .collect();
            }
        }
        let result = call(
            &engine,
            "recommend",
            json!({"seeds":seeds,"exclude":exclude,"limit":12}),
        )
        .and_then(|v| serde_json::from_value::<TopTracksResponse>(v).map_err(|e| e.to_string()));
        on_ui(move |ui, app| {
            if !current(app, gen) {
                return;
            }
            ui.set_suggestions_loading(false);
            match result {
                Ok(r) => {
                    app.catalog.suggestions = r.tracks;
                    refresh_suggestion_rows(app);
                    ui.set_suggestion_message(if app.catalog.suggestions.is_empty(){"No hay recomendaciones disponibles por ahora. Añade algunas canciones y vuelve a actualizar."}else{"Basadas en las canciones de esta playlist."}.into());
                }
                Err(e) => {
                    log(&e);
                    ui.set_suggestion_message("No se pudieron cargar las recomendaciones. Pulsa Actualizar para reintentar.".into());
                }
            }
        });
    });
}

pub fn load_pins(app: &mut App) {
    let Some(engine) = app.engine.clone() else {
        return;
    };
    let epoch = app.catalog.pins_epoch;
    std::thread::spawn(move || {
        let result = call(&engine, "pins", json!({}));
        on_ui(move |_, app| {
            if epoch != app.catalog.pins_epoch || !app.catalog.pin_pending.is_empty() {
                return;
            }
            if let Ok(v) = result {
                if let Ok(pins) = serde_json::from_value::<Vec<String>>(v) {
                    app.catalog.pins = pins;
                    let lists = app.playlists.clone();
                    browse::show_playlists(app, lists);
                    if let Some(ui) = app.ui.upgrade() {
                        update_pin_state(&ui, app);
                    }
                }
            }
        });
    });
}
pub fn pin_target(app: &App) -> Option<String> {
    match &app.current_source {
        Some(browse::Source::Playlist(p)) => Some(p.uri.clone()),
        Some(browse::Source::Liked) => Some(format!(
            "spotify:user:{}:collection",
            app.username.as_deref()?
        )),
        _ => None,
    }
}
pub fn update_pin_state(ui: &MainWindow, app: &App) {
    let target = pin_target(app);
    ui.set_list_can_pin(target.is_some());
    ui.set_list_pinned(
        target.as_ref().is_some_and(|uri| {
            spotify_core::catalog::pin_position(&app.catalog.pins, uri).is_some()
        }),
    );
}
pub fn toggle_pin(_ui: &MainWindow, app: &mut App) {
    let Some(uri) = pin_target(app) else {
        return;
    };
    if !app.catalog.pin_pending.insert(uri.clone()) {
        return;
    }
    let Some(engine) = app.engine.clone() else {
        app.catalog.pin_pending.remove(&uri);
        return;
    };
    app.catalog.pins_epoch += 1;
    let pinned = spotify_core::catalog::pin_position(&app.catalog.pins, &uri).is_some();
    let existing = spotify_core::catalog::pin_position(&app.catalog.pins, &uri)
        .and_then(|i| app.catalog.pins.get(i))
        .cloned()
        .unwrap_or_else(|| uri.clone());
    std::thread::spawn(move || {
        let result = call(
            &engine,
            "set_pinned",
            json!({"uris":[existing],"pinned":!pinned}),
        );
        on_ui(move |ui, app| {
            app.catalog.pin_pending.remove(&uri);
            match result {
                Ok(_) => {
                    app.catalog.pins.retain(|p| {
                        p != &uri
                            && !(spotify_core::catalog::is_collection(p)
                                && spotify_core::catalog::is_collection(&uri))
                    });
                    if !pinned {
                        app.catalog.pins.insert(0, uri);
                    }
                    let lists = app.playlists.clone();
                    browse::show_playlists(app, lists);
                    update_pin_state(ui, app);
                    queue_ui::toast(
                        ui,
                        app,
                        if pinned {
                            "Ya no está fijada"
                        } else {
                            "Fijada en Tu biblioteca"
                        },
                    );
                }
                Err(e) => {
                    log(&e);
                    queue_ui::toast(ui, app, "No se pudo cambiar el fijado. Inténtalo de nuevo.");
                }
            }
        });
    });
}
fn open_group(ui: &MainWindow, app: &mut App, kind: usize, index: usize) {
    let Some(target) = app
        .catalog
        .targets
        .get(kind)
        .and_then(|group| group.get(index))
        .cloned()
    else {
        return;
    };
    let source = match target {
        Destination::Album(a) => browse::Source::Album(a),
        Destination::Artist(a) => browse::Source::ArtistTop(a),
        Destination::Playlist(p) => browse::Source::Playlist(p),
    };
    browse::open_source(ui, app, source);
}
fn source_url(url: &str) -> bool {
    [
        "https://es.wikipedia.org/wiki/",
        "https://en.wikipedia.org/wiki/",
    ]
    .iter()
    .any(|prefix| url.starts_with(prefix))
}
pub fn wire(ui: &MainWindow) {
    ui.on_play_suggestion(|index| { with_app(|_, app| {
        if let Some(track) = app.catalog.suggestions.get(index as usize).cloned() {
            play_req(app, PlayReq::Track(track.uri));
        }
    }); });
    ui.on_more_catalog(|kind| {
        with_app(|_, app| {
            let kind = kind as usize;
            if kind < 6 {
                app.catalog.limits[kind] += 12;
                let items = app.catalog.targets[kind].clone();
                set_group(app, kind, items);
            }
        });
    });
    ui.on_open_catalog(|kind, index| {
        with_app(|ui, app| open_group(ui, app, kind as usize, index as usize));
    });
    ui.on_open_list_artist(|index| {
        with_app(|ui, app| open_list_artist(ui, app, index as usize));
    });
    ui.on_refresh_suggestions(|| {
        with_app(|ui, app| load_suggestions(ui, app, true));
    });
    ui.on_add_suggestion(|index| {
        with_app(|ui, app| {
            let Some(track) = app.catalog.suggestions.get(index as usize).cloned() else {
                return;
            };
            let Some(browse::Source::Playlist(p)) = app.current_source.clone() else {
                return;
            };
            if actions::can_edit(app, &p) {
                actions::add_to_target(ui, app, p, track);
            }
        });
    });
    ui.on_open_catalog_source(|| {
        with_app(|ui, _| {
            let url = ui.get_artist_bio_source();
            if source_url(&url) {
                open_in_browser(&url);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_cache_stores_fresh_values_and_stays_bounded() {
        for i in 0..60 {
            mem_put(format!("test-key-{i}"), json!({ "idx": i }));
        }
        assert!(mem_cache().lock().unwrap().len() <= 48);
        let (val, fresh) = mem_get("test-key-59").expect("latest entry cached");
        assert_eq!(val["idx"], 59);
        assert!(fresh);
    }
}
