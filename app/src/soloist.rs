//! Spotify Soloist: reproductor oficial de Spotify (proceso local + WebSocket en 127.0.0.1).
//! Se usa cuando Spotify niega las claves de audio a librespot (librespot-org/librespot#1649).
//! Soloist se descarga del servidor oficial de Spotify, no se redistribuye, y cada usuario usa su propia clave.

use crate::engine::data_dir;
use serde_json::{json, Value};
use spotify_core::soloist_protocol::{self as proto, Live, Update};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tungstenite::{stream::MaybeTlsStream, Message};

pub const KEY_PAGE: &str = "https://developer.spotify.com/dashboard/soloist";
pub const DEVICE_NAME: &str = "SpotifyMac (Soloist)";
pub const KEY_SECRET: &str = "soloistKey";
const DOWNLOAD_URL: &str = "https://soloist-builds.spotifycdn.com/soloist_release_windows_x86_64.zip";
/// Spotify caduca cada versión a los 90 días: se baja una nueva bastante antes.
const REFRESH_AFTER: Duration = Duration::from_secs(60 * 86400);
const EXPIRED_EXIT_CODE: i32 = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Off,
    Downloading,
    Starting,
    /// Esperando a que se elija «SpotifyMac (Soloist)» en la lista de dispositivos de Spotify.
    NeedsPairing,
    Ready,
    Failed(String),
}

pub enum Event {
    Status(Status),
    Update(Update, Box<Live>),
    DaysLeft(i32),
    DeviceId(String),
}

pub struct Soloist {
    stop: Arc<AtomicBool>,
    tx: Sender<String>,
    child: Arc<Mutex<Option<Child>>>,
}

fn root() -> PathBuf {
    data_dir().join("soloist")
}
fn binary() -> PathBuf {
    let name = if cfg!(windows) { "soloist.exe" } else { "soloist" };
    root().join("builds").join(name)
}
fn data() -> PathBuf {
    root().join("data")
}
fn log_path() -> PathBuf {
    root().join("soloist.log")
}

fn build_is_old() -> bool {
    std::fs::metadata(binary())
        .and_then(|m| m.modified())
        .map_or(true, |t| SystemTime::now().duration_since(t).unwrap_or_default() > REFRESH_AFTER)
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// Descarga el zip oficial, comprueba la firma digital de Spotify AB y deja `soloist.exe` en su carpeta.
#[cfg(windows)]
fn install() -> Result<(), String> {
    let resp = ureq::get(DOWNLOAD_URL).timeout(Duration::from_secs(180)).call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    resp.into_reader().take(64 * 1024 * 1024).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut file = zip.by_name("soloist.exe").map_err(|_| "el archivo descargado no contiene Soloist")?;
    let dir = root().join("builds");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("soloist.exe.part");
    let mut out = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    std::io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
    drop(out);

    // Solo se instala si Windows confirma que lo firmó Spotify AB.
    let ps = format!(
        "$s = Get-AuthenticodeSignature -LiteralPath '{}'; if ($s.Status -eq 'Valid' -and $s.SignerCertificate.Subject -like '*Spotify AB*') {{ 'OK' }} else {{ 'MAL' }}",
        tmp.display()
    );
    let verdict = no_window(Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &ps]))
        .output()
        .map_err(|e| format!("no se pudo verificar la firma: {e}"))?;
    if !String::from_utf8_lossy(&verdict.stdout).contains("OK") {
        let _ = std::fs::remove_file(&tmp);
        return Err("la firma digital de Soloist no es válida; no se instaló".into());
    }
    let _ = std::fs::remove_file(binary());
    std::fs::rename(&tmp, binary()).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn install() -> Result<(), String> {
    Err("Soloist solo está disponible en Windows por ahora.".into())
}

/// Cierra un Soloist nuestro que siga abierto sin la app (según `soloist.pid`).
fn stop_orphan() {
    #[cfg(windows)]
    {
        let Ok(text) = std::fs::read_to_string(data().join("soloist.pid")) else { return };
        let Ok(pid) = text.trim().parse::<u32>() else { return };
        // Solo si de verdad es un soloist.exe (no otro proceso que reutilizó ese número).
        let list = no_window(Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])).output();
        if list.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("soloist.exe")) {
            let _ = no_window(Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"])).output();
            std::thread::sleep(Duration::from_millis(400));
        }
    }
}

fn write_quality_prefs(level: u32) {
    let path = data().join("settings").join("prefs");
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::write(&path, proto::quality_prefs(&existing, level));
}

impl Soloist {
    /// Arranca todo en un hilo: descarga si hace falta, abre el proceso y conecta el WebSocket.
    pub fn start(key: String, on_event: impl Fn(Event) + Send + Sync + 'static) -> Arc<Soloist> {
        let (tx, rx) = mpsc::channel::<String>();
        let me = Arc::new(Soloist { stop: Arc::new(AtomicBool::new(false)), tx, child: Arc::default() });
        let (stop, child_slot) = (me.stop.clone(), me.child.clone());
        let on_event = Arc::new(on_event);
        std::thread::Builder::new()
            .name("soloist".into())
            .spawn(move || run(key, rx, stop, child_slot, on_event))
            .expect("hilo de Soloist");
        me
    }

