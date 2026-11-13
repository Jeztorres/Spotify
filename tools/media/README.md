# Fondos animados de Windows

La UI sigue usando Slint. `app/src/media.rs` traslada las fuentes de
`AppleMusicArtistMotion.swift`, `AppleMusicAlbumMotion.swift` y `MusicVideo.swift`:
Apple Music para cabeceras/logotipos y YouTube para videoclips.

FFmpeg decodifica el HLS del fondo a cuadros RGBA de 960 × 540, a 15 FPS,
sin audio ni guardar videos en disco. El proceso termina al navegar o cerrar
la app. Los resultados del catálogo se guardan tres días; los errores de red
no se guardan como resultados vacíos.

Distribución usada: BtbN FFmpeg-Builds, LGPL, shared, 2026-11-10.
Origen: https://github.com/BtbN/FFmpeg-Builds/releases/tag/latest
Archivo: `ffmpeg-master-latest-win64-lgpl-shared.zip`
SHA256 descargado: `1ceb057674b3325285a8ec10322e7562f24297172bc716ab2b1d981c21f40aac`

Se conserva la licencia y la documentación del proveedor dentro de `runtime/`.
En desarrollo se encuentra automáticamente bajo este directorio.
Al distribuir la app, copiar el contenido de `runtime/ffmpeg-master-latest-win64-lgpl-shared/bin/`
a `media/` junto al ejecutable, manteniendo sus DLL, licencia y avisos.
La interfaz mantiene la portada estática si el catálogo no ofrece animación.

Los videoclips abren el reproductor oficial de YouTube en el navegador y pausan
Spotify para evitar audio simultáneo. No se descargan ni se extrae su video.
