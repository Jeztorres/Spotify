//! Cabeceras de Apple Music y videoclips: las mismas fuentes que la app Swift.
//! FFmpeg solo decodifica el fondo HLS, sin audio ni archivos de video en disco.
use super::*;
use slint::winit_030::WinitWindowAccessor;
use std::io::Read;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};

/// Lado del video del reproductor grande: la mejor calidad que ofrece Apple hasta 1080p.
const NOW_SIDE: u32 = 1080;
type LatestFrame = Arc<Mutex<FrameMailbox>>;
type RecyclePool = Arc<Mutex<Vec<Vec<u8>>>>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Page,
    Track,
}

impl Output {
    fn index(self) -> usize {
        match self {
            Self::Page => 0,
            Self::Track => 1,
        }
    }
}

fn return_buffer(pool: &RecyclePool, mut buf: Vec<u8>) {
    if let Ok(mut guard) = pool.lock() {
        if guard.len() < 6 {
            buf.clear();
            guard.push(buf);
        }
    }
}

fn take_buffer(pool: &RecyclePool, len: usize) -> Vec<u8> {
    if let Ok(mut guard) = pool.lock() {
        if let Some(mut buf) = guard.pop() {
            if buf.capacity() >= len {
                buf.resize(len, 0);
                return buf;
            }
        }
    }
    vec![0; len]
}

#[derive(Default)]
struct FrameMailbox {
    newest: Option<Vec<u8>>,
    scheduled: bool,
    recycle: Option<RecyclePool>,
}
impl FrameMailbox {
    /// Agrupa imágenes si la UI está ocupada: solo deja una notificación pendiente.
    fn push(&mut self, frame: Vec<u8>) -> bool {
        if let Some(dropped) = self.newest.replace(frame) {
            if let Some(pool) = &self.recycle {
                return_buffer(pool, dropped);
            }
        }
        if self.scheduled { return false; }
        self.scheduled = true;
        true
    }
    fn take(&mut self) -> Option<Vec<u8>> {
        self.scheduled = false;
        self.newest.take()
    }
}

// ---- Subida directa a textura OpenGL persistente (sin reasignar/borrar texturas ni copiar RGBA en UI) ----

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;

#[derive(Clone, Copy)]
struct GlFuncs {
    gen_textures: unsafe extern "system" fn(i32, *mut u32),
    delete_textures: unsafe extern "system" fn(i32, *const u32),
    bind_texture: unsafe extern "system" fn(u32, u32),
    tex_image_2d: unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const std::ffi::c_void),
    tex_sub_image_2d: unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const std::ffi::c_void),
    tex_parameter_i: unsafe extern "system" fn(u32, u32, i32),
    pixel_store_i: unsafe extern "system" fn(u32, i32),
}

struct PendingGlFrame {
    tex_idx: usize,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    recycle: RecyclePool,
}

#[derive(Default)]
struct GlSlot {
    textures: [Option<NonZeroU32>; 2],
    sizes: [(u32, u32); 2],
    pending: Option<PendingGlFrame>,
}

#[derive(Default)]
struct GlVideoState {
    available: bool,
    funcs: Option<GlFuncs>,
    slots: [GlSlot; 2],
}

static GL_VIDEO: OnceLock<Mutex<GlVideoState>> = OnceLock::new();

fn gl_state() -> &'static Mutex<GlVideoState> {
    GL_VIDEO.get_or_init(|| Mutex::new(GlVideoState::default()))
}

unsafe fn load_gl_proc<T>(get_proc: &dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void, name: &std::ffi::CStr) -> Option<T> {
    let ptr = get_proc(name);
    if ptr.is_null() {
        return None;
    }
    Some(std::mem::transmute_copy(&ptr))
}

