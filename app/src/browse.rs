//! Navegación por contenido: biblioteca, listas de canciones (playlist, álbum, favoritas, populares) y búsqueda.

use super::*;
use spotify_core::search_ranking;
use spotify_core::{AlbumItem, ArtistItem, SavedTrack, SearchResponse, SearchResults, SpAlbum, TopTracksResponse};

/// Qué lista de canciones se muestra.
#[derive(Clone)]
pub enum Source {
    Liked,
    Playlist(Playlist),
    Album(AlbumItem),
    ArtistTop(ArtistItem),
}

#[derive(Clone, Copy)]
pub enum Cards {
    Home,
    Made,
    Mixes,
    Artists,
    Albums,
    Playlists,
}

/// «Mejor resultado» de una búsqueda.
#[derive(Clone)]
enum TopHit {
    Artist(ArtistItem),
    Track(Track),
    Album(AlbumItem),
    Playlist(Playlist),
}

const SEARCH_PAGE: usize = 20;
const MORE_TRACKS_PAGE: usize = 50;
const MAX_RECENTS: usize = 8;

pub struct SearchState {
    gen: u64,
    timer: Timer,
    /// Consulta normalizada de los resultados mostrados.
    query: String,
    top: Option<TopHit>,
    more_tracks: bool,
    loading_more: bool,
    recents: Vec<String>,
    pub m_recents: Rc<VecModel<slint::SharedString>>,
    cache: std::collections::HashMap<String, (Instant, SearchResults)>,
    tracks: Vec<Track>,
    artists: Vec<ArtistItem>,
    albums: Vec<AlbumItem>,
    playlists: Vec<Playlist>,
    pub m_tracks: Rc<VecModel<TrackEntry>>,
    pub m_artists: Rc<VecModel<CardEntry>>,
    pub m_albums: Rc<VecModel<CardEntry>>,
    pub m_playlists: Rc<VecModel<CardEntry>>,
}

impl SearchState {
    /// Canción de los resultados en esa posición.
    pub fn track(&self, i: usize) -> Option<Track> {
        self.tracks.get(i).cloned()
    }

    pub fn new() -> Self {
        let recents = load_recents();
        SearchState {
            gen: 0,
            timer: Timer::default(),
            query: String::new(),
            top: None,
            more_tracks: false,
            loading_more: false,
            m_recents: Rc::new(VecModel::from(recents.iter().map(|q| q.as_str().into()).collect::<Vec<slint::SharedString>>())),
            recents,
            cache: Default::default(),
            tracks: vec![],
            artists: vec![],
            albums: vec![],
            playlists: vec![],
            m_tracks: Rc::default(),
            m_artists: Rc::default(),
            m_albums: Rc::default(),
            m_playlists: Rc::default(),
        }
    }
}

fn model_for(app: &App, which: Cards) -> Rc<VecModel<CardEntry>> {
    match which {
        Cards::Home => app.cards.clone(),
        Cards::Made => app.made.clone(),
        Cards::Mixes => app.mixes.clone(),
        Cards::Artists => app.search.m_artists.clone(),
        Cards::Albums => app.search.m_albums.clone(),
        Cards::Playlists => app.search.m_playlists.clone(),
    }
}

/// Rellena una rejilla de tarjetas y pide las portadas (si una fila cambió mientras tanto, se descarta).
pub fn fill_cards(app: &mut App, which: Cards, items: Vec<(String, String, Option<String>)>) {
    let model = model_for(app, which);
    model.set_vec(
        items
            .iter()
            .map(|(t, s, _)| CardEntry { title: t.clone().into(), subtitle: s.clone().into(), cover: Default::default() })
            .collect::<Vec<_>>(),
    );
    for (i, (title, _, url)) in items.into_iter().enumerate() {
        let Some(url) = url else { continue };
        load_cover(app, &url, 180, move |_ui, app, d| {
            let model = model_for(app, which);
            if let Some(mut row) = model.row_data(i) {
                if row.title.as_str() == title {
                    row.cover = if matches!(which, Cards::Artists) { to_image(&images::circle(d)) } else { to_image(d) };
                    model.set_row_data(i, row);
                }
            }
        });
    }
}

// ---------- Biblioteca ----------

/// Los fijados aparecen primero; cada fila conserva su URI aunque cambie de posición.
pub fn show_playlists(app: &mut App, playlists: Vec<Playlist>) {
    app.playlists = playlists;
    rebuild_library(app);
}

/// Reconstruye «Tu biblioteca»: Canciones que te gustan, playlists (marcando las tuyas),
/// álbumes guardados y artistas seguidos. `kind` alimenta los filtros de la barra lateral.
pub fn rebuild_library(app: &mut App) {
    let covers: std::collections::HashMap<String, Image> = (0..app.library.row_count())
        .filter_map(|i| app.library.row_data(i))
        .map(|row| (row.uri.to_string(), row.cover))
        .collect();
    let me = app.username.clone().unwrap_or_default();
    let pinned = |uri: &str| spotify_core::catalog::pin_position(&app.catalog.pins, uri).is_some();
    let liked = format!("spotify:user:{me}:collection");
    let mut entries = vec![LibraryEntry {
        uri: liked.clone().into(), title: "Canciones que te gustan".into(), subtitle: "Playlist".into(),
        cover: Image::default(), pinned: pinned(&liked), active: false, special: true, kind: "liked".into(),
    }];
    entries.extend(app.playlists.iter().map(|p| {
        let owner_id = p.owner.as_ref().map(|o| o.id.as_str()).unwrap_or("");
        let owner = p.owner.as_ref().and_then(|o| o.display_name.as_deref()).unwrap_or("");
        let mine = !me.is_empty() && owner_id == me;
        LibraryEntry {
            uri: p.uri.clone().into(), title: p.name.clone().into(),
            subtitle: if owner.is_empty() { "Playlist".into() } else { format!("Playlist · {owner}").into() },
            cover: covers.get(&p.uri).cloned().unwrap_or_default(), pinned: pinned(&p.uri),
            active: false, special: false, kind: if mine { "own" } else { "playlist" }.into(),
        }
    }));
    entries.extend(app.lib_albums.iter().map(|p| LibraryEntry {
        uri: p.uri.clone().into(), title: p.name.clone().into(),
        subtitle: match &p.artist_name { Some(a) => format!("Álbum · {a}"), None => "Álbum".into() }.into(),
        cover: covers.get(&p.uri).cloned().unwrap_or_default(), pinned: pinned(&p.uri),
        active: false, special: false, kind: "album".into(),
    }));
    entries.extend(app.lib_artists.iter().map(|a| LibraryEntry {
        uri: a.uri.clone().into(), title: a.name.clone().into(), subtitle: "Artista".into(),
        cover: covers.get(&a.uri).cloned().unwrap_or_default(), pinned: pinned(&a.uri),
        active: false, special: false, kind: "artist".into(),
    }));
    entries.sort_by_key(|entry| spotify_core::catalog::pin_position(&app.catalog.pins, entry.uri.as_str()).unwrap_or(usize::MAX));
    app.library.set_vec(entries);
    highlight_library(app, None);
    let mut jobs: Vec<(String, String, bool)> = app.playlists.iter()
        .filter_map(|p| p.image_url().map(|u| (p.uri.clone(), u.to_owned(), false))).collect();
    jobs.extend(app.lib_albums.iter().filter_map(|p| p.image_url().map(|u| (p.uri.clone(), u.to_owned(), false))));
    jobs.extend(app.lib_artists.iter().filter_map(|a| {
        a.images.as_ref().and_then(|i| i.first()).map(|i| (a.uri.clone(), i.url.clone(), true))
    }));
    for (uri, url, round) in jobs {
        if covers.get(&uri).is_some_and(|c| c.size().width > 0) { continue; }
        if round { load_library_avatar(app, uri, url); } else { load_library_cover(app, uri, url); }
    }
    fill_missing_covers(app);
}

