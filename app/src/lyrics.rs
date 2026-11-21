//! Letras sincronizadas: primero AMLL TTML DB (letras de la comunidad con el cantante de cada
//! línea, para mostrar los duetos a izquierda y derecha como Apple Music), luego el servicio de
//! Spotify (color-lyrics, vía motor) y, si no hay, LRCLIB (https://lrclib.net), abierta y sin clave.
use super::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub time: Option<i64>,
    pub text: String,
    /// Dueto: la canta la otra voz (se muestra a la derecha).
    pub right: bool,
    /// Coros de fondo de la línea (entre paréntesis), si los hay.
    pub bg: String,
}

#[derive(Clone)]
struct Found {
    lines: Vec<Line>,
    synced: bool,
    provider: String,
}

pub struct State {
    pub model: Rc<VecModel<LyricEntry>>,
    pub lines: Vec<Line>,
    pub key: String,
    pub synced: bool,
    pub pending: bool,
    pub attempted: bool,
    /// Artista y título de la letra cargada (identidad para la caché de traducciones).
    artist: String,
    title: String,
    /// Clave de la traducción que se está pidiendo ahora (vacía si ninguna).
    translating: String,
}

impl State {
    pub fn new() -> Self {
        Self {
            model: Rc::new(VecModel::default()),
            lines: Vec::new(),
            key: String::new(),
            synced: false,
            pending: false,
            attempted: false,
            artist: String::new(),
            title: String::new(),
            translating: String::new(),
        }
    }
}

/// Pide la letra de `track` una sola vez por canción y la publica en la interfaz.
pub fn request(ui: &MainWindow, app: &mut App, track: &Track) {
    let key = track.identity().to_owned();
    if app.lyrics.key == key && (app.lyrics.pending || app.lyrics.attempted) {
        return;
    }
    app.lyrics.key = key.clone();
    app.lyrics.lines.clear();
    app.lyrics.synced = false;
    app.lyrics.pending = true;
    app.lyrics.attempted = false;
    app.lyrics.artist = track.artists.first().map(|a| a.name.clone()).unwrap_or_default();
    app.lyrics.title = track.name.clone();
    app.lyrics.model.set_vec(Vec::new());
    ui.set_lyrics_has_translation(false);
    ui.set_lyrics_same_language(false);
    ui.set_lyrics_synced(false);
    ui.set_lyric_active(-1);
    ui.set_lyrics_provider("".into());
    ui.set_lyrics_message("Buscando letra…".into());

    // Ya pedida por adelantado (era la siguiente de la cola): se muestra al instante.
    if let Some(found) = cached(&key) {
        return publish(ui, app, found);
    }
    let engine = app.engine.clone();
    let track = track.clone();
    std::thread::spawn(move || {
        let found = fetch(engine.as_deref(), &track);
        remember(&key, &found);
        on_ui(move |ui, app| {
            if app.lyrics.key != key {
                return;
            }
            publish(ui, app, found);
        });
    });
}

/// Letras ya buscadas (también «no hay letra»), por identidad de canción. Pocas: solo la actual y las próximas.
fn cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, Option<Found>>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Option<Found>>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn cached(key: &str) -> Option<Option<Found>> {
    cache().lock().ok()?.get(key).cloned()
}

fn remember(key: &str, found: &Option<Found>) {
    if let Ok(mut c) = cache().lock() {
        if c.len() >= 40 { c.clear(); }
        c.insert(key.to_owned(), found.clone());
    }
}

/// Busca en segundo plano la letra (y, si el conmutador «ES» está activo, la traducción) de la canción
/// que viene después: al cambiar de canción aparece sin esperar a la red.
pub fn prefetch(ui: &MainWindow, app: &App, track: &Track) {
    let key = track.identity().to_owned();
    if key == app.lyrics.key || cached(&key).is_some() {
        return;
    }
    // Una sola búsqueda anticipada por canción.
    static PREFETCHING: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    let busy = PREFETCHING.get_or_init(Default::default);
    if !busy.lock().map(|mut b| b.insert(key.clone())).unwrap_or(false) {
        return;
    }
    let translate_too = ui.get_show_translation();
    let engine = app.engine.clone();
    let track = track.clone();
    std::thread::spawn(move || {
        let found = fetch(engine.as_deref(), &track);
        remember(&key, &found);
        if let Ok(mut b) = busy.lock() { b.remove(&key); }
        if let (true, Some(found)) = (translate_too, &found) {
            let texts: Vec<String> = found.lines.iter().map(|l| l.text.clone()).collect();
            let artist = track.artists.first().map(|a| a.name.clone()).unwrap_or_default();
            if let Err(e) = translate::translate(&translate::song_key(&artist, &track.name, &texts), &texts) {
                log(&format!("traducción anticipada: {e}"));
            }
        }
    });
}

