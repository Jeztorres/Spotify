//! Configuración, perfil y cierre de sesión (las mismas pantallas que la app de Mac).
use super::*;
use spotify_core::{ArtistItem, SpImage};

/// Alguien de las listas de seguidores / seguidos: otro usuario o un artista.
#[derive(Clone, Debug, PartialEq)]
pub enum Person {
    User(String),
    Artist(ArtistItem),
}

#[derive(Default)]
pub struct State {
    artists: Vec<ArtistItem>,
    playlists: Vec<Playlist>,
    pub artist_model: Rc<VecModel<CardEntry>>,
    pub playlist_model: Rc<VecModel<CardEntry>>,
    followers: Vec<Person>,
    following: Vec<Person>,
    pub followers_model: Rc<VecModel<CardEntry>>,
    pub following_model: Rc<VecModel<CardEntry>>,
    /// Perfil ajeno abierto (`None` = el propio).
    viewing: Option<String>,
    /// Número de la visita al perfil: descarta respuestas de un perfil que ya se dejó.
    visit: u64,
    /// Marca del último perfil cargado (para no pedirlo en cada visita).
    loaded: Option<Instant>,
    loading: bool,
    cache_applied: bool,
    last_json: Option<Value>,
}

fn prefs_path() -> std::path::PathBuf {
    engine::data_dir().join("prefs.json")
}

const EQ_BAND_COUNT: usize = 9;

/// Carga las preferencias guardadas y las pone en la interfaz.
fn eq_preset_bands(idx: i32) -> [f32; EQ_BAND_COUNT] {
    match idx {
        1 => [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 1.0, 1.0],       // Graves
        2 => [-1.0, 0.0, 2.0, 3.0, 4.0, 3.0, 2.0, 0.0, -1.0],    // Pop
        3 => [5.0, 4.0, 3.0, 0.0, -2.0, 1.0, 3.0, 4.0, 5.0],     // Rock
        4 => [-2.0, -2.0, -1.0, 2.0, 4.0, 4.0, 3.0, 1.0, 0.0],   // Vocal
        5 => [6.0, 5.0, 3.0, 0.0, -1.0, 1.0, 3.0, 4.0, 5.0],     // Electro
        _ => [0.0; EQ_BAND_COUNT],                               // Plano
    }
}

fn get_eq_bands(ui: &MainWindow) -> [f32; EQ_BAND_COUNT] {
    use slint::Model;
    let model = ui.get_pref_eq_bands();
    std::array::from_fn(|i| model.row_data(i).unwrap_or(0.0).clamp(-12.0, 12.0))
}

fn set_eq_bands_ui(ui: &MainWindow, bands: [f32; EQ_BAND_COUNT]) {
    ui.set_pref_eq_bands(ModelRc::new(VecModel::from(bands.to_vec())));
}