/// Foto redonda de un artista seguido.
fn load_library_avatar(app: &App, uri: String, url: String) {
    load_cover(app, &url, 96, move |_, app, d| {
        for i in 0..app.library.row_count() {
            if let Some(mut row) = app.library.row_data(i).filter(|r| r.uri.as_str() == uri) {
                row.cover = to_image(&images::circle(d));
                app.library.set_row_data(i, row);
                break;
            }
        }
    });
}

fn load_library_cover(app: &App, uri: String, url: String) {
    load_cover(app, &url, 48, move |_, app, d| {
        for i in 0..app.library.row_count() {
            if let Some(mut row) = app.library.row_data(i).filter(|r| r.uri.as_str() == uri) {
                row.cover = to_image(d);
                app.library.set_row_data(i, row);
                break;
            }
        }
    });
}

fn mosaic_cache(id: &str) -> std::path::PathBuf {
    let dir = engine::data_dir().join("profile-cache");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("cover-{id}.txt"))
}

/// Mosaico de 4 portadas de álbum distintas (como Spotify) o, si no hay 4, la del primer álbum.
fn mosaic(urls: &[String]) -> Option<String> {
    const HOST: &str = "https://i.scdn.co/image/";
    let mut distinct: Vec<&String> = vec![];
    for u in urls { if !distinct.contains(&u) { distinct.push(u); } }
    if distinct.len() >= 4 {
        if let Some(ids) = distinct[..4].iter().map(|u| u.strip_prefix(HOST)).collect::<Option<Vec<_>>>() {
            return Some(format!("https://mosaic.scdn.co/640/{}", ids.concat()));
        }
    }
    distinct.first().map(|u| u.to_string())
}

/// Playlists que el motor devolvió sin portada (solo calcula unas pocas y con prisa): se calcula aquí
/// el mosaico a partir de sus canciones y se guarda en disco para que la próxima vez salga al instante.
fn fill_missing_covers(app: &mut App) {
    let missing: Vec<(String, String)> = app.playlists.iter()
        .filter(|p| p.image_url().is_none())
        .map(|p| (p.uri.clone(), p.id.clone())).collect();
    if missing.is_empty() { return; }
    let engine = app.engine.clone();
    std::thread::spawn(move || {
        for (uri, id) in missing {
            let path = mosaic_cache(&id);
            let mut url = std::fs::read_to_string(&path).ok().filter(|u| u.starts_with("https://"));
            if url.is_none() {
                let Some(engine) = engine.as_ref() else { break };
                let page = engine.call("playlist_items", json!({"target": id, "offset": 0, "limit": 30}), Duration::from_secs(20)).ok()
                    .and_then(|v| serde_json::from_value::<Page<PlaylistItem>>(v).ok());
                let covers: Vec<String> = page.map(|p| p.items.iter().filter_map(|i| i.resolved().and_then(|t| t.image_url()).map(str::to_owned)).collect()).unwrap_or_default();
                url = mosaic(&covers);
                if let Some(u) = &url { let _ = std::fs::write(&path, u); }
            }
            let Some(url) = url else { continue };
            on_ui(move |_, app| {
                if let Some(p) = app.playlists.iter_mut().find(|p| p.uri == uri) {
                    p.images = Some(vec![spotify_core::SpImage { url: url.clone() }]);
                }
                load_library_cover(app, uri, url);
            });
        }
    });
}
pub fn open_library_row(ui:&MainWindow,app:&mut App,row:usize){
    let Some(entry)=app.library.row_data(row)else{return;};
    if entry.special{open_source(ui,app,Source::Liked);}
    else if entry.kind=="album"{if let Some(p)=app.lib_albums.iter().find(|p|p.uri==entry.uri.as_str()).cloned(){open_source(ui,app,Source::Playlist(p));}}
    else if entry.kind=="artist"{if let Some(a)=app.lib_artists.iter().find(|a|a.uri==entry.uri.as_str()).cloned(){open_source(ui,app,Source::ArtistTop(ArtistItem::new(&a.id,&a.name)));}}
    else if let Some(p)=app.playlists.iter().find(|p|p.uri==entry.uri.as_str()).cloned(){open_source(ui,app,Source::Playlist(p));}
}
fn highlight_library(app:&App,_active:Option<usize>){
    for i in 0..app.library.row_count(){if let Some(mut row)=app.library.row_data(i){
        let active=match &app.current_source{Some(Source::Liked)=>row.special,Some(Source::Playlist(p))=>row.uri.as_str()==p.uri,Some(Source::ArtistTop(a))=>row.kind=="artist"&&row.uri.as_str().ends_with(&a.id),_=>false};
        if row.active!=active{row.active=active;app.library.set_row_data(i,row);}
    }}
}
/// Recargar tras una escritura también invalida las páginas que todavía estaban llegando.
pub fn reload_playlist(ui:&MainWindow,app:&mut App,id:&str){
    let Some(Source::Playlist(p))=app.current_source.clone().filter(|s|matches!(s,Source::Playlist(p) if p.id==id))else{return;};
    let (query,sort,ascending,panel)=(ui.get_list_filter(),ui.get_list_sort(),ui.get_list_ascending(),ui.get_detail_panel());
    open_source(ui,app,Source::Playlist(p));
    ui.set_list_filter(query);ui.set_list_sort(sort);ui.set_list_ascending(ascending);ui.set_detail_panel(panel);
}

