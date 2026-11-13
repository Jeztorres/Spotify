# Inicio: cómo lo construye Fastpotify y qué hace Tempo

Revisado en `github.com/crmne/fastpotify` (clon del 2026-11-04). Complementa `FASTPOTIFY_ANALISIS.md`.
Marcas: **[Público]** = Web API documentada. **[Privado]** = interfaz interna no documentada de Spotify.

## 1. Archivos y funciones de Fastpotify que construyen Home

| Pieza | Archivo / función | Origen de datos |
| --- | --- | --- |
| Pantalla | `src/ui/home.rs`: `quick_access_tiles`, `made_for_you`, `recently_played`, `top_artists`, `top_tracks`, `recommendations`, `new_releases` | — |
| «Hecho para ti» y «Tus mixes principales» | `src/made_for_you.rs` (`MadeForYou::load`, `fetch`) | **[Privado]** Pathfinder `api-partner.spotify.com/pathfinder/v2/query`, secciones `spotify:section:0JQ5DAUnp4wcj0bCb3wh3S` y `…0JQ5DAnM3wGh0gz1MXnu89`, con *hash* de consulta persistida. El propio código dice que sigue la integración de Psst y «puede cambiar sin aviso». Usa la sesión de librespot (credencial del cliente de escritorio) |
| Respaldo de «Hecho para ti» | `src/backend.rs`: `shared_made_for_you`, `with_shared_made_for_you_fallback` | **[Público]** búsqueda de playlists cuyo dueño es Spotify, usada solo si falla la lectura interna. Son resultados de búsqueda, no personalizados |
| Escuchado recientemente | `src/api/client.rs::recently_played` (`/me/player/recently-played`) + `src/history.rs` (`merged`, `played_track`, `counts_after`) | **[Público]** + historial local de reproducciones de ≥30 s (o la mitad), fusionado por hora |
| Accesos rápidos | `home.rs::quick_access_tiles` | Contextos del historial (playlist, álbum, artista, Canciones que te gustan) resueltos contra la biblioteca; si faltan, Canciones que te gustan y las primeras playlists |
| Artistas y canciones favoritos | `client.rs::top_artists`, `top_tracks` (`/me/top/artists`, `/me/top/tracks`) | **[Público]**, scope `user-top-read` |
| Recomendaciones | `client.rs::recommendations` (`/recommendations`) con semillas | **[Público pero retirado]** para apps nuevas desde nov. 2024; Fastpotify recuerda el 403/404 y oculta la fila |
| Novedades | `src/new_releases.rs` | Web API, 1 h de caché, con respaldo |
| Caché | `src/account_cache.rs`; `made_for_you.rs` (`CACHE_AGE` 10 min, un solo refresco simultáneo) | Instantáneas por cuenta, atómicas, revalidadas detrás |
| 429 / cuotas | `src/api/client.rs` (semáforos 6/4, `cooldown_until`, `Retry-After`, `QUOTA_EXCEEDED` sin reintentos); `made_for_you.rs` (`cooldown`) | — |

Resumen de las preguntas:

1. **Hecho para ti:** solo por Pathfinder [Privado] con la sesión de librespot; el respaldo [Público] es una búsqueda genérica.
2. **Escuchado recientemente:** `/me/player/recently-played` [Público, `user-read-recently-played`] + historial local.
3. **Recomendaciones personalizadas:** las de Home salen de Pathfinder [Privado]; `/recommendations` [Público] ya no está disponible para apps nuevas.
4. **Favoritos:** `/me/top/artists` y `/me/top/tracks` [Público, `user-top-read`], más Canciones que te gustan (`/me/tracks`).
5. **Web API usada:** perfil, biblioteca, búsqueda, top, recently-played, álbumes/artistas, `/me/player`.
6. **Sesión de librespot:** reproducción por Connect (estado empujado, sin sondeo) y lecturas internas (rootlist, Pathfinder, metadatos por lotes) para ahorrar cuota.
7. **Caché:** ver tabla (10 min a 1 mes según dato), por cuenta, con revalidación en segundo plano.
8. **429:** `Retry-After` pausa solo ese grant; hasta 3 reintentos con tope de 30 s; `QUOTA_EXCEEDED` corta al instante; las mutaciones no se repiten.
9. **Público frente a privado:** ver columna «Origen de datos». Lo privado es todo lo que pasa por la sesión de librespot y Pathfinder.