pub fn apply_i18n(ui: &MainWindow, lang: &str) {
    let locale = if lang == "en" { "en" } else { "es" };
    rust_i18n::set_locale(locale);
    ui.set_pref_lang(locale.into());
    let g = ui.global::<I18n>();
    g.set_lang_label(t!("lang_label").as_ref().into());
    g.set_lang_detail(t!("lang_detail").as_ref().into());
    g.set_nav_home(t!("nav_home").as_ref().into());
    g.set_nav_search(t!("nav_search").as_ref().into());
    g.set_nav_library(t!("nav_library").as_ref().into());
    g.set_search_filter_all(t!("search_filter_all").as_ref().into());
    g.set_search_filter_tracks(t!("search_filter_tracks").as_ref().into());
    g.set_search_filter_artists(t!("search_filter_artists").as_ref().into());
    g.set_search_filter_albums(t!("search_filter_albums").as_ref().into());
    g.set_search_filter_playlists(t!("search_filter_playlists").as_ref().into());
    g.set_home_filter_all(t!("home_filter_all").as_ref().into());
    g.set_home_filter_made_for_you(t!("home_filter_made_for_you").as_ref().into());
    g.set_home_filter_recent(t!("home_filter_recent").as_ref().into());
    g.set_home_refresh(t!("home_refresh").as_ref().into());
    g.set_settings_title(t!("settings_title").as_ref().into());
    g.set_settings_subtitle(t!("settings_subtitle").as_ref().into());
    g.set_settings_group_audio(t!("settings_group_audio").as_ref().into());
    g.set_settings_group_appearance(t!("settings_group_appearance").as_ref().into());
    g.set_settings_group_account(t!("settings_group_account").as_ref().into());
    g.set_eq_title(t!("eq_title").as_ref().into());
    g.set_eq_on_detail(t!("eq_on_detail").as_ref().into());
    g.set_eq_off_detail(t!("eq_off_detail").as_ref().into());
    g.set_eq_preset_flat(t!("eq_preset_flat").as_ref().into());
    g.set_eq_preset_bass(t!("eq_preset_bass").as_ref().into());
    g.set_eq_preset_pop(t!("eq_preset_pop").as_ref().into());
    g.set_eq_preset_rock(t!("eq_preset_rock").as_ref().into());
    g.set_eq_preset_vocal(t!("eq_preset_vocal").as_ref().into());
    g.set_eq_preset_electro(t!("eq_preset_electro").as_ref().into());
    g.set_eq_preset_custom(t!("eq_preset_custom").as_ref().into());
    g.set_crossfade_title(t!("crossfade_title").as_ref().into());
    g.set_crossfade_off(t!("crossfade_off").as_ref().into());
    g.set_crossfade_on_prefix(t!("crossfade_on_prefix").as_ref().into());
    g.set_crossfade_on_suffix(t!("crossfade_on_suffix").as_ref().into());
    g.set_normalise_title(t!("normalise_title").as_ref().into());
    g.set_normalise_detail(t!("normalise_detail").as_ref().into());
    g.set_lyrics_open_title(t!("lyrics_open_title").as_ref().into());
    g.set_lyrics_open_detail(t!("lyrics_open_detail").as_ref().into());
    g.set_soloist_title(t!("soloist_title").as_ref().into());
    g.set_soloist_detail(t!("soloist_detail").as_ref().into());
    g.set_btn_configure(t!("btn_configure").as_ref().into());
    g.set_motion_title(t!("motion_title").as_ref().into());
    g.set_motion_detail(t!("motion_detail").as_ref().into());
    g.set_battery_title(t!("battery_title").as_ref().into());
    g.set_battery_detail(t!("battery_detail").as_ref().into());
    g.set_sidebar_title(t!("sidebar_title").as_ref().into());
    g.set_sidebar_detail(t!("sidebar_detail").as_ref().into());
    g.set_tray_title(t!("tray_title").as_ref().into());
    g.set_tray_detail(t!("tray_detail").as_ref().into());
    g.set_account_default(t!("account_default").as_ref().into());
    g.set_account_detail(t!("account_detail").as_ref().into());
    g.set_btn_view_profile(t!("btn_view_profile").as_ref().into());
    g.set_cache_title(t!("cache_title").as_ref().into());
    g.set_cache_detail(t!("cache_detail").as_ref().into());
    g.set_btn_clear(t!("btn_clear").as_ref().into());
    g.set_updates_title(t!("updates_title").as_ref().into());
    g.set_btn_check_update(t!("btn_check_update").as_ref().into());
    g.set_btn_update_now(t!("btn_update_now").as_ref().into());
    g.set_btn_updating(t!("btn_updating").as_ref().into());
    g.set_sign_out_title(t!("sign_out_title").as_ref().into());
    g.set_sign_out_detail(t!("sign_out_detail").as_ref().into());
    g.set_btn_sign_out(t!("btn_sign_out").as_ref().into());
    g.set_footer_premium(t!("footer_premium").as_ref().into());
    g.set_profile_badge_me(t!("profile_badge_me").as_ref().into());
    g.set_profile_badge_other(t!("profile_badge_other").as_ref().into());
    g.set_profile_follow(t!("profile_follow").as_ref().into());
    g.set_profile_following(t!("profile_following").as_ref().into());
    g.set_profile_recent_artists(t!("profile_recent_artists").as_ref().into());
    g.set_profile_public_playlists(t!("profile_public_playlists").as_ref().into());
    g.set_menu_add_like(t!("menu_add_like").as_ref().into());
    g.set_menu_remove_like(t!("menu_remove_like").as_ref().into());
    g.set_menu_add_queue(t!("menu_add_queue").as_ref().into());
    g.set_menu_add_playlist(t!("menu_add_playlist").as_ref().into());
    g.set_menu_new_playlist(t!("menu_new_playlist").as_ref().into());
    g.set_menu_go_artist(t!("menu_go_artist").as_ref().into());
    g.set_menu_go_radio(t!("menu_go_radio").as_ref().into());
    g.set_menu_go_album(t!("menu_go_album").as_ref().into());
    g.set_menu_credits(t!("menu_credits").as_ref().into());
    g.set_menu_share(t!("menu_share").as_ref().into());
    g.set_menu_copy_link(t!("menu_copy_link").as_ref().into());
    g.set_btn_back(t!("btn_back").as_ref().into());
    g.set_btn_close(t!("btn_close").as_ref().into());
    g.set_btn_view_source(t!("btn_view_source").as_ref().into());
}

