# Portadas animadas y encabezados de artista

## 1. Qué es m8tec/apple-music-animated-artworks (y por qué no se conecta)

- **Cómo funciona** (`AppleMusicClient.cs`): extrae un token *bearer* de la página web de Apple Music, llama a `amp-api.music.apple.com/v1/catalog/{storefront}/albums/{id}?extend=editorialVideo` y lee `motionDetailSquare` (portada cuadrada) y `motionDetailTall` (vertical, la de pantalla de bloqueo/vista ampliada). Ambos son streams HLS (`.m3u8`) alojados en la CDN de Apple. El servicio de m8tec guarda en caché esas URLs y ofrece `/api/v1/artwork/search` y `/url`.
- **Licencia:** el repositorio **no tiene licencia** (la API de GitHub devuelve `license: null`) y el README dice «solo con fines educativos» y pide «respetar los Términos de Apple Music». Los videos son propiedad de Apple y de los sellos; el proyecto no concede ningún derecho sobre ellos y obtiene el acceso con un token extraído de la web de Apple, no con una API pública.
- **Conclusión:** reutilizar esos videos en Tempo sería extracción y redistribución sin autorización. **No se ha implementado** ningún cliente de `artwork.m8tec.top` ni de `amp-api.music.apple.com`. Lo que sí cabe es pedir permiso por escrito a Apple/sellos/artistas, o usar videos propios.

## 2. Lo implementado (`AnimatedArtwork.swift`)

| Pieza | Función |
| --- | --- |
| `AnimatedAlbumArtwork` | Portada reutilizable: imagen estática siempre debajo y, si hay video autorizado, un bucle encima con fundido de 0,9 s |
| `MotionVideoLayer` | Capa de animación para poner sobre cualquier imagen (se usa también en el encabezado del artista) |
| `LoopingVideoView` / `LoopingPlayerView` | AVFoundation: `AVPlayerLayer` + `AVQueuePlayer` + `AVPlayerLooper`. El video se copia a una `AVMutableComposition` **solo de imagen**: sin pista de audio, muteado, sin salida externa y sin impedir la suspensión de pantalla |
| `AnimatedArtworkProvider` | Protocolo de fuentes. Solo hay una incluida: `LocalManifestProvider` |
| `AnimatedArtworkService` | Resuelve fuentes, máximo 3 videos a la vez, copia a caché solo si la entrada dice `"cache": true` |
| `MotionPolicy` | Desactiva todo con Reducir movimiento, Modo de bajo consumo o el interruptor «Portadas animadas autorizadas» (menú de la paleta, barra lateral) |

**Integración:** portada de 320 pt en la pantalla de álbum, tarjetas `MediaCard` de álbumes (Inicio, «Más de…», «Álbumes que te pueden gustar») y foto nítida del encabezado del artista. Sin animación autorizada no cambia nada de lo anterior.

**Reglas cumplidas**

- Reproducción en bucle y sin audio; sin sesión ni pistas de audio, y el reproductor (librespot/SDK) no se toca.
- Se pausa cuando la tarjeta sale de pantalla, la ventana queda oculta/minimizada o el modo de movimiento lo prohíbe.
- Cero peticiones a Spotify: la consulta usa los ids y nombres que la pantalla ya tiene.
- Caché: las URLs https se transmiten y **no** se guardan; solo se descargan a `~/Library/Caches/SpotifyMac/AnimatedArtwork` (tope 80 MB por archivo) si la entrada declara `"cache": true`.

## 3. Cómo dar de alta videos autorizados

Archivo `~/Library/Application Support/SpotifyMac/AnimatedArtwork/manifest.json` (los videos van en esa misma carpeta, o en https):

```json
{"entries":[
  {"spotifyAlbumID":"ID_DEL_ALBUM","artist":"Artista","album":"Álbum",
   "square":"album.mp4","tall":"album-tall.mp4",
   "license":"Cedido por <titular>, <fecha>, <alcance>","cache":false},
  {"spotifyArtistID":"ID_DEL_ARTISTA","artist":"Artista","header":"artista.mp4",
   "license":"Cedido por <titular>, <fecha>, <alcance>"}
]}
```