fn publish(ui: &MainWindow, app: &mut App, found: Option<Found>) {
            app.lyrics.pending = false;
            app.lyrics.attempted = true;
            match found {
                Some(found) => {
                    app.lyrics.synced = found.synced;
                    app.lyrics.lines = found.lines.clone();
                    app.lyrics
                        .model
                        .set_vec(found.lines.iter().map(|l| LyricEntry { text: l.text.clone().into(), translation: "".into(), right: l.right, bg: l.bg.clone().into() }).collect::<Vec<_>>());
                    ui.set_lyrics_synced(found.synced);
                    ui.set_lyrics_provider(found.provider.into());
                    ui.set_lyrics_message("".into());
                    update_active(ui, app);
                    maybe_translate(ui, app);
                }
                None => {
                    app.lyrics.lines.clear();
                    app.lyrics.model.set_vec(Vec::new());
                    ui.set_lyrics_synced(false);
                    ui.set_lyrics_provider("".into());
                    ui.set_lyric_active(-1);
                    ui.set_lyrics_message("No hay letra disponible para esta canción.".into());
                }
            }
}

/// Índice de la línea que suena ahora (`-1` si la letra no está sincronizada).
pub fn update_active(ui: &MainWindow, app: &App) {
    if !app.lyrics.synced || app.lyrics.lines.is_empty() {
        ui.set_lyric_active(-1);
        return;
    }
    let position = app.player.position();
    let mut index = -1;
    for (i, line) in app.lyrics.lines.iter().enumerate() {
        match line.time {
            Some(t) if t <= position + 380 => index = i as i32,
            Some(_) => break,
            None => {}
        }
    }
    ui.set_lyric_active(index);
}

/// Pide (o recupera de la caché) la traducción de la letra cargada si el conmutador «ES» está activo.
pub fn maybe_translate(ui: &MainWindow, app: &mut App) {
    if !ui.get_show_translation() || app.lyrics.lines.is_empty() {
        return;
    }
    let texts: Vec<String> = app.lyrics.lines.iter().map(|l| l.text.clone()).collect();
    let key = translate::song_key(&app.lyrics.artist, &app.lyrics.title, &texts);
    if let Some(t) = translate::cached(&key) {
        return apply_translation(ui, app, &t);
    }
    if app.lyrics.translating == key {
        return;
    }
    app.lyrics.translating = key.clone();
    ui.set_lyrics_translating(true);
    let lyrics_key = app.lyrics.key.clone();
    std::thread::spawn(move || {
        let result = translate::translate(&key, &texts);
        on_ui(move |ui, app| {
            if app.lyrics.translating == key {
                app.lyrics.translating.clear();
                ui.set_lyrics_translating(false);
            }
            if app.lyrics.key != lyrics_key {
                return;
            }
            match result {
                Ok(t) => apply_translation(ui, app, &t),
                Err(e) => log(&format!("traducción de letra: {e}")),
            }
        });
    });
}

fn apply_translation(ui: &MainWindow, app: &mut App, t: &translate::Translation) {
    let same = t.is_target_language();
    let mut any = false;
    for (i, line) in app.lyrics.lines.iter().enumerate() {
        let text = if same {
            String::new()
        } else {
            t.lines.get(&i).filter(|s| !s.trim().eq_ignore_ascii_case(line.text.trim())).cloned().unwrap_or_default()
        };
        any |= !text.is_empty();
        if let Some(mut row) = app.lyrics.model.row_data(i) {
            if row.translation.as_str() != text {
                row.translation = text.into();
                app.lyrics.model.set_row_data(i, row);
            }
        }
    }
    ui.set_lyrics_has_translation(any);
    ui.set_lyrics_same_language(same);
}

/// Botón «ES»: alterna el estado global de traducción (se recuerda entre sesiones).
pub fn toggle_translation(ui: &MainWindow, app: &mut App) {
    ui.set_show_translation(!ui.get_show_translation());
    settings::save(ui);
    maybe_translate(ui, app);
}