fn load_prefs(ui: &MainWindow) {
    let saved = std::fs::read(prefs_path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let get = |key: &str, default: bool| saved.as_ref().and_then(|v| v.get(key)).and_then(Value::as_bool).unwrap_or(default);
    let get_f = |key: &str, default: f32| saved.as_ref().and_then(|v| v.get(key)).and_then(Value::as_f64).map(|x| x as f32).unwrap_or(default);
    let get_i = |key: &str, default: i32| saved.as_ref().and_then(|v| v.get(key)).and_then(Value::as_i64).map(|x| x as i32).unwrap_or(default);
    let lang = saved.as_ref().and_then(|v| v.get("lang")).and_then(Value::as_str).unwrap_or("es");
    apply_i18n(ui, lang);
    ui.set_pref_motion(get("motion", true));
    ui.set_pref_battery_saver(get("battery_saver", false));
    ui.set_pref_lyrics(get("lyrics_on_open", false));
    ui.set_sidebar_visible(get("sidebar", true));
    ui.set_show_translation(get("translate_lyrics", false));
    ui.set_pref_tray(get("tray", true));
    ui.set_pref_wavy_scrubber(get("wavy_scrubber", false));
    ui.set_pref_normalise(get("normalise", true));
    ui.set_pref_eq_enabled(get("eq_enabled", true));
    ui.set_pref_crossfade(get_f("crossfade_secs", 0.0));
    let preset = get_i("eq_preset", 0);
    ui.set_pref_eq_preset(preset);
    let mut bands = eq_preset_bands(preset);
    if let Some(arr) = saved.as_ref().and_then(|v| v.get("eq_bands")).and_then(Value::as_array) {
        if arr.len() == EQ_BAND_COUNT {
            for (i, val) in arr.iter().enumerate() {
                if let Some(db) = val.as_f64() {
                    bands[i] = (db as f32).clamp(-12.0, 12.0);
                }
            }
        }
    }
    set_eq_bands_ui(ui, bands);
    ui.set_motion_paused(!ui.get_pref_motion());
}

pub fn save(ui: &MainWindow) {
    save_prefs(ui);
}

fn save_prefs(ui: &MainWindow) {
    // Si la barra lateral se ocultó sola por el panel derecho, se guarda la preferencia del usuario.
    let sidebar = ui.get_sidebar_visible() || ui.get_sidebar_auto_hidden();
    let bands = get_eq_bands(ui);
    let value = json!({
        "lang": ui.get_pref_lang().as_str(),
        "motion": ui.get_pref_motion(),
        "battery_saver": ui.get_pref_battery_saver(),
        "lyrics_on_open": ui.get_pref_lyrics(),
        "sidebar": sidebar,
        "translate_lyrics": ui.get_show_translation(),
        "tray": ui.get_pref_tray(),
        "wavy_scrubber": ui.get_pref_wavy_scrubber(),
        "normalise": ui.get_pref_normalise(),
        "eq_enabled": ui.get_pref_eq_enabled(),
        "crossfade_secs": ui.get_pref_crossfade(),
        "eq_preset": ui.get_pref_eq_preset(),
        "eq_bands": bands.to_vec(),
    });
    let _ = std::fs::write(prefs_path(), value.to_string());
}

/// Calcula en qué fracción de la barra de progreso empieza el crossfade de salida.
pub fn update_crossfade_marker(ui: &MainWindow, duration_ms: u32) {
    let cf = ui.get_pref_crossfade();
    if cf < 0.5 || duration_ms == 0 {
        ui.set_crossfade_start(1.0);
        return;
    }
    let dur_s = duration_ms as f32 / 1000.0;
    let eff = cf.min(dur_s / 3.0);
    ui.set_crossfade_start((1.0_f32 - eff / dur_s).clamp(0.0, 1.0));
}

/// Envía al motor las preferencias de reproducción (crossfade y ecualizador de 5 bandas).
pub fn sync_engine_prefs(ui: &MainWindow, app: &App) {
    let Some(engine) = &app.engine else { return };
    engine.fire("settings", json!({
        "crossfade_secs": ui.get_pref_crossfade(),
        "normalise": ui.get_pref_normalise(),
        "eq_enabled": ui.get_pref_eq_enabled(),
        "eq_bands": get_eq_bands(ui),
    }));
}

/// `spotify:image:…` / `spotify:mosaic:…` → URL https.
fn image_url(raw: &str) -> Option<String> {
    if let Some(id) = raw.strip_prefix("spotify:image:") {
        return Some(format!("https://i.scdn.co/image/{id}"));
    }
    if let Some(ids) = raw.strip_prefix("spotify:mosaic:") {
        return Some(format!("https://mosaic.scdn.co/640/{}", ids.split(':').collect::<String>()));
    }
    (raw.starts_with("https://")).then(|| raw.to_owned())
}

fn card_cover(app: &mut App, url: Option<String>, apply: impl FnOnce(&mut App, Image) + Send + 'static) {
    if let Some(url) = url {
        load_cover(app, &url, 320, move |_, app, d| apply(app, to_image(d)));
    }
}

pub fn open_profile(ui: &MainWindow, app: &mut App) {
    push_history(ui, app);
    media::reset(ui, app);
    ui.set_profile_scroll(0.0);
    ui.set_page(5);
    ui.set_page_title("Perfil".into());
    ui.set_list_tint(slint::Color::from_rgb_u8(42, 45, 62));
    ui.set_profile_is_me(true);
    app.settings.visit += 1;
    if app.settings.viewing.take().is_some() {
        // Volviendo de un perfil ajeno: se vuelve a pintar el propio (desde la caché si está fresca).
        reset_profile_view(ui, app);
        app.settings.loaded = None;
        app.settings.cache_applied = false;
    }
    load_profile(ui, app);
    load_people(app, None);
}

fn reset_profile_view(ui: &MainWindow, app: &mut App) {
    app.settings.last_json = None;
    app.settings.artist_model.set_vec(Vec::<CardEntry>::new());
    app.settings.playlist_model.set_vec(Vec::<CardEntry>::new());
    app.settings.followers_model.set_vec(Vec::<CardEntry>::new());
    app.settings.following_model.set_vec(Vec::<CardEntry>::new());
    ui.set_profile_name("".into());
    ui.set_profile_stats("".into());
    ui.set_profile_image(Image::default());
    ui.set_profile_tint(slint::Color::from_rgb_u8(40, 44, 60));
    ui.set_profile_pill(slint::Color::from_rgb_u8(52, 57, 78));
    ui.set_profile_accent(slint::Color::from_rgb_u8(190, 190, 190));
}

/// Perfil de otro usuario (`/users/{id}`): creador de una playlist, seguidor, seguido…
pub fn open_user_profile(ui: &MainWindow, app: &mut App, user: String) {
    if app.username.as_deref() == Some(user.as_str()) {
        return open_profile(ui, app);
    }
    push_history(ui, app);
    media::reset(ui, app);
    ui.set_profile_scroll(0.0);
    ui.set_page(5);
    ui.set_page_title("Perfil".into());
    ui.set_list_tint(slint::Color::from_rgb_u8(42, 45, 62));
    ui.set_profile_is_me(false);
    app.settings.viewing = Some(user.clone());
    app.settings.visit += 1;
    reset_profile_view(ui, app);
    ui.set_profile_message("Cargando perfil…".into());
    let Some(engine) = app.engine.clone() else {
        return ui.set_profile_message("Conecta con Spotify para ver este perfil.".into());
    };
    let visit = app.settings.visit;
    let who = user.clone();
    std::thread::spawn(move || {
        catalog::cached_call(&engine, "user_profile", json!({"user": who}), |reply| {
            on_ui(move |ui, app| {
                if app.settings.visit == visit {
                    apply_profile(ui, app, reply, false);
                }
            });
        });
    });
    load_follow_state(ui, app, format!("spotify:user:{user}"));
    load_people(app, Some(user));
}

fn current_follow_uri(ui: &MainWindow, app: &App) -> Option<String> {
    match ui.get_page() {
        3 => match &app.current_source {
            Some(browse::Source::ArtistTop(a)) => Some(a.uri.clone()),
            _ => None,
        },
        5 if !ui.get_profile_is_me() => app.settings.viewing.as_ref().map(|id| format!("spotify:user:{id}")),
        _ => None,
    }
}

/// Consulta en el motor si el usuario actual sigue al artista o perfil abierto.
pub fn load_follow_state(ui: &MainWindow, app: &App, uri: String) {
    ui.set_target_following(false);
    let Some(engine) = app.engine.clone() else { return };
    ui.set_target_follow_busy(true);
    std::thread::spawn(move || {
        let reply = engine.call("is_following", json!({"uri": uri}), std::time::Duration::from_secs(10));
        let following = reply.as_ref().ok().and_then(|v| v.get("following")).and_then(Value::as_bool).unwrap_or(false);
        on_ui(move |ui, app| {
            if current_follow_uri(ui, app).as_deref() == Some(uri.as_str()) {
                ui.set_target_following(following);
                ui.set_target_follow_busy(false);
            }
        });
    });
}

fn toggle_follow(ui: &MainWindow, app: &App) {
    if ui.get_target_follow_busy() { return; }
    let Some(uri) = current_follow_uri(ui, app) else { return };
    let want = !ui.get_target_following();
    let Some(engine) = app.engine.clone() else { return };
    ui.set_target_follow_busy(true);
    std::thread::spawn(move || {
        let ok = engine.call("set_following", json!({"uri": uri, "following": want}), std::time::Duration::from_secs(10)).is_ok();
        on_ui(move |ui, app| {
            if current_follow_uri(ui, app).as_deref() == Some(uri.as_str()) {
                if ok { ui.set_target_following(want); }
                ui.set_target_follow_busy(false);
            }
        });
    });
}

/// Seguidores y seguidos (usuarios y artistas) del perfil abierto.
fn load_people(app: &mut App, user: Option<String>) {
    let Some(engine) = app.engine.clone() else { return };
    let visit = app.settings.visit;
    for cmd in ["user_followers", "user_following"] {
        let (engine, user) = (engine.clone(), user.clone());
        std::thread::spawn(move || {
            let args = match &user { Some(u) => json!({"user": u}), None => json!({}) };
            catalog::cached_call(&engine, cmd, args, |reply| {
                let Ok(value) = reply else { return };
                on_ui(move |_, app| {
                    if app.settings.visit == visit {
                        apply_people(app, cmd == "user_followers", &value);
                    }
                });
            });
        });
    }
}

/// `{"profiles":[{"uri":"spotify:user:…"|"spotify:artist:…","name":…,"image_url":…,"followers_count":…}]}`
pub(crate) fn parse_people(value: &Value) -> Vec<(Person, String, String, Option<String>)> {
    let list = value.get("profiles").or_else(|| value.get("items")).and_then(Value::as_array).cloned().unwrap_or_default();
    list.iter().filter_map(|v| {
        let uri = v.get("uri").and_then(Value::as_str)?;
        let name = v.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
        let id = uri.rsplit(':').next()?.to_owned();
        if id.is_empty() { return None; }
        let image = v.get("image_url").or_else(|| v.get("imageUrl")).and_then(Value::as_str).and_then(image_url);
        let followers = v.get("followers_count").or_else(|| v.get("followersCount")).and_then(Value::as_i64);
        let (person, sub) = if uri.starts_with("spotify:artist:") {
            let mut a = ArtistItem::new(&id, &name);
            a.images = image.clone().map(|u| vec![SpImage { url: u }]);
            (Person::Artist(a), "Artista".to_owned())
        } else {
            (Person::User(id.clone()), followers.map(|n| count_text(n, "seguidor", "seguidores")).unwrap_or_else(|| "Perfil".into()))
        };
        Some((person, if name.is_empty() { id } else { name }, sub, image))
    }).collect()
}

fn apply_people(app: &mut App, followers: bool, value: &Value) {
    let people = parse_people(value);
    let model = if followers { app.settings.followers_model.clone() } else { app.settings.following_model.clone() };
    model.set_vec(people.iter().map(|(_, name, sub, _)| CardEntry { title: name.clone().into(), subtitle: sub.clone().into(), cover: Image::default() }).collect::<Vec<_>>());
    let jobs: Vec<_> = people.iter().enumerate().map(|(i, (_, name, _, url))| (i, name.clone(), url.clone())).collect();
    if followers { app.settings.followers = people.into_iter().map(|p| p.0).collect(); }
    else { app.settings.following = people.into_iter().map(|p| p.0).collect(); }
    for (i, name, url) in jobs {
        let Some(url) = url else { continue };
        load_cover(app, &url, 180, move |_, app, d| {
            let model = if followers { app.settings.followers_model.clone() } else { app.settings.following_model.clone() };
            if let Some(mut row) = model.row_data(i).filter(|r| r.title.as_str() == name) {
                row.cover = to_image(&images::circle(d));
                model.set_row_data(i, row);
            }
        });
    }
}

pub fn open_profile_people(ui: &MainWindow, app: &mut App, tab: i32) {
    push_history(ui, app);
    ui.set_profile_people_tab(tab);
    ui.set_page(6);
    ui.set_page_title(if tab == 1 { "Seguidores" } else { "Siguiendo" }.into());
}

pub fn open_settings(ui: &MainWindow, app: &mut App) {
    push_history(ui, app);
    media::reset(ui, app);
    ui.set_page(4);
    ui.set_page_title("Configuración".into());
    ui.set_list_tint(slint::Color::from_rgb_u8(42, 45, 62));
    ui.set_cache_message("".into());
}

fn profile_cache_path() -> std::path::PathBuf {
    let dir = engine::data_dir().join("profile-cache");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("user-profile.json")
}

fn read_profile_cache(path: &std::path::Path) -> Option<(Value, bool)> {
    let bytes = std::fs::read(path).ok()?;
    let value = serde_json::from_slice::<Value>(&bytes).ok()?;
    let fresh = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(300));
    Some((value, fresh))
}

