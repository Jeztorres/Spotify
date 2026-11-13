//! Credenciales guardadas en el Administrador de credenciales de Windows (equivale al Llavero de macOS).

use keyring::Entry;

const SERVICE: &str = "SpotifyMac";
pub const ENGINE_CREDENTIALS: &str = "engineCredentials";

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
