//! Cliente del proceso `spotifymac-engine` (librespot). Habla JSON, una línea por mensaje.
//! Solo usa hilos estándar: sin `tokio` en la app para no gastar memoria.

use serde_json::{json, Map, Value};
use spotify_core::engine_protocol::{parse_line, parse_reply, EngineEvent, EngineStatus};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Reply = Result<Value, String>;
type Pending = Arc<Mutex<HashMap<u64, Sender<Reply>>>>;

pub struct Engine {
    alive: Arc<AtomicBool>,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next: AtomicU64,
    child: Mutex<Child>,
    /// Lecturas en vuelo: quien pide lo mismo mientras tanto espera la misma respuesta.
    inflight: Mutex<HashMap<String, Vec<Sender<Reply>>>>,
}

/// Lecturas de catálogo sin efectos: se pueden compartir entre llamadas simultáneas.
fn is_shared_read(cmd: &str) -> bool {
    matches!(
        cmd,
        "rootlist" | "playlist_items" | "playlist_header" | "album_details" | "album_tracks" | "artist_top"
            | "artist_albums" | "artist_related" | "artist_overview" | "recently_played" | "home_sections"
            | "search" | "artists" | "track_credits" | "user_profile" | "user_followers" | "user_following" | "lyrics"
            | "liked_contains" | "liked_items" | "pins" | "canvas" | "devices" | "recommend"
    )
}

/// Carpeta de datos de la app (`%APPDATA%\SpotifyMac` en Windows, `~/Library/Application Support/SpotifyMac` en macOS).
pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library/Application Support"))
        .unwrap_or_else(std::env::temp_dir);
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("SpotifyMac");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Carpeta de caché de la app (`%LOCALAPPDATA%\SpotifyMac` en Windows, `~/Library/Caches/SpotifyMac` en macOS).
pub fn cache_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library/Caches"))
        .unwrap_or_else(std::env::temp_dir);
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("SpotifyMac");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn binary_name() -> &'static str {
    if cfg!(windows) { "spotifymac-engine.exe" } else { "spotifymac-engine" }
}

/// Busca el binario del motor: junto al ejecutable (o su destino real), directorio de trabajo,
/// rutas estándar de instalación, `PATH`, variable de entorno o `Engine/target/release` (desarrollo).
pub fn find_binary() -> Option<PathBuf> {
    let name = binary_name();
    let mut candidates = Vec::new();
    if let Some(p) = std::env::var_os("SPOTIFYMAC_ENGINE") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
            // target/release → raíz del repo → Engine/target/release
            candidates.push(dir.join("../../Engine/target/release").join(name));
        }
        if let Ok(canon) = std::fs::canonicalize(&exe) {
            if let Some(dir) = canon.parent() {
                candidates.push(dir.join(name));
                candidates.push(dir.join("../../Engine/target/release").join(name));
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(name));
    }
    candidates.push(data_dir().join(name));
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = PathBuf::from(local);
            candidates.push(local.join("Programs\\SpotifyMac").join(name));
            candidates.push(local.join("SpotifyMac").join(name));
        }
        for env_key in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(pf) = std::env::var_os(env_key) {
                candidates.push(PathBuf::from(pf).join("SpotifyMac").join(name));
            }
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            candidates.push(dir.join(name));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

impl Engine {
    /// Arranca el proceso. `on_event` se llama desde un hilo propio con cada evento del motor.
    pub fn spawn(on_event: impl Fn(EngineEvent) + Send + 'static) -> Result<Arc<Engine>, String> {
        let bin = find_binary().ok_or_else(|| {
            format!(
                "No se encontró {} junto a la app ni en la carpeta de instalación. Copia {} junto al ejecutable o ejecuta el instalador.",
                binary_name(),
                binary_name()
            )
        })?;
        let mut cmd = Command::new(&bin);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        // Los errores del motor (nunca incluyen tokens) van a engine.log.
        match File::create(data_dir().join("engine.log")) {
            Ok(f) => cmd.stderr(Stdio::from(f)),
            Err(_) => cmd.stderr(Stdio::null()),
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().map_err(|e| format!("No se pudo iniciar el motor: {e}"))?;
        let stdin = child.stdin.take().ok_or("sin stdin")?;
        let stdout = child.stdout.take().ok_or("sin stdout")?;
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));

        let (reader_pending, reader_alive) = (pending.clone(), alive.clone());
        std::thread::Builder::new()
            .name("engine-reader".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some((id, reply)) = parse_reply(&line) {
                        if let Some(tx) = reader_pending.lock().unwrap().remove(&id) {
                            let _ = tx.send(reply);
                        }
                    } else if let Some(event) = parse_line(&line) {
                        on_event(event);
                    }
                }
                // El motor terminó: se avisa a quien espera respuesta y a la app.
                reader_alive.store(false, Ordering::SeqCst);
                for (_, tx) in reader_pending.lock().unwrap().drain() {
                    let _ = tx.send(Err("El motor se cerró".into()));
                }
                on_event(EngineEvent::Status { state: EngineStatus::Disconnected, username: None });
            })
            .map_err(|e| e.to_string())?;

        Ok(Arc::new(Engine { alive, stdin: Mutex::new(stdin), pending, next: AtomicU64::new(1), child: Mutex::new(child), inflight: Mutex::default() }))
    }

    /// ¿Sigue vivo el proceso del motor?
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn write(&self, id: u64, cmd: &str, args: Value) -> std::io::Result<()> {
        let mut msg: Map<String, Value> = match args {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        msg.insert("id".into(), json!(id));
        msg.insert("cmd".into(), json!(cmd));
        let mut line = Value::Object(msg).to_string();
        line.push('\n');
        let mut stdin = self.stdin.lock().unwrap();
        stdin.write_all(line.as_bytes())?;
        stdin.flush()
    }

    /// Orden con respuesta. Bloquea el hilo que llama: usar siempre desde un hilo de trabajo.
    /// Dos lecturas idénticas a la vez (misma orden y argumentos) comparten una sola petición al motor.
    pub fn call(&self, cmd: &str, args: Value, timeout: Duration) -> Reply {
        if !is_shared_read(cmd) {
            return self.call_once(cmd, args, timeout);
        }
        let key = format!("{cmd}|{args}");
        let waiter = {
            let mut inflight = self.inflight.lock().unwrap();
            match inflight.get_mut(&key) {
                Some(waiters) => {
                    let (tx, rx) = mpsc::channel();
                    waiters.push(tx);
                    Some(rx)
                }
                None => {
                    inflight.insert(key.clone(), Vec::new());
                    None
                }
            }
        };
        if let Some(rx) = waiter {
            return rx.recv_timeout(timeout).unwrap_or_else(|_| Err("El motor no respondió".into()));
        }
        let reply = self.call_once(cmd, args, timeout);
        let waiters = self.inflight.lock().unwrap().remove(&key).unwrap_or_default();
        for tx in waiters {
            let _ = tx.send(reply.clone());
        }
        reply
    }

    fn call_once(&self, cmd: &str, args: Value, timeout: Duration) -> Reply {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(e) = self.write(id, cmd, args) {
            self.pending.lock().unwrap().remove(&id);
            return Err(format!("Motor no disponible: {e}"));
        }
        match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err("El motor no respondió".into())
            }
        }
    }

    /// Orden sin esperar respuesta (play, pausa, volumen…).
    pub fn fire(&self, cmd: &str, args: Value) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let _ = self.write(id, cmd, args);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