fn write_profile_cache(path: &std::path::Path, value: &Value) {
    if let Ok(bytes) = serde_json::to_vec(value) {
        let _ = std::fs::write(path, bytes);
    }
}

pub fn preload(ui: &MainWindow, app: &mut App) {
    load_profile_impl(ui, app, false);
}

fn load_profile(ui: &MainWindow, app: &mut App) {
    load_profile_impl(ui, app, true);
}

fn load_profile_impl(ui: &MainWindow, app: &mut App, show_ui_errors: bool) {
    if app.settings.loaded.is_some_and(|t| t.elapsed() < Duration::from_secs(300)) {
        return;
    }
    if !app.settings.cache_applied {
        app.settings.cache_applied = true;
        if let Some((cached, fresh)) = read_profile_cache(&profile_cache_path()) {
            apply_profile(ui, app, Ok(cached), fresh);
            if fresh {
                return;
            }
        }
    }
    if app.settings.loading {
        return;
    }
    let Some(engine) = app.engine.clone() else {
        if show_ui_errors && app.settings.last_json.is_none() {
            ui.set_profile_message("Conecta con Spotify para ver tu perfil.".into());
        }
        return;
    };
    app.settings.loading = true;
    if show_ui_errors && app.settings.last_json.is_none() {
        ui.set_profile_message("Cargando perfil…".into());
    }
    let cache_path = profile_cache_path();
    std::thread::spawn(move || {
        let reply = engine.call("user_profile", json!({}), Duration::from_secs(20));
        if let Ok(fresh) = &reply {
            write_profile_cache(&cache_path, fresh);
        }
        on_ui(move |ui, app| {
            app.settings.loading = false;
            match reply {
                Ok(p) => apply_profile(ui, app, Ok(p), true),
                Err(e) => {
                    if show_ui_errors && app.settings.last_json.is_none() {
                        ui.set_profile_message(format!("No se pudo cargar el perfil ({e}).").into());
                    } else if app.settings.last_json.is_some() {
                        log(&format!("user_profile: se mantiene la copia guardada ({e})"));
                    }
                }
            }
        });
    });
}