// ---------- Listas de canciones ----------

pub(crate) fn apply_list_colors(ui: &MainWindow, color: Option<color::Rgb>) {
    if let Some(c) = color {
        ui.set_list_accent(slint::Color::from_rgb_u8(c[0], c[1], c[2]));
        let [r, g, b] = color::deep(c);
        ui.set_list_tint(slint::Color::from_rgb_u8(r, g, b));
        let [pr, pg, pb] = color::pill(c);
        ui.set_list_pill(slint::Color::from_rgb_u8(pr, pg, pb));
    }
}

fn apply_list_cover(ui: &MainWindow, d: &Decoded) {
    ui.set_list_cover(to_image(d));
    if ui.get_list_editorial() {
        ui.set_list_cover_blur(to_image(&images::editorial_blur(d)));
    }
    apply_list_colors(ui, d.color);
}

pub fn open_radio(ui: &MainWindow, app: &mut App, seed_uri: String, title: String, subtitle: String, cover_url: Option<String>) {
    push_history(ui, app);
    media::reset(ui, app);
    app.current_source = None;
    app.context = None;
    app.tracks.clear();
    ui.set_list_filter("".into());
    ui.set_list_sort(0);
    ui.set_list_ascending(true);
    ui.set_list_is_album(false);
    ui.set_list_is_liked(false);
    ui.set_list_is_playlist(false);
    ui.set_list_can_edit(false);
    ui.set_list_kind("RADIO".into());
    ui.set_list_description(subtitle.into());
    ui.set_list_summary("".into());
    ui.set_list_tracks_chip("".into());
    ui.set_list_header_artist("".into());
    ui.set_list_header_artist_clickable(false);
    ui.set_list_artist_avatar(Image::default());
    ui.set_list_editorial(false);
    ui.set_has_list_banner(false);
    ui.set_list_banner(Image::default());
    ui.set_list_cover_blur(Image::default());
    ui.set_list_cover(Image::default());
    apply_list_colors(ui, None);
    ui.set_list_message("Generando radio…".into());
    ui.set_list_scroll(0.0);
    ui.set_page(2);
    ui.set_page_title(title.into());
    app.list_gen += 1;
    let gen = app.list_gen;
    if let Some(url) = cover_url {
        load_cover(app, &url, 400, move |ui, app, d| {
            if app.list_gen == gen {
                apply_list_cover(ui, d);
            }
        });
    }
    refresh_rows(app);
    highlight_library(app, None);
    let Some(engine) = app.engine.clone() else {
        ui.set_list_message("Conecta con Spotify para cargar esta radio.".into());
        return;
    };
    std::thread::spawn(move || {
        let res = engine
            .call("recommend", json!({"seeds": [seed_uri], "limit": 30}), Duration::from_secs(25))
            .and_then(|v| serde_json::from_value::<spotify_core::TopTracksResponse>(v).map_err(|e| e.to_string()));
        on_ui(move |ui, app| {
            if app.list_gen != gen { return; }
            match res {
                Ok(resp) if !resp.tracks.is_empty() => {
                    let tracks = resp.tracks;
                    let duration: i64 = tracks.iter().map(|t| t.duration_ms as i64).sum();
                    let mins = duration / 60_000;
                    ui.set_list_message("".into());
                    ui.set_list_summary(format!("{} canciones · {} min", tracks.len(), mins).into());
                    ui.set_list_tracks_chip(
                        format!("{} {}", tracks.len(), if tracks.len() == 1 { "CANCIÓN" } else { "CANCIONES" }).into(),
                    );
                    if ui.get_list_cover().size().width == 0 {
                        let urls: Vec<String> = tracks.iter().take(30).filter_map(|t| t.image_url().map(str::to_owned)).collect();
                        if let Some(url) = mosaic(&urls) {
                            load_cover(app, &url, 400, move |ui, app, d| {
                                if app.list_gen == gen && ui.get_list_cover().size().width == 0 {
                                    apply_list_cover(ui, d);
                                }
                            });
                        }
                    }
                    let artists: Vec<ArtistItem> = tracks.iter()
                        .flat_map(|t| t.artists.iter())
                        .filter_map(|a| {
                            let id = a.id.as_deref().filter(|s| !s.is_empty())?;
                            Some(ArtistItem::new(id, &a.name))
                        })
                        .collect();
                    app.tracks = tracks;
                    refresh_rows(app);
                    catalog::set_list_artists(app, artists);
                }
                _ => ui.set_list_message("No se pudieron cargar recomendaciones para esta radio.".into()),
            }
        });
    });
}