pub fn on_render_state(state: slint::RenderingState, api: &slint::GraphicsAPI) {
    let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
        return;
    };
    match state {
        slint::RenderingState::RenderingSetup => {
            unsafe {
                let Some(funcs) = (|| -> Option<GlFuncs> {
                    Some(GlFuncs {
                        gen_textures: load_gl_proc(get_proc_address, c"glGenTextures")?,
                        delete_textures: load_gl_proc(get_proc_address, c"glDeleteTextures")?,
                        bind_texture: load_gl_proc(get_proc_address, c"glBindTexture")?,
                        tex_image_2d: load_gl_proc(get_proc_address, c"glTexImage2D")?,
                        tex_sub_image_2d: load_gl_proc(get_proc_address, c"glTexSubImage2D")?,
                        tex_parameter_i: load_gl_proc(get_proc_address, c"glTexParameteri")?,
                        pixel_store_i: load_gl_proc(get_proc_address, c"glPixelStorei")?,
                    })
                })() else {
                    return;
                };
                // Desactiva el bloqueo por VSync de OpenGL para no limitar ni frenar el hilo de UI;
                // DWM de Windows sigue componiendo sin tearing.
                if let Some(swap_interval) = load_gl_proc::<unsafe extern "system" fn(i32) -> i32>(get_proc_address, c"wglSwapIntervalEXT") {
                    let _ = swap_interval(0);
                }
                let mut ids = [0u32; 4];
                (funcs.gen_textures)(4, ids.as_mut_ptr());
                for id in ids {
                    if id == 0 { continue; }
                    (funcs.bind_texture)(GL_TEXTURE_2D, id);
                    (funcs.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
                    (funcs.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
                    (funcs.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
                    (funcs.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
                }
                (funcs.bind_texture)(GL_TEXTURE_2D, 0);
                if let Ok(mut st) = gl_state().lock() {
                    st.slots[0].textures = [NonZeroU32::new(ids[0]), NonZeroU32::new(ids[1])];
                    st.slots[0].sizes = [(0, 0); 2];
                    st.slots[1].textures = [NonZeroU32::new(ids[2]), NonZeroU32::new(ids[3])];
                    st.slots[1].sizes = [(0, 0); 2];
                    st.available = st.slots.iter().all(|s| s.textures[0].is_some() && s.textures[1].is_some());
                    st.funcs = Some(funcs);
                }
            }
        }
        slint::RenderingState::BeforeRendering => {
            if let Ok(mut st) = gl_state().lock() {
                if !st.available { return; }
                let Some(funcs) = &st.funcs else { return };
                let funcs_copy = GlFuncs { ..*funcs };
                for slot in &mut st.slots {
                    let Some(frame) = slot.pending.take() else { continue };
                    let Some(tex) = slot.textures[frame.tex_idx] else { continue };
                    unsafe {
                        (funcs_copy.bind_texture)(GL_TEXTURE_2D, tex.get());
                        if slot.sizes[frame.tex_idx] != (frame.width, frame.height) {
                            (funcs_copy.tex_image_2d)(
                                GL_TEXTURE_2D,
                                0,
                                GL_RGBA as i32,
                                frame.width as i32,
                                frame.height as i32,
                                0,
                                GL_RGBA,
                                GL_UNSIGNED_BYTE,
                                std::ptr::null(),
                            );
                            slot.sizes[frame.tex_idx] = (frame.width, frame.height);
                        }
                        (funcs_copy.pixel_store_i)(GL_UNPACK_ALIGNMENT, 1);
                        (funcs_copy.tex_sub_image_2d)(
                            GL_TEXTURE_2D,
                            0,
                            0,
                            0,
                            frame.width as i32,
                            frame.height as i32,
                            GL_RGBA,
                            GL_UNSIGNED_BYTE,
                            frame.pixels.as_ptr().cast(),
                        );
                        (funcs_copy.pixel_store_i)(GL_UNPACK_ALIGNMENT, 4);
                        (funcs_copy.bind_texture)(GL_TEXTURE_2D, 0);
                    }
                    return_buffer(&frame.recycle, frame.pixels);
                }
            }
        }
        slint::RenderingState::RenderingTeardown => {
            if let Ok(mut st) = gl_state().lock() {
                if let Some(funcs) = st.funcs.take() {
                    let ids: Vec<u32> = st.slots.iter_mut().flat_map(|s| {
                        s.pending = None;
                        s.sizes = [(0, 0); 2];
                        s.textures.iter_mut().filter_map(|t| t.take().map(NonZeroU32::get))
                    }).collect();
                    if !ids.is_empty() {
                        unsafe { (funcs.delete_textures)(ids.len() as i32, ids.as_ptr()); }
                    }
                }
                st.available = false;
            }
        }
        _ => {}
    }
}

fn clear_gl_pending(output: Output) {
    if let Ok(mut st) = gl_state().lock() {
        if let Some(dropped) = st.slots[output.index()].pending.take() {
            return_buffer(&dropped.recycle, dropped.pixels);
        }
    }
}

fn queue_gl_frame(
    output: Output,
    tex_idx: usize,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    recycle: &RecyclePool,
) -> Result<Image, Vec<u8>> {
    let Ok(mut st) = gl_state().lock() else {
        return Err(pixels);
    };
    if !st.available {
        return Err(pixels);
    }
    let idx = output.index();
    let Some(tex) = st.slots[idx].textures[tex_idx] else {
        return Err(pixels);
    };
    if let Some(dropped) = st.slots[idx].pending.replace(PendingGlFrame {
        tex_idx,
        width,
        height,
        pixels,
        recycle: recycle.clone(),
    }) {
        return_buffer(&dropped.recycle, dropped.pixels);
    }
    drop(st);
    let image = unsafe {
        slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(tex, [width, height].into())
    }
    .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
    .build();
    Ok(image)
}

pub struct Media {
    output: Output,
    child: Option<Child>,
    frames: Option<LatestFrame>,
    recycle: RecyclePool,
    tex_idx: usize,
    videos: Vec<Video>,
    model: Rc<VecModel<CardEntry>>,
    /// Dónde guardar el primer fotograma del video actual (se muestra al instante la próxima vez).
    poster: Option<PathBuf>,
    width: u32,
    height: u32,
    /// Clave del video actual (para no reiniciar el mismo álbum al cambiar de canción).
    key: Option<String>,
    /// Si el video de la página (perfil de artista o álbum) sigue visible dentro del scroll.
    page_visible: bool,
}
#[derive(Clone)]
struct Video {
    id: String,
    title: String,
    channel: String,
}
impl Media {
    pub fn new(_ui: &MainWindow, output: Output) -> Self {
        let model = Rc::new(VecModel::default());
        Self {
            output,
            child: None,
            frames: None,
            recycle: Arc::new(Mutex::new(Vec::new())),
            tex_idx: 0,
            videos: vec![],
            model,
            poster: None,
            width: 1280,
            height: 720,
            key: None,
            page_visible: true,
        }
    }
    fn stop(&mut self) {
        self.frames = None;
        clear_gl_pending(self.output);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // Esperar al proceso fuera de la UI evita congelar la navegación.
            std::thread::spawn(move || { let _ = child.wait(); });
        }
    }
}
impl Drop for Media {
    fn drop(&mut self) {
        self.stop();
    }
}
pub fn reset(ui: &MainWindow, app: &mut App) {
    app.media.stop();
    app.media.page_visible = true;
    app.media.videos.clear();
    app.media.model.set_vec(vec![]);
    ui.set_motion_frame(Image::default());
    ui.set_motion_ready(false);
    ui.set_artist_logo(Image::default());
    ui.set_has_artist_logo(false);
}
fn normalized(s: &str) -> String {
    spotify_core::text::words(s).join("")
}
// Apple y Spotify suelen añadir sufijos de edición distintos al mismo álbum.
fn album_base(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut end = name.len();
    for suffix in [" - single", " - ep", " (deluxe", " [deluxe", " - deluxe", " (expanded", " [expanded", " - expanded", " (remastered", " [remastered", " - remastered"] {
        if let Some(i) = lower.find(suffix) { end = end.min(i); }
    }
    normalized(&name[..end])
}
fn http_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(6))
            .timeout_read(Duration::from_secs(12))
            .max_idle_connections(16)
            .max_idle_connections_per_host(8)
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/130.0.0.0 Safari/537.36")
            .build()
    })
}
fn http(url: &str) -> ureq::Request {
    http_agent().get(url).timeout(Duration::from_secs(12))
}
fn json_request(url: &str, params: &[(&str, &str)]) -> Option<Value> {
    let mut request = http(url);
    for (k, v) in params {
        request = request.query(k, v);
    }
    serde_json::from_reader(request.call().ok()?.into_reader()).ok()
}
fn cache_file(key: &str) -> PathBuf {
    let hash = key.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    engine::data_dir().join(format!("motion-{hash:016x}.json"))
}
fn cached_lookup_ttl(
    key: &str,
    positive_ttl: u64,
    null_ttl: u64,
    fetch: impl FnOnce() -> Option<Value>,
) -> Option<Value> {
    let path = cache_file(key);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            let age = now().saturating_sub(v["at"].as_u64().unwrap_or(0));
            let ttl = if v["data"].is_null() { null_ttl } else { positive_ttl };
            if age < ttl {
                return Some(v["data"].clone());
            }
        }
    }
    let data = fetch()?; // Los fallos de red nunca se guardan como «sin animación».
    if let Ok(bytes) = serde_json::to_vec(&json!({"at":now(),"data":data})) {
        let _ = std::fs::write(path, bytes);
    }
    Some(data)
}
fn cached_lookup(key: &str, fetch: impl FnOnce() -> Option<Value>) -> Option<Value> {
    cached_lookup_ttl(key, 3 * 86400, 86400, fetch)
}
fn poster_file(key: &str) -> PathBuf {
    let path = cache_file(key);
    path.with_file_name(format!("{}-poster.jpg", path.file_stem().and_then(|s| s.to_str()).unwrap_or("motion")))
}
fn save_poster(path: PathBuf, frame: Vec<u8>, width: u32, height: u32) {
    std::thread::spawn(move || {
        let rgb: Vec<u8> = frame.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        let Ok(file) = std::fs::File::create(&path) else { return };
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 85);
        if encoder.encode(&rgb, width, height, image::ExtendedColorType::Rgb8).is_err() {
            let _ = std::fs::remove_file(&path);
        }
    });
}
fn mostly_dark(frame: &[u8]) -> bool {
    let (mut sum, mut n) = (0u64, 0u64);
    for p in frame.chunks_exact(4).step_by(211) {
        sum += (p[0] as u64 * 3 + p[1] as u64 * 6 + p[2] as u64) / 10;
        n += 1;
    }
    n == 0 || sum / n < 28
}
fn load_poster(path: &std::path::Path) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let img = image::load_from_memory(&std::fs::read(path).ok()?).ok()?.to_rgba8();
    Some(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(img.as_raw(), img.width(), img.height()))
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn serialized(html: &str) -> Option<Value> {
    let marker = "id=\"serialized-server-data\"";
    let rest = html.split_once(marker)?.1.split_once('>')?.1;
    serde_json::from_str(rest.split_once("</script>")?.0).ok()
}
fn collect_motion(value: &Value, found: &mut serde_json::Map<String, Value>) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                if key.starts_with("motion") {
                    if let Some(url) = v
                        .get("video")
                        .and_then(Value::as_str)
                        .filter(|u| valid_stream(u))
                    {
                        found.entry(key.clone()).or_insert(json!(url));
                    }
                    collect_motion(v, found);
                } else if key == "artistLogo" {
                    if let Some(template) = v.pointer("/dictionary/url").and_then(Value::as_str) {
                        let w = v
                            .pointer("/dictionary/width")
                            .and_then(Value::as_f64)
                            .unwrap_or(1200.0)
                            .max(1.0);
                        let h = v
                            .pointer("/dictionary/height")
                            .and_then(Value::as_f64)
                            .unwrap_or(600.0);
                        let width = w.min(1200.0);
                        let height = (h * width / w).round().max(1.0);
                        let url = template
                            .replace("{w}", &format!("{width:.0}"))
                            .replace("{h}", &format!("{height:.0}"))
                            .replace(
                                "{c}",
                                v.get("cropStyle").and_then(Value::as_str).unwrap_or("bb"),
                            )
                            .replace("{f}", "png");
                        if url.starts_with("https://") {
                            found.entry("logo".to_owned()).or_insert(json!(url));
                        }
                    }
                } else {
                    collect_motion(v, found);
                }
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_motion(v, found);
            }
        }
        _ => {}
    }
}
fn page_artwork(html: &str) -> Option<Value> {
    let data = serialized(html)?;
    let mut all = serde_json::Map::new();
    collect_motion(&data, &mut all);
    let logo = all.get("logo").cloned();
    // No tomar el fondo de un artista o álbum recomendado al pie de la página.
    if let Some(sections) = data
        .pointer("/data/0/data/sections")
        .and_then(Value::as_array)
    {
        for section in sections {
            let items = section
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| vec![section.clone()]);
            for item in items {
                let mut out = serde_json::Map::new();
                collect_motion(&item, &mut out);
                if out.keys().any(|k| k.starts_with("motion")) {
                    if let Some(logo) = logo { out.insert("logo".into(), logo); }
                    return Some(Value::Object(out));
                }
            }
        }
        return Some(logo.map(|logo|json!({"logo":logo})).unwrap_or(Value::Null));
    }
    let mut out = serde_json::Map::new();
    collect_motion(&data, &mut out);
    Some(Value::Object(out))
}
/// Id de Apple de un artista (nombre exacto). `Some(Null)` si Apple no lo tiene.
fn apple_artist_id(artist: &str) -> Option<Value> {
    cached_lookup(&format!("artist-id|{}", normalized(artist)), || {
        let search = json_request(
            "https://itunes.apple.com/search",
            &[("term", artist), ("entity", "musicArtist"), ("country", "us"), ("limit", "10")],
        )?;
        let id = search.get("results")?.as_array()?.iter()
            .find(|v| v.get("artistName").and_then(Value::as_str).is_some_and(|n| normalized(n) == normalized(artist)))
            .and_then(|v| v.get("artistId")).cloned();
        Some(id.unwrap_or(Value::Null))
    })
}
/// Id del álbum dentro de la discografía completa del artista (una consulta cubre todos sus álbumes).
/// La búsqueda general devuelve sencillos y colaboraciones antes que el álbum real.
fn album_id_from_discography(artist: &str, album: &str) -> Option<Value> {
    let artist_id = apple_artist_id(artist)?.as_u64()?;
    let discography = cached_lookup(&format!("discography|{artist_id}"), || {
        let r = json_request(
            "https://itunes.apple.com/lookup",
            &[("id", &artist_id.to_string()), ("entity", "album"), ("country", "us"), ("limit", "200")],
        )?;
        Some(r.get("results").cloned().unwrap_or(Value::Null))
    })?;
    let albums: Vec<&Value> = discography.as_array()?.iter().filter(|v| v.get("collectionId").is_some()).collect();
    let name = |v: &&Value| v.get("collectionName").and_then(Value::as_str).unwrap_or("").to_owned();
    albums.iter().find(|v| normalized(&name(v)) == normalized(album))
        .or_else(|| albums.iter().find(|v| album_base(&name(v)) == album_base(album)))
        .and_then(|v| v.get("collectionId")).cloned()
}
fn lookup(artist: &str, album: Option<&str>) -> Option<Value> {
    let from_discography = match album {
        Some(album) => album_id_from_discography(artist, album),
        None => Some(apple_artist_id(artist)?),
    };
    if let Some(id) = from_discography.as_ref().and_then(Value::as_u64) {
        let kind = if album.is_some() { "album" } else { "artist" };
        let html = http(&format!("https://music.apple.com/us/{kind}/{id}")).call().ok()?.into_string().ok()?;
        return page_artwork(&html);
    }
    from_discography?; // fallo de red: no se guarda como «sin animación»
    let Some(album) = album else { return Some(Value::Null) };
    // Respaldo: búsqueda general por artista + título.
    let search = json_request(
        "https://itunes.apple.com/search",
        &[("term", &format!("{artist} {album}")), ("entity", "album"), ("country", "us"), ("limit", "25")],
    )?;
    let results = search.get("results")?.as_array()?;
    let matches_artist = |v: &&Value| {
        v.get("artistName").and_then(Value::as_str).is_some_and(|name| normalized(name) == normalized(artist))
    };
    let title = |v: &&Value| v.get("collectionName").and_then(Value::as_str).unwrap_or("").to_owned();
    let candidate = results.iter().filter(matches_artist).find(|v| normalized(&title(v)) == normalized(album))
        .or_else(|| results.iter().filter(matches_artist).find(|v| album_base(&title(v)) == album_base(album)));
    let Some(id) = candidate.and_then(|v| v.get("collectionId")).and_then(Value::as_u64) else {
        return Some(Value::Null);
    };
    let html = http(&format!("https://music.apple.com/us/album/{id}")).call().ok()?.into_string().ok()?;
    page_artwork(&html)
}
fn valid_stream(url: &str) -> bool {
    // Los fondos proceden del catálogo de Apple; no se aceptan rutas locales ni otros protocolos.
    url.starts_with("https://")
        && url
            .split('?')
            .next()
            .is_some_and(|u| u.ends_with(".m3u8") || u.ends_with(".mp4"))
}
fn decoder() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let base = exe.parent()?;
    let bin_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let mut candidates = vec![
        base.join("media").join(bin_name),
        base.join(bin_name),
        crate::engine::data_dir().join("media").join(bin_name),
    ];
    #[cfg(windows)]
    candidates.push(base.join(
        "../../tools/media/runtime/ffmpeg-master-latest-win64-lgpl-shared/bin/ffmpeg.exe",
    ));
    #[cfg(not(windows))]
    candidates.extend([
        base.join("../Resources/media/ffmpeg"),
        base.join("../Resources/ffmpeg"),
        PathBuf::from("/opt/homebrew/bin/ffmpeg"),
        PathBuf::from("/usr/local/bin/ffmpeg"),
        PathBuf::from("/usr/bin/ffmpeg"),
    ]);
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(bin_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(windows)]
fn ensure_windows_ffmpeg() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let dest = crate::engine::data_dir().join("media/ffmpeg.exe");
        if dest.is_file() {
            return;
        }
        let Some(parent) = dest.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let url = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
        let Ok(resp) = ureq::get(url).timeout(Duration::from_secs(240)).call() else {
            log("fondos: no se pudo descargar FFmpeg para Windows");
            return;
        };
        let zip_tmp = dest.with_extension("zip.part");
        let Ok(mut zip_out) = std::fs::File::create(&zip_tmp) else {
            return;
        };
        if std::io::copy(&mut resp.into_reader().take(256 * 1024 * 1024), &mut zip_out).is_err() {
            let _ = std::fs::remove_file(&zip_tmp);
            return;
        }
        drop(zip_out);
        let exe_tmp = dest.with_extension("exe.part");
        let extracted = (|| -> Option<()> {
            let file = std::fs::File::open(&zip_tmp).ok()?;
            let mut archive = zip::ZipArchive::new(file).ok()?;
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i).ok()?;
                if entry.name().ends_with("/bin/ffmpeg.exe") || entry.name() == "ffmpeg.exe" {
                    let mut out = std::fs::File::create(&exe_tmp).ok()?;
                    std::io::copy(&mut entry, &mut out).ok()?;
                    return Some(());
                }
            }
            None
        })();
        let _ = std::fs::remove_file(&zip_tmp);
        if extracted.is_none() || std::fs::rename(&exe_tmp, &dest).is_err() {
            let _ = std::fs::remove_file(&exe_tmp);
            log("fondos: no se pudo extraer FFmpeg para Windows");
            return;
        }
        log("fondos: FFmpeg para Windows instalado");
        on_ui(|ui, app| {
            if !ui.get_now_playing_open() && matches!(ui.get_page(), 2 | 3) && app.media.page_visible {
                restore(ui, app);
            }
        });
    });
}

