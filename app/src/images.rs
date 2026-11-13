//! Descarga y decodifica portadas en hilos de trabajo, con caché en disco y en memoria acotadas.

use spotify_core::color::{self, Rgb};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Imagen ya decodificada y reducida (RGBA 8 bits) y su color dominante.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub color: Option<Rgb>,
}

type Completion = Box<dyn FnOnce(Option<Arc<Decoded>>) + Send>;
type Job = (String, String, u32);

/// Cola de descargas con pocos hilos (las portadas no deben competir con el audio ni la UI).
pub struct Loader {
    queue: Arc<(Mutex<JobQueue>, Condvar)>,
    cache: Arc<Mutex<MemoryCache>>,
}

#[derive(Default)]
struct JobQueue {
    urgent: VecDeque<Job>,
    normal: VecDeque<Job>,
    pending: HashMap<String, Vec<Completion>>,
    closed: bool,
}

impl Loader {
    pub fn new(workers: usize) -> Loader {
        let queue = Arc::new((Mutex::new(JobQueue::default()), Condvar::new()));
        let cache = Arc::new(Mutex::new(MemoryCache::new(64 * 1024 * 1024)));
        for i in 0..workers.max(1) {
            let (queue, cache) = (queue.clone(), cache.clone());
            // Un trabajador queda libre para la vista abierta incluso durante precargas largas.
            let urgent_only = workers > 1 && i == 0;
            let _ = std::thread::Builder::new().name(format!("covers-{i}")).spawn(move || loop {
                let (lock, ready) = &*queue;
                let mut jobs = lock.lock().unwrap();
                let (key, url, side) = loop {
                    if jobs.closed { return; }
                    if let Some(job) = jobs.urgent.pop_front().or_else(|| {
                        if urgent_only { None } else { jobs.normal.pop_front() }
                    }) { break job; }
                    jobs = ready.wait(jobs).unwrap();
                };
                drop(jobs);
                let hit = cache.lock().unwrap().get(&key);
                let result = hit.or_else(|| fetch(&url, side).ok().map(Arc::new));
                // Publicar la caché y retirar la solicitud bajo el mismo cerrojo evita duplicados.
                let callbacks = {
                    let mut jobs = lock.lock().unwrap();
                    if let Some(d) = &result {
                        cache.lock().unwrap().put(key.clone(), d.clone());
                    }
                    jobs.pending.remove(&key).unwrap_or_default()
                };
                for done in callbacks { done(result.clone()); }
            });
        }
        Loader { queue, cache }
    }

    /// Las imágenes en memoria se entregan inmediatamente; las demás en un hilo de trabajo.
    pub fn load(&self, url: &str, side: u32, urgent: bool, done: impl FnOnce(Option<Arc<Decoded>>) + Send + 'static) {
        let key = format!("{}@{side}", artwork_variants(url, side)[0]);
        let (lock, ready) = &*self.queue;
        let mut jobs = lock.lock().unwrap();
        let hit = self.cache.lock().unwrap().get(&key);
        if let Some(hit) = hit {
            drop(jobs);
            done(Some(hit));
            return;
        }
        if let Some(callbacks) = jobs.pending.get_mut(&key) {
            callbacks.push(Box::new(done));
            // Una precarga que ahora es visible asciende a la cola prioritaria.
            if urgent {
                if let Some(index) = jobs.normal.iter().position(|job| job.0 == key) {
                    let job = jobs.normal.remove(index).unwrap();
                    jobs.urgent.push_back(job);
                }
            }
        } else {
            jobs.pending.insert(key.clone(), vec![Box::new(done)]);
            let job = (key, url.to_owned(), side);
            if urgent { jobs.urgent.push_back(job); } else { jobs.normal.push_back(job); }
        }
        drop(jobs);
        ready.notify_all();
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        lock.lock().unwrap().closed = true;
        ready.notify_all();
    }
}

