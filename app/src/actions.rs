//! Acciones de biblioteca, favoritos, playlists y menús. Las mutaciones esperan confirmación del motor.
use super::*;
use spotify_core::{ArtistItem, SpArtist, SpImage, SpOwner};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct Actions {
    pub saved: HashMap<String, bool>,
    pub like_pending: HashSet<String>,
    checking: HashSet<String>,
    saved_epoch: u64,
    own_ids: HashSet<String>,
    menu_track: Option<Track>,
    destinations: Vec<Playlist>,
    artists: Vec<SpArtist>,
    picker_artists: Vec<SpArtist>,
    picker_track: Option<Track>,
    menu_artist_model: Rc<VecModel<ArtistChoice>>,
    picker_artist_model: Rc<VecModel<ArtistChoice>>,
    artist_avatars: HashMap<String, Image>,
    artist_avatars_loading: HashSet<String>,
    info_artist_id: Option<String>,
    editing: Option<Playlist>,
    adding: Option<Track>,
    cover: Option<Vec<u8>>,
    editor_gen: u64,
    delete_target: Option<Playlist>,
    info_gen: u64,
}

pub fn visible_track(app: &App, index: usize) -> Option<Track> {
    app.tracks.get(*app.visible_indices.get(index)?).cloned()
}
fn track_at(app: &App, kind: i32, index: usize) -> Option<Track> {
    match kind {
        0 => visible_track(app, index),
        1 => app.search.track(index),
        3 => app.catalog.suggestions.get(index).cloned(),
        _ => app.player.track.clone(),
    }
}
pub fn can_edit(app: &App, p: &Playlist) -> bool {
    p.uri.starts_with("spotify:playlist:")
        && (app.actions.own_ids.contains(&p.id)
            || matches!((&app.username, &p.owner), (Some(me),Some(owner)) if !me.is_empty() && *me == owner.id))
}
fn current_playlist(app: &App) -> Option<Playlist> {
    match &app.current_source {
        Some(browse::Source::Playlist(p)) => Some(p.clone()),
        _ => None,
    }
}
fn error(ui: &MainWindow, app: &mut App, action: &str, detail: &str) {
    log(&format!("{action}: {detail}"));
    queue_ui::toast(
        ui,
        app,
        &format!("No se pudo {action}. Vuelve a intentarlo."),
    );
}

pub fn render_likes(ui: &MainWindow, app: &App) {
    for model in [&app.rows, &app.search.m_tracks, &app.catalog.suggestion_model] {
        for i in 0..model.row_count() {
            if let Some(mut row) = model.row_data(i) {
                let saved = app
                    .actions
                    .saved
                    .get(row.uri.as_str())
                    .copied()
                    .unwrap_or(false);
                let busy = app.actions.like_pending.contains(row.uri.as_str());
                if row.saved != saved || row.like_busy != busy {
                    row.saved = saved;
                    row.like_busy = busy;
                    model.set_row_data(i, row);
                }
            }
        }
    }
    let uri = app
        .player
        .track
        .as_ref()
        .map(|t| t.uri.as_str())
        .unwrap_or("");
    ui.set_player_saved(app.actions.saved.get(uri).copied().unwrap_or(false));
    ui.set_player_like_busy(app.actions.like_pending.contains(uri));
    ui.set_player_can_like(uri.starts_with("spotify:track:"));
    if let Some(t) = &app.actions.menu_track {
        ui.set_menu_saved(app.actions.saved.get(&t.uri).copied().unwrap_or(false));
        ui.set_menu_like_busy(app.actions.like_pending.contains(&t.uri));
    }
}
pub fn request_saved(app: &mut App, tracks: Vec<Track>) {
    let Some(engine) = app.engine.clone() else {
        return;
    };
    let mut uris = vec![];
    for t in tracks {
        if t.uri.starts_with("spotify:track:")
            && !app.actions.saved.contains_key(&t.uri)
            && !app.actions.like_pending.contains(&t.uri)
            && app.actions.checking.insert(t.uri.clone())
        {
            uris.push(t.uri);
        }
    }
    if uris.is_empty() {
        return;
    }
    let epoch = app.actions.saved_epoch;
    std::thread::spawn(move || {
        for chunk in uris.chunks(100) {
            let values = engine
                .call(
                    "liked_contains",
                    json!({"uris":chunk}),
                    Duration::from_secs(30),
                )
                .and_then(|v| serde_json::from_value::<Vec<bool>>(v).map_err(|e| e.to_string()));
            let targets = chunk.to_vec();
            on_ui(move |ui, app| {
                if epoch != app.actions.saved_epoch {
                    return;
                }
                if let Ok(values) = values {
                    if values.len() == targets.len() {
                        for (uri, saved) in targets.iter().zip(values) {
                            if !app.actions.like_pending.contains(uri) {
                                app.actions.saved.entry(uri.clone()).or_insert(saved);
                            }
                        }
                    }
                }
                for uri in targets {
                    app.actions.checking.remove(&uri);
                }
                render_likes(ui, app);
            });
        }
    });
}
fn toggle_saved(ui: &MainWindow, app: &mut App, track: Track) {
    if !track.uri.starts_with("spotify:track:")
        || !app.actions.like_pending.insert(track.uri.clone())
    {
        return;
    }
    let Some(engine) = app.engine.clone() else {
        app.actions.like_pending.remove(&track.uri);
        return;
    };
    let before = app.actions.saved.get(&track.uri).copied();
    if let Some(saved) = before {
        app.actions.saved.insert(track.uri.clone(), !saved);
    }
    render_likes(ui, app);
    std::thread::spawn(move || {
        let result = (|| -> Result<bool, String> {
            let was_saved = match before {
                Some(v) => v,
                None => {
                    let v = engine.call(
                        "liked_contains",
                        json!({"uris":[track.uri]}),
                        Duration::from_secs(30),
                    )?;
                    v.as_array()
                        .and_then(|a| a.first())
                        .and_then(Value::as_bool)
                        .ok_or("Respuesta de favoritos inválida")?
                }
            };
            engine.call(
                "set_liked",
                json!({"uris":[track.uri],"saved":!was_saved}),
                Duration::from_secs(30),
            )?;
            Ok(!was_saved)
        })();
        on_ui(move |ui, app| {
            app.actions.like_pending.remove(&track.uri);
            match result {
                Ok(saved) => {
                    app.actions.saved.insert(track.uri.clone(), saved);
                    if !saved && matches!(app.current_source, Some(browse::Source::Liked)) {
                        app.tracks.retain(|t| t.uri != track.uri);
                        refresh_rows(app);
                    }
                    queue_ui::toast(
                        ui,
                        app,
                        if saved {
                            "Guardada en Tus me gusta"
                        } else {
                            "Eliminada de Tus me gusta"
                        },
                    );
                }
                Err(e) => {
                    if let Some(value) = before {
                        app.actions.saved.insert(track.uri.clone(), value);
                    } else {
                        app.actions.saved.remove(&track.uri);
                    }
                    error(ui, app, "actualizar Tus me gusta", &e);
                    if ui.get_track_menu_open() {
                        ui.set_menu_error(
                            "No se pudo actualizar Tus me gusta. Inténtalo de nuevo.".into(),
                        );
                    }
                }
            }
            render_likes(ui, app);
        });
    });
}