pub fn toggle(ui: &MainWindow, app: &mut App) {
    let open = !ui.get_lyrics_open();
    ui.set_lyrics_open(open);
    if open || lyrics_visible(ui) {
        if let Some(track) = app.player.track.clone() {
            request(ui, app, &track);
        }
    }
}

pub fn seek(ui: &MainWindow, app: &mut App, index: i32) {
    let Some(line) = app.lyrics.lines.get(index.max(0) as usize) else {
        return;
    };
    let Some(ms) = line.time else {
        return;
    };
    ctl(app, Ctl::Seek(ms));
    app.player.set_position(ms);
    render_progress(ui, app);
}

fn fetch(engine: Option<&Engine>, track: &Track) -> Option<Found> {
    if let Some(id) = track.id.as_deref().filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())) {
        if let Some(found) = amll(id) {
            return Some(found);
        }
    }
    if let (Some(engine), Some(id)) = (engine, track.id.as_deref()) {
        if !id.is_empty() {
            if let Some(found) = spotify(engine, id) {
                return Some(found);
            }
        }
    }
    lrclib(track)
}

fn spotify(engine: &Engine, id: &str) -> Option<Found> {
    let reply = engine
        .call("lyrics", json!({ "target": id }), Duration::from_secs(12))
        .ok()?;
    let lyrics = reply.get("lyrics")?;
    let raw = lyrics.get("lines")?.as_array()?;
    if raw.is_empty() {
        return None;
    }
    let synced = lyrics.get("syncType").and_then(Value::as_str).unwrap_or("") != "UNSYNCED";
    let lines = raw
        .iter()
        .map(|line| {
            let text = line.get("words").and_then(Value::as_str).unwrap_or("").trim();
            Line {
                time: if synced {
                    line.get("startTimeMs").and_then(milliseconds)
                } else {
                    None
                },
                text: if text.is_empty() { "♪".to_owned() } else { text.to_owned() },
                right: false,
                bg: String::new(),
            }
        })
        .collect();
    let provider = lyrics
        .get("providerDisplayName")
        .and_then(Value::as_str)
        .or_else(|| lyrics.get("provider").and_then(Value::as_str))
        .unwrap_or("Spotify");
    Some(Found {
        lines,
        synced,
        provider: format!("{provider} · Spotify"),
    })
}

fn milliseconds(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

fn lrclib(track: &Track) -> Option<Found> {
    let artist = track.artists.first()?.name.as_str();
    let title = spotify_core::search_ranking::clean_title(&track.name);
    let seconds = track.duration_ms.max(0) as f64 / 1000.0;
    let duration = (track.duration_ms / 1000).to_string();
    let mut request = ureq::get("https://lrclib.net/api/get")
        .timeout(Duration::from_secs(12))
        .set("User-Agent", "SpotifyMac-Windows/0.1 (lyrics)")
        .query("artist_name", artist)
        .query("track_name", &title)
        .query("duration", &duration);
    if let Some(album) = &track.album {
        request = request.query("album_name", &album.name);
    }
    let mut item = read_json(request);
    let empty = item.as_ref().is_none_or(|v| {
        v.get("syncedLyrics").is_none_or(Value::is_null) && v.get("plainLyrics").is_none_or(Value::is_null)
    });
    if empty {
        let search = ureq::get("https://lrclib.net/api/search")
            .timeout(Duration::from_secs(12))
            .set("User-Agent", "SpotifyMac-Windows/0.1 (lyrics)")
            .query("artist_name", artist)
            .query("track_name", &title);
        let list = read_json(search).and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let close = |v: &Value| (v.get("duration").and_then(Value::as_f64).unwrap_or(0.0) - seconds).abs() <= 5.0;
        item = list
            .iter()
            .find(|v| close(v) && v.get("syncedLyrics").and_then(Value::as_str).is_some_and(|s| !s.is_empty()))
            .or_else(|| list.iter().find(|v| close(v)))
            .cloned();
    }
    let item = item?;
    if item.get("instrumental").and_then(Value::as_bool) == Some(true) {
        return Some(Found {
            lines: vec![Line { time: None, text: "♪ Instrumental ♪".into(), right: false, bg: String::new() }],
            synced: false,
            provider: "LRCLIB".into(),
        });
    }
    if let Some(lrc) = item.get("syncedLyrics").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let lines = parse_lrc(lrc);
        if !lines.is_empty() {
            return Some(Found { lines, synced: true, provider: "LRCLIB".into() });
        }
    }
    if let Some(plain) = item.get("plainLyrics").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let lines = plain
            .lines()
            .map(|line| Line { time: None, text: line.to_owned(), right: false, bg: String::new() })
            .collect::<Vec<_>>();
        if !lines.is_empty() {
            return Some(Found { lines, synced: false, provider: "LRCLIB".into() });
        }
    }
    None
}