/// Recorte circular con borde suavizado, también para el renderizador por software.
pub(crate) fn circle(d: &Decoded) -> Decoded {
    let side = d.width.min(d.height);
    let (left, top) = ((d.width - side) / 2, (d.height - side) / 2);
    let radius = side as f32 / 2.0;
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let i = (((y + top) * d.width + x + left) * 4) as usize;
            let distance = ((x as f32 + 0.5 - radius).powi(2) + (y as f32 + 0.5 - radius).powi(2)).sqrt();
            let alpha = (radius - distance + 0.5).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[d.rgba[i], d.rgba[i + 1], d.rgba[i + 2], (d.rgba[i + 3] as f32 * alpha).round() as u8]);
        }
    }
    Decoded { width: side, height: side, rgba, color: d.color }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn feather_edges(rgba: &mut [u8], width: u32, height: u32, left_frac: f32, bottom_frac: f32) {
    let left_px = (width as f32 * left_frac).max(1.0);
    let bottom_start = height as f32 * (1.0 - bottom_frac);
    let bottom_px = (height as f32 * bottom_frac).max(1.0);
    for y in 0..height {
        let vy = if bottom_frac > 0.0 && (y as f32) > bottom_start {
            smoothstep(1.0 - ((y as f32) - bottom_start) / bottom_px)
        } else {
            1.0
        };
        for x in 0..width {
            let vx = if left_frac > 0.0 && (x as f32) < left_px {
                smoothstep((x as f32) / left_px)
            } else {
                1.0
            };
            let i = ((y * width + x) * 4) as usize + 3;
            rgba[i] = ((rgba[i] as f32) * vx * vy).round() as u8;
        }
    }
}

/// Desenfoca y funde la portada para cabeceras editoriales («This Is»), evitando bordes cuadrados.
pub(crate) fn editorial_blur(d: &Decoded) -> Decoded {
    let Some(src) = image::RgbaImage::from_raw(d.width, d.height, d.rgba.clone()) else {
        return Decoded { width: d.width, height: d.height, rgba: d.rgba.clone(), color: d.color };
    };
    let max_side = d.width.max(d.height).max(1);
    let small = if max_side > 180 {
        let scale = 180.0 / max_side as f32;
        let nw = ((d.width as f32 * scale).round() as u32).max(1);
        let nh = ((d.height as f32 * scale).round() as u32).max(1);
        image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Triangle)
    } else {
        src
    };
    let blurred = image::imageops::blur(&small, 11.0);
    let (w, h) = (blurred.width(), blurred.height());
    let mut rgba = blurred.into_raw();
    feather_edges(&mut rgba, w, h, 0.0, 0.34);
    Decoded { width: w, height: h, rgba, color: d.color }
}

/// Suaviza los bordes de una cabecera editorial ancha (o la desenfoca si es cuadrada).
pub(crate) fn editorial_banner(d: &Decoded) -> Decoded {
    if d.width * 5 <= d.height * 6 {
        return editorial_blur(d);
    }
    let mut rgba = d.rgba.clone();
    feather_edges(&mut rgba, d.width, d.height, 0.24, 0.30);
    Decoded { width: d.width, height: d.height, rgba, color: d.color }
}

/// Caché LRU en memoria limitada por bytes.
struct MemoryCache {
    max_bytes: usize,
    bytes: usize,
    map: HashMap<String, Arc<Decoded>>,
    order: VecDeque<String>,
}

impl MemoryCache {
    fn new(max_bytes: usize) -> Self {
        MemoryCache { max_bytes, bytes: 0, map: HashMap::new(), order: VecDeque::new() }
    }
    fn get(&mut self, key: &str) -> Option<Arc<Decoded>> {
        let v = self.map.get(key).cloned()?;
        self.order.retain(|k| k != key);
        self.order.push_back(key.to_owned());
        Some(v)
    }
    fn put(&mut self, key: String, value: Arc<Decoded>) {
        self.bytes += value.rgba.len();
        if let Some(old) = self.map.insert(key.clone(), value) {
            self.bytes -= old.rgba.len();
        }
        self.order.retain(|k| *k != key);
        self.order.push_back(key);
        while self.bytes > self.max_bytes && self.order.len() > 1 {
            if let Some(oldest) = self.order.pop_front() {
                if let Some(v) = self.map.remove(&oldest) {
                    self.bytes -= v.rgba.len();
                }
            }
        }
    }
}

fn covers_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = crate::engine::cache_dir().join("covers");
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
}