- Una entrada **sin `license` se ignora**: es tu declaración de que tienes derecho a usar ese video.
- Formatos MP4/MOV (H.264/HEVC). HLS no se admite (así no se pueden enchufar flujos de Apple por accidente).
- Se admiten nombres (sin distinguir mayúsculas ni acentos) o ids de Spotify. Las rutas no pueden salir de la carpeta.
- La variante `tall` ya se resuelve pero todavía no tiene un lugar en la interfaz.

## 4. Clasificación de fuentes

| Fuente | Estado |
| --- | --- |
| **Oficial** | Imágenes estáticas de Spotify Web API (ya usadas); AVFoundation/AVKit del sistema; videos propios o cedidos por escrito (manifiesto local) |
| **Comunitaria** | `artwork.m8tec.top` y su repositorio: sirve solo como referencia del formato (cuadrada/vertical, HLS). **No integrada** |
| **Requiere autorización adicional** | Cualquier video de Apple Music (`motionDetailSquare/Tall`, encabezados de artista de Apple) y cualquier video de sellos o artistas. Hace falta permiso escrito del titular antes de añadirlo al manifiesto |

---

# Videos de encabezado de artista (Apple Music)

## Cómo se representan técnicamente
- En el repositorio de m8tec solo aparece la cadena `?extend=editorialVideo&platform=web` sobre `amp-api.music.apple.com/v1/catalog/{storefront}/{albums|…}/{id}`. Del objeto `editorialVideo` se leen `motionDetailSquare.video` y `motionDetailTall.video`: URLs a streams HLS (`.m3u8`) de la CDN de Apple, con variante cuadrada y vertical. El código de m8tec que revisé solo consulta álbumes y playlists, no encabezados de artista; no he encontrado en él nada específico de artistas.
- Ese endpoint es el de la web de Apple Music y exige un token que el proyecto extrae de la propia página. No es una API para terceros.

## ¿Existe API pública autorizada?
- **No.** En la documentación oficial de Apple Music API (`developer.apple.com`, consultada el 2026-11-04), `Artists.Attributes` solo lista: `artwork`, `editorialNotes`, `genreNames`, `inFavorites`, `name` y `url`. `Albums.Attributes` tampoco incluye video. Ninguna de las dos menciona `editorialVideo`, `motionDetailSquare` ni `motionDetailTall`.
- `artwork` de artista es una imagen estática. MusicKit (Swift) expone esos mismos modelos oficiales, así que tampoco da acceso al video.
- Por tanto `editorialVideo` es un campo **no documentado**: usarlo implica depender de un endpoint interno y de un token extraído, justo lo que no se debe hacer.

## Fuentes que sí permiten reutilizar un video de artista en Tempo
Ninguna pública. Quedan vías con permiso expreso y por escrito del titular de los derechos (artista, sello o management):
1. Un video de encabezado entregado por el propio artista/sello (kit de prensa, material promocional), con licencia que cubra su uso en una app de terceros.
2. Un video propio, o con licencia abierta que permita uso y redistribución (por ejemplo Creative Commons con atribución), que muestre al artista.
3. Un acuerdo directo con Apple si quisieras usar sus videos; Apple no ofrece licencia pública para ello.

## Qué hace Tempo
- La entrada del manifiesto con `"header"` y `spotifyArtistID` o `artist` activa el video (ver sección 3 más arriba). Sin entrada con `license`, o con movimiento desactivado, se queda la fotografía original del artista (`MotionVideoLayer` no dibuja nada).
- Es el mismo `MotionVideoLayer` ya integrado en el encabezado del artista (`ArtistView.hero`): no se rediseñó el perfil ni se tocaron las portadas de álbumes. No hay animaciones sintéticas.
- No se usa ningún token extraído ni endpoint privado de Apple.
- Verificado con una prueba automática (`MotionPlaybackTests`) que genera un MP4 de 1 s, lo reproduce con `LoopingPlayerView`: sigue en marcha tras varias vueltas, sin pista de audio, con `isMuted`, y se pausa cuando la ventana está oculta. **No** está verificado visualmente dentro de la app.

---

# Prueba verificable: Artist Page Motion Artwork (beabadoobee, 2026-11-04)

Solo se leyeron metadatos (JSON, HTML público y la lista maestra `.m3u8`). **No se descargó ningún video** y Tempo no se modificó.