fn read_json(request: ureq::Request) -> Option<Value> {
    serde_json::from_reader(request.call().ok()?.into_reader()).ok()
}

/// «[01:23.45] texto» → líneas con tiempo. Ignora etiquetas sin tiempo ([ar:], [ti:]…).
pub fn parse_lrc(lrc: &str) -> Vec<Line> {
    let mut out: Vec<(i64, String)> = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim_start();
        let mut times = Vec::new();
        while let Some(tail) = rest.strip_prefix('[') {
            let Some(close) = tail.find(']') else { break };
            let tag = &tail[..close];
            if let Some((minutes, seconds)) = tag.split_once(':') {
                if let (Ok(minutes), Ok(seconds)) = (minutes.parse::<i64>(), seconds.parse::<f64>()) {
                    times.push(minutes * 60_000 + (seconds * 1000.0).round() as i64);
                }
            }
            rest = &tail[close + 1..];
        }
        let text = rest.trim();
        for time in times {
            out.push((time, if text.is_empty() { "♪".to_owned() } else { text.to_owned() }));
        }
    }
    out.sort_by_key(|(time, _)| *time);
    out.into_iter().map(|(time, text)| Line { time: Some(time), text, right: false, bg: String::new() }).collect()
}

pub fn wire(ui: &MainWindow) {
    ui.on_toggle_lyrics(|| {
        with_app(|ui, app| toggle(ui, app));
    });
    ui.on_seek_lyric(|index| {
        with_app(|ui, app| seek(ui, app, index));
    });
    ui.on_toggle_translation(|| {
        with_app(|ui, app| toggle_translation(ui, app));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc_with_offsets_and_empty_lines() {
        let lines = parse_lrc("[00:12.50]Hola\n[00:15.00]mundo\n[00:18.00]");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].time, Some(12_500));
        assert_eq!(lines[0].text, "Hola");
        assert_eq!(lines[1].text, "mundo");
        assert_eq!(lines[2].text, "♪");
    }

    #[test]
    fn ignores_metadata_tags() {
        let lines = parse_lrc("[ar:The Weeknd]\n[ti:Blinding Lights]\n[00:01.00]No");
        assert_eq!(lines, vec![Line { time: Some(1_000), text: "No".into(), right: false, bg: String::new() }]);
    }
}

// ---------- AMLL TTML DB (duetos) ----------

/// Letra de AMLL TTML DB por ID de canción de Spotify (404 si la comunidad aún no la tiene).
fn amll(id: &str) -> Option<Found> {
    let url = format!("https://raw.githubusercontent.com/amll-dev/amll-ttml-db/refs/heads/main/spotify-lyrics/{id}.ttml");
    let body = ureq::get(&url)
        .timeout(Duration::from_secs(8))
        .set("User-Agent", "SpotifyMac/0.1 (lyrics)")
        .call()
        .ok()?
        .into_string()
        .ok()?;
    parse_ttml(&body)
}

const TTM: &str = "http://www.w3.org/ns/ttml#metadata";

/// `hh:mm:ss.mmm`, `mm:ss.mmm`, `ss.mmm` o `12.5s` → milisegundos.
fn ttml_time(v: &str) -> Option<i64> {
    let v = v.trim();
    if let Some(sec) = v.strip_suffix('s') {
        return sec.parse::<f64>().ok().map(|s| (s * 1000.0).round() as i64);
    }
    let mut total = 0.0f64;
    for part in v.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some((total * 1000.0).round() as i64)
}