fn populate_destinations(ui: &MainWindow, app: &mut App) {
    app.actions.destinations = app
        .playlists
        .iter()
        .filter(|p| can_edit(app, p))
        .cloned()
        .collect();
    ui.set_menu_destinations(ModelRc::new(VecModel::from(
        app.actions
            .destinations
            .iter()
            .map(|p| slint::SharedString::from(p.name.as_str()))
            .collect::<Vec<_>>(),
    )));
}

fn open_track_menu_page(ui: &MainWindow, app: &mut App, track: Track, page: i32) {
    ui.set_menu_error("".into());
    ui.set_menu_title(track.name.clone().into());
    ui.set_menu_artist(track.artist_line().into());
    ui.set_menu_can_like(track.uri.starts_with("spotify:track:"));
    ui.set_menu_can_album(
        track
            .album
            .as_ref()
            .is_some_and(|a| a.id.is_some() || a.uri.is_some()),
    );
    app.actions.artists = track
        .artists
        .iter()
        .filter(|a| a.id.is_some())
        .cloned()
        .collect();
    let choices = artist_choices(app, &app.actions.artists);
    app.actions.menu_artist_model.set_vec(choices);
    ui.set_menu_artists(ModelRc::from(app.actions.menu_artist_model.clone()));
    fetch_artist_avatars(app, &app.actions.artists.clone());
    if page == 1 {
        populate_destinations(ui, app);
    }
    ui.set_menu_page(page);
    ui.set_track_menu_open(true);
    app.actions.menu_track = Some(track.clone());
    render_likes(ui, app);
    request_saved(app, vec![track]);
}

fn open_track_menu(ui: &MainWindow, app: &mut App, track: Track) {
    open_track_menu_page(ui, app, track, 0);
}