fn disk_path(url: &str) -> PathBuf {
    // FNV-1a: nombre de archivo estable sin dependencias extra.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    covers_dir().join(format!("{h:016x}.img"))
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let path = disk_path(url);
    if let Ok(bytes) = std::fs::read(&path) {
        return Ok(bytes);
    }
    // Una sola conexión reutilizada (sin repetir el saludo TLS por cada portada) y reintentos ante cortes
    // de red: antes un tiempo de espera dejaba la portada vacía para siempre.
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(6))
            .timeout(Duration::from_secs(20))
            .max_idle_connections_per_host(8)
            .build()
    });
    let mut last = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(400 * attempt));
        }
        match agent.get(url).call() {
            Ok(resp) => {
                let mut bytes = Vec::new();
                match resp.into_reader().take(8 * 1024 * 1024).read_to_end(&mut bytes) {
                    Ok(_) if !bytes.is_empty() => {
                        let _ = std::fs::write(&path, &bytes);
                        return Ok(bytes);
                    }
                    Ok(_) => last = "respuesta vacía".into(),
                    Err(e) => last = e.to_string(),
                }
            }
            // 404 y similares: no tiene sentido reintentar.
            Err(ureq::Error::Status(code, _)) => return Err(format!("HTTP {code}")),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// Tamaños conocidos del CDN de Spotify (prefijo del id → lado en píxeles), de menor a mayor.
const FAMILIES: &[&[(&str, u32)]] = &[
    // Álbumes: miniatura, tarjeta, portada y alta resolución.
    &[("ab67616d00004851", 64), ("ab67616d00001e02", 300), ("ab67616d0000b273", 640), ("ab67616d000082c1", 1200)],
    // Artistas.
    &[("ab6761610000f178", 160), ("ab67616100005174", 320), ("ab6761610000e5eb", 640)],
    // Portadas subidas por usuarios.
    &[("ab67706c0000da84", 300), ("ab67706c0000bebb", 640)],
    // Playlists editoriales de Spotify.
    &[("ab67706f00000003", 300), ("ab67706f00000002", 640)],
];

/// La versión más ligera que cubre los píxeles de pantalla, con la original como respaldo.
fn artwork_variants(url: &str, side: u32) -> Vec<String> {
    const BASE: &str = "https://i.scdn.co/image/";
    // Mosaicos de playlists: https://mosaic.scdn.co/{60|300|640}/…
    if let Some(rest) = url.strip_prefix("https://mosaic.scdn.co/") {
        if let Some((size, tail)) = rest.split_once('/') {
            if let Ok(current) = size.parse::<u32>() {
                let wanted = if side > 300 { 640 } else if side > 60 { 300 } else { 60 };
                if wanted != current {
                    return vec![format!("https://mosaic.scdn.co/{wanted}/{tail}"), url.to_owned()];
                }
            }
        }
        return vec![url.to_owned()];
    }
    let Some(id) = url.strip_prefix(BASE).filter(|id| id.len() == 40 && id.bytes().all(|c| c.is_ascii_hexdigit())) else {
        return vec![url.to_owned()];
    };
    for family in FAMILIES {
        let Some(&(_, current)) = family.iter().find(|(prefix, _)| id.starts_with(prefix)) else { continue };
        // La más pequeña que cubre el tamaño pedido (o la mayor si ninguna llega).
        let &(prefix, size) = family.iter().find(|(_, px)| *px >= side).unwrap_or(family.last().unwrap());
        if size != current {
            return vec![format!("{BASE}{prefix}{}", &id[16..]), url.to_owned()];
        }
        return vec![url.to_owned()];
    }
    vec![url.to_owned()]
}

pub(crate) fn decode(bytes: &[u8], side: u32) -> Result<Decoded, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let tiny = img.thumbnail(24, 24).to_rgba8();
    let color = color::dominant(tiny.as_raw());
    // Bilineal para miniaturas pequeñas (rápido); Lanczos para portadas grandes donde importa el detalle.
    let filter = if side <= 160 {
        image::imageops::FilterType::Triangle
    } else {
        image::imageops::FilterType::Lanczos3
    };
    let small = if img.width() > side || img.height() > side {
        img.resize(side.max(1), side.max(1), filter).to_rgba8()
    } else { img.to_rgba8() };
    Ok(Decoded { width: small.width(), height: small.height(), rgba: small.into_raw(), color })
}