/// Texto de un nodo sin los fragmentos con papel especial (coros, traducción, romanización).
fn ttml_text(node: roxmltree::Node) -> String {
    let mut out = String::new();
    for child in node.children() {
        if child.is_text() {
            out.push_str(child.text().unwrap_or(""));
        } else if child.is_element() && child.attribute((TTM, "role")).is_none() {
            out.push_str(&ttml_text(child));
        }
    }
    out
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_ttml(body: &str) -> Option<Found> {
    let doc = roxmltree::Document::parse(body).ok()?;
    let mut lines = Vec::new();
    let mut agents: Vec<String> = Vec::new();
    for p in doc.descendants().filter(|n| n.has_tag_name("p")) {
        let text = squash(&ttml_text(p));
        let bg = p.children()
            .filter(|c| c.is_element() && c.attribute((TTM, "role")) == Some("x-bg"))
            .map(|c| squash(&ttml_text(c)))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() && bg.is_empty() {
            continue;
        }
        let agent = p.attribute((TTM, "agent")).unwrap_or("v1").to_owned();
        if !agents.contains(&agent) {
            agents.push(agent.clone());
        }
        let bg = if bg.is_empty() || bg.starts_with('(') { bg } else { format!("({})", bg.trim_matches(|c| c == '(' || c == ')')) };
        lines.push((p.attribute("begin").and_then(ttml_time), text, agent, bg));
    }
    if lines.is_empty() {
        return None;
    }
    // La primera voz va a la izquierda; las demás (dueto), a la derecha. Un grupo cantando
    // a la vez (v1000…) se queda con la primera voz.
    // `v1`, `v2`… son voces; `v1000` (grupo) y `v2000` (otros) no cuentan como dueto.
    let solo = |a: &str| a.strip_prefix('v').and_then(|n| n.parse::<u32>().ok()).is_none_or(|n| n < 1000);
    let first = agents.iter().find(|a| solo(a)).cloned().unwrap_or_else(|| agents[0].clone());
    let duet = agents.iter().filter(|a| solo(a)).count() > 1;
    let synced = lines.iter().all(|l| l.0.is_some());
    let lines = lines.into_iter().map(|(time, text, agent, bg)| Line {
        time: if synced { time } else { None },
        text: if text.is_empty() { "♪".to_owned() } else { text },
        right: duet && agent != first && solo(&agent),
        bg,
    }).collect();
    Some(Found { lines, synced, provider: "AMLL TTML DB (comunidad)".into() })
}

#[cfg(test)]
mod ttml_tests {
    use super::*;

    const SAMPLE: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" xmlns:itunes="http://music.apple.com/lyric-ttml-internal">
      <head><metadata><ttm:agent type="person" xml:id="v1"/><ttm:agent type="person" xml:id="v2"/></metadata></head>
      <body><div>
        <p begin="00:01.000" end="00:03.000" ttm:agent="v1"><span begin="00:01.000" end="00:01.500">Hello</span> <span begin="00:01.500" end="00:02.000">there</span><span ttm:role="x-bg"><span begin="00:02.000" end="00:02.500">ooh</span></span><span ttm:role="x-translation">Hola</span></p>
        <p begin="1:04.250" end="1:06.000" ttm:agent="v2">I am the other voice</p>
        <p begin="70.5s" end="72s" ttm:agent="v1000">Together</p>
      </div></body></tt>"#;

    #[test]
    fn duet_lines_go_left_and_right() {
        let f = parse_ttml(SAMPLE).expect("parsea");
        assert!(f.synced);
        assert_eq!(f.lines.len(), 3);
        assert_eq!(f.lines[0].text, "Hello there");
        assert_eq!(f.lines[0].bg, "(ooh)");
        assert_eq!(f.lines[0].time, Some(1000));
        assert!(!f.lines[0].right);
        assert_eq!(f.lines[1].time, Some(64250));
        assert!(f.lines[1].right);
        assert_eq!(f.lines[2].time, Some(70500));
        assert!(!f.lines[2].right);
    }

    #[test]
    fn single_voice_stays_left() {
        let one = SAMPLE.replace("ttm:agent=\"v2\"", "ttm:agent=\"v1\"");
        assert!(parse_ttml(&one).unwrap().lines.iter().all(|l| !l.right));
    }
}

#[cfg(test)]
mod ttml_real_tests {
    /// Archivos reales de AMLL TTML DB (opcional: `AMLL_SAMPLES=carpeta cargo test`).
    #[test]
    fn parses_real_files() {
        let Ok(dir) = std::env::var("AMLL_SAMPLES") else { return };
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let body = std::fs::read_to_string(e.path()).unwrap();
            let f = super::parse_ttml(&body).expect("parsea");
            let right = f.lines.iter().filter(|l| l.right).count();
            println!("{:?}: {} líneas, sincronizada={}, a la derecha={}, con coros={}", e.file_name(), f.lines.len(), f.synced, right, f.lines.iter().filter(|l| !l.bg.is_empty()).count());
            for l in f.lines.iter().take(6) { println!("   {:>7?} {} {}  {}", l.time, if l.right { "→" } else { "←" }, l.text, l.bg); }
        }
    }
}