## Apple Music Art (`applemusic.aritra.ovh`)
- **Cómo funciona:** es una app web (Next.js). Para URLs `/artist/` llama a `GET https://api.aritra.ovh/v1/artist-media?url=<URL de Apple>` y espera `image_url`, `ambient_video_url` y `artist_name`. Para álbumes/playlists usa `/v1/covers`; los videos se bajan por un servicio propio (`/v1/download-video`, WebSocket).
- **Resultado con beabadoobee:** HTTP 200, `ambient_video_url: null`, `master_Streams.tall/square: []`. Solo devolvió `image_url`: la imagen Open Graph de Apple (PNG 1200×630, `og:image`). **El servicio no devolvió ningún video** y el nombre sale como «beabadoobee on Apple Music» (el `og:title`), lo que indica que lee las metaetiquetas de la página.
- **API pública documentada:** no. `/docs` y `/openapi.json` responden 200 con cuerpo vacío. No hay términos de uso en la web (solo formulario de reporte, Cookiebot y Analytics) ni licencia declarada.

## Lo que sí existe en la página pública de Apple
La página `music.apple.com/gb/artist/beabadoobee/1307650474` incluye en sus datos serializados el recurso real, aunque Apple Music Art no lo extraiga:

| Campo | Qué es |
| --- | --- |
| `motionArtistFullscreen16x9` y `motionArtistWide16x9` | video HLS 16:9 + `previewFrame` (imagen 3840×2160) + colores de fondo |
| `motionArtistSquare1x1` | video HLS cuadrado |
| formato | `application/vnd.apple.mpegurl` en `mvod.itunes.apple.com/itunes-assets/HLSMusic221/…/*_default.m3u8` |
| variantes (lista maestra del 16:9) | H.264 (`avc1.64001f`) y HEVC (`hvc1.2.…`), SDR, 25 fps, desde 480×270 hasta 1024×576 y más; sin audio (`CLOSED-CAPTIONS=NONE`) |

Es decir: **sí hay un video real de encabezado para este artista** (nombrado `motionArtist*`, no `motionDetail*` de las portadas de álbum). Es distinto de lo que m8tec/Auralcanvas ofrecen para álbumes.

## Autenticación y vía técnica
- La lista `.m3u8` se sirvió **sin autenticación** desde la CDN de Apple (`cache-control: private`, `x-robots-tag: noindex`).
- Para llegar a la URL hay que leer el JSON incrustado en el HTML de la página (interfaz interna sin documentar, que puede cambiar). No pasa por Apple Music API ni MusicKit: `Artists.Attributes` oficial no incluye video.

## Auralcanvas (`MaverickXu/Auralcanvas…`)
Sin licencia en GitHub (`license: null`). README: «solo educativo; todos los artworks son copyright de **Apple Inc.** y de los artistas». Para **artistas** ofrece imágenes de perfil y banners PNG, no videos; los videos que ofrece son portadas de álbum.

## Situación de permisos
| Fuente | Estado |
| --- | --- |
| Apple Music API / MusicKit | Oficial, pero **no expone** estos videos |
| Página pública de Apple (`motionArtist*`) | Técnicamente accesible sin login; es contenido de Apple y de los artistas/sellos, sin licencia para terceros. **Requiere autorización** |
| Apple Music Art / m8tec / Auralcanvas | Comunitarias, sin licencia ni términos; no autorizan nada sobre el contenido de Apple. Además Apple Music Art no devolvió el video de este artista |

**Conclusión:** existe una fuente técnicamente funcional (los datos de la propia página de Apple), pero **no una integración autorizada**. Leer esos datos y transmitir el video dentro de Tempo equivale a usar contenido de Apple fuera de Apple Music sin permiso. La vía autorizada sigue siendo el manifiesto con `license` (video cedido por artista, sello o Apple por escrito). Antes de integrar nada, habría que obtener ese permiso; si Apple o el sello lo concedieran, el formato HLS exigiría ampliar `LoopingPlayerView` (hoy solo MP4/MOV).

---

# Videos de encabezado de artista: implementación automática (con fuente licenciada)

## ¿Existe una fuente autorizada hoy?
**No.** (1) Apple Music API/MusicKit no expone `motionArtist*`; (2) la página pública de Apple los contiene, pero leerla es usar una interfaz interna y contenido sin licencia para terceros; (3) Apple Music Art, m8tec y Auralcanvas no tienen licencia ni términos, y Apple Music Art ni siquiera devolvió el video de beabadoobee. Por eso Tempo **no lee la página de Apple, ni usa tokens, ni descarga nada de Apple**.