fn count_text(n: i64, one: &str, many: &str) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    format!("{out} {}", if n == 1 { one } else { many })
}

fn apply_profile(ui: &MainWindow, app: &mut App, reply: Result<Value, String>, mark_loaded: bool) {
    let is_me = app.settings.viewing.is_none();
    let p = match reply {
        Ok(p) => p,
        Err(e) => {
            ui.set_profile_message(format!("No se pudo cargar el perfil ({e}).").into());
            return;
        }
    };
    ui.set_profile_message("".into());
    if mark_loaded && is_me {
        app.settings.loaded = Some(Instant::now());
    }
    if app.settings.last_json.as_ref() == Some(&p) {
        return;
    }
    app.settings.last_json = Some(p.clone());
    let text = |key: &str| p.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let fallback = if is_me { app.username.clone() } else { Some(text("username")).filter(|n| !n.is_empty()) };
    let name = Some(text("name")).filter(|n| !n.is_empty())
        .or(fallback).unwrap_or_else(|| "Perfil".into());
    ui.set_profile_name(name.clone().into());
    if is_me { ui.set_user_name(name.into()); }
    let mut stats = vec![];
    let playlists = p.get("public_playlists").or_else(|| p.get("publicPlaylists")).and_then(Value::as_array).cloned().unwrap_or_default();
    let total = p.get("total_public_playlists_count").or_else(|| p.get("totalPublicPlaylistsCount")).and_then(Value::as_i64).unwrap_or(playlists.len() as i64);
    let playlists_txt = count_text(total, "playlist pública", "playlists públicas");
    ui.set_profile_playlists_count(playlists_txt.clone().into());
    stats.push(playlists_txt);
    if let Some(n) = p.get("followers_count").or_else(|| p.get("followersCount")).and_then(Value::as_i64) {
        let txt = count_text(n, "seguidor", "seguidores");
        ui.set_profile_followers_count(txt.clone().into());
        stats.push(txt);
    } else {
        ui.set_profile_followers_count("".into());
    }
    if let Some(n) = p.get("following_count").or_else(|| p.get("followingCount")).and_then(Value::as_i64) {
        let txt = count_text(n, "seguido", "seguidos");
        ui.set_profile_following_count(txt.clone().into());
        stats.push(txt);
    } else {
        ui.set_profile_following_count("".into());
    }
    ui.set_profile_stats(stats.join("  ·  ").into());

    if let Some(url) = p.get("image_url").or_else(|| p.get("imageUrl")).and_then(Value::as_str).and_then(image_url) {
        let visit = app.settings.visit;
        load_cover(app, &url, 400, move |ui, app, d| {
            if app.settings.visit == visit || is_me {
                if let Some(c) = d.color {
                    ui.set_profile_accent(slint::Color::from_rgb_u8(c[0], c[1], c[2]));
                    let [r, g, b] = color::deep(c);
                    ui.set_profile_tint(slint::Color::from_rgb_u8(r, g, b));
                    let [pr, pg, pb] = color::pill(c);
                    ui.set_profile_pill(slint::Color::from_rgb_u8(pr, pg, pb));
                }
                let img = to_image(d);
                if is_me {
                    ui.set_my_profile_image(img.clone());
                }
                if app.settings.visit == visit {
                    ui.set_profile_image(img);
                }
            }
        });
    }

    let entry_name = |v: &Value| v.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
    let entry_image = |v: &Value| v.get("image_url").or_else(|| v.get("imageUrl")).and_then(Value::as_str).and_then(image_url);
    let id_of = |v: &Value| v.get("uri").and_then(Value::as_str).unwrap_or("").rsplit(':').next().unwrap_or("").to_owned();

    let artists: Vec<(ArtistItem, Option<String>)> = p.get("recently_played_artists").or_else(|| p.get("recentlyPlayedArtists"))
        .and_then(Value::as_array).into_iter().flatten().filter(|v| !id_of(v).is_empty()).map(|v| {
            let url = entry_image(v);
            let mut a = ArtistItem::new(&id_of(v), &entry_name(v));
            a.images = url.clone().map(|u| vec![SpImage { url: u }]);
            (a, url)
        }).collect();
    let lists: Vec<(Playlist, Option<String>, String)> = playlists.iter().filter(|v| !id_of(v).is_empty()).map(|v| {
        let url = entry_image(v);
        let uri = v.get("uri").and_then(Value::as_str).unwrap_or("").to_owned();
        let mut pl = Playlist::new(&id_of(v), &uri, &entry_name(v));
        pl.images = url.clone().map(|u| vec![SpImage { url: u }]);
        let sub = v.get("followers_count").or_else(|| v.get("followersCount")).and_then(Value::as_i64)
            .map(|n| count_text(n, "seguidor", "seguidores")).unwrap_or_else(|| "Playlist".into());
        (pl, url, sub)
    }).collect();

    app.settings.artists = artists.iter().map(|(a, _)| a.clone()).collect();
    app.settings.playlists = lists.iter().map(|(p, _, _)| p.clone()).collect();
    app.settings.artist_model.set_vec(artists.iter().map(|(a, _)| CardEntry {
        title: a.name.clone().into(), subtitle: "Artista".into(), cover: Image::default(),
    }).collect::<Vec<_>>());
    app.settings.playlist_model.set_vec(lists.iter().map(|(p, _, sub)| CardEntry {
        title: p.name.clone().into(), subtitle: sub.clone().into(), cover: Image::default(),
    }).collect::<Vec<_>>());
    for (i, (_, url)) in artists.into_iter().enumerate() {
        card_cover(app, url, move |app, image| {
            if let Some(mut row) = app.settings.artist_model.row_data(i) {
                row.cover = image;
                app.settings.artist_model.set_row_data(i, row);
            }
        });
    }
    for (i, (_, url, _)) in lists.into_iter().enumerate() {
        card_cover(app, url, move |app, image| {
            if let Some(mut row) = app.settings.playlist_model.row_data(i) {
                row.cover = image;
                app.settings.playlist_model.set_row_data(i, row);
            }
        });
    }
}

