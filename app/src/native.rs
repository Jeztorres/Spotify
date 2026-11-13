//! Diálogo nativo de archivos y preparación de portadas; no invoca shells.
use std::path::{Path, PathBuf};

pub fn choose_cover() -> Result<Option<PathBuf>, String> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Controls::Dialogs::*;
        let mut buffer = vec![0u16; 32768];
        let filter: Vec<u16> = "Imágenes (JPG, PNG)\0*.jpg;*.jpeg;*.png\0\0"
            .encode_utf16()
            .collect();
        let title: Vec<u16> = "Elegir portada de playlist\0".encode_utf16().collect();
        let mut dialog: OPENFILENAMEW = std::mem::zeroed();
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrTitle = title.as_ptr();
        dialog.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER;
        if GetOpenFileNameW(&mut dialog) != 0 {
            let len = buffer.iter().position(|&x| x == 0).unwrap_or(buffer.len());
            return Ok(Some(PathBuf::from(String::from_utf16_lossy(
                &buffer[..len],
            ))));
        }
        let error = CommDlgExtendedError();
        if error == 0 {
            Ok(None)
        } else {
            Err(format!("No se pudo abrir el selector ({error})."))
        }
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

pub fn encode_cover(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory(bytes)
        .map_err(|_| "La imagen no es un JPG o PNG válido.")?
        .resize_to_fill(640, 640, image::imageops::FilterType::Lanczos3)
        .to_rgb8();
    for quality in [92, 85, 75, 60, 45, 30] {
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality)
            .encode_image(&image)
            .map_err(|e| e.to_string())?;
        if jpeg.len() <= 256 * 1024 {
            return Ok(jpeg);
        }
    }
    Err("No se pudo reducir la portada al tamaño que acepta Spotify.".into())
}

pub fn read_cover(path: &Path) -> Result<Vec<u8>, String> {
    if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > 20 * 1024 * 1024 {
        return Err("Elige una imagen de menos de 20 MB.".into());
    }
    encode_cover(&std::fs::read(path).map_err(|e| e.to_string())?)
}

pub struct StagedCover(pub PathBuf);
impl StagedCover {
    pub fn new(bytes: &[u8]) -> Result<Self, String> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spotifymac-cover-{}-{stamp}.jpg",
            std::process::id()
        ));
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for StagedCover {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn copy(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text.to_owned()))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cover_is_square_jpeg_under_spotify_limit() {
        let image = image::DynamicImage::new_rgb8(900, 450);
        let mut encoded = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let jpeg = encode_cover(encoded.get_ref()).unwrap();
        assert!(jpeg.len() <= 256 * 1024);
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (640, 640));
        assert!(encode_cover(b"invalid image").is_err());
    }
}