**Autorización que falta:** un permiso escrito de Apple (o del sello/artista/management, según quién tenga los derechos) que cubra reproducir esos videos de forma remota dentro de una app de terceros, y un catálogo `Spotify artist ID → URLs HLS` publicado por el titular. Con eso, conectar la fuente es solo configurar una URL.

## Qué quedó listo (modular, sin trabajo manual por artista)
| Pieza | Archivo | Función |
| --- | --- | --- |
| `ArtistMotionVideoProvider` | `ArtistMotion.swift` | Resuelve el video del encabezado por **id de Spotify** (el que la pantalla ya tiene: cero peticiones a Spotify). Prioridad `wide16x9` → `fullscreen16x9` → `square1x1`. Solo `https` y `.m3u8/.mp4/.mov/.m4v`. Memoriza resultados, también negativos (1 h) |
| Verificación de artista | `ArtistMotionVideoProvider.matches` | El registro debe traer el mismo `spotifyArtistID` y, si hay nombres en ambos lados, nombres compatibles; si no, se rechaza |
| `LicensedCatalogSource` | `ArtistMotion.swift` | Lee **un único JSON** con cientos de artistas desde `artistMotionCatalogURL` (UserDefaults, https). Petición condicional (ETag), TTL 6 h, copia en disco, reintento tras 15 min si falla. Sin URL configurada no hace ninguna petición. Entradas sin licencia o sin id de Spotify se ignoran |
| Contrato del catálogo | `ArtistMotionCatalog` | `{"provider","license","entries":[{"spotifyArtistID","appleArtistID","artistName","wide16x9","fullscreen16x9","square1x1","license?","cache?"}]}` |
| HLS | `AnimatedArtwork.swift` | `LoopingPlayerView` reproduce `.m3u8` con AVFoundation (límite 720p / 4 Mb/s, silenciado, en bucle). HLS nunca se copia a caché de disco. AVFoundation **no** reproduce HLS desde `file://`: el HLS debe ser https |
| Encabezado | `Views.swift` (`ArtistView.hero`) | `MotionVideoLayer` a todo el ancho detrás del nombre, con un degradado de lectura que solo aparece cuando el video está activo. Sin video: foto normal, diseño idéntico al anterior |

Se respetan Reducir movimiento, Modo de bajo consumo, el interruptor de la app, pausa fuera de pantalla/ventana oculta y máximo de 3 videos a la vez. No se tocan portadas de álbum ni el reproductor de música.

Para activar el catálogo cuando exista una fuente licenciada:

```bash
defaults write com.jeztorres.SpotifyMac artistMotionCatalogURL "https://…/artist-motion.json"
```

## Pruebas
- 22 pruebas automáticas (21 + 1 opcional de red): catálogo (una petición atiende cientos de consultas, 304/ETag, copia en disco, reintento tras fallo, sin URL = sin red), prioridad de variantes, rechazo de artista equivocado, http, sin licencia y consultas que no son de encabezado.
- **Reproducción HLS verificada:** (a) un stream HLS fMP4 generado por la prueba y servido por HTTP local; (b) con `TEMPO_NETWORK_TESTS=1`, el stream de ejemplo que Apple publica para probar reproductores. En ambos: arranca, sigue en bucle tras varias vueltas, `isMuted` y sin error.
- **No verificado visualmente dentro de Tempo por mí:** el sistema bloquea el control de SpotifyMac por computer-use. No hay video real de beabadoobee: no existe fuente autorizada. Para la comprobación visual dejé una entrada temporal (ver abajo).

---

# NopXx/apple-music-artwork-search (revisado el 2026-11-04) — no resuelve videos de artistas