#[cfg(target_os = "macos")]
fn ensure_macos_ffmpeg() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};

    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let url = if cfg!(target_arch = "aarch64") {
            "https://github.com/eugeneware/ffmpeg-static/releases/latest/download/ffmpeg-darwin-arm64"
        } else {
            "https://github.com/eugeneware/ffmpeg-static/releases/latest/download/ffmpeg-darwin-x64"
        };
        let dest = crate::engine::data_dir().join("media/ffmpeg");
        if dest.is_file() {
            return;
        }
        let Some(parent) = dest.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(resp) = ureq::get(url).timeout(Duration::from_secs(180)).call() else {
            log("fondos: no se pudo descargar FFmpeg para macOS");
            return;
        };
        let tmp = dest.with_extension("part");
        let Ok(mut out) = std::fs::File::create(&tmp) else {
            return;
        };
        if std::io::copy(&mut resp.into_reader().take(128 * 1024 * 1024), &mut out).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        drop(out);
        if let Ok(meta) = std::fs::metadata(&tmp) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&tmp, perms);
        }
        if std::fs::rename(&tmp, &dest).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        log("fondos: FFmpeg para macOS instalado");
        on_ui(|ui, app| {
            if !ui.get_now_playing_open() && matches!(ui.get_page(), 2 | 3) {
                restore(ui, app);
            }
        });
    });
}
#[cfg(windows)]
struct HighResTimer {
    handle: windows_sys::Win32::Foundation::HANDLE,
}
#[cfg(windows)]
impl HighResTimer {
    fn new() -> Option<Self> {
        use windows_sys::Win32::System::Threading::{
            CreateWaitableTimerExW, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS,
        };
        let handle = unsafe {
            CreateWaitableTimerExW(
                std::ptr::null(),
                std::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        (!handle.is_null()).then_some(Self { handle })
    }
    fn sleep_for(&self, dur: Duration) -> bool {
        use windows_sys::Win32::System::Threading::{SetWaitableTimer, WaitForSingleObject, INFINITE};
        // Unidades negativas de 100 ns indican espera relativa con precisión sub-milisegundo.
        let ticks_100ns = (dur.as_nanos() / 100).min(i64::MAX as u128) as i64;
        if ticks_100ns <= 0 {
            return true;
        }
        let due_time = -ticks_100ns;
        unsafe {
            if SetWaitableTimer(self.handle, &due_time, 0, None, std::ptr::null(), 0) == 0 {
                return false;
            }
            WaitForSingleObject(self.handle, INFINITE);
        }
        true
    }
}
#[cfg(windows)]
impl Drop for HighResTimer {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}
#[cfg(not(windows))]
struct HighResTimer;
#[cfg(not(windows))]
impl HighResTimer {
    fn new() -> Option<Self> {
        None
    }
}
fn sleep_until(timer: Option<&HighResTimer>, deadline: Instant) {
    let now = Instant::now();
    if now >= deadline {
        return;
    }
    let rem = deadline - now;
    #[cfg(windows)]
    if let Some(t) = timer {
        if t.sleep_for(rem) {
            return;
        }
    }
    #[cfg(not(windows))]
    let _ = timer;
    std::thread::sleep(rem);
}
fn deliver_frame(
    output: Output,
    slot_weak: &std::sync::Weak<Mutex<FrameMailbox>>,
    recycle: &RecyclePool,
    frame: Vec<u8>,
) -> bool {
    let Some(slot) = slot_weak.upgrade() else {
        return_buffer(recycle, frame);
        return false;
    };
    let schedule = {
        let Ok(mut guard) = slot.lock() else {
            return false;
        };
        guard.push(frame)
    };
    if schedule {
        let notify_slot = slot;
        if slint::invoke_from_event_loop(move || {
            with_app(|ui, app| present_frame(ui, app, output, &notify_slot));
        })
        .is_err()
        {
            return false;
        }
    }
    true
}
fn cached_video_path(url: &str) -> PathBuf {
    crate::engine::cache_dir()
        .join("motion-videos")
        .join(format!("{:016x}.mp4", fnv1a_hash(url.as_bytes())))
}

fn fnv1a_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn cache_video_in_background(binary: PathBuf, url: String, dest: PathBuf) {
    static INFLIGHT: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let set = INFLIGHT.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    if let Ok(mut guard) = set.lock() {
        if !guard.insert(url.clone()) {
            return;
        }
    }
    std::thread::spawn(move || {
        let _cleanup = scopeguard_remove(set, url.clone());
        if dest.is_file() {
            return;
        }
        if let Some(parent) = dest.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let tmp = dest.with_extension("mp4.part");
        let _ = std::fs::remove_file(&tmp);
        let mut cmd = Command::new(&binary);
        cmd.args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-rw_timeout",
            "15000000",
            "-i",
            &url,
            "-an",
            "-sn",
            "-c",
            "copy",
            "-movflags",
            "+faststart",
        ])
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        if cmd.status().is_ok_and(|s| s.success()) && tmp.is_file() {
            let _ = std::fs::rename(&tmp, &dest);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    });
}

struct InflightGuard<'a>(&'a Mutex<std::collections::HashSet<String>>, String);
impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut g) = self.0.lock() {
            g.remove(&self.1);
        }
    }
}
fn scopeguard_remove(
    set: &'static Mutex<std::collections::HashSet<String>>,
    url: String,
) -> InflightGuard<'static> {
    InflightGuard(set, url)
}