fn open_add_playlist(ui: &MainWindow, app: &mut App, track: Track) {
    open_track_menu_page(ui, app, track, 1);
}
fn open_album(ui: &MainWindow, app: &mut App, track: &Track) {
    let Some(a) = track.album.as_ref() else {
        return;
    };
    let Some(id) = a.id.clone().or_else(|| {
        a.uri
            .as_ref()
            .and_then(|u| u.strip_prefix("spotify:album:").map(str::to_owned))
    }) else {
        return;
    };
    let mut p = Playlist::new(&id, &format!("spotify:album:{id}"), &a.name);
    p.images = a.images.clone();
    p.artist_name = Some(spotify_core::catalog::primary_artist(&track.artists));
    p.artist_id = track.artists.first().and_then(|a| a.id.clone());
    crate::close_now_playing_view(ui, app, false);
    browse::open_source(ui, app, browse::Source::Playlist(p));
}
fn share_url(track: &Track) -> Option<String> {
    let id = track.uri.strip_prefix("spotify:track:")?;
    (!id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric()))
        .then(|| format!("https://open.spotify.com/track/{id}"))
}
fn menu_action(ui: &MainWindow, app: &mut App, action: i32) {
    let Some(track) = app.actions.menu_track.clone() else {
        return;
    };
    match action {
        0 => toggle_saved(ui, app, track),
        1 => {
            queue_ui::add_track(ui, app, track);
            ui.set_track_menu_open(false);
        }
        2 => {
            populate_destinations(ui, app);
            ui.set_menu_page(1);
        }
        3 => {
            ui.set_track_menu_open(false);
            open_editor(ui, app, None, Some(track));
        }
        4 => {
            if app.actions.artists.len() == 1 {
                open_artist(ui, app, 0);
            } else {
                ui.set_menu_page(2);
            }
        }
        5 => {
            ui.set_track_menu_open(false);
            open_album(ui, app, &track);
        }
        6 => {
            ui.set_track_menu_open(false);
            ui.set_info_open(true);
            ui.set_info_title(format!("Créditos · {}", track.name).into());
            ui.set_info_text("Buscando la grabación exacta en MusicBrainz…".into());
            ui.set_info_url("".into());
            let primary_id = track
                .artists
                .first()
                .and_then(|a| a.id.clone())
                .filter(|id| !id.is_empty());
            app.actions.info_artist_id = primary_id.clone();
            ui.set_info_avatar(
                primary_id
                    .as_deref()
                    .and_then(|id| app.actions.artist_avatars.get(id))
                    .cloned()
                    .unwrap_or_default(),
            );
            fetch_artist_avatars(app, &track.artists);
            app.actions.info_gen += 1;
            let gen = app.actions.info_gen;
            std::thread::spawn(move || {
                let result = credits::load(&track);
                on_ui(move |ui, app| {
                    if gen != app.actions.info_gen {
                        return;
                    }
                    match result {
                        Ok((text, url)) => {
                            ui.set_info_text(text.into());
                            ui.set_info_url(url.into());
                        }
                        Err(e) => {
                            log(&format!("créditos: {e}"));
                            ui.set_info_text("No se pudieron consultar los créditos. Vuelve a intentarlo más tarde.".into());
                        }
                    }
                });
            });
        }
        7 => {
            ui.set_menu_page(3);
            ui.set_share_link(share_url(&track).unwrap_or_default().into());
        }
        8 => {
            if let Some(link) = share_url(&track) {
                match native::copy(&link) {
                    Ok(()) => {
                        ui.set_track_menu_open(false);
                        queue_ui::toast(ui, app, "Enlace copiado");
                    }
                    Err(e) => {
                        log(&e);
                        ui.set_menu_error("No se pudo copiar el enlace.".into());
                    }
                }
            }
        }
        9 => {
            if let Some(link) = share_url(&track) {
                open_in_browser(&link);
            }
        }
        10 => {
            ui.set_track_menu_open(false);
            let title = format!("Radio de {}", track.name);
            let sub = format!("Basada en {} · {}", track.name, track.artist_line());
            let cover = track.image_url().map(str::to_owned);
            browse::open_radio(ui, app, track.uri.clone(), title, sub, cover);
        }
        _ => {}
    }
}
fn navigate_to_artist(ui: &MainWindow, app: &mut App, artist: &SpArtist) {
    let Some(id) = artist.id.as_deref().filter(|id| !id.is_empty()) else {
        return;
    };
    ui.set_track_menu_open(false);
    ui.set_artist_picker_open(false);
    crate::close_now_playing_view(ui, app, false);
    browse::open_source(
        ui,
        app,
        browse::Source::ArtistTop(spotify_core::ArtistItem::new(id, &artist.name)),
    );
}
fn open_artist(ui: &MainWindow, app: &mut App, index: usize) {
    let Some(artist) = app.actions.artists.get(index).cloned() else {
        return;
    };
    navigate_to_artist(ui, app, &artist);
}
fn filter_valid_artists(raw: Vec<SpArtist>) -> Vec<SpArtist> {
    let mut seen = HashSet::new();
    raw.into_iter()
        .filter(|a| a.id.as_ref().is_some_and(|id| !id.is_empty() && seen.insert(id.clone())))
        .collect()
}
fn artists_for(app: &App, kind: i32, index: usize) -> Vec<SpArtist> {
    let raw = if kind == 4 {
        app.catalog.album_artists.clone()
    } else {
        track_at(app, kind, index).map(|t| t.artists).unwrap_or_default()
    };
    filter_valid_artists(raw)
}
fn album_name_for_track(track: &Track) -> Option<String> {
    let album = track.album.as_ref()?;
    let has_id = album.id.as_deref().is_some_and(|id| !id.is_empty())
        || album.uri.as_deref().is_some_and(|u| u.starts_with("spotify:album:"));
    (has_id && !album.name.is_empty()).then(|| album.name.clone())
}
fn artist_choices(app: &App, artists: &[SpArtist]) -> Vec<ArtistChoice> {
    artists
        .iter()
        .map(|a| {
            let avatar = a
                .id
                .as_deref()
                .and_then(|id| app.actions.artist_avatars.get(id))
                .cloned()
                .unwrap_or_default();
            ArtistChoice {
                name: a.name.as_str().into(),
                avatar,
            }
        })
        .collect()
}