- **Variantes de artista:** el código no contiene `motionArtistWide16x9`, `motionArtistFullscreen16x9` ni `motionArtistSquare1x1`. Solo lee `attributes.editorialVideo` (`motionDetailSquare`, `motionDetailTall`…) de `amp-api…/catalog/us/{albums|songs}/{id}`. `TYPE_MAP = { album: 'albums', track: 'songs' }`: **no existe el tipo `artists`**.
- **Artistas:** no soportados. La documentación solo admite URLs de álbum y canción. Una URL `/artist/…` se trata como una búsqueda de iTunes.
- **Prueba real con beabadoobee** (instancia pública del propio proyecto, URL del artista): devolvió una canción ajena («death bed (feat. beabadoobee)» de Powfu) con `animation: null`. Ningún video de artista.
- **Pruebas del proyecto:** no hay tests en el repositorio, ni ejemplo documentado con un artista; el único ejemplo del README es un álbum de IVE.
- **APIs internas:** extrae el JWT de desarrollador del bundle JS de `music.apple.com` y llama a `amp-api.music.apple.com` con él, más `Origin: https://music.apple.com`. Es el mismo mecanismo que m8tec, con token extraído.
- **Mantenimiento:** 18 commits, creado el 2026-04-28, último push 2026-11-04, 5 estrellas, 0 forks, 0 issues; un solo autor. Activo pero joven, y depende de un token y un endpoint que Apple puede cambiar.
- **Licencia:** el README dice «MIT», pero no hay archivo `LICENSE`, `package.json` no declara licencia y GitHub la detecta como `null`. Aun siendo MIT, cubriría solo el código del proyecto: no concede nada sobre los videos, que son de Apple y de los artistas/sellos, ni sobre el uso del token o del endpoint, que se rigen por los términos de Apple.
- **Conclusión:** no resuelve el problema de los videos de encabezado de artista, y no se implementó nada en Tempo.

---

# Fuente automática de Apple Music (`AppleMusicArtistMotion.swift`, 2026-11-04)

Se pidió que los videos se busquen solos para cualquier artista, como en Apple Music.

- **Flujo:** nombre de Spotify → iTunes Search API (`entity=musicArtist`, debe coincidir el nombre exacto) → página pública `music.apple.com/us/artist/{id}` → JSON `serialized-server-data` → `motionArtistWide16x9` / `Fullscreen16x9` / `Square1x1` (`.video`, HLS). Sin tokens. El video no se descarga: lo transmite `LoopingPlayerView` desde la CDN de Apple.
- **Caché:** en `~/Library/Caches/SpotifyMac/AnimatedArtwork/apple-artist-motion-us.json`. Resultados con video, 3 días; sin video, 1 día. Si hay un error de red o HTTP, se pausa 10 min.
- **Orden:** primero el catálogo licenciado (si se configuró), luego Apple Music. El manifiesto local sigue teniendo prioridad.
- **Desactivar:** `defaults write com.jeztorres.SpotifyMac appleMusicArtistMotion -bool NO`.
- **Riesgos:** es una interfaz no documentada (puede romperse si Apple cambia la página) y el contenido es de Apple y de los artistas: es para uso personal, no para distribuir la app.
- **Cobertura:** no todos los artistas tienen video. Prueba del 2026-11-04 (tienda `us`): beabadoobee y Billie Eilish sí; Taylor Swift, Bad Bunny y Peso Pluma no.
- **Pruebas:** `AppleMusicArtistMotionTests` (7, una real con `TEMPO_NETWORK_TESTS=1`). Para compilar las pruebas usa `--scratch-path` fuera de Documents: los atributos extendidos de esa carpeta rompen la firma del `.xctest`.

## Título con diseño (`artistLogo`, 2026-11-04)
- La misma página del artista trae `artistLogo`: un PNG transparente con el nombre diseñado. BTS lo tiene; beabadoobee no.
- `AppleMusicArtistMotionSource.logoURL(fromPage:)` rellena la plantilla `{w}x{h}{c}.{f}` (hasta 1200 px de ancho, `bb`, `png`). Sale de la misma consulta y caché que el video, sin peticiones extra; la caché pasó a `apple-artist-motion-v2-us.json`.
- `ArtistTitle` (Views.swift) muestra el logo en el encabezado en lugar del nombre en texto, con un fundido. Sin logo se queda el texto.