fn clear_cache(ui: &MainWindow) {
    let dir = crate::engine::cache_dir().join("covers");
    let mut bytes = 0u64;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(entry.path()).is_ok() {
                bytes += len;
            }
        }
    }
    ui.set_cache_message(format!("Caché vaciada ({:.1} MB liberados).", bytes as f64 / 1_048_576.0).into());
}

fn sign_out(ui: &MainWindow, app: &mut App) {
    if let Some(engine) = app.engine.clone() {
        std::thread::spawn(move || {
            let _ = engine.call("logout", json!({}), Duration::from_secs(8));
        });
    }
    secrets::remove(secrets::ENGINE_CREDENTIALS);
    let _ = std::fs::remove_file(profile_cache_path());
    app.ready = false;
    app.settings.loaded = None;
    app.settings.loading = false;
    app.settings.cache_applied = false;
    app.settings.last_json = None;
    app.history.clear();
    app.forward_history.clear();
    ui.set_can_go_back(false);
    ui.set_can_go_forward(false);
    ui.set_page(0);
    ui.set_profile_name("".into());
    ui.set_my_profile_image(Image::default());
    ui.set_profile_image(Image::default());
    ui.set_profile_tint(slint::Color::from_rgb_u8(40, 44, 60));
    ui.set_profile_pill(slint::Color::from_rgb_u8(52, 57, 78));
    ui.set_profile_accent(slint::Color::from_rgb_u8(190, 190, 190));
    ui.set_user_name("".into());
    set_login(ui, "needs-login", "Sesión cerrada. Vuelve a iniciar sesión para continuar.");
}

