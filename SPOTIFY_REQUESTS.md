# Solicitudes y caché

## Auditoría de Spotify Web API

| Área | Endpoints | Activación anterior | Cambio |
| --- | --- | --- | --- |
| Inicio y biblioteca | `GET /me`, `/me/playlists`, `/me/tracks` | Al abrir la app o Favoritas; `/me/tracks` recorría todas las páginas | Caché por cuenta; Favoritas carga una página y ofrece «Cargar todas» |
| Búsqueda | `GET /search` | Tras escribir; la paginación se activaba al aparecer el marcador de scroll | Debounce de 1 s, mínimo tres caracteres salvo Intro, límite 10, caché compartida entre consultas equivalentes (minúsculas y espacios normalizados) y botón «Cargar más» con offset real |
| Artistas | `GET /artists/{id}`, `/artists/{id}/albums`, `/artists/{id}/top-tracks`, `/artists/{id}/related-artists`, `/artists?ids=…` | Varias tareas al entrar; discografía en cuatro grupos paginados; búsqueda y resolución de nombres | Paginación combinada de discografía, enriquecimiento al desplazarse, máximo de seis nombres auxiliares, sustitución del endpoint por lotes retirado y memoria de endpoints no disponibles durante un día |
| Álbumes y playlists | `GET /albums/{id}`, `/albums/{id}/tracks`, `/playlists/{id}/items` | Carga completa de canciones y recomendaciones al abrir | Caché; primera página inicial, resto a petición; recomendaciones al desplazarse |
| Perfiles | `GET /users/{id}`, `/users/{id}/playlists` | Al abrir una playlist o un perfil | Usa playlists ya encontradas cuando existen; deja de repetir endpoints no disponibles durante un día |
| Reproducción | `GET /me/player`, `PUT/POST /me/player/*` | Sondeo cada 3 s originalmente; después cada 60/180 s | El estado actual no se cachea. Las órdenes tienen prioridad y siguen usando Spotify. El sondeo se detiene durante el 429. Mientras suena en este Mac, el evento `player_state_changed` del Web Playback SDK sustituye al sondeo y a las lecturas de confirmación tras cada orden; con la ventana en segundo plano el intervalo se duplica |

Todas las llamadas a `api.spotify.com/v1` pasan por `SpotifyRequestManager`. Hay tres niveles de paso: órdenes de reproducción, lecturas que el usuario espera y enriquecimiento en segundo plano (`spotifyBackground { … }`: artistas de una playlist, nombres auxiliares, fotos de perfil, recomendaciones por género). El segundo plano cede el turno y no puede usar los últimos 4 huecos de la ventana de 30 s. `Retry-After` se acepta en segundos o como fecha HTTP y nunca se acorta. Al iniciar, el nombre y las playlists se muestran desde la copia guardada (aunque haya vencido) y se revalidan detrás. El token se refresca una sola vez aunque varias peticiones lo necesiten a la vez, y un 401 solo provoca un refresco si el token rechazado sigue siendo el actual. El token OAuth y Web Playback SDK permanecen en Spotify. El gestor permite una llamada de red a la vez, al menos 1,5 s entre llamadas y un máximo de 12 cada 30 s; comparte GET idénticos simultáneos. Respeta `Cache-Control: no-store`, `max-age` y ETag. Nunca reintenta automáticamente un 429. Conserva `Retry-After` y `QUOTA_EXCEEDED` entre reinicios y muestra datos previamente almacenados cuando es posible.

Las respuestas de catálogo se guardan temporalmente bajo `~/Library/Application Support/SpotifyMac/SpotifyCache`; los datos se separan por sesión y se borran al cerrar sesión. La copia vencida puede mostrarse durante un 429 hasta 30 días en catálogo y 1 día en datos privados. No se cachea el estado de reproducción ni se guarda audio.

## Expiraciones configurables

Se ajustan mediante `UserDefaults` en segundos, con prefijo `spotifyCacheTTL.`: `search` (6 h), `artist` (7 d), `album` (7 d), `playlist` (1 h), `library` (30 min), `liked` (1 h), `profile` (1 d), `other` (1 h), `covers` (7 d como máximo local al iniciar), `maximumStaleAge` (30 d) y `privateMaximumStaleAge` (1 d). `Cache-Control` de Spotify puede exigir una expiración más corta. Las portadas usan `URLCache` con 200 MB de disco y sus encabezados HTTP.

La caché pública está en `~/Library/Application Support/SpotifyMac/PublicMetadataCache`. Sus claves `publicCacheTTL.` son `biography` (7 d), `history` (7 d), `events` (1 h), `videos` (1 d) y `publicMetadata` (3 d).

## Diagnóstico

El archivo `~/Library/Application Support/SpotifyMac/spotify-api-diagnostics.log` registra endpoint, llamadas reales, aciertos de caché, duplicados evitados, peticiones bloqueadas, 429, `Retry-After` y tipo `quota` o `rate`. No registra tokens ni texto de búsqueda. Se trunca después de 2 MB.

## Fuentes complementarias y condiciones

