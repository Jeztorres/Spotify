//! Actualizador integrado contra el release `latest` en GitHub (Windows y macOS).
use super::*;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const RELEASE_API: &str = "https://api.github.com/repos/Jeztorres/Spotify/releases/tags/latest";

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReleaseInfo {
    commit: String,
    build: Option<String>,
    asset_name: String,
    asset_url: String,
}

static PENDING: Mutex<Option<ReleaseInfo>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

pub fn cleanup_old_binaries() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    for name in ["spotify-win.exe.old", "spotifymac-engine.exe.old"] {
        let old = dir.join(name);
        if old.exists() {
            let _ = std::fs::remove_file(old);
        }
    }
}

fn target_asset_names() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &["SpotifyMac-Windows.zip", "spotify-win.exe"]
    }
    #[cfg(target_os = "macos")]
    {
        &["SpotifyMac-macOS.dmg"]
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        &[]
    }
}

fn parse_release_info(v: &Value, preferred_assets: &[&str]) -> Option<ReleaseInfo> {
    let body = v.get("body").and_then(Value::as_str).unwrap_or("");
    let mut commit = None;
    let mut build = None;
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Commit:") {
            let c = rest.trim();
            if !c.is_empty() {
                commit = Some(c.to_owned());
            }
        } else if let Some(rest) = trimmed.strip_prefix("Build:") {
            let b = rest.trim();
            if !b.is_empty() {
                build = Some(b.to_owned());
            }
        }
    }
    let commit = commit?;
    let assets = v.get("assets").and_then(Value::as_array)?;
    for wanted in preferred_assets {
        if let Some(asset) = assets
            .iter()
            .find(|a| a.get("name").and_then(Value::as_str) == Some(*wanted))
        {
            if let Some(url) = asset.get("browser_download_url").and_then(Value::as_str) {
                if url.starts_with("https://") {
                    return Some(ReleaseInfo {
                        commit,
                        build,
                        asset_name: (*wanted).to_owned(),
                        asset_url: url.to_owned(),
                    });
                }
            }
        }
    }
    None
}

fn is_newer_commit(local_short: &str, remote_commit: &str) -> bool {
    if local_short.is_empty() || local_short == "sin-git" || remote_commit.is_empty() {
        return false;
    }
    !remote_commit.starts_with(local_short) && !local_short.starts_with(remote_commit)
}

fn fetch_release_info() -> Result<ReleaseInfo, String> {
    let resp = ureq::get(RELEASE_API)
        .set("User-Agent", "SpotifyMac-Updater")
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| format!("No se pudo consultar GitHub: {e}"))?;
    let json: Value = serde_json::from_reader(resp.into_reader().take(2 * 1024 * 1024))
        .map_err(|e| format!("Respuesta inválida de GitHub: {e}"))?;
    parse_release_info(&json, target_asset_names())
        .ok_or_else(|| "El release aún no incluye el paquete para este sistema.".to_owned())
}

fn set_status(available: bool, busy: bool, msg: String) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(|ui, _| {
            ui.set_update_available(available);
            ui.set_update_busy(busy);
            ui.set_update_status(msg.into());
        });
    });
}

fn check_updates(silent_if_current: bool) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    if !silent_if_current {
        set_status(false, true, "Buscando actualizaciones…".into());
    }
    std::thread::spawn(move || {
        let result = fetch_release_info();
        BUSY.store(false, Ordering::SeqCst);
        match result {
            Ok(info) => {
                let local = env!("APP_COMMIT_HASH");
                if is_newer_commit(local, &info.commit) {
                    let short: String = info.commit.chars().take(7).collect();
                    let label = match &info.build {
                        Some(b) => format!("{short} · {b}"),
                        None => short,
                    };
                    if let Ok(mut g) = PENDING.lock() {
                        *g = Some(info);
                    }
                    set_status(
                        true,
                        false,
                        format!("Nueva versión disponible ({label}). Pulsa para actualizar y reiniciar."),
                    );
                } else if !silent_if_current {
                    set_status(false, false, "Ya tienes la versión más reciente.".into());
                }
            }
            Err(err) => {
                if !silent_if_current {
                    set_status(false, false, err);
                }
            }
        }
    });
}