## 2. Qué implementa Tempo (solo oficial + local)

Archivos: `Sources/SpotifyMac/Home.swift` (nuevo: modelos, `HomeModel`, vista), `API.swift` (`recentlyPlayed`, `topArtists`, `topTracks`, `latestReleases`), `Auth.swift` (scopes opcionales), `Store.swift` (`home`, `playTracks`), `SpotifyRequestManager.swift` (TTL), `Views.swift` (se quitó el `HomeView` antiguo), `Tests/SpotifyMacTests/HomeTests.swift`. Copias previas: `backups/*.before-home.swift.txt`.

| Sección del Inicio | Fuente en Tempo |
| --- | --- |
| Accesos rápidos (8) | Contextos recientes + Canciones que te gustan + tus playlists |
| Escuchado recientemente | `/me/player/recently-played` + historial local de Tempo (solo lo posterior a la última entrada de Spotify) |
| Playlists de Spotify en tu biblioteca | Tus playlists cuyo dueño es `spotify` (Daily Mix, Discover Weekly… si las guardaste o sigues). Sin dueño conocido no se muestran |
| Tus artistas favoritos | `/me/top/artists` (6 meses). Sin permiso: más escuchados en Tempo, rotulado así |
| Tus canciones más escuchadas | `/me/top/tracks`; al pulsar reproduce la lista desde esa canción |
| Novedades de tus artistas | `/artists/{id}/albums` (álbumes y sencillos) de los 6 artistas principales, prioridad baja |
| Tus playlists | Como antes |

- **Permisos:** `user-read-recently-played` y `user-top-read` se piden solo si el usuario pulsa «Permitir» en el Inicio (un inicio de sesión). Sin pulsarlo, la sesión y la reproducción quedan como estaban. Si el usuario cancela, se revierte el ajuste.
- **Caché y cuota:** todo pasa por `SpotifyRequestManager` (ritmo, 429, `Retry-After`, datos vencidos). TTL nuevos: recientes 5 min (`recent`), top 6 h (`top`), lanzamientos de artista 1 día (`artistReleases`). Al abrir se pinta lo guardado (aunque esté vencido) y se revalida detrás; el Inicio no recarga antes de 5 min salvo el botón de actualizar. Durante un 429 se ven los datos guardados y un aviso discreto.
- **Historial local:** `~/Library/Application Support/SpotifyMac/LocalHistory-<cuenta>.json`, 200 entradas, solo canciones con ≥30 s (o la mitad) de escucha en Tempo. Es independiente de Spotify.

## 3. Diferencias con el Inicio de Spotify y con Fastpotify

- **No hay «Hecho para ti», Tus mixes principales, Descubrimiento semanal propios ni «Basado en…».** Son contenido generado por Spotify solo accesible por Pathfinder [Privado]. Tempo no los imita ni los sustituye por playlists inventadas; muestra únicamente las que ya están en tu biblioteca, con ese rótulo.
- **No hay `/recommendations`.** Spotify lo retiró para apps nuevas; «Novedades de tus artistas» es una selección derivada de tus artistas más escuchados, rotulada como tal, no una recomendación oficial.
- **Sin Pathfinder ni Client ID compartido para el Inicio.** El Inicio solo usa tu Client ID y tu sesión OAuth. (El motor librespot que ya existía, con sus lecturas de sesión, no se ha tocado ni ampliado.)
- **Sin podcasts ni «Nuevos episodios».** No se han añadido scopes de podcasts.
- **Límites del modo desarrollo:** las apps en modo desarrollo pueden devolver 403 en algunos endpoints; con los permisos ya concedidos, un 403 se muestra como aviso discreto y se conservan los datos guardados; el banner de permisos solo aparece cuando la sesión no los tiene.