fn update_artist_choice_model(model: &VecModel<ArtistChoice>, artists: &[SpArtist], id: &str, avatar: &Image) {
    for (i, artist) in artists.iter().enumerate() {
        if artist.id.as_deref() == Some(id) {
            if let Some(mut row) = model.row_data(i) {
                row.avatar = avatar.clone();
                model.set_row_data(i, row);
            }
        }
    }
}

fn apply_artist_avatar(ui: &MainWindow, app: &mut App, id: &str, avatar: Image) {
    app.actions.artist_avatars.insert(id.to_owned(), avatar.clone());
    update_artist_choice_model(&app.actions.menu_artist_model, &app.actions.artists, id, &avatar);
    update_artist_choice_model(&app.actions.picker_artist_model, &app.actions.picker_artists, id, &avatar);
    if ui.get_info_open() && app.actions.info_artist_id.as_deref() == Some(id) {
        ui.set_info_avatar(avatar);
    }
}

fn fetch_artist_avatars(app: &mut App, artists: &[SpArtist]) {
    let mut ids = Vec::new();
    for artist in artists {
        let Some(id) = artist.id.as_deref().filter(|id| !id.is_empty()) else {
            continue;
        };
        if !app.actions.artist_avatars.contains_key(id)
            && app.actions.artist_avatars_loading.insert(id.to_owned())
        {
            ids.push(id.to_owned());
        }
    }
    if ids.is_empty() {
        return;
    }
    let Some(engine) = app.engine.clone() else {
        for id in ids {
            app.actions.artist_avatars_loading.remove(&id);
        }
        return;
    };
    let ids_cleanup = ids.clone();
    std::thread::spawn(move || {
        let reply = engine.call("artists", json!({ "ids": ids }), Duration::from_secs(10));
        let parsed: Vec<(String, Option<String>)> = if let Ok(result) = reply {
            result
                .get("artists")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| serde_json::from_value::<ArtistItem>(v.clone()).ok())
                .map(|a| {
                    let url = a.image_url().map(str::to_owned);
                    (a.id, url)
                })
                .collect()
        } else {
            vec![]
        };
        on_ui(move |_ui, app| {
            for id in &ids_cleanup {
                app.actions.artist_avatars_loading.remove(id);
            }
            for (id, url) in parsed {
                let Some(url) = url else { continue; };
                let id_for_cb = id.clone();
                load_cover_with_priority(app, &url, 64, true, move |ui, app, decoded| {
                    let img = to_image(&images::circle(decoded));
                    apply_artist_avatar(ui, app, &id_for_cb, img);
                });
            }
        });
    });
}

fn open_artist_picker(ui: &MainWindow, app: &mut App, artists: Vec<SpArtist>, album: Option<String>, track: Option<Track>) {
    app.actions.picker_artists = artists;
    app.actions.picker_track = track.clone();
    ui.set_artist_picker_album(album.clone().unwrap_or_default().into());
    ui.set_artist_picker_album_cover(Image::default());
    let choices = artist_choices(app, &app.actions.picker_artists);
    app.actions.picker_artist_model.set_vec(choices);
    ui.set_artist_picker_items(ModelRc::from(app.actions.picker_artist_model.clone()));
    ui.set_artist_picker_open(true);
    fetch_artist_avatars(app, &app.actions.picker_artists.clone());
    if album.is_some() {
        if let Some(url) = track.as_ref().and_then(|t| t.image_url()).map(str::to_owned) {
            let picker_uri = track.as_ref().map(|t| t.uri.clone());
            load_cover_with_priority(app, &url, 64, true, move |ui, app, decoded| {
                if ui.get_artist_picker_open()
                    && app.actions.picker_track.as_ref().map(|t| &t.uri) == picker_uri.as_ref()
                {
                    ui.set_artist_picker_album_cover(to_image(decoded));
                }
            });
        }
    }
}