fn restart_active_video(output: Output) {
    on_ui(move |ui, app| {
        if ui.get_motion_paused() {
            return;
        }
        match output {
            Output::Page => {
                if matches!(ui.get_page(), 2 | 3)
                    && !ui.get_now_playing_open()
                    && app.media.page_visible
                {
                    start_page_source(app);
                }
            }
            Output::Track => {
                if ui.get_now_playing_open() {
                    if let Some(track) = app.player.track.clone() {
                        app.media_now.key = None;
                        load_now(ui, app, &track);
                    }
                }
            }
        }
    });
}

fn start(
    ui: &MainWindow,
    app: &mut App,
    output: Output,
    variant: VariantInfo,
    poster: PathBuf,
    target_w: u32,
    target_h: u32,
) {
    let Some(binary) = decoder() else {
        log("fondos: falta el decodificador FFmpeg");
        #[cfg(target_os = "macos")]
        ensure_macos_ffmpeg();
        #[cfg(windows)]
        ensure_windows_ffmpeg();
        return;
    };
    if !valid_stream(&variant.url)
        || ui.get_motion_paused()
        || (output == Output::Page && !app.media.page_visible)
    {
        return;
    }
    let local_cache = cached_video_path(&variant.url);
    let use_local = local_cache.is_file();
    if !use_local {
        cache_video_in_background(binary.clone(), variant.url.clone(), local_cache.clone());
    }
    let input_source = if use_local {
        local_cache.to_string_lossy().into_owned()
    } else {
        variant.url.clone()
    };
    let battery_saver = ui.get_pref_battery_saver();
    let (target_w, target_h) = if battery_saver {
        if target_w == target_h {
            (target_w.min(640), target_h.min(640))
        } else {
            (target_w.min(960), target_h.min(540))
        }
    } else {
        (target_w, target_h)
    };
    let use_ffmpeg_re = variant.fps.is_none_or(|f| f <= 1.0);
    let (width, height, filter) = if variant.width > 0 && variant.height > 0 {
        if variant.width <= target_w && variant.height <= target_h {
            // Slint recorta y escala en GPU (image-fit: cover + GL_LINEAR): ahorra CPU por fotograma.
            (variant.width, variant.height, None)
        } else {
            (
                target_w,
                target_h,
                Some(format!(
                    "scale={target_w}:{target_h}:force_original_aspect_ratio=increase:flags=bilinear,crop={target_w}:{target_h}"
                )),
            )
        }
    } else {
        (
            target_w,
            target_h,
            Some(format!(
                "scale={target_w}:{target_h}:force_original_aspect_ratio=increase:flags=bilinear,crop={target_w}:{target_h}"
            )),
        )
    };
    let mut args = vec![
        "-nostdin".to_owned(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
    ];
    if !use_local {
        args.extend([
            "-reconnect".into(),
            "1".into(),
            "-reconnect_streamed".into(),
            "1".into(),
            "-reconnect_delay_max".into(),
            "2".into(),
            "-rw_timeout".into(),
            "15000000".into(),
            // Arranque rápido con análisis ligero pero suficiente para HLS.
            "-probesize".into(),
            "131072".into(),
            "-analyzeduration".into(),
            "250000".into(),
        ]);
    }
    args.extend(["-threads".into(), "2".into()]);
    #[cfg(target_os = "macos")]
    args.extend(["-hwaccel".into(), "videotoolbox".into()]);
    if use_ffmpeg_re {
        args.push("-re".into());
    }
    args.extend([
        "-stream_loop".into(),
        "-1".into(),
        "-i".into(),
        input_source,
        "-an".into(),
        "-sn".into(),
    ]);
    let mut vf_parts = Vec::new();
    if let Some(f) = filter {
        vf_parts.push(f);
    }
    if battery_saver && variant.fps.is_some_and(|f| f > 15.0) {
        vf_parts.push("fps=15".into());
    }
    if !vf_parts.is_empty() {
        args.push("-vf".into());
        args.push(vf_parts.join(","));
    }
    args.extend([
        "-pix_fmt".into(),
        "rgba".into(),
        "-fps_mode".into(),
        "passthrough".into(),
        "-f".into(),
        "rawvideo".into(),
        "pipe:1".into(),
    ]);
    let mut command = Command::new(binary);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let Ok(mut child) = command.spawn() else {
        return;
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        return;
    };
    let media = match output {
        Output::Page => &mut app.media,
        Output::Track => &mut app.media_now,
    };
    media.stop();
    media.child = Some(child);
    media.poster = (!poster.exists()).then_some(poster);
    media.width = width;
    media.height = height;
    let recycle = media.recycle.clone();
    // Entrega directa con temporización precisa y reutilización de buffers, sin límites artificiales de FPS.
    let latest: LatestFrame = Arc::new(Mutex::new(FrameMailbox {
        recycle: Some(recycle.clone()),
        ..Default::default()
    }));
    media.frames = Some(latest.clone());
    let frame_bytes = width as usize * height as usize * 4;
    let mut paced_fps = variant.fps.filter(|f| *f > 1.0 && f.is_finite());
    if battery_saver {
        paced_fps = paced_fps.map(|f| f.min(15.0));
    }
    if use_ffmpeg_re && paced_fps.is_none() {
        paced_fps = None;
    }
    if let Some(fps) = paced_fps {
        // Buffer adelantado de 4 fotogramas absorbe microtirones de red/segmentos mientras el hilo
        // temporizador mantiene la cadencia real del video sin sufrir el límite de 15.6 ms de FFmpeg en Windows.
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(4);
        let reader_recycle = recycle.clone();
        let reader_weak = Arc::downgrade(&latest);
        std::thread::spawn(move || loop {
            if reader_weak.strong_count() == 0 {
                break;
            }
            let mut frame = take_buffer(&reader_recycle, frame_bytes);
            if stdout.read_exact(&mut frame).is_err() {
                return_buffer(&reader_recycle, frame);
                if reader_weak.strong_count() > 0 {
                    restart_active_video(output);
                }
                break;
            }
            if tx.send(frame).is_err() {
                break;
            }
        });
        let pacer_weak = Arc::downgrade(&latest);
        std::thread::spawn(move || {
            let timer = HighResTimer::new();
            let interval = Duration::from_secs_f64(1.0 / fps);
            let mut next_deadline: Option<Instant> = None;
            while let Ok(frame) = rx.recv() {
                if pacer_weak.strong_count() == 0 {
                    return_buffer(&recycle, frame);
                    break;
                }
                if let Some(deadline) = next_deadline {
                    sleep_until(timer.as_ref(), deadline);
                    let now = Instant::now();
                    let next = deadline + interval;
                    next_deadline = Some(if now > next + interval * 2 {
                        now + interval
                    } else {
                        next
                    });
                } else {
                    next_deadline = Some(Instant::now() + interval);
                }
                if !deliver_frame(output, &pacer_weak, &recycle, frame) {
                    break;
                }
            }
        });
    } else {
        let slot_weak = Arc::downgrade(&latest);
        std::thread::spawn(move || loop {
            if slot_weak.strong_count() == 0 {
                break;
            }
            let mut frame = take_buffer(&recycle, frame_bytes);
            if stdout.read_exact(&mut frame).is_err() {
                return_buffer(&recycle, frame);
                if slot_weak.strong_count() > 0 {
                    restart_active_video(output);
                }
                break;
            }
            if !deliver_frame(output, &slot_weak, &recycle, frame) {
                break;
            }
        });
    }
}
fn present_frame(ui: &MainWindow, app: &mut App, output: Output, slot: &LatestFrame) {
    let media = match output {
        Output::Page => &mut app.media,
        Output::Track => &mut app.media_now,
    };
    // Una notificación del video anterior nunca puede pintar sobre el artista nuevo.
    if !media.frames.as_ref().is_some_and(|active| Arc::ptr_eq(active, slot)) {
        return;
    }
    let visible = match output {
        Output::Page => {
            matches!(ui.get_page(), 2 | 3) && !ui.get_now_playing_open() && media.page_visible
        }
        Output::Track => ui.get_now_playing_open(),
    };
    if !visible || ui.get_motion_paused() {
        media.stop();
        // Sin clave, al volver a abrir la pantalla completa se reinicia el video.
        media.key = None;
        return;
    }
    if ui.get_pref_battery_saver() {
        let minimized = ui
            .window()
            .with_winit_window(|w| w.is_minimized().unwrap_or(false))
            .unwrap_or(false);
        if minimized {
            let _ = slot.lock().unwrap().take();
            return;
        }
    }
    let Some(frame) = slot.lock().unwrap().take() else { return };
    let (width, height) = (media.width, media.height);
    if media.poster.is_some() && !mostly_dark(&frame) {
        if let Some(path) = media.poster.take() {
            save_poster(path, frame.clone(), width, height);
        }
    }
    let next_idx = 1 - media.tex_idx;
    let image = match queue_gl_frame(output, next_idx, width, height, frame, &media.recycle) {
        Ok(img) => {
            media.tex_idx = next_idx;
            img
        }
        Err(frame) => {
            let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&frame, width, height);
            return_buffer(&media.recycle, frame);
            Image::from_rgba8(buffer)
        }
    };
    match output {
        Output::Page => {
            ui.set_motion_frame(image);
            ui.set_motion_ready(true);
        }
        Output::Track => {
            ui.set_track_motion_frame(image);
            ui.set_track_motion_ready(true);
        }
    }
    // Redibujado inmediato en winit para evitar la cuantización del TimerBasedFrameThrottle de Slint en Windows.
    if cfg!(windows) || !ui.get_pref_battery_saver() {
        ui.window().with_winit_window(|w| w.request_redraw());
    }
}
/// Atributos `CLAVE=valor` de una línea `#EXT-X-STREAM-INF` (los valores entre comillas pueden llevar comas).
fn hls_attributes(line: &str) -> Vec<(String, String)> {
    let (mut out, mut key, mut value, mut in_key, mut quoted) = (vec![], String::new(), String::new(), true, false);
    for c in line.chars().chain(std::iter::once(',')) {
        match c {
            '=' if in_key => in_key = false,
            '"' if !in_key => quoted = !quoted,
            ',' if !quoted => {
                if !key.is_empty() { out.push((key.trim().to_owned(), value.clone())); }
                key.clear(); value.clear(); in_key = true;
            }
            _ if in_key => key.push(c),
            _ => value.push(c),
        }
    }
    out
}
/// Variante para un fondo de 1280×720: lado corto ≤ 720 y, si se puede, H.264
/// (decodifica mucho más rápido que HEVC). Sin esto FFmpeg abre la de mayor resolución (a veces 4K) y tarda en arrancar.
#[derive(Clone)]
struct VariantInfo {
    url: String,
    width: u32,
    height: u32,
    fps: Option<f64>,
}
fn pick_variant_info(master: &str, text: &str, max_short: u32) -> Option<VariantInfo> {
    struct V {
        url: String,
        w: u32,
        h: u32,
        short: u32,
        fps_milli: u32,
        fps: Option<f64>,
        bw: u64,
        avc: bool,
    }
    let mut found = vec![];
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") else { continue };
        let attrs = hls_attributes(rest);
        let get = |k: &str| attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str()).unwrap_or("");
        let (w, h) = get("RESOLUTION").split_once('x').map(|(w, h)| (w.parse().unwrap_or(0), h.parse().unwrap_or(0))).unwrap_or((0u32, 0u32));
        let bw = get("BANDWIDTH").parse().unwrap_or(0u64);
        let fps_f = get("FRAME-RATE").parse::<f64>().ok().filter(|f| f.is_finite() && *f > 0.0);
        let fps_milli = fps_f.map(|f| (f * 1000.0) as u32).unwrap_or(0);
        let avc = get("CODECS").contains("avc1");
        let Some(uri) = lines.by_ref().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')) else { break };
        let url = if uri.starts_with("https://") { uri.to_owned() } else {
            let base = master.split('?').next()?.rsplit_once('/')?.0;
            format!("{base}/{}", uri.trim_start_matches("./"))
        };
        if w > 0 && valid_stream(&url) {
            found.push(V { url, w, h, short: w.min(h), fps_milli, fps: fps_f, bw, avc });
        }
    }
    // Prioridad 1: elegir el nivel de FPS máximo real (p. ej. 60/120 fps sobre 30 fps) aunque sea HEVC o >720p.
    let max_fps = found.iter().map(|v| v.fps_milli).max().unwrap_or(0);
    let top_fps: Vec<&V> = found.iter().filter(|v| v.fps_milli + 500 >= max_fps).collect();
    let any_avc = top_fps.iter().any(|v| v.avc);
    let pool: Vec<&V> = top_fps.into_iter().filter(|v| v.avc || !any_avc).collect();
    let fitting: Vec<&V> = pool.iter().copied().filter(|v| v.short <= max_short).collect();
    let choice = if fitting.is_empty() { pool.iter().min_by_key(|v| (v.short, v.bw)) } else { fitting.iter().max_by_key(|v| (v.short, v.fps_milli, v.bw)) };
    choice.map(|v| VariantInfo {
        url: v.url.clone(),
        width: v.w,
        height: v.h,
        fps: v.fps,
    })
}
#[cfg_attr(not(test), allow(dead_code))]
fn pick_variant(master: &str, text: &str, max_short: u32) -> Option<String> {
    pick_variant_info(master, text, max_short).map(|v| v.url)
}
fn resolve_variant(url: &str, max_short: u32) -> VariantInfo {
    if !url.split('?').next().is_some_and(|u| u.ends_with(".m3u8")) {
        return VariantInfo { url: url.to_owned(), width: 0, height: 0, fps: None };
    }
    let key = format!("variant-v4|{max_short}|{url}");
    let cached = cached_lookup(&key, || {
        let text = http(url).timeout(Duration::from_secs(6)).call().ok()?.into_string().ok()?;
        let Some(v) = pick_variant_info(url, &text, max_short) else {
            return Some(Value::Null);
        };
        Some(json!({ "url": v.url, "width": v.width, "height": v.height, "fps": v.fps }))
    });
    if let Some(obj) = cached.as_ref().and_then(Value::as_object) {
        if let Some(resolved_url) = obj.get("url").and_then(Value::as_str) {
            return VariantInfo {
                url: resolved_url.to_owned(),
                width: obj.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
                height: obj.get("height").and_then(Value::as_u64).unwrap_or(0) as u32,
                fps: obj.get("fps").and_then(Value::as_f64).filter(|f| f.is_finite() && *f > 0.0),
            };
        }
    }
    VariantInfo { url: url.to_owned(), width: 0, height: 0, fps: None }
}
pub fn load(app: &mut App, artist: String, album: Option<String>) {
    let gen = app.list_gen;
    // Un álbum es cuadrado (hasta 1080); un artista, 16:9. Así la portada animada no se recorta dos veces.
    let max_short = if album.is_some() { 1080 } else { 720 };
    let (width, height) = if album.is_some() { (1080, 1080) } else { (1280, 720) };
    std::thread::spawn(move || {
        let key = format!("v3|{artist}|{}", album.as_deref().unwrap_or(""));
        // Fotograma guardado de la visita anterior: la cabecera aparece animada al instante
        // mientras FFmpeg descarga y decodifica el video real.
        let poster = poster_file(&key);
        if let Some(buffer) = load_poster(&poster) {
            on_ui(move |ui, app| {
                if app.list_gen == gen
                    && matches!(ui.get_page(), 2 | 3)
                    && !ui.get_now_playing_open()
                    && !ui.get_motion_ready()
                {
                    ui.set_motion_frame(Image::from_rgba8(buffer));
                    ui.set_motion_ready(true);
                }
            });
        }
        let record = cached_lookup(&key, || lookup(&artist, album.as_deref()));
        let stream = record.as_ref().and_then(|record| {
            [
                "motionArtistWide16x9",
                "motionArtistFullscreen16x9",
                "motionArtistSquare1x1",
                "motionDetailSquare",
                "motionDetailTall",
            ]
            .iter()
            .find_map(|k| record.get(k).and_then(Value::as_str))
            .map(|u| resolve_variant(u, max_short))
        });
        on_ui(move |ui, app| {
            if app.list_gen != gen || !matches!(ui.get_page(), 2 | 3) || ui.get_now_playing_open() {
                return;
            }
            let Some(record) = record else {
                return;
            };
            if let Some(url) = record.get("logo").and_then(Value::as_str) {
                load_cover(app, url, 640, move |ui, app, d| {
                    if app.list_gen == gen {
                        ui.set_artist_logo(to_image(d));
                        ui.set_has_artist_logo(true);
                    }
                });
            }
            if let Some(variant) = stream {
                start(ui, app, Output::Page, variant, poster, width, height);
            }
        });
    });
}