pub fn open_source(ui: &MainWindow, app: &mut App, src: Source) {
    // Los recientes y el Inicio también representan álbumes como Playlist.
    let src = match src {
        Source::Playlist(p) if p.uri.starts_with("spotify:album:") => Source::Album(AlbumItem {
            id: p.uri.rsplit(':').next().unwrap_or(&p.id).to_owned(), uri: p.uri, name: p.name, images: p.images,
            artists: p.artist_name.map(|name| vec![spotify_core::SpArtist { name, id: p.artist_id }]),
            release_date: None, album_group: None, album_type: None,
        }),
        other => other,
    };
    let (kind, description, image) = match &src {
        Source::Liked => ("PLAYLIST", "Tus canciones favoritas en un solo lugar".to_owned(), None),
        Source::Playlist(p) => ("PLAYLIST", [p.owner.as_ref().and_then(|o| o.display_name.clone()).or(p.creator_display_name.clone()).unwrap_or_default(), p.plain_description().unwrap_or_default()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "), p.image_url().map(str::to_owned)),
        Source::Album(a) => ("ÁLBUM", spotify_core::catalog::primary_artist(a.artists.as_deref().unwrap_or_default()), a.image_url().map(str::to_owned)),
        Source::ArtistTop(a) => ("ARTISTA", "Canciones populares".to_owned(), a.image_url().map(str::to_owned)),
    };
    // El historial guarda la pantalla que se deja (con la fuente anterior todavía en `current_source`).
    let same = matches!(ui.get_page(), 2 | 3) && app.current_source.as_ref().is_some_and(|c| {
        crate::NavEntry { page: 0, source: Some(c.clone()) }.key() == crate::NavEntry { page: 0, source: Some(src.clone()) }.key()
    });
    if !same { crate::push_history(ui, app); }
    app.current_source = Some(src.clone());
    ui.set_artist_picker_open(false);
    catalog::reset(ui,app);
    ui.set_artist_show_all(false);
    ui.set_artist_catalog_tab(0);
    ui.set_list_is_album(matches!(&src,Source::Album(_)));
    ui.set_list_is_liked(matches!(&src, Source::Liked));
    ui.set_list_show_all(false);
    catalog::update_pin_state(ui,app);
    ui.set_list_filter("".into()); ui.set_list_sort(0); ui.set_list_ascending(true);
    ui.set_list_is_playlist(matches!(&src, Source::Playlist(_)));
    ui.set_list_can_edit(matches!(&src, Source::Playlist(p) if actions::can_edit(app, p)));
    ui.set_list_in_library(matches!(&src, Source::Playlist(p) if app.playlists.iter().any(|x| x.id == p.id)));
    ui.set_list_kind(kind.into());
    ui.set_list_description(description.into());
    ui.set_list_cover(Image::default());
    ui.set_list_cover_blur(Image::default());
    ui.set_list_banner(Image::default());
    ui.set_has_list_banner(false);
    // Playlists hechas por Spotify («This Is», mixes editoriales): cabecera fundida en lugar de portada cuadrada.
    ui.set_list_editorial(matches!(&src, Source::Playlist(p) if p.owner.as_ref().is_some_and(|o| o.id == "spotify") || p.name.starts_with("This Is")));
    if matches!(&src, Source::Liked) {
        ui.set_list_tint(slint::Color::from_rgb_u8(56, 46, 130));
        ui.set_list_pill(slint::Color::from_rgb_u8(84, 70, 170));
    } else {
        ui.set_list_tint(slint::Color::from_rgb_u8(42, 45, 62));
        ui.set_list_pill(slint::Color::from_rgb_u8(66, 72, 98));
    }
    ui.set_list_accent(slint::Color::from_rgb_u8(235, 235, 235));
    ui.set_artist_accent(slint::Color::from_rgb_u8(190, 190, 190));
    match &src {
        Source::Album(a) => {
            let artists = a.artists.clone().unwrap_or_default();
            let names: Vec<&str> = artists.iter().map(|x| x.name.as_str()).filter(|n| !n.is_empty()).collect();
            ui.set_list_header_artist(names.join(", ").into());
            ui.set_list_header_artist_clickable(artists.iter().any(|x| x.id.as_deref().is_some_and(|id| !id.is_empty())));
            app.catalog.album_artists = artists;
        }
        Source::Playlist(p) => {
            let owner = p.owner.as_ref().and_then(|o| o.display_name.clone()).or_else(|| p.creator_display_name.clone()).unwrap_or_default();
            ui.set_list_header_artist(owner.into());
            let owner_id = p.owner.as_ref().map(|o| o.id.clone()).filter(|id| !id.is_empty());
            ui.set_list_header_artist_clickable(owner_id.is_some());
            ui.set_list_artist_avatar(Image::default());
            if let (Some(id), Some(engine)) = (owner_id, app.engine.clone()) {
                let gen = app.list_gen + 1; // `list_gen` sube unas líneas más abajo
                std::thread::spawn(move || {
                    catalog::cached_call(&engine, "user_profile", json!({"user": id}), |reply| {
                        let url = reply.ok().and_then(|v| v.get("image_url").or_else(|| v.get("imageUrl")).and_then(Value::as_str).map(str::to_owned));
                        let Some(url) = url.map(|u| u.replace("spotify:image:", "https://i.scdn.co/image/")).filter(|u| u.starts_with("https://")) else { return };
                        on_ui(move |_, app| {
                            if app.list_gen != gen { return; }
                            load_cover(app, &url, 40, move |ui, app, d| {
                                if app.list_gen == gen { ui.set_list_artist_avatar(to_image(&images::circle(d))); }
                            });
                        });
                    });
                });
            }
        }
        Source::Liked => {
            let cached_name = ui.get_user_name().to_string();
            let raw_id = app.username.as_deref().unwrap_or("");
            let display = if !cached_name.is_empty() && cached_name != raw_id {
                cached_name
            } else {
                "Tu biblioteca".into()
            };
            ui.set_list_header_artist(display.into());
            ui.set_list_header_artist_clickable(false);
            if let Some(engine) = app.engine.clone() {
                let gen = app.list_gen + 1;
                std::thread::spawn(move || {
                    catalog::cached_call(&engine, "my_profile", json!({}), |reply| {
                        let Ok(v) = reply else { return };
                        let name = v.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
                        let url = v.get("image_url").or_else(|| v.get("imageUrl")).and_then(Value::as_str)
                            .map(|u| u.replace("spotify:image:", "https://i.scdn.co/image/"))
                            .filter(|u| u.starts_with("https://"));
                        on_ui(move |ui, app| {
                            if let Some(n) = &name { ui.set_user_name(n.clone().into()); }
                            if app.list_gen != gen { return; }
                            if let Some(n) = name { ui.set_list_header_artist(n.into()); }
                            if let Some(u) = url {
                                load_cover(app, &u, 40, move |ui, app, d| {
                                    if app.list_gen == gen { ui.set_list_artist_avatar(to_image(&images::circle(d))); }
                                });
                            }
                        });
                    });
                });
            }
        }
        Source::ArtistTop(_) => {}
    }
    ui.set_list_message("Cargando canciones…".into());
    ui.set_list_summary("".into());
    let (title, context, active) = match &src {
        // Se reproduce como la colección completa de Spotify («Tus me gusta»), no solo con las
        // canciones ya cargadas en pantalla: antes, al acabarse esas, el autoplay ponía canciones ajenas.
        Source::Liked => (
            "Canciones que te gustan".to_owned(),
            app.username.as_deref().filter(|u| !u.is_empty()).map(|u| format!("spotify:user:{u}:collection")),
            Some(0),
        ),
        Source::Playlist(p) => {
            (p.name.clone(), Some(p.uri.clone()), app.playlists.iter().position(|x| x.uri == p.uri).map(|i| i + 1))
        }
        Source::Album(a) => (a.name.clone(), Some(a.uri.clone()), None),
        Source::ArtistTop(a) => (a.name.clone(), None, None),
    };
    ui.set_list_scroll(0.0);
    ui.set_artist_scroll(0.0);
    ui.set_page(if matches!(&src,Source::ArtistTop(_)){3}else{2});
    ui.set_page_title(title.into());
    ui.set_list_scroll(0.0);
    ui.set_artist_scroll(0.0);
    app.context = context;
    app.tracks.clear();
    app.list_gen += 1;
    let gen = app.list_gen;
    let _ = slint::invoke_from_event_loop(move || {
        with_app(|ui, app| {
            if app.list_gen == gen {
                ui.set_list_scroll(0.0);
                ui.set_artist_scroll(0.0);
            }
        });
    });
    if let Some(url) = image {
        let artist_page = matches!(&src, Source::ArtistTop(_));
        // Se pide al tamaño en pantalla: el renderizador reduce mal las imágenes muy grandes.
        load_cover(app, &url, 400, move |ui, app, d| {
            if app.list_gen != gen { return; }
            if artist_page { return apply_artist_photo(ui, app, d); }
            apply_list_cover(ui, d);
        });
    }
    refresh_rows(app);
    highlight_library(app, active);
    let Some(engine) = app.engine.clone() else { ui.set_list_message("Conecta con Spotify para cargar esta lista.".into()); return };

    match &src {
        Source::Album(a) => catalog::load_album(app, a.clone()),
        Source::ArtistTop(a) => {
            catalog::load_artist(app, a.clone());
            settings::load_follow_state(ui, app, a.uri.clone());
        }
        _ => {}
    }
    // Playlists editoriales de Spotify («This Is»…): foto ancha de cabecera, como en la app de Mac.
    if let Source::Playlist(p) = &src {
        if p.uri.starts_with("spotify:playlist:") {
            let (engine, id) = (engine.clone(), p.id.clone());
            std::thread::spawn(move || {
                let header = engine.call("playlist_header", json!({"target": id}), Duration::from_secs(20)).ok()
                    .and_then(|v| serde_json::from_value::<spotify_core::PlaylistHeader>(v).ok());
                let Some(url) = header.and_then(|h| h.image_url()) else { return };
                on_ui(move |_, app| {
                    if app.list_gen != gen { return; }
                    load_cover(app, &url, 1200, move |ui, app, d| {
                        if app.list_gen != gen { return; }
                        ui.set_list_banner(to_image(&images::editorial_banner(d)));
                        ui.set_has_list_banner(true);
                        if ui.get_list_cover().size().width == 0 {
                            apply_list_colors(ui, d.color);
                        } else if let Some(c) = d.color {
                            let cur = ui.get_list_accent();
                            let (_, cur_s, _) = color::to_hsv([cur.red(), cur.green(), cur.blue()]);
                            let (_, new_s, _) = color::to_hsv(c);
                            if cur_s < 0.15 && new_s >= 0.25 {
                                apply_list_colors(ui, Some(c));
                            }
                        }
                    });
                });
            });
        }
    }
    std::thread::spawn(move || {
        // Cada página se publica según llega; si el usuario abrió otra lista, `gen` ya no coincide.
        let publish = |tracks: Vec<Track>| {
            on_ui(move |ui, app| {
                if app.list_gen == gen {
                    let duration: i64 = tracks.iter().map(|t| t.duration_ms).sum();
                    let mins = duration / 60_000;
                    ui.set_list_summary(format!("{} canciones · {} min", tracks.len(), mins).into());
                    ui.set_list_tracks_chip(
                        format!("{} {}", tracks.len(), if tracks.len() == 1 { "CANCIÓN" } else { "CANCIONES" }).into(),
                    );
                    let dur_chip = if mins <= 0 {
                        String::new()
                    } else if mins >= 60 {
                        if mins % 60 == 0 {
                            format!("{} H", mins / 60)
                        } else {
                            format!("{} H {} MIN", mins / 60, mins % 60)
                        }
                    } else {
                        format!("{mins} MIN")
                    };
                    ui.set_list_duration_chip(dur_chip.into());
                    ui.set_list_message(if tracks.is_empty() { "Esta lista todavía no tiene canciones." } else { "" }.into());
                    if matches!(app.current_source, Some(Source::Liked)) {
                        for track in &tracks { app.actions.saved.entry(track.uri.clone()).or_insert(true); }
                    }
                    // Playlist sin portada propia: mosaico con sus álbumes, como en Spotify.
                    if ui.get_list_cover().size().width == 0 && matches!(app.current_source, Some(Source::Playlist(_))) {
                        let urls: Vec<String> = tracks.iter().take(30).filter_map(|t| t.image_url().map(str::to_owned)).collect();
                        if let Some(url) = mosaic(&urls) {
                            load_cover(app, &url, 400, move |ui, app, d| {
                                if app.list_gen == gen && ui.get_list_cover().size().width == 0 {
                                    apply_list_cover(ui, d);
                                }
                            });
                        }
                    }
                    app.tracks = tracks;
                    refresh_rows(app);
                    if matches!(app.current_source, Some(Source::Album(_)) | Some(Source::Playlist(_)) | Some(Source::Liked)) {
                        let artists: Vec<ArtistItem> = app.tracks.iter()
                            .flat_map(|t| t.artists.iter())
                            .filter_map(|a| {
                                let id = a.id.as_deref().filter(|s| !s.is_empty())?;
                                Some(ArtistItem::new(id, &a.name))
                            })
                            .collect();
                        catalog::set_list_artists(app, artists);
                    }
                    if matches!(app.current_source,Some(Source::Playlist(_))){catalog::refresh_suggestion_rows(app);catalog::load_suggestions(ui,app,false);}
                }
            });
        };
        let call = |cmd: &str, args: Value| {
            let result = engine.call(cmd, args, Duration::from_secs(60));
            if result.is_err() {
                on_ui(move |ui, app| { if app.list_gen == gen { ui.set_list_message("No se pudo cargar la lista. Vuelve a abrirla para reintentar.".into()); } });
            }
            result
        };
        match src {
            Source::Playlist(p) => {
                let (mut all, mut offset) = (Vec::<Track>::new(), 0usize);
                loop {
                    let page = call("playlist_items", json!({"target": p.id, "offset": offset, "limit": 100, "fresh":offset==0}))
                        .and_then(|v| serde_json::from_value::<Page<PlaylistItem>>(v).map_err(|e| e.to_string()));
                    let Ok(page) = page else { return log("playlist_items falló") };
                    let n = page.items.len();
                    all.extend(page.items.iter().filter_map(|i| i.resolved().cloned()));
                    publish(all.clone());
                    offset += n;
                    if n < 100 {
                        break;
                    }
                }
            }
            Source::Liked => {
                let (mut all, mut offset) = (Vec::<Track>::new(), 0usize);
                loop {
                    let page = call("liked_items", json!({"offset": offset, "limit": 100}))
                        .and_then(|v| serde_json::from_value::<Page<SavedTrack>>(v).map_err(|e| e.to_string()));
                    let Ok(page) = page else { return log("liked_items falló") };
                    let n = page.items.len();
                    all.extend(page.items.into_iter().map(|s| s.track));
                    publish(all.clone());
                    offset += n;
                    if n == 0 {
                        break;
                    }
                }
            }
            Source::Album(a) => {
                let album = SpAlbum { name: a.name.clone(), images: a.images.clone(), id: Some(a.id.clone()), uri: Some(a.uri.clone()) };
                catalog::cached_call(&engine, "album_tracks", json!({"target": a.id}), |result| {
                    match result.and_then(|v| serde_json::from_value::<Page<Track>>(v).map_err(|e| e.to_string())) {
                        Ok(page) => publish(page.items.into_iter().map(|t| Track { album: Some(album.clone()), ..t }).collect()),
                        Err(e) => {
                            log(&format!("album_tracks falló: {e}"));
                            on_ui(move |ui, app| { if app.list_gen == gen { ui.set_list_message("No se pudo cargar la lista. Vuelve a abrirla para reintentar.".into()); } });
                        }
                    }
                });
            }
            Source::ArtistTop(a) => {
                // Lo guardado aparece al instante; la versión fresca la sustituye si cambió.
                catalog::cached_call(&engine, "artist_top", json!({"target": a.id}), |result| {
                    match result.and_then(|v| serde_json::from_value::<TopTracksResponse>(v).map_err(|e| e.to_string())) {
                        Ok(r) => publish(r.tracks),
                        Err(e) => {
                            log(&format!("artist_top falló: {e}"));
                            on_ui(move |ui, app| { if app.list_gen == gen { ui.set_list_message("No se pudo cargar la lista. Vuelve a abrirla para reintentar.".into()); } });
                        }
                    }
                });
            }
        }
    });
}

/// Pulsar una canción de la lista abierta: suena en su contexto (playlist/álbum) o como lista de canciones.
pub fn play_list_track(ui: &MainWindow, app: &mut App, i: usize) {
    let Some(&original) = app.visible_indices.get(i) else { return };
    let Some(track) = app.tracks.get(original).cloned() else { return };
    // La vista puede estar ordenada o filtrada; el salto dentro de Spotify usa la posición original.
    let req = match app.context.clone() {
        Some(uri) => PlayReq::Context { uri, start: Some(track.uri.clone()), skip: original },
        None if app.tracks.len() > 1 => PlayReq::Tracks {
            uris: app.visible_indices.iter().map(|&i| app.tracks[i].uri.clone()).collect(),
            start: track.uri.clone(),
        },
        None => PlayReq::Track(track.uri.clone()),
    };
    play_req(app, req);
    // Respuesta inmediata: el estado real llega del reproductor en un momento.
    app.player.track = Some(track);
    app.player.playing = true;
    app.player.set_position(0);
    render_player(ui, app);
    refresh_rows(app);
}

// ---------- Búsqueda ----------

fn recents_path() -> std::path::PathBuf {
    engine::data_dir().join("recent_searches.json")
}

fn load_recents() -> Vec<String> {
    std::fs::read(recents_path()).ok()
        .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
        .unwrap_or_default().into_iter().take(MAX_RECENTS).collect()
}

/// El término más reciente va primero; sin duplicados (sin distinguir mayúsculas) y como mucho 8.
pub(crate) fn push_recent(list: &mut Vec<String>, q: &str) {
    let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
    if q.is_empty() { return; }
    list.retain(|x| !x.eq_ignore_ascii_case(&q));
    list.insert(0, q);
    list.truncate(MAX_RECENTS);
}

fn save_recents(app: &mut App) {
    let _ = std::fs::write(recents_path(), serde_json::to_vec(&app.search.recents).unwrap_or_default());
    app.search.m_recents.set_vec(app.search.recents.iter().map(|q| q.as_str().into()).collect::<Vec<slint::SharedString>>());
}

/// Se recuerda lo que el usuario buscó de verdad (Intro o al abrir/reproducir un resultado), no cada letra.
pub fn remember_search(ui: &MainWindow, app: &mut App) {
    let text = ui.get_search_text().to_string();
    push_recent(&mut app.search.recents, &text);
    save_recents(app);
}

pub fn remove_recent(app: &mut App, i: usize) {
    if i < app.search.recents.len() {
        app.search.recents.remove(i);
        save_recents(app);
    }
}

pub fn clear_recents(app: &mut App) {
    app.search.recents.clear();
    save_recents(app);
}

/// Pulsar una búsqueda reciente o un género: se escribe en el buscador y se busca al momento.
pub fn search_for(ui: &MainWindow, app: &mut App, q: String, filter: i32) {
    if ui.get_page() != 1 { push_history(ui, app); }
    ui.set_page(1);
    ui.set_page_title("Buscar".into());
    ui.set_search_text(q.clone().into());
    ui.set_search_filter(filter);
    search_now(ui, app, q, 0);
}

/// Espera de 300 ms tras la última tecla: no se lanza una petición por letra.
pub fn search_edited(ui: &MainWindow, app: &mut App, text: String) {
    search_now(ui, app, text, 300);
}

fn search_now(ui: &MainWindow, app: &mut App, text: String, delay_ms: u64) {
    app.search.gen += 1;
    let (gen, q) = (app.search.gen, text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase());
    app.search.timer.stop();
    if q.is_empty() {
        clear_search(ui, app);
        ui.set_search_filter(0);
        ui.set_search_message("".into());
        return;
    }
    ui.set_search_message("Buscando…".into());
    let run = move || { with_app(|_, app| run_search(app, q.clone(), gen)); };
    if delay_ms == 0 {
        let _ = slint::invoke_from_event_loop(run);
    } else {
        app.search.timer.start(TimerMode::SingleShot, Duration::from_millis(delay_ms), run);
    }
}

fn clear_search(ui: &MainWindow, app: &mut App) {
    let s = &mut app.search;
    (s.tracks, s.artists, s.albums, s.playlists) = (vec![], vec![], vec![], vec![]);
    s.top = None;
    s.more_tracks = false;
    s.query.clear();
    s.m_tracks.set_vec(Vec::<TrackEntry>::new());
    s.m_artists.set_vec(Vec::<CardEntry>::new());
    s.m_albums.set_vec(Vec::<CardEntry>::new());
    s.m_playlists.set_vec(Vec::<CardEntry>::new());
    ui.set_search_top_kind("".into());
    ui.set_search_more_tracks(false);
}

fn run_search(app: &mut App, q: String, gen: u64) {
    if let Some((at, results)) = app.search.cache.get(&q) {
        if at.elapsed() < Duration::from_secs(120) {
            let results = results.clone();
            if let Some(ui) = app.ui.upgrade() { publish_search(&ui, app, q, results); }
            return;
        }
    }
    let Some(engine) = app.engine.clone() else { return };
    std::thread::spawn(move || {
        let reply = engine
            .call("search", json!({"q": q, "types": "track,artist,album,playlist", "limit": SEARCH_PAGE, "offset": 0}), Duration::from_secs(30))
            .and_then(|v| serde_json::from_value::<SearchResponse>(v).map_err(|e| e.to_string()));
        match reply {
            Ok(r) => {
                let results = SearchResults {
                    tracks: r.tracks.map(|p| p.items.into_iter().flatten().collect()).unwrap_or_default(),
                    artists: r.artists.map(|p| p.items.into_iter().flatten().collect()).unwrap_or_default(),
                    albums: r.albums.map(|p| p.items.into_iter().flatten().collect()).unwrap_or_default(),
                    playlists: r.playlists.map(|p| p.items.into_iter().flatten().collect()).unwrap_or_default(),
                };
                on_ui(move |ui, app| {
                    if app.search.gen == gen {
                        if app.search.cache.len() >= 12 {
                            if let Some(oldest) = app.search.cache.iter().min_by_key(|(_, (at, _))| *at).map(|(q, _)| q.clone()) {
                                app.search.cache.remove(&oldest);
                            }
                        }
                        app.search.cache.insert(q.clone(), (Instant::now(), results.clone()));
                        publish_search(ui, app, q, results);
                    }
                });
            }
            Err(e) => {
                log(&format!("búsqueda falló: {e}"));
                on_ui(move |ui, app| {
                    if app.search.gen == gen {
                        ui.set_search_message("No se pudo completar la búsqueda. Vuelve a intentarlo.".into());
                    }
                });
            }
        }
    });
}

fn publish_search(ui: &MainWindow, app: &mut App, q: String, results: SearchResults) {
    let top = search_ranking::top_artist(&q, &results);
    let tracks = search_ranking::tracks_preferring(&results.tracks, top.as_ref());
    let artists = search_ranking::artists_with_top(&results.artists, top.as_ref());
    let hit = top.map(TopHit::Artist)
        .or_else(|| tracks.first().cloned().map(TopHit::Track))
        .or_else(|| results.albums.first().cloned().map(TopHit::Album))
        .or_else(|| results.playlists.first().cloned().map(TopHit::Playlist));
    app.search.more_tracks = results.tracks.len() >= SEARCH_PAGE;
    app.search.query = q.clone();
    apply_search(ui, app, q, tracks, artists, results.albums, results.playlists);
    show_top(ui, app, hit);
}

fn show_top(ui: &MainWindow, app: &mut App, hit: Option<TopHit>) {
    app.search.top = hit.clone();
    let Some(hit) = hit else { return ui.set_search_top_kind("".into()) };
    let (kind, title, subtitle, url, round) = match &hit {
        TopHit::Artist(a) => ("Artista", a.name.clone(), String::new(), a.image_url().map(str::to_owned), true),
        TopHit::Track(t) => ("Canción", t.name.clone(), t.artist_line(), t.image_url().map(str::to_owned), false),
        TopHit::Album(a) => ("Álbum", a.name.clone(), a.artist_line(), a.image_url().map(str::to_owned), false),
        TopHit::Playlist(p) => ("Playlist", p.name.clone(), p.owner.as_ref().and_then(|o| o.display_name.clone()).unwrap_or_default(), p.image_url().map(str::to_owned), false),
    };
    ui.set_search_top(CardEntry { title: title.clone().into(), subtitle: subtitle.into(), cover: Image::default() });
    ui.set_search_top_kind(kind.into());
    ui.set_search_top_round(round);
    if let Some(url) = url {
        let gen = app.search.gen;
        load_cover(app, &url, 200, move |ui, app, d| {
            if app.search.gen != gen { return; }
            let mut card = ui.get_search_top();
            if card.title.as_str() != title { return; }
            card.cover = if round { to_image(&images::circle(d)) } else { to_image(d) };
            ui.set_search_top(card);
        });
    }
}

fn apply_search(
    ui: &MainWindow,
    app: &mut App,
    q: String,
    tracks: Vec<Track>,
    artists: Vec<ArtistItem>,
    albums: Vec<AlbumItem>,
    playlists: Vec<Playlist>,
) {
    let empty = tracks.is_empty() && artists.is_empty() && albums.is_empty() && playlists.is_empty();
    ui.set_search_message(if empty { format!("Sin resultados para «{q}».") } else { String::new() }.into());
    fill_cards(app, Cards::Artists, artists.iter().take(SEARCH_PAGE).map(|a| (a.name.clone(), "Artista".to_owned(), a.image_url().map(str::to_owned))).collect());
    app.search.tracks = tracks;
    show_search_tracks(ui, app, 0);

    let owner = |p: &Playlist| p.owner.as_ref().and_then(|o| o.display_name.clone().or_else(|| Some(o.id.clone()))).unwrap_or_default();
    fill_cards(
        app,
        Cards::Albums,
        albums
            .iter()
            .take(SEARCH_PAGE)
            .map(|a| {
                let year: String = a.release_date.as_deref().unwrap_or("").chars().take(4).collect();
                (a.name.clone(), [a.artist_line(), year].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "), a.image_url().map(str::to_owned))
            })
            .collect(),
    );
    fill_cards(app, Cards::Playlists, playlists.iter().take(SEARCH_PAGE).map(|p| (p.name.clone(), owner(p), p.image_url().map(str::to_owned))).collect());
    app.search.artists = artists.into_iter().take(SEARCH_PAGE).collect();
    app.search.albums = albums.into_iter().take(SEARCH_PAGE).collect();
    app.search.playlists = playlists.into_iter().take(SEARCH_PAGE).collect();
}

/// Publica las canciones de los resultados desde `from` (las anteriores conservan su portada).
fn show_search_tracks(ui: &MainWindow, app: &mut App, from: usize) {
    let uri = app.player.track.as_ref().map(|t| t.uri.clone());
    let rows = track_rows(&app.search.tracks, uri.as_deref());
    if from == 0 {
        app.search.m_tracks.set_vec(rows);
    } else {
        for row in rows.into_iter().skip(from) { app.search.m_tracks.push(row); }
    }
    ui.set_search_more_tracks(app.search.more_tracks);
    let gen = app.search.gen;
    let covers: Vec<_> = app.search.tracks.iter().enumerate().skip(from)
        .filter_map(|(i, t)| t.image_url().map(|url| (i, t.uri.clone(), url.to_owned()))).collect();
    for (i, wanted, url) in covers {
        load_cover_with_priority(app, &url, 44, i < from + 12, move |_, app, image| {
            if app.search.gen != gen { return; }
            if let Some(mut row) = app.search.m_tracks.row_data(i).filter(|row| row.uri.as_str() == wanted) {
                row.cover = to_image(image);
                app.search.m_tracks.set_row_data(i, row);
            }
        });
    }
    let shown = app.search.tracks.iter().skip(from).cloned().collect();
    actions::request_saved(app, shown);
    actions::render_likes(ui, app);
}

/// Pestaña Canciones: siguiente página de resultados.
pub fn load_more_tracks(ui: &MainWindow, app: &mut App) {
    if app.search.loading_more || !app.search.more_tracks || app.search.query.is_empty() { return; }
    let Some(engine) = app.engine.clone() else { return };
    app.search.loading_more = true;
    ui.set_search_loading_more(true);
    let (q, gen, offset) = (app.search.query.clone(), app.search.gen, app.search.tracks.len());
    std::thread::spawn(move || {
        let reply = engine
            .call("search", json!({"q": q, "types": "track", "limit": MORE_TRACKS_PAGE, "offset": offset}), Duration::from_secs(30))
            .and_then(|v| serde_json::from_value::<SearchResponse>(v).map_err(|e| e.to_string()));
        on_ui(move |ui, app| {
            app.search.loading_more = false;
            ui.set_search_loading_more(false);
            if app.search.gen != gen { return; }
            match reply {
                Ok(r) => {
                    let known: std::collections::HashSet<String> = app.search.tracks.iter().map(|t| t.uri.clone()).collect();
                    let page: Vec<Track> = r.tracks.map(|p| p.items.into_iter().flatten().collect()).unwrap_or_default();
                    app.search.more_tracks = page.len() >= MORE_TRACKS_PAGE;
                    let from = app.search.tracks.len();
                    app.search.tracks.extend(page.into_iter().filter(|t| !known.contains(&t.uri)));
                    show_search_tracks(ui, app, from);
                }
                Err(e) => log(&format!("más canciones: {e}")),
            }
        });
    });
}

/// Mejor resultado: abrir (artista, álbum, playlist) o reproducir (canción).
pub fn open_top(ui: &MainWindow, app: &mut App) {
    remember_search(ui, app);
    match app.search.top.clone() {
        Some(TopHit::Artist(a)) => open_source(ui, app, Source::ArtistTop(a)),
        Some(TopHit::Album(a)) => open_source(ui, app, Source::Album(a)),
        Some(TopHit::Playlist(p)) => open_source(ui, app, Source::Playlist(p)),
        Some(TopHit::Track(t)) => play_now(ui, app, PlayReq::Track(t.uri.clone()), t),
        None => {}
    }
}

pub fn play_top(ui: &MainWindow, app: &mut App) {
    remember_search(ui, app);
    match app.search.top.clone() {
        Some(TopHit::Artist(a)) => play_req(app, PlayReq::Context { uri: a.uri, start: None, skip: 0 }),
        Some(TopHit::Album(a)) => play_req(app, PlayReq::Context { uri: a.uri, start: None, skip: 0 }),
        Some(TopHit::Playlist(p)) => play_req(app, PlayReq::Context { uri: p.uri, start: None, skip: 0 }),
        Some(TopHit::Track(t)) => play_now(ui, app, PlayReq::Track(t.uri.clone()), t),
        None => {}
    }
}

/// Botón de reproducción de una tarjeta de artista: suena el artista (sus populares).
pub fn play_search_artist(ui: &MainWindow, app: &mut App, i: usize) {
    let Some(a) = app.search.artists.get(i).cloned() else { return };
    remember_search(ui, app);
    play_req(app, PlayReq::Context { uri: a.uri, start: None, skip: 0 });
}

fn play_now(ui: &MainWindow, app: &mut App, req: PlayReq, track: Track) {
    play_req(app, req);
    app.player.track = Some(track);
    app.player.playing = true;
    app.player.set_position(0);
    render_player(ui, app);
}

/// Pulsar una canción de los resultados: suena la lista de resultados empezando por ella.
pub fn play_search_track(ui: &MainWindow, app: &mut App, i: usize) {
    let Some(track) = app.search.tracks.get(i).cloned() else { return };
    remember_search(ui, app);
    let uris: Vec<String> = app.search.tracks.iter().take(100).map(|t| t.uri.clone()).collect();
    play_req(app, PlayReq::Tracks { uris, start: track.uri.clone() });
    app.player.track = Some(track);
    app.player.playing = true;
    app.player.set_position(0);
    render_player(ui, app);
}

/// kind: 0 artista · 1 álbum · 2 playlist
pub fn open_search_result(ui: &MainWindow, app: &mut App, kind: i32, i: usize) {
    let src = match kind {
        0 => app.search.artists.get(i).cloned().map(Source::ArtistTop),
        1 => app.search.albums.get(i).cloned().map(Source::Album),
        _ => app.search.playlists.get(i).cloned().map(Source::Playlist),
    };
    if let Some(src) = src {
        remember_search(ui, app);
        open_source(ui, app, src);
    }
}

#[cfg(test)]
mod recent_tests {
    use super::push_recent;

    #[test]
    fn recents_are_unique_newest_first_and_capped() {
        let mut list = vec![];
        for q in ["uno", "dos", "UNO", "tres", "  cuatro   cinco ", "6", "7", "8", "9", "10"] {
            push_recent(&mut list, q);
        }
        assert_eq!(list.len(), 8);
        assert_eq!(list[0], "10");
        assert!(list.contains(&"cuatro cinco".to_owned()));
        assert_eq!(list.iter().filter(|q| q.eq_ignore_ascii_case("uno")).count(), 1);
        push_recent(&mut list, "   ");
        assert_eq!(list[0], "10");
    }
}