fn install_update(info: ReleaseInfo) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    set_status(true, true, "Descargando actualización…".into());
    std::thread::spawn(move || {
        let res = apply_release(&info);
        BUSY.store(false, Ordering::SeqCst);
        if let Err(err) = res {
            set_status(true, false, err);
        }
    });
}

#[cfg(windows)]
fn apply_release(info: &ReleaseInfo) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("No se encontró el ejecutable: {e}"))?;
    let dir = exe.parent().ok_or("Carpeta de ejecución inválida")?;

    let resp = ureq::get(&info.asset_url)
        .set("User-Agent", "SpotifyMac-Updater")
        .timeout(Duration::from_secs(240))
        .call()
        .map_err(|e| format!("Error al descargar: {e}"))?;

    if info.asset_name.ends_with(".zip") {
        let zip_tmp = dir.join("update.zip.part");
        let mut out = std::fs::File::create(&zip_tmp)
            .map_err(|e| format!("No se pudo escribir archivo temporal: {e}"))?;
        if let Err(e) = std::io::copy(&mut resp.into_reader().take(256 * 1024 * 1024), &mut out) {
            let _ = std::fs::remove_file(&zip_tmp);
            return Err(format!("Error al guardar descarga: {e}"));
        }
        drop(out);

        set_status(true, true, "Instalando actualización…".into());
        let extract_res = extract_windows_zip(&zip_tmp, dir, &exe);
        let _ = std::fs::remove_file(&zip_tmp);
        extract_res?;
    } else {
        let new_exe = exe.with_extension("exe.new");
        let mut out = std::fs::File::create(&new_exe)
            .map_err(|e| format!("No se pudo crear el nuevo ejecutable: {e}"))?;
        if let Err(e) = std::io::copy(&mut resp.into_reader().take(128 * 1024 * 1024), &mut out) {
            let _ = std::fs::remove_file(&new_exe);
            return Err(format!("Error al guardar descarga: {e}"));
        }
        drop(out);
        set_status(true, true, "Reiniciando con la nueva versión…".into());
        replace_running_file(&exe, &new_exe)?;
    }

    let _ = Command::new(&exe).spawn();
    std::process::exit(0);
}