## Portadas animadas de álbum automáticas (`AppleMusicAlbumMotion.swift`, 2026-11-04)
- **Flujo:** artista y título de Spotify → iTunes Search (`entity=album`, mismo artista y mismo título; primero exacto quitando « - Single/EP», luego sin paréntesis) → `music.apple.com/us/album/{id}` → `motionDetailSquare` (cuadrada) y `motionDetailTall` (vertical).
- `motionVideos` lee solo la primera sección con videos (la cabecera), para no coger los de «Más de…».
- Se ve en todos los sitios que ya usaban `AnimatedAlbumArtwork`/`MotionVideoLayer`: la portada grande del álbum y las tarjetas de álbum.
- **`AppleMusicThrottle`:** iTunes Search admite unas 20 consultas por minuto, así que artistas y álbumes comparten una búsqueda cada 3,1 s, y las páginas de Apple van a una por segundo. Una tarjeta que sale de pantalla mientras espera no gasta su turno.
- **Caché:** `apple-album-motion-v1-us.json`. Con video, 3 días; sin video, 1 día. Si Apple falla, pausa de 10 min.
- **Desactivar:** `defaults write com.jeztorres.SpotifyMac appleMusicAlbumMotion -bool NO`.

## Otros cambios (2026-11-04)
- Se quitó «Historia musical» del perfil del artista, y con ella las consultas a MusicBrainz e iTunes que solo servían para esa sección.
- **Bug de recarga al bajar:** las tareas de carga perezosa estaban dentro de un `LazyVStack`, que recrea las vistas al hacer scroll; cada vez vaciaba y volvía a pedir Sugerencias, Videos, Conciertos y Perfil. Ahora `Store.lazyLoaded` las carga una vez por visita al artista, y si una carga se cancela a medias se reintenta.

## Corrección: la mayoría de álbumes no se encontraban (2026-11-04)
- **Causa:** la búsqueda general de iTunes (`artista + título`) devolvía sencillos y colaboraciones antes que el álbum real. *Un Verano Sin Ti*, *Cosa Nuestra* y *"Awaken, My Love!"* sí tienen portada animada, pero quedaban como «sin animación».
- **Ahora:** id de Apple del artista (30 días) → discografía completa `lookup?id=…&entity=album&limit=200` (1 día; una consulta para todos sus álbumes) → título exacto, o sin paréntesis. La búsqueda general queda como respaldo. La caché pasó a `apple-album-motion-v2-us.json`.
- Una búsqueda cancelada (la tarjeta sale de pantalla) ya no se guarda como «sin animación» ni activa la pausa de 10 min.
- Ahora piden animación también las tarjetas de la discografía del artista, «Aparece en», el último lanzamiento, los resultados de búsqueda y los álbumes de una playlist.
- Videos simultáneos: de 3 a 6. Las tarjetas que se quedaban sin turno nunca lo volvían a pedir; ahora `AnimatedArtworkService.slotFreed` les avisa cuando hay un hueco libre.

## Dónde se anima (2026-11-04)
- **Solo** en la pantalla del álbum (portada grande y portada de la cabecera compacta) y en la pantalla grande de reproducción (`NowPlayingView.cover`, con el álbum que suena).
- Las tarjetas (Inicio, «Más de…», discografía, «Aparece en», búsqueda, playlists, último lanzamiento) muestran solo la imagen y no consultan a Apple.
- Por eso desapareció el parámetro `motion` de `MediaCard` y la extensión `AlbumItem.motionQuery`.

---

# Avisos del llavero e inicio de sesión (2026-11-04)
- **Avisos «SpotifyMac quiere acceder…» (unos 10):** cada dato de sesión era una entrada propia del llavero y se leía varias veces. Además la firma provisional cambiaba en cada compilación, así que macOS trataba la app como otra distinta.
  - `Keychain.swift` guarda ahora todo en una sola entrada (`session-v2`, un diccionario JSON), leída una vez por arranque y guardada en memoria.
  - `build_app.sh` firma con el primer certificado «Apple Development» (o el que indique `SIGN_ID`). La identidad no cambia entre compilaciones, así que el llavero recuerda el permiso.
  - Las entradas antiguas no se leen, para no volver a disparar avisos: hay que iniciar sesión una vez (también en librespot).
- **Pantalla de inicio de sesión** (`Login.swift`):
  - Fondo con degradado en malla y ecualizador en movimiento (quieto con «Reducir movimiento»).
  - Tarjeta de cristal en 3 pasos: Bienvenida → Configurar (abrir el panel, Redirect URI copiable, Client ID validado en vivo con 32 caracteres hexadecimales y botón de pegar) → Conectar (animación Mac ⇄ Spotify mientras espera, «Volver a abrir», «Cancelar», errores legibles que tiemblan).
  - Si ya hay un Client ID válido, empieza en «Conectar».
  - La página que se abre en el navegador al terminar también se rediseñó y se cierra sola si todo salió bien.