fn fetch(url: &str, side: u32) -> Result<Decoded, String> {
    let mut last_error = String::new();
    for candidate in artwork_variants(url, side) {
        match download(&candidate).and_then(|bytes| decode(&bytes, side)) {
            Ok(image) => return Ok(image),
            Err(error) => {
                // Un archivo dañado en la caché no debe bloquear la portada para siempre.
                let _ = std::fs::remove_file(disk_path(&candidate));
                last_error = error;
            }
        }
    }
    log_failure(url, &last_error);
    Err(last_error)
}

fn log_failure(url: &str, error: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(crate::engine::data_dir().join("app.log")) {
        let _ = writeln!(f, "portada no cargada: {url} ({error})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc;

    #[test]
    fn circular_artist_image_crops_center_and_softens_edges() {
        let rgba = (0..12).flat_map(|_| (0..24).flat_map(|x| [x as u8, 80, 90, 255])).collect();
        let d = Decoded { width: 24, height: 12, rgba, color: Some([1, 2, 3]) };
        let round = circle(&d);
        assert_eq!((round.width, round.height), (12, 12));
        assert_eq!(round.rgba[3], 0);
        let center = ((6 * 12 + 6) * 4) as usize;
        assert_eq!(&round.rgba[center..center + 4], &[12, 80, 90, 255]);
        assert!(round.rgba.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
        assert_eq!(round.color, d.color);
    }

    #[test]
    fn cached_cover_does_not_wait_for_the_download_queue() {
        let mut cache = MemoryCache::new(1024);
        cache.put("cached@1".into(), Arc::new(Decoded { width: 1, height: 1, rgba: vec![1, 2, 3, 255], color: None }));
        let loader = Loader { queue: Arc::new((Mutex::new(JobQueue::default()), Condvar::new())), cache: Arc::new(Mutex::new(cache)) };
        let (done_tx, done_rx) = mpsc::channel();
        loader.load("cached", 1, true, move |d| { done_tx.send(d.unwrap().rgba.clone()).unwrap(); });
        assert_eq!(done_rx.try_recv().unwrap(), vec![1, 2, 3, 255]);
        let jobs = loader.queue.0.lock().unwrap();
        assert!(jobs.normal.is_empty() && jobs.urgent.is_empty());
    }

    #[test]
    fn repeated_requests_share_one_job_and_promote_visible_cover() {
        // Sin trabajadores: podemos inspeccionar las solicitudes antes de descargarlas.
        let loader = Loader {
            queue: Arc::new((Mutex::new(JobQueue::default()), Condvar::new())),
            cache: Arc::new(Mutex::new(MemoryCache::new(1024))),
        };
        let (tx, rx) = mpsc::channel();
        let large = "https://i.scdn.co/image/ab67616d0000b2730123456789abcdef01234567";
        let small = "https://i.scdn.co/image/ab67616d000048510123456789abcdef01234567";
        for (url, urgent) in [(large, false), (small, true)] {
            let tx = tx.clone();
            loader.load(url, 44, urgent, move |d| { tx.send(d.unwrap()).unwrap(); });
        }
        let mut jobs = loader.queue.0.lock().unwrap();
        assert!(jobs.normal.is_empty());
        assert_eq!(jobs.urgent.len(), 1);
        let (key, _, _) = jobs.urgent.pop_front().unwrap();
        let callbacks = jobs.pending.remove(&key).unwrap();
        assert_eq!(callbacks.len(), 2);
        drop(jobs);
        let decoded = Arc::new(Decoded { width: 1, height: 1, rgba: vec![0; 4], color: None });
        for done in callbacks { done(Some(decoded.clone())); }
        assert!(Arc::ptr_eq(&rx.try_recv().unwrap(), &rx.try_recv().unwrap()));
    }

    #[test]
    fn artwork_requests_larger_album_and_preserves_original_fallback() {
        let url = "https://i.scdn.co/image/ab67616d000048510123456789abcdef01234567";
        let choices = artwork_variants(url, 1200);
        assert!(choices[0].contains("ab67616d000082c1"));
        assert_eq!(choices.len(), 2);
        assert_eq!(choices.last().unwrap(), url);
        for other in ["https://example.com/cover.jpg", "https://i.scdn.co/image/unknown", "https://i.scdn.co/image/ab67706f000000020123456789abcdef01234567"] {
            assert_eq!(artwork_variants(other, 1200), vec![other]);
        }
    }

    #[test]
    fn artwork_uses_smallest_sufficient_variant_with_original_fallback() {
        let small_artist = "https://i.scdn.co/image/ab6761610000f1780123456789abcdef01234567";
        assert_eq!(artwork_variants(small_artist, 380)[0], "https://i.scdn.co/image/ab6761610000e5eb0123456789abcdef01234567");
        assert_eq!(artwork_variants(small_artist, 200)[0], "https://i.scdn.co/image/ab676161000051740123456789abcdef01234567");
        let big_artist = "https://i.scdn.co/image/ab6761610000e5eb0123456789abcdef01234567";
        assert!(artwork_variants(big_artist, 100)[0].contains("ab6761610000f178"));
        assert_eq!(artwork_variants(big_artist, 100).last().unwrap(), big_artist);
        let editorial = "https://i.scdn.co/image/ab67706f000000030123456789abcdef01234567";
        assert_eq!(artwork_variants(editorial, 400).len(), 2);
        assert_eq!(artwork_variants("https://mosaic.scdn.co/60/abc", 160)[0], "https://mosaic.scdn.co/300/abc");
        assert_eq!(artwork_variants("https://mosaic.scdn.co/640/abc", 160)[0], "https://mosaic.scdn.co/300/abc");
        let album = "https://i.scdn.co/image/ab67616d0000b2730123456789abcdef01234567";
        assert!(artwork_variants(album, 44)[0].contains("ab67616d00004851"));
        assert!(artwork_variants(album, 88)[0].contains("ab67616d00001e02"));
        assert!(artwork_variants(album, 360)[0].contains("ab67616d0000b273"));
    }

    #[test]
    fn decode_preserves_original_detail_and_aspect_ratio_without_upscaling() {
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(800, 400).write_to(&mut encoded, image::ImageFormat::Png).unwrap();
        let full = decode(encoded.get_ref(), 1200).unwrap();
        assert_eq!((full.width, full.height), (800, 400));
        let reduced = decode(encoded.get_ref(), 200).unwrap();
        assert_eq!((reduced.width, reduced.height), (200, 100));
    }

    #[test]
    fn editorial_blur_downscales_and_fades_bottom_edge() {
        let mut rgba = vec![0u8; 400 * 400 * 4];
        for (idx, px) in rgba.chunks_exact_mut(4).enumerate() {
            let x = idx % 400;
            px.copy_from_slice(&[if x < 200 { 240 } else { 20 }, 60, 180, 255]);
        }
        let d = Decoded { width: 400, height: 400, rgba, color: Some([20, 60, 180]) };
        let blurred = editorial_blur(&d);
        assert!(blurred.width <= 180 && blurred.height <= 180);
        assert_eq!(blurred.color, d.color);
        let top_center = ((10 * blurred.width + blurred.width / 2) * 4) as usize;
        assert!(blurred.rgba[top_center + 3] > 200);
        // Al desenfocar la frontera blanco/azul, el centro toma un tono mezclado en vez de corte duro.
        assert!(blurred.rgba[top_center] > 40 && blurred.rgba[top_center] < 210);
        let bottom_row = (((blurred.height - 1) * blurred.width + blurred.width / 2) * 4) as usize;
        assert!(blurred.rgba[bottom_row + 3] <= 5);
    }

    #[test]
    fn editorial_banner_feathers_wide_images_and_blurs_square_ones() {
        let wide = Decoded { width: 300, height: 100, rgba: vec![255; 300 * 100 * 4], color: None };
        let faded = editorial_banner(&wide);
        assert_eq!((faded.width, faded.height), (300, 100));
        assert_eq!(faded.rgba[3], 0);
        let right_mid = ((40 * 300 + 250) * 4) as usize;
        assert_eq!(faded.rgba[right_mid + 3], 255);
        let bottom_right = ((99 * 300 + 250) * 4) as usize;
        assert!(faded.rgba[bottom_right + 3] <= 5);

        let square = Decoded { width: 300, height: 300, rgba: vec![200; 300 * 300 * 4], color: None };
        let blurred_sq = editorial_banner(&square);
        assert!(blurred_sq.width <= 180);
    }
}