#[cfg(windows)]
fn extract_windows_zip(zip_path: &Path, dir: &Path, current_exe: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("No se pudo abrir ZIP: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("ZIP inválido: {e}"))?;

    let current_name = current_exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("spotify-win.exe");

    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("Error leyendo ZIP: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let norm = entry.name().replace('\\', "/");
        let target = if norm == "spotify-win.exe" || norm.ends_with("/spotify-win.exe") {
            dir.join(current_name)
        } else if norm == "spotifymac-engine.exe" || norm.ends_with("/spotifymac-engine.exe") {
            dir.join("spotifymac-engine.exe")
        } else if norm == "media/ffmpeg.exe" || norm.ends_with("/media/ffmpeg.exe") {
            let media_dir = dir.join("media");
            let _ = std::fs::create_dir_all(&media_dir);
            media_dir.join("ffmpeg.exe")
        } else {
            continue;
        };

        let new_path = PathBuf::from(format!("{}.new", target.display()));
        let mut out = std::fs::File::create(&new_path)
            .map_err(|e| format!("No se pudo extraer {}: {e}", target.display()))?;
        if let Err(e) = std::io::copy(&mut entry, &mut out) {
            let _ = std::fs::remove_file(&new_path);
            return Err(format!("Error extrayendo {}: {e}", target.display()));
        }
        staged.push((target, new_path));
    }

    if staged.is_empty() {
        return Err("El paquete ZIP no contiene ejecutables válidos.".into());
    }

    for (target, new_path) in staged {
        replace_running_file(&target, &new_path)?;
    }
    Ok(())
}

#[cfg(windows)]
fn replace_running_file(target: &Path, new_path: &Path) -> Result<(), String> {
    if target.exists() {
        let old_path = PathBuf::from(format!("{}.old", target.display()));
        let _ = std::fs::remove_file(&old_path);
        std::fs::rename(target, &old_path)
            .map_err(|e| format!("No se pudo reemplazar {}: {e}", target.display()))?;
    }
    std::fs::rename(new_path, target)
        .map_err(|e| format!("No se pudo mover la nueva versión a {}: {e}", target.display()))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn apply_release(info: &ReleaseInfo) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("No se encontró el ejecutable: {e}"))?;
    let app_bundle = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
        .ok_or("La actualización automática en macOS requiere ejecutar desde SpotifyMac.app.")?;

    let tmp_dir = std::env::temp_dir().join("spotifymac-update");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("No se pudo crear carpeta temporal: {e}"))?;

    let dmg_path = tmp_dir.join("SpotifyMac-macOS.dmg");
    let mount_dir = tmp_dir.join("mnt");
    std::fs::create_dir_all(&mount_dir).map_err(|e| format!("No se pudo crear punto de montaje: {e}"))?;

    let resp = ureq::get(&info.asset_url)
        .set("User-Agent", "SpotifyMac-Updater")
        .timeout(Duration::from_secs(240))
        .call()
        .map_err(|e| format!("Error al descargar: {e}"))?;
    let mut out = std::fs::File::create(&dmg_path)
        .map_err(|e| format!("No se pudo guardar DMG: {e}"))?;
    std::io::copy(&mut resp.into_reader().take(256 * 1024 * 1024), &mut out)
        .map_err(|e| format!("Error al escribir DMG: {e}"))?;
    drop(out);

    set_status(true, true, "Instalando actualización…".into());
    let attach = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
        .arg(&mount_dir)
        .arg(&dmg_path)
        .status()
        .map_err(|e| format!("No se pudo montar el DMG: {e}"))?;
    if !attach.success() {
        return Err("Falló el montaje del DMG de actualización.".into());
    }

    let src_app = mount_dir.join("SpotifyMac.app");
    let copy_ok = Command::new("ditto")
        .arg(&src_app)
        .arg(&app_bundle)
        .status()
        .is_ok_and(|s| s.success());
    let _ = Command::new("hdiutil")
        .args(["detach", "-quiet"])
        .arg(&mount_dir)
        .status();
    let _ = std::fs::remove_dir_all(&tmp_dir);

    if !copy_ok {
        return Err("No se pudo reemplazar SpotifyMac.app (verifica permisos en Aplicaciones).".into());
    }

    let _ = Command::new("open").args(["-n"]).arg(&app_bundle).spawn();
    std::process::exit(0);
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn apply_release(_info: &ReleaseInfo) -> Result<(), String> {
    Err("La actualización automática solo está disponible en Windows y macOS.".into())
}

pub fn wire(ui: &MainWindow) {
    ui.on_check_update(|| {
        if let Ok(mut g) = PENDING.lock() {
            if let Some(info) = g.take() {
                install_update(info);
                return;
            }
        }
        check_updates(false);
    });
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(3));
        check_updates(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commit_build_and_matching_asset() {
        let json = serde_json::json!({
            "body": "Commit: 5a1e694abcdef1234567890\nBuild: #24\n",
            "assets": [
                {
                    "name": "SpotifyMac-macOS.dmg",
                    "browser_download_url": "https://github.com/Jeztorres/Spotify/releases/download/latest/SpotifyMac-macOS.dmg"
                },
                {
                    "name": "SpotifyMac-Windows.zip",
                    "browser_download_url": "https://github.com/Jeztorres/Spotify/releases/download/latest/SpotifyMac-Windows.zip"
                }
            ]
        });
        let info = parse_release_info(&json, &["SpotifyMac-Windows.zip"]).unwrap();
        assert_eq!(info.commit, "5a1e694abcdef1234567890");
        assert_eq!(info.build.as_deref(), Some("#24"));
        assert_eq!(info.asset_name, "SpotifyMac-Windows.zip");
    }

    #[test]
    fn commit_comparison_detects_updates_and_matches_short_hashes() {
        assert!(!is_newer_commit("5a1e694", "5a1e694abcdef"));
        assert!(is_newer_commit("5a1e694", "9b2c3d4abcdef"));
        assert!(!is_newer_commit("sin-git", "9b2c3d4abcdef"));
    }
}