## Ajustes tras probarlo (2026-11-04)
- **Segundo inicio de sesión:** el motor librespot tenía su propia credencial (en el llavero antiguo) y pedía otro login en el navegador. Ahora, si no hay credencial guardada, se conecta con el token del inicio de sesión principal (incluye `streaming`), mediante la orden nueva `token` del motor (`Engine/src/main.rs`). Si Spotify no acepta ese token, se registra en el log y queda el botón para activar la reproducción como antes.
- **Diseño:** `Login.swift` es ahora sobrio y nativo: fondo oscuro quieto, una columna alineada a la izquierda, controles del sistema y dos pasos («Configurar» solo la primera vez, y «Conectar»). Se quitaron el degradado animado, los brillos, el ecualizador y el temblor de errores. La página del navegador también se simplificó.

## Un solo inicio de sesión (2026-11-04)
- **Por qué se abrían 2 páginas:** librespot rechaza (`INVALID_CREDENTIALS`) los tokens emitidos para el Client ID propio. El motor necesitaba su propio inicio de sesión.
- **Por qué no salían las playlists:** el Client ID propio tenía guardado un bloqueo por límite de peticiones de unas 4 h (`spotifyCooldownUntil`). Todas las peticiones a la Web API se bloqueaban, y el motor (que lee la lista de playlists sin la Web API) no estaba conectado.
- **Ahora:** con el motor instalado, el inicio de sesión lo hace él (orden `login` con todos los `scopes` de la app). Se abre una sola página, el motor queda conectado y devuelve el token, que la app usa para la Web API. Se guarda en el llavero como `authClient` para renovarlo con el mismo cliente.
- Al iniciar sesión se borra el bloqueo guardado (`SpotifyRequestManager.resetCooldown`), porque era de la app anterior.
- Sin motor instalado se sigue usando el flujo anterior con el Client ID propio.
- Las sesiones antiguas con el Client ID propio piden una vez el inicio de sesión nuevo.
- **Sin verificar:** que la Web API acepte todos los endpoints con el token del motor. Se comprueba iniciando sesión con una cuenta real.

## Perfil del artista: salto al bajar y secciones nuevas (2026-11-04)
- **Salto («sube y baja») en «A los fans también les gusta»:** `LazyVStack` recalculaba alturas al hacer scroll. Ahora es `VStack`, y las secciones de abajo se cargan con `whenVisible` (`onScrollVisibilityChange`), solo cuando aparecen en pantalla. `.id(artist.id)` hace que cada artista sea una vista nueva.
- **Nuevo, desde la página del artista de Spotify** (orden `artist_overview` del motor, consulta Pathfinder `queryArtistOverview`, hash `9f8134ef…`):
  - «Playlists del artista» / «Más playlists de…»: las que hace Spotify con él (`featuringV2`) y las del propio artista (`profile.playlistsV2`).
  - «Aparece en»: las playlists donde aparece (`discoveredOnV2`). Si no hay, se usan las de la búsqueda.
  - «Aparece en estos álbumes» suma los de `relatedContent.appearsOn`.
- Requiere el motor conectado. El hash de la consulta puede cambiar si Spotify actualiza su reproductor web; el registro muestra `[ArtistOverview]` con los conteos.

## Cabecera grande en playlists editoriales (2026-11-04)
- La orden `playlist_header` del motor devuelve `format` y `format_attributes` de la playlist. Es la misma descarga que la lista de canciones, que queda en caché para `playlist_items`.
- `PlaylistHeader.imageURL` busca `header_image_url_desktop` (o cualquier clave con `header_image`) sin depender de mayúsculas ni guiones bajos.
- `TrackListView.header`: si hay imagen, muestra una cabecera a todo el ancho (540 pt) con el título grande, la descripción, los chips y los botones encima de un degradado. Si no, queda la cabecera de siempre con portada cuadrada.
- **Sin verificar con datos reales:** el nombre exacto de la clave. El registro `[PlaylistHeader]` muestra las claves de cada playlist.