fn prompt_or_open_artist(ui: &MainWindow, app: &mut App, kind: i32, index: usize) {
    let artists = artists_for(app, kind, index);
    let track = track_at(app, kind, index);
    let album_name = if kind == 2 {
        track.as_ref().and_then(album_name_for_track)
    } else {
        None
    };
    if let Some(album) = album_name {
        open_artist_picker(ui, app, artists, Some(album), track);
        return;
    }
    app.actions.picker_track = None;
    ui.set_artist_picker_album("".into());
    ui.set_artist_picker_album_cover(Image::default());
    if artists.is_empty() {
        return;
    }
    if artists.len() == 1 {
        navigate_to_artist(ui, app, &artists[0]);
    } else {
        open_artist_picker(ui, app, artists, None, None);
    }
}
fn open_picked_artist(ui: &MainWindow, app: &mut App, index: usize) {
    let Some(artist) = app.actions.picker_artists.get(index).cloned() else {
        return;
    };
    navigate_to_artist(ui, app, &artist);
}
fn open_picked_album(ui: &MainWindow, app: &mut App) {
    let Some(track) = app.actions.picker_track.clone() else {
        return;
    };
    ui.set_artist_picker_open(false);
    open_album(ui, app, &track);
}
fn add_to_playlist(ui:&MainWindow,app:&mut App,index:usize){
    let (Some(track),Some(p))=(app.actions.menu_track.clone(),app.actions.destinations.get(index).cloned())else{return;};
    add_to_target(ui,app,p,track);
}
pub fn add_to_target(ui:&MainWindow,app:&mut App,p:Playlist,track:Track){
    let Some(engine)=app.engine.clone()else{return;};
    if !can_edit(app, &p) || ui.get_menu_adding() {
        return;
    }
    ui.set_menu_adding(true);
    std::thread::spawn(move || {
        let result = engine.call(
            "playlist_add",
            json!({"target":p.id,"uris":[track.uri]}),
            Duration::from_secs(30),
        );
        on_ui(move |ui, app| {
            ui.set_menu_adding(false);
            match result {
                Ok(_) => {
                    ui.set_track_menu_open(false);
                    queue_ui::toast(ui, app, &format!("Añadida a «{}»", p.name));
                    if matches!(&app.current_source,Some(browse::Source::Playlist(current)) if current.id == p.id)
                    {
                        browse::reload_playlist(ui,app,&p.id);
                    }
                }
                Err(e) => {
                    log(&e);
                    ui.set_menu_error("No se pudo añadir la canción. Revisa la conexión y vuelve a intentarlo.".into());
                    ui.set_suggestion_message("No se pudo añadir la canción. Inténtalo de nuevo.".into());
                    queue_ui::toast(ui,app,"No se pudo añadir la canción.");
                }
            }
        });
    });
}