pub fn wire(ui: &MainWindow) {
    if let Some((artists, playlists, followers, following)) = with_app(|_, app| (app.settings.artist_model.clone(), app.settings.playlist_model.clone(), app.settings.followers_model.clone(), app.settings.following_model.clone())) {
        ui.set_profile_artists(ModelRc::from(artists));
        ui.set_profile_playlists(ModelRc::from(playlists));
        ui.set_profile_followers(ModelRc::from(followers));
        ui.set_profile_following(ModelRc::from(following));
    }
    load_prefs(ui);
    ui.on_open_settings(|| { with_app(|ui, app| open_settings(ui, app)); });
    ui.on_open_profile(|| { with_app(|ui, app| open_profile(ui, app)); });
    ui.on_pref_changed(|| {
        with_app(|ui, app| {
            // «Portadas animadas»: la pausa de movimiento es la misma que usa el botón de la portada.
            let want_paused = !ui.get_pref_motion();
            if ui.get_motion_paused() != want_paused {
                ui.invoke_toggle_motion();
            }
            save_prefs(ui);
            sync_engine_prefs(ui, app);
            update_crossfade_marker(ui, app.player.track.as_ref().map_or(0, |t| t.duration_ms as u32));
        });
    });
    ui.on_select_eq_preset(|idx| {
        with_app(|ui, app| {
            ui.set_pref_eq_preset(idx);
            if idx < 6 {
                set_eq_bands_ui(ui, eq_preset_bands(idx));
            }
            save_prefs(ui);
            sync_engine_prefs(ui, app);
        });
    });
    ui.on_select_lang(|lang| {
        with_app(|ui, _| {
            apply_i18n(ui, &lang);
            save_prefs(ui);
        });
    });
    ui.on_set_eq_band(|band, db| {
        with_app(|ui, app| {
            let idx = band as usize;
            if idx < EQ_BAND_COUNT {
                let mut bands = get_eq_bands(ui);
                bands[idx] = db.clamp(-12.0, 12.0);
                set_eq_bands_ui(ui, bands);
                ui.set_pref_eq_preset(6);
                save_prefs(ui);
                sync_engine_prefs(ui, app);
            }
        });
    });
    ui.on_toggle_follow(|| { with_app(|ui, app| toggle_follow(ui, app)); });
    ui.on_clear_cache(|| { with_app(|ui, _| clear_cache(ui)); });
    ui.on_sign_out(|| { with_app(|ui, app| sign_out(ui, app)); });
    ui.on_open_profile_people(|tab| { with_app(|ui, app| open_profile_people(ui, app, tab)); });
    ui.on_open_profile_item(|kind, i| {
        with_app(|ui, app| {
            let i = i.max(0) as usize;
            let person = match kind {
                0 => return if let Some(a) = app.settings.artists.get(i).cloned() { browse::open_source(ui, app, browse::Source::ArtistTop(a)) },
                1 => return if let Some(p) = app.settings.playlists.get(i).cloned() { browse::open_source(ui, app, browse::Source::Playlist(p)) },
                2 => app.settings.followers.get(i).cloned(),
                _ => app.settings.following.get(i).cloned(),
            };
            match person {
                Some(Person::User(id)) => open_user_profile(ui, app, id),
                Some(Person::Artist(a)) => browse::open_source(ui, app, browse::Source::ArtistTop(a)),
                None => {}
            }
        });
    });
    // Chip del creador en la cabecera de una playlist: abre su perfil.
    ui.on_open_list_owner(|| {
        with_app(|ui, app| {
            let owner = match &app.current_source {
                Some(browse::Source::Playlist(p)) => p.owner.as_ref().map(|o| o.id.clone()).filter(|id| !id.is_empty()),
                _ => None,
            };
            if let Some(id) = owner { open_user_profile(ui, app, id); }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_split_into_users_and_artists() {
        let v = json!({"profiles": [
            {"uri": "spotify:user:ana", "name": "Ana", "followers_count": 3, "image_url": "spotify:image:abc"},
            {"uri": "spotify:artist:xyz", "name": "Banda"},
            {"uri": "spotify:user:", "name": "vacío"}
        ]});
        let people = parse_people(&v);
        assert_eq!(people.len(), 2);
        assert_eq!(people[0].0, Person::User("ana".into()));
        assert_eq!(people[0].2, "3 seguidores");
        assert_eq!(people[0].3.as_deref(), Some("https://i.scdn.co/image/abc"));
        assert!(matches!(&people[1].0, Person::Artist(a) if a.id == "xyz"));
    }

    #[test]
    fn profile_cache_round_trip_preserves_payload_and_reports_fresh() {
        let dir = std::env::temp_dir().join(format!("spotifymac-test-profile-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("user-profile.json");
        let payload = json!({
            "name": "Test User",
            "followers_count": 42,
            "public_playlists": [{"name": "Mix", "uri": "spotify:playlist:1"}]
        });
        write_profile_cache(&path, &payload);
        let (loaded, fresh) = read_profile_cache(&path).expect("profile cache");
        assert_eq!(loaded, payload);
        assert!(fresh);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