/// Portada animada del álbum que suena, para la pantalla completa.
pub fn load_now(ui: &MainWindow, app: &mut App, track: &Track) {
    let Some(album) = track.album.as_ref().filter(|a| !a.name.trim().is_empty()) else {
        stop_now(ui, app);
        return;
    };
    let Some(artist) = track.artists.first().filter(|a| !a.name.trim().is_empty()) else {
        stop_now(ui, app);
        return;
    };
    let key = format!("v3|{}|{}", artist.name, album.name);
    if app.media_now.key.as_deref() == Some(key.as_str()) {
        return;
    }
    app.media_now.stop();
    app.media_now.key = Some(key.clone());
    ui.set_track_motion_frame(Image::default());
    ui.set_track_motion_ready(false);
    let artist = artist.name.clone();
    let album = album.name.clone();
    std::thread::spawn(move || {
        let poster = poster_file(&key);
        if let Some(buffer) = load_poster(&poster) {
            let expected = key.clone();
            on_ui(move |ui, app| {
                if app.media_now.key.as_deref() == Some(expected.as_str()) && ui.get_now_playing_open() {
                    ui.set_track_motion_frame(Image::from_rgba8(buffer));
                    ui.set_track_motion_ready(true);
                }
            });
        }
        let record = cached_lookup(&key, || lookup(&artist, Some(&album)));
        let stream = record.as_ref().and_then(|record| {
            ["motionDetailSquare", "motionArtistSquare1x1", "motionDetailTall"]
                .iter()
                .find_map(|k| record.get(*k).and_then(Value::as_str))
                .map(|u| resolve_variant(u, NOW_SIDE))
        });
        on_ui(move |ui, app| {
            if app.media_now.key.as_deref() != Some(key.as_str()) || !ui.get_now_playing_open() {
                return;
            }
            if let Some(variant) = stream {
                start(ui, app, Output::Track, variant, poster, NOW_SIDE, NOW_SIDE);
            }
        });
    });
}