fn open_editor(ui: &MainWindow, app: &mut App, editing: Option<Playlist>, adding: Option<Track>) {
    if ui.get_playlist_saving() {
        return;
    }
    if editing.as_ref().is_some_and(|p| !can_edit(app, p)) {
        return;
    }
    app.actions.editor_gen += 1;
    let gen = app.actions.editor_gen;
    ui.set_cover_loading(false);
    ui.set_editor_editing(editing.is_some());
    ui.set_editor_name(
        editing
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default()
            .into(),
    );
    ui.set_editor_description(
        editing
            .as_ref()
            .and_then(Playlist::plain_description)
            .unwrap_or_default()
            .into(),
    );
    ui.set_editor_cover(Image::default());
    ui.set_editor_has_cover(false);
    ui.set_playlist_error("".into());
    ui.set_editor_adding(
        adding
            .as_ref()
            .map(|t| format!("Se añadirá: {} · {}", t.name, t.artist_line()))
            .unwrap_or_default()
            .into(),
    );
    if let Some(url) = editing
        .as_ref()
        .and_then(|p| p.image_url())
        .map(str::to_owned)
    {
        load_cover(app, &url, 200, move |ui, app, d| {
            if app.actions.editor_gen == gen {
                ui.set_editor_cover(to_image(d));
            }
        });
    }
    app.actions.editing = editing;
    app.actions.adding = adding;
    app.actions.cover = None;
    ui.set_create_open(true);
}
fn choose_cover() {
    let picked = native::choose_cover();
    with_app(|ui, app| match picked {
        Err(e) => ui.set_playlist_error(e.into()),
        Ok(None) => {}
        Ok(Some(path)) => {
            let gen = app.actions.editor_gen;
            ui.set_cover_loading(true);
            std::thread::spawn(move || {
                let result = native::read_cover(&path);
                on_ui(move |ui, app| {
                    if app.actions.editor_gen != gen {
                        return;
                    }
                    ui.set_cover_loading(false);
                    if !ui.get_create_open() {
                        return;
                    }
                    match result {
                        Ok(bytes) => {
                            if let Ok(d) = images::decode(&bytes, 400) {
                                ui.set_editor_cover(to_image(&d));
                            }
                            app.actions.cover = Some(bytes);
                            ui.set_editor_has_cover(true);
                            ui.set_playlist_error("".into());
                        }
                        Err(e) => ui.set_playlist_error(e.into()),
                    }
                });
            });
        }
    });
}
#[derive(Debug)]
struct SaveOutcome {
    playlist: Playlist,
    error: Option<String>,
    added: bool,
    covered: bool,
}
fn perform_save(
    editing: Option<Playlist>,
    adding: Option<Track>,
    cover: Option<Vec<u8>>,
    username: Option<String>,
    name: String,
    description: String,
    mut call: impl FnMut(&str, Value) -> Result<Value, String>,
) -> Result<SaveOutcome, String> {
    let mut p = match editing {
        Some(mut p) => {
            if p.name != name || p.plain_description().unwrap_or_default() != description {
                call(
                    "playlist_edit",
                    json!({"target":p.id,"name":name,"description":description}),
                )?;
                p.name = name.clone();
                p.description = Some(description.clone());
            }
            p
        }
        None => {
            let value = call(
                "playlist_create",
                json!({"name":name,"description":description}),
            )?;
            let mut p: Playlist = serde_json::from_value(value).map_err(|e| e.to_string())?;
            if p.owner.is_none() {
                p.owner = username.map(|id| SpOwner {
                    id,
                    display_name: None,
                });
            }
            p
        }
    };
    let mut outcome = SaveOutcome {
        playlist: p.clone(),
        error: None,
        added: false,
        covered: false,
    };
    if let Some(track) = adding {
        if let Err(e) = call("playlist_add", json!({"target":p.id,"uris":[track.uri]})) {
            log(&format!("playlist_add: {e}"));
            outcome.error =
                Some("La playlist está guardada, pero no se pudo añadir la canción.".into());
            return Ok(outcome);
        }
        outcome.added = true;
    }
    if let Some(bytes) = cover {
        let upload = (|| -> Result<Value, String> {
            let staged = native::StagedCover::new(&bytes)?;
            call("playlist_set_cover", json!({"target":p.id,"path":staged.0}))
        })();
        match upload {
            Ok(value) => {
                if let Some(images) = value
                    .get("images")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<Vec<SpImage>>(v).ok())
                    .filter(|v| !v.is_empty())
                {
                    p.images = Some(images);
                }
                outcome.playlist = p;
                outcome.covered = true;
            }
            Err(e) => {
                log(&format!("playlist_set_cover: {e}"));
                outcome.error =
                    Some("Los datos están guardados, pero no se pudo subir la portada.".into());
            }
        }
    }
    Ok(outcome)
}
fn save_editor(ui: &MainWindow, app: &mut App, name: String, description: String) {
    let name = name.trim().to_owned();
    let description = description.trim().to_owned();
    if name.is_empty() {
        ui.set_playlist_error("Escribe un nombre para la playlist.".into());
        return;
    }
    if description.chars().count() > 300 {
        ui.set_playlist_error("La descripción admite hasta 300 caracteres.".into());
        return;
    }
    if ui.get_playlist_saving() || ui.get_cover_loading() {
        return;
    }
    if app
        .actions
        .editing
        .as_ref()
        .is_some_and(|p| !can_edit(app, p))
    {
        return;
    }
    let Some(engine) = app.engine.clone() else {
        ui.set_playlist_error("Conecta con Spotify primero.".into());
        return;
    };
    ui.set_playlist_saving(true);
    ui.set_playlist_error("".into());
    let (editing, adding, cover, username) = (
        app.actions.editing.clone(),
        app.actions.adding.clone(),
        app.actions.cover.clone(),
        app.username.clone(),
    );
    std::thread::spawn(move || {
        let result = perform_save(
            editing,
            adding,
            cover,
            username,
            name,
            description,
            |cmd, args| {
                engine.call(
                    cmd,
                    args,
                    Duration::from_secs(
                        if cmd == "playlist_create" || cmd == "playlist_set_cover" {
                            60
                        } else {
                            30
                        },
                    ),
                )
            },
        );
        on_ui(move |ui, app| {
            ui.set_playlist_saving(false);
            match result {
                Err(e) => {
                    log(&format!("guardar playlist: {e}"));
                    ui.set_playlist_error(
                        "No se pudo guardar la playlist. Revisa la conexión e inténtalo de nuevo."
                            .into(),
                    );
                }
                Ok(outcome) => {
                    let p = outcome.playlist;
                    app.actions.own_ids.insert(p.id.clone());
                    let mut lists = app.playlists.clone();
                    if let Some(i) = lists.iter().position(|x| x.id == p.id) {
                        lists[i] = p.clone();
                    } else {
                        lists.insert(0, p.clone());
                    }
                    browse::show_playlists(app, lists);
                    if matches!(&app.current_source,Some(browse::Source::Playlist(current)) if current.id==p.id)
                    {
                        app.current_source = Some(browse::Source::Playlist(p.clone()));
                        ui.set_page_title(p.name.clone().into());
                        ui.set_list_description(p.plain_description().unwrap_or_default().into());
                    }
                    if outcome.added {
                        app.actions.adding = None;
                        ui.set_editor_adding("".into());
                    }
                    if outcome.covered {
                        app.actions.cover = None;
                    }
                    app.actions.editing = Some(p.clone());
                    ui.set_editor_editing(true);
                    if let Some(e) = outcome.error {
                        log(&e);
                        ui.set_playlist_error(
                            format!(
                                "{e}\nPuedes volver a pulsar Guardar para reintentar lo pendiente."
                            )
                            .into(),
                        );
                    } else {
                        ui.set_create_open(false);
                        crate::close_now_playing_view(ui, app, false);
                        browse::open_source(ui, app, browse::Source::Playlist(p));
                        queue_ui::toast(ui, app, "Playlist guardada");
                    }
                }
            }
        });
    });
}
fn playlist_action(ui: &MainWindow, app: &mut App, action: i32) {
    if action==3{ui.set_list_menu_open(false);catalog::toggle_pin(ui,app);return;}
    let Some(p) = current_playlist(app) else {
        return;
    };
    ui.set_list_menu_open(false);
    match action {
        0 => open_editor(ui, app, Some(p), None),
        1 => {
            if !app.playlists.iter().any(|x| x.id == p.id) {
                return;
            }
            ui.set_delete_title(
                if can_edit(app, &p) {
                    "Eliminar playlist"
                } else {
                    "Quitar de Tu biblioteca"
                }
                .into(),
            );
            ui.set_delete_message(format!("«{}» dejará de aparecer en tu biblioteca de Spotify. Esta acción se sincroniza con tu cuenta.",p.name).into());
            ui.set_delete_error("".into());
            app.actions.delete_target = Some(p);
            ui.set_delete_open(true);
        }
        2 => {
            if let Err(e) = native::copy(&format!("https://open.spotify.com/playlist/{}", p.id)) {
                error(ui, app, "copiar el enlace", &e);
            } else {
                queue_ui::toast(ui, app, "Enlace de playlist copiado");
            }
        }
        _ => {}
    }
}
fn delete_playlist(ui: &MainWindow, app: &mut App) {
    if ui.get_playlist_deleting() {
        return;
    }
    let (Some(p), Some(engine)) = (app.actions.delete_target.clone(), app.engine.clone()) else {
        return;
    };
    ui.set_playlist_deleting(true);
    ui.set_delete_error("".into());
    std::thread::spawn(move || {
        let result = engine.call(
            "playlist_remove",
            json!({"target":p.id}),
            Duration::from_secs(30),
        );
        on_ui(move |ui, app| {
            ui.set_playlist_deleting(false);
            match result {
                Ok(_) => {
                    ui.set_delete_open(false);
                    app.actions.delete_target = None;
                    let lists = app
                        .playlists
                        .iter()
                        .filter(|x| x.id != p.id)
                        .cloned()
                        .collect();
                    browse::show_playlists(app, lists);
                    if matches!(&app.current_source,Some(browse::Source::Playlist(x)) if x.id==p.id)
                    {
                        app.list_gen += 1;
                        app.current_source = None;
                        app.tracks.clear();
                        refresh_rows(app);
                        ui.set_page(0);
                        ui.set_page_title("Inicio".into());
                    }
                    queue_ui::toast(ui, app, "Playlist quitada de tu biblioteca");
                }
                Err(e) => {
                    log(&e);
                    ui.set_delete_error(
                        "No se pudo quitar la playlist. Inténtalo de nuevo.".into(),
                    );
                }
            }
        });
    });
}

