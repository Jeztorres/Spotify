//! Credenciales guardadas en el Administrador de credenciales de Windows; en macOS se guardan
//! en un archivo privado (0600) dentro de Application Support para evitar avisos del Llavero en
//! builds sin certificado Developer ID.

pub const ENGINE_CREDENTIALS: &str = "engineCredentials";

#[cfg(not(target_os = "macos"))]
mod backend {
    use keyring::Entry;

    const SERVICE: &str = "SpotifyMac";

    pub fn get(key: &str) -> Option<String> {
        let bytes = Entry::new(SERVICE, key).ok()?.get_secret().ok()?;
        String::from_utf8(bytes).ok()
    }

    pub fn set(key: &str, value: &str) -> Result<(), String> {
        // `set_secret` guarda bytes tal cual (hasta 2560), el doble que como texto UTF-16.
        Entry::new(SERVICE, key)
            .and_then(|e| e.set_secret(value.as_bytes()))
            .map_err(|e| format!("No se pudo guardar la credencial: {e}"))
    }

    pub fn remove(key: &str) {
        if let Ok(e) = Entry::new(SERVICE, key) {
            let _ = e.delete_credential();
        }
    }
}

#[cfg(target_os = "macos")]
mod backend {
    use serde_json::{Map, Value};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn path() -> PathBuf {
        crate::engine::data_dir().join(".secrets.json")
    }

    fn read_map() -> Map<String, Value> {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    fn write_map(map: &Map<String, Value>) -> Result<(), String> {
        let p = path();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("No se pudo guardar la credencial: {e}"))?;
        }
        let data = serde_json::to_string(map).map_err(|e| format!("No se pudo guardar la credencial: {e}"))?;
        let tmp = p.with_extension("json.part");
        std::fs::write(&tmp, data).map_err(|e| format!("No se pudo guardar la credencial: {e}"))?;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        std::fs::rename(&tmp, &p).map_err(|e| format!("No se pudo guardar la credencial: {e}"))
    }

    pub fn get(key: &str) -> Option<String> {
        let _g = guard();
        read_map().get(key).and_then(Value::as_str).map(str::to_owned)
    }

    pub fn set(key: &str, value: &str) -> Result<(), String> {
        let _g = guard();
        let mut map = read_map();
        map.insert(key.to_owned(), Value::String(value.to_owned()));
        write_map(&map)
    }

    pub fn remove(key: &str) {
        let _g = guard();
        let mut map = read_map();
        if map.remove(key).is_some() {
            let _ = write_map(&map);
        }
    }
}

pub fn get(key: &str) -> Option<String> {
    backend::get(key)
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    backend::set(key, value)
}

pub fn remove(key: &str) {
    backend::remove(key);
}