pub fn stop_now(ui: &MainWindow, app: &mut App) {
    app.media_now.stop();
    app.media_now.key = None;
    ui.set_track_motion_frame(Image::default());
    ui.set_track_motion_ready(false);
}

fn start_page_source(app: &mut App) {
    match app.current_source.clone() {
        Some(browse::Source::ArtistTop(a)) => load(app, a.name, None),
        Some(browse::Source::Album(a)) => load(
            app,
            spotify_core::catalog::primary_artist(&a.artists.unwrap_or_default()),
            Some(a.name),
        ),
        _ => {}
    }
}

/// Al cerrar la pantalla completa, vuelve el movimiento de la página que quedó debajo.
pub fn restore(ui: &MainWindow, app: &mut App) {
    app.media.stop();
    if !app.media.page_visible {
        return;
    }
    ui.set_motion_frame(Image::default());
    ui.set_motion_ready(false);
    start_page_source(app);
}
fn valid_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn text_runs(v: &Value) -> String {
    v.get("simpleText")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            v.get("runs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|r| r.get("text").and_then(Value::as_str))
                .collect()
        })
}
fn parse_videos(html: &str) -> Vec<Video> {
    let Some(start) = ["var ytInitialData = ", "window[\"ytInitialData\"] = "]
        .iter()
        .find_map(|m| html.split_once(m).map(|(_, tail)| tail))
    else {
        return vec![];
    };
    // Deserialización incremental: no depende del espacio entre JSON y </script>.
    let Some(Ok(value)) = serde_json::Deserializer::from_str(start)
        .into_iter::<Value>()
        .next()
    else {
        return vec![];
    };
    fn walk(v: &Value, out: &mut Vec<Video>) {
        if let Some(r) = v.get("videoRenderer") {
            if let Some(id) = r
                .get("videoId")
                .and_then(Value::as_str)
                .filter(|s| valid_id(s))
            {
                if !out.iter().any(|v| v.id == id) {
                    out.push(Video {
                        id: id.into(),
                        title: text_runs(&r["title"]),
                        channel: text_runs(&r["ownerText"]),
                    });
                }
            }
        } else {
            match v {
                Value::Object(m) => {
                    for v in m.values() {
                        walk(v, out)
                    }
                }
                Value::Array(a) => {
                    for v in a {
                        walk(v, out)
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = vec![];
    walk(&value, &mut out);
    out
}
fn relevant(v: &Video, artist: &str) -> bool {
    let title = normalized(&v.title);
    let name = normalized(artist);
    let channel = normalized(&v.channel);
    !name.is_empty()
        && (title.contains(&name) || channel.contains(&name))
        && ![
            "reaction",
            "karaoke",
            "cover",
            "lyrics",
            "letra",
            "slowed",
            "spedup",
            "nightcore",
            "teaser",
            "interview",
            "shorts",
            "dancepractice",
        ]
        .iter()
        .any(|s| title.contains(s))
}
pub fn videos(_app: &mut App, _artist: String) {}
pub fn wire(ui: &MainWindow) {
    ui.on_toggle_motion(|| {
        with_app(|ui, app| {
            let paused = !ui.get_motion_paused();
            ui.set_motion_paused(paused);
            if paused {
                app.media.stop();
                stop_now(ui, app);
            } else {
                if app.media.page_visible
                    && matches!(ui.get_page(), 2 | 3)
                    && !ui.get_now_playing_open()
                {
                    start_page_source(app);
                }
                if ui.get_now_playing_open() {
                    if let Some(track) = app.player.track.clone() {
                        load_now(ui, app, &track);
                    }
                }
            }
        });
    });
    ui.on_sync_page_motion(|visible| {
        with_app(|ui, app| {
            if app.media.page_visible == visible {
                return;
            }
            app.media.page_visible = visible;
            if !visible {
                app.media.stop();
            } else if !ui.get_motion_paused()
                && matches!(ui.get_page(), 2 | 3)
                && !ui.get_now_playing_open()
                && app.media.child.is_none()
            {
                start_page_source(app);
            }
        });
    });
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_delivery_coalesces_backlog_and_schedules_again_after_presenting() {
        let mut mailbox = FrameMailbox::default();
        assert!(mailbox.push(vec![1]));
        for frame in 2..=120 { assert!(!mailbox.push(vec![frame])); }
        assert_eq!(mailbox.take(), Some(vec![120]));
        assert!(mailbox.push(vec![121]));
        assert_eq!(mailbox.take(), Some(vec![121]));
        assert_eq!(mailbox.take(), None);
    }

    #[test]
    fn equivalent_hls_resolution_prefers_native_sixty_fps() {
        let playlist = "#EXTM3U\n#EXT-X-STREAM-INF:RESOLUTION=1280x720,BANDWIDTH=3000000,FRAME-RATE=30,CODECS=\"avc1.64001f\"\n30.m3u8\n#EXT-X-STREAM-INF:RESOLUTION=1280x720,BANDWIDTH=2500000,FRAME-RATE=59.94,CODECS=\"avc1.64001f\"\n60.m3u8\n";
        assert_eq!(pick_variant("https://a.test/master.m3u8", playlist, 720).as_deref(), Some("https://a.test/60.m3u8"));
    }
    #[test]
    fn variant_prefers_h264_up_to_720p_and_resolves_relative_urls() {
        let master = "https://a.test/x/master.m3u8?t=1";
        let text = "#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=9000000,RESOLUTION=3840x2160,CODECS=\"hvc1.2,mp4a\"
hi.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a\"
mid.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=640x360,CODECS=\"avc1.4d401e\"
low.m3u8
";
        assert_eq!(pick_variant(master, text, 720).as_deref(), Some("https://a.test/x/mid.m3u8"));
        assert_eq!(pick_variant(master, "#EXTM3U
#EXTINF:6,
seg.ts
", 720), None);
    }

    #[test]
    fn header_does_not_borrow_recommended_album_motion() {
        let html = r#"<script id="serialized-server-data">{"data":[{"data":{"sections":[{"items":[{"motionArtistWide16x9":{"video":"https://a.test/main.m3u8"}}]},{"items":[{"motionDetailSquare":{"video":"https://a.test/other.m3u8"}}]}]}}]}</script>"#;
        let v = page_artwork(html).unwrap();
        assert_eq!(v["motionArtistWide16x9"], "https://a.test/main.m3u8");
        assert!(v.get("motionDetailSquare").is_none());
    }
    #[test]
    fn logo_preserves_ratio_and_requests_png() {
        let mut out = serde_json::Map::new();
        collect_motion(
            &json!({"artistLogo":{"dictionary":{"url":"https://a.test/{w}x{h}{c}.{f}","width":2400,"height":600}}}),
            &mut out,
        );
        assert_eq!(out["logo"], "https://a.test/1200x300bb.png");
    }
    #[test]
    fn high_fps_variant_wins_over_codec_and_resolution_cap() {
        let playlist = "#EXTM3U\n#EXT-X-STREAM-INF:RESOLUTION=1280x720,BANDWIDTH=3000000,FRAME-RATE=30,CODECS=\"avc1.64001f\"\n30_720_avc.m3u8\n#EXT-X-STREAM-INF:RESOLUTION=1920x1080,BANDWIDTH=4500000,FRAME-RATE=60,CODECS=\"hvc1.2.4\"\n60_1080_hevc.m3u8\n";
        let picked = pick_variant_info("https://a.test/master.m3u8", playlist, 720).unwrap();
        assert_eq!(picked.url, "https://a.test/60_1080_hevc.m3u8");
        assert_eq!((picked.width, picked.height), (1920, 1080));
        assert!((picked.fps.unwrap() - 60.0).abs() < 0.01);
    }

    #[test]
    fn buffer_pool_recycles_dropped_and_returned_frames() {
        let pool: RecyclePool = Arc::new(Mutex::new(Vec::new()));
        let mut mailbox = FrameMailbox {
            recycle: Some(pool.clone()),
            ..Default::default()
        };
        assert!(mailbox.push(vec![1; 16]));
        // Al sobrescribir el fotograma pendiente, su buffer regresa al pool de reutilización.
        assert!(!mailbox.push(vec![2; 16]));
        assert_eq!(pool.lock().unwrap().len(), 1);
        let reused = take_buffer(&pool, 16);
        assert_eq!(reused.len(), 16);
        assert!(pool.lock().unwrap().is_empty());
        return_buffer(&pool, reused);
        assert_eq!(pool.lock().unwrap().len(), 1);
    }

    #[test]
    fn video_ids_cannot_inject_urls_and_lyrics_are_not_clips() {
        assert!(!valid_id("../bad?id=x"));
        assert!(valid_id("Abc_123-xYz"));
        assert!(!relevant(
            &Video {
                id: "".into(),
                title: "Queen lyrics".into(),
                channel: "Queen".into()
            },
            "Queen"
        ));
        assert!(!valid_stream("file:///a.mp4"));
    }
}