pub fn library_changed(ui: &MainWindow, app: &mut App, kind: &str) {
    if kind == "liked" {
        app.actions.saved_epoch += 1;
        app.actions.checking.clear();
        app.actions
            .saved
            .retain(|uri, _| app.actions.like_pending.contains(uri));
        let mut tracks = app.tracks.clone();
        if let Some(track) = app.player.track.clone() {
            tracks.push(track);
        }
        request_saved(app, tracks);
        render_likes(ui, app);
        if matches!(app.current_source, Some(browse::Source::Liked))
            && app.actions.like_pending.is_empty()
        {
            browse::open_source(ui, app, browse::Source::Liked);
        }
    }
}

pub fn wire(ui: &MainWindow) {
    ui.on_list_filter_changed(|| {
        with_app(|_, app| refresh_rows(app));
    });
    ui.on_toggle_saved(|kind, index| {
        with_app(|ui, app| {
            if let Some(t) = track_at(app, kind, index as usize) {
                toggle_saved(ui, app, t);
            }
        });
    });
    ui.on_open_track_menu(|kind, index| {
        with_app(|ui, app| {
            if let Some(t) = track_at(app, kind, index as usize) {
                open_track_menu(ui, app, t);
            }
        });
    });
    ui.on_open_add_playlist(|kind, index| {
        with_app(|ui, app| {
            if let Some(t) = track_at(app, kind, index as usize) {
                open_add_playlist(ui, app, t);
            }
        });
    });
    ui.on_menu_action(|action| {
        with_app(|ui, app| menu_action(ui, app, action));
    });
    ui.on_open_artist_radio(|| {
        with_app(|ui, app| {
            if let Some(browse::Source::ArtistTop(a)) = app.current_source.clone() {
                let title = format!("Radio de {}", a.name);
                let sub = format!("Canciones similares a {}", a.name);
                let cover = a.image_url().map(str::to_owned);
                browse::open_radio(ui, app, a.uri, title, sub, cover);
            }
        });
    });
    ui.on_menu_add_playlist(|index| {
        with_app(|ui, app| add_to_playlist(ui, app, index as usize));
    });
    ui.on_menu_open_artist(|index| {
        with_app(|ui, app| open_artist(ui, app, index as usize));
    });
    ui.on_open_track_artist(|kind, index| {
        with_app(|ui, app| prompt_or_open_artist(ui, app, kind, index as usize));
    });
    ui.on_open_track_album(|kind, index| {
        with_app(|ui, app| {
            if let Some(t) = track_at(app, kind, index as usize) {
                open_album(ui, app, &t);
            }
        });
    });
    ui.on_open_picked_artist(|index| {
        with_app(|ui, app| open_picked_artist(ui, app, index as usize));
    });
    ui.on_open_picked_album(|| {
        with_app(open_picked_album);
    });
    ui.on_new_playlist(|| {
        with_app(|ui, app| open_editor(ui, app, None, None));
    });
    ui.on_create_playlist(|name, description| {
        with_app(|ui, app| save_editor(ui, app, name.to_string(), description.to_string()));
    });
    ui.on_choose_cover(choose_cover);
    ui.on_playlist_action(|action| {
        with_app(|ui, app| playlist_action(ui, app, action));
    });
    ui.on_confirm_delete(|| {
        with_app(delete_playlist);
    });
    ui.on_library_context(|index| {
        with_app(|ui, app| {
            if index < 0 {return;}
            browse::open_library_row(ui, app, index as usize);
            ui.set_list_menu_open(true);
        });
    });
    ui.on_open_info_source(|| {
        with_app(|ui, _| {
            let url = ui.get_info_url();
            if url.starts_with("https://musicbrainz.org/recording/") {
                open_in_browser(&url);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sharing_rejects_non_tracks_and_invalid_identifiers() {
        let mut track: Track = serde_json::from_value(
            json!({"uri":"spotify:track:abc123","name":"Song","artists":[],"duration_ms":1}),
        )
        .unwrap();
        assert_eq!(
            share_url(&track).as_deref(),
            Some("https://open.spotify.com/track/abc123")
        );
        track.uri = "spotify:track:abc/other?key=1".into();
        assert!(share_url(&track).is_none());
        track.uri = "spotify:episode:abc123".into();
        assert!(share_url(&track).is_none());
    }
    #[test]
    fn retry_after_partial_creation_does_not_create_a_second_playlist() {
        let track: Track = serde_json::from_value(
            json!({"uri":"spotify:track:abc123","name":"Song","artists":[],"duration_ms":1}),
        )
        .unwrap();
        let mut commands = vec![];
        let first = perform_save(
            None,
            Some(track.clone()),
            None,
            Some("me".into()),
            "New".into(),
            "".into(),
            |cmd, _| {
                commands.push(cmd.to_owned());
                if cmd == "playlist_create" {
                    Ok(json!({"id":"p","uri":"spotify:playlist:p","name":"New"}))
                } else {
                    Err("temporary failure".into())
                }
            },
        )
        .unwrap();
        assert_eq!(commands, ["playlist_create", "playlist_add"]);
        assert!(first.error.is_some());
        assert!(!first.added);
        assert_eq!(first.playlist.owner.as_ref().unwrap().id, "me");
        commands.clear();
        let retried = perform_save(
            Some(first.playlist),
            Some(track),
            None,
            Some("me".into()),
            "New".into(),
            "".into(),
            |cmd, _| {
                commands.push(cmd.to_owned());
                Ok(Value::Null)
            },
        )
        .unwrap();
        assert_eq!(commands, ["playlist_add"]);
        assert!(retried.added);
        assert!(retried.error.is_none());
    }
    #[test]
    fn cover_upload_has_a_real_file_and_cleans_it_after_failure() {
        let p = Playlist::new("p", "spotify:playlist:p", "Old");
        let mut commands = vec![];
        let mut staged = None;
        let result = perform_save(
            Some(p),
            None,
            Some(vec![1, 2, 3]),
            None,
            "Updated".into(),
            "Description".into(),
            |cmd, args| {
                commands.push(cmd.to_owned());
                if cmd == "playlist_set_cover" {
                    let path = std::path::PathBuf::from(args["path"].as_str().unwrap());
                    assert!(path.is_file());
                    staged = Some(path);
                    Err("upload failed".into())
                } else {
                    Ok(Value::Null)
                }
            },
        )
        .unwrap();
        assert_eq!(commands, ["playlist_edit", "playlist_set_cover"]);
        assert_eq!(result.playlist.name, "Updated");
        assert!(result.error.is_some());
        assert!(!result.covered);
        assert!(!staged.unwrap().exists());
    }

    #[test]
    fn artist_picker_filter_deduplicates_and_skips_missing_ids() {
        let t: Track = serde_json::from_value(json!({
            "uri": "spotify:track:1",
            "name": "Collab",
            "artists": [
                {"id": "a1", "name": "Artista Uno"},
                {"id": "a1", "name": "Artista Uno"},
                {"id": "", "name": "Sin ID"},
                {"id": "a2", "name": "Artista Dos"}
            ],
            "duration_ms": 1000
        }))
        .unwrap();
        let filtered = filter_valid_artists(t.artists);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].name, "Artista Uno");
        assert_eq!(filtered[1].name, "Artista Dos");
    }

    #[test]
    fn album_name_extraction_from_track() {
        let t: Track = serde_json::from_value(json!({
            "uri": "spotify:track:2",
            "name": "Die For You - Remix",
            "artists": [{"id": "a1", "name": "The Weeknd"}, {"id": "a2", "name": "Ariana Grande"}],
            "album": {
                "id": "alb1",
                "name": "Starboy (Deluxe)",
                "uri": "spotify:album:alb1"
            },
            "duration_ms": 230000
        }))
        .unwrap();
        assert_eq!(album_name_for_track(&t), Some("Starboy (Deluxe)".to_string()));

        let t_no_album: Track = serde_json::from_value(json!({
            "uri": "spotify:track:3",
            "name": "Track Solo",
            "artists": [{"id": "a1", "name": "The Weeknd"}],
            "duration_ms": 200000
        }))
        .unwrap();
        assert_eq!(album_name_for_track(&t_no_album), None);
    }
}