| Fuente | Uso en la app | Condición aplicada |
| --- | --- | --- |
| Deezer | Identificación verificada por álbum, nombres relacionados y títulos de canciones para buscar su equivalente en Spotify | Solo metadatos; no audio ni estadísticas atribuidas a Spotify. [Condiciones de Deezer](https://cdn-content.dzcdn.net/pdf/CGU-developers.pdf) piden aprobación antes de publicar una app en línea. |
| TheAudioDB | Biografía, fotos y datos del artista | Se identifica la fuente. La clave gratuita `123` sirve para desarrollo; [sus condiciones](https://www.theaudiodb.com/docs_terms_of_use.php) exigen suscripción para publicar en una tienda de apps. |
| MusicBrainz | Identidad, colaboradores, géneros y discografía histórica | Un máximo de una petición por segundo y User-Agent identificable según [su documentación](https://musicbrainz.org/doc/MusicBrainz_API). |
| Wikidata | Datos enciclopédicos por QID; SPARQL solo si EntityData falla | Consulta SPARQL pequeña y específica; User-Agent y caché según [normas de Wikimedia](https://www.mediawiki.org/wiki/Wikimedia_APIs/Rate_limits). |
| Wikipedia | Biografías y reseñas enlazadas al artículo | API pública, fuente visible, caché y User-Agent según [etiqueta de MediaWiki](https://www.mediawiki.org/wiki/API:Etiquette). |
| Cover Art Archive | Portadas de lanzamientos históricos con MBID | Solo URLs de imágenes por ID de MusicBrainz; [API oficial](https://musicbrainz.org/doc/Cover_Art_Archive/API). |
| iTunes Search API | Títulos y años como último respaldo de discografía histórica | No usa previews ni artwork promocional; [Apple limita esos recursos](https://developer.apple.com/library/archive/documentation/AudioVideo/Conceptual/iTuneSearchAPI/) a promoción con enlace a iTunes. |

Las fuentes externas solo complementan metadatos. La búsqueda de pistas reproducibles, autenticación, biblioteca y controles siguen dependiendo de Spotify. Un 429 sin contenido previo de Spotify no puede convertirse en una búsqueda Spotify funcional hasta que venza el período indicado.

## Motor librespot (sin cuota de Web API)

Con el motor `Engine/` conectado (cuenta Premium), la reproducción y sus controles van por Spotify Connect, y estas lecturas salen de la sesión de streaming: lista de playlists (rootlist), canciones de playlists (también ajenas) y de álbumes, populares, discografía, artistas relacionados y fichas de varios artistas en una petición. Si el motor falla, se usa Web API con su caché. Búsqueda, Favoritas, perfil y ficha de artista siguen en Web API.

Otras reducciones: las recomendaciones de una playlist se calculan una vez por playlist (no con cada página), las recomendaciones por género de un álbum usan un género (2 búsquedas en vez de 4), y Favoritas es incremental: si la primera página coincide con la guardada (mismo total y misma canción más reciente), las demás páginas salen de la copia guardada.

## Spotify Connect y videoclips

- **Selector de dispositivos** (icono junto a pantalla completa en la píldora, y botón «Dispositivos» en pantalla completa): `GET /me/player/devices` solo al abrir el selector o pulsar actualizar (nunca se cachea, prioridad de orden) y `PUT /me/player` para pasar la música conservando canción y posición. Del teléfono/Alexa a este Mac funciona desde la app de Spotify eligiendo «SpotifyMac» (el motor librespot es un dispositivo Connect). Cuando otro dispositivo toma la reproducción, el motor envía `stopped: true` y la app hace una sola lectura de `/me/player`; mientras suena en otro dispositivo el sondeo pasa a 30 s.
- **Videoclips**: Spotify no ofrece sus videos a terceros. Se busca el videoclip oficial en YouTube (Data API si hay clave `youtube` en keys.json; si no, la página pública de resultados) y se muestra con el reproductor oficial incrustado. Resultados guardados 3 días en `UserDefaults` (`musicVideoCache.v2`). En pantalla completa el botón «Video» lo pone de fondo, silenciado y sincronizado con Spotify (ajuste ±0,5 s recordado por video). En la página del artista se abre en una ventana con sonido y se pausa Spotify.

## Conciertos

Sección «Próximos conciertos» en la página del artista (sustituye a las tarjetas que abrían YouTube y Songkick). Fuentes: **Ticketmaster Discovery API** (la app trae su clave en `BundledKeys.swift`, ofuscada; `ticketmaster` en keys.json la sustituye; artista exacto por `attractions.json` y luego `events.json?attractionId=…`, en todo el mundo) y, opcional, **SeatGeek** (clave `seatgeek`), que solo añade fechas que Ticketmaster no tenga (mismo día y ciudad). Cada fecha muestra ciudad, recinto, día y hora local, precio desde, estado (a la venta, venta próximamente, agotado, cancelado, pospuesto) y el enlace oficial de compra. El usuario no configura nada.

Ahorro de cuota (5000 llamadas/día y 5/s compartidas por la clave de serie): caché en disco `concerts-cache.json` (fechas 12 h, sin conciertos 24 h, id del artista 30 días; la copia vencida se muestra sin red o sin cuota), consulta solo si la sección sigue visible 1 s, una llamada a la vez con 250 ms de separación y peticiones iguales compartidas, lectura de `Rate-Limit-Available`/`Rate-Limit-Reset` (con menos de 300 restantes o tras un 429 solo caché hasta el reinicio) y tope local de 400 llamadas/día por instalación.

## Letras

Botón «Letra» en pantalla completa: letra sincronizada en el hueco de la portada (la línea actual se resalta y se sigue sola; pulsar una línea salta a ese momento). Fuente 1: Spotify (servicio color-lyrics, el de la app oficial, normalmente Musixmatch) por la sesión del motor, orden `lyrics`. Fuente 2: LRCLIB (lrclib.net, abierta, sin clave; `/api/get` y, si no, `/api/search` con duración ±5 s). Se muestra el proveedor bajo la letra.