    /// Orden directa por el WebSocket local: `{"type":"command","command":name, …args}`.
    pub fn command(&self, name: &str, args: Value) {
        let mut msg = match args {
            Value::Object(m) => m,
            _ => Default::default(),
        };
        msg.insert("type".into(), json!("command"));
        msg.insert("command".into(), json!(name));
        let _ = self.tx.send(Value::Object(msg).to_string());
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Soloist {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    key: String,
    rx: mpsc::Receiver<String>,
    stop: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
    on_event: Arc<dyn Fn(Event) + Send + Sync>,
) {
    let emit = |s: Status| on_event(Event::Status(s));
    let mut retried_after_expiry = false;
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if !binary().is_file() || build_is_old() {
            emit(Status::Downloading);
            if let Err(e) = install() {
                if !binary().is_file() {
                    return emit(Status::Failed(format!("No se pudo descargar Soloist: {e}")));
                }
                // Hay una versión anterior: se sigue con ella.
            }
        }
        emit(Status::Starting);
        stop_orphan();
        let _ = std::fs::create_dir_all(data());
        let cache = data().join("cache");
        let _ = std::fs::create_dir_all(&cache);
        let port_file = data().join("ws.port");
        let _ = std::fs::remove_file(&port_file);
        write_quality_prefs(4); // «Muy alta (320 kbit/s)»

        // La clave va como argumento (así lo documenta Spotify); nunca se escribe en el registro.
        let mut cmd = Command::new(binary());
        cmd.args(["--device-name", DEVICE_NAME, "--api-key", &key, "--data-dir"])
            .arg(data())
            .arg("--cache-dir")
            .arg(&cache)
            .args(["--cache-size", "500", "--initial-volume", "70", "--ws", "127.0.0.1:0", "--verbose"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match no_window(&mut cmd).spawn() {
            Ok(c) => c,
            Err(e) => return emit(Status::Failed(format!("No se pudo abrir Soloist: {e}"))),
        };
        let (out, err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        *child_slot.lock().unwrap() = Some(child);

        // La salida se guarda en soloist.log (recortada) y se lee para sacar el id y los días restantes.
        let log = Arc::new(Mutex::new(std::fs::File::create(log_path()).ok()));
        let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut readers = Vec::new();
        for stream in [Box::new(out) as Box<dyn Read + Send>, Box::new(err)] {
            let (log, written, on_event) = (log.clone(), written.clone(), on_event.clone());
            readers.push(std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if written.fetch_add(line.len() + 1, Ordering::Relaxed) < 2_000_000 {
                        if let Some(f) = log.lock().unwrap().as_mut() {
                            let _ = writeln!(f, "{line}");
                        }
                    }
                    let info=proto::parse_log_line(&line);
                    if let Some(id)=info.device_id {on_event(Event::DeviceId(id));}
                    if let Some(d) = info.days_left {
                        on_event(Event::DaysLeft(d));
                    }
                }
            }));
        }

        // Puerto del WebSocket: Soloist lo escribe en <data>/ws.port.
        let mut port = None;
        for _ in 0..80 {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            if let Some(n) = std::fs::read_to_string(&port_file).ok().and_then(|s| s.trim().parse::<u16>().ok()) {
                port = Some(n);
                break;
            }
            let exited = child_slot.lock().unwrap().as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_some());
            if exited {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if let Some(port) = port {
            socket_loop(port, &rx, &stop, &on_event);
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }

        // El proceso terminó (o nunca respondió): se mira por qué.
        let code = child_slot.lock().unwrap().take().and_then(|mut c| {
            let _ = c.kill();
            c.wait().ok().and_then(|s| s.code())
        });
        for r in readers {
            let _ = r.join();
        }
        if code == Some(EXPIRED_EXIT_CODE) && !retried_after_expiry {
            // Versión caducada: se baja la actual y se vuelve a abrir.
            retried_after_expiry = true;
            let _ = std::fs::remove_file(binary());
            continue;
        }
        let text = std::fs::read_to_string(log_path()).unwrap_or_default().to_lowercase();
        return emit(Status::Failed(if text.contains("api key") || text.contains("unauthorized") {
            "Spotify rechazó la clave de Soloist. Copia una nueva y vuelve a configurarlo.".into()
        } else {
            "Soloist se cerró. Vuelve a intentarlo.".into()
        }));
    }
}

fn socket_loop(port: u16, rx: &mpsc::Receiver<String>, stop: &AtomicBool, on_event: &Arc<dyn Fn(Event) + Send + Sync>) {
    let Ok((mut ws, _)) = tungstenite::connect(format!("ws://127.0.0.1:{port}")) else { return };
    if let MaybeTlsStream::Plain(s) = ws.get_mut() {
        let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
    }
    let mut live = Live::default();
    let _ = ws.send(Message::Text(json!({"type": "command", "command": "get_auth_state"}).to_string()));
    while !stop.load(Ordering::SeqCst) {
        while let Ok(m) = rx.try_recv() {
            if ws.send(Message::Text(m)).is_err() {
                return;
            }
        }
        match ws.read() {
            Ok(Message::Text(t)) => {
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                for u in proto::handle(&mut live, &v, now_ms()) {
                    match u {
                        Update::Ask(cmd) => {
                            let mut m = json!({"type": "command", "command": cmd});
                            if cmd == "get_queue" {
                                m["limit"] = json!(30);
                            }
                            let _ = ws.send(Message::Text(m.to_string()));
                        }
                        Update::Auth { logged_in, .. } => {
                            on_event(Event::Status(if logged_in { Status::Ready } else { Status::NeedsPairing }));
                            on_event(Event::Update(u, Box::new(live.clone())));
                        }
                        other => on_event(Event::Update(other, Box::new(live.clone()))),
                    }
                }
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
    }
}
