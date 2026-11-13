# Fastpotify (Spotifast): análisis y adaptación a SpotifyMac

Código revisado: `github.com/crmne/fastpotify` (el proyecto se llama ahora **Spotifast**; Rust + egui, licencia MIT), clon del 2026-11-04. Fuentes principales: `src/api/client.rs`, `src/api/gateway.rs`, `src/auth.rs`, `src/credentials.rs`, `src/account_cache.rs`, `src/liked.rs`, `src/backend.rs`, `docs/_reference/how-it-connects.md`.

Marcas: **[Confirmado]** = está en el código o en la documentación de Fastpotify. **[Propuesta]** = recomendación mía.

## 1. Separación entre Web API y reproducción

- **[Confirmado]** Usa dos credenciales independientes. La *Web API* (biblioteca, catálogo, búsqueda, control remoto) usa el Client ID que cada usuario registra. La *reproducción local* usa librespot con otra aprobación y una credencial reutilizable propia. Hay una tercera opción, "Soloist": el reproductor oficial de Spotify como proceso hijo, controlado por WebSocket en loopback.
- **[Confirmado]** Arquitectura: `src/api/` (transporte Web API) → `gateway.rs` (elige qué grant atiende cada petición) → `backend.rs` (runtime de red) / `player.rs` (motor de audio). La interfaz solo emite `Action`s y nunca bloquea en red.
- **[Confirmado]** Spotify responde 429 desde la primera petición de Web API hecha con la identidad del cliente de escritorio, así que esa identidad nunca se usa para Web API (`auth.rs`).

## 2. librespot y Spotify Connect

- **[Confirmado]** librespot registra el equipo como dispositivo Connect, recibe transferencias y reproduce. Los cambios de estado llegan por *push* (Dealer/cluster). Solo se consulta Web API cuando cambia algo en **otro** dispositivo, con un mínimo de 1 s entre consultas y fusionando cambios. Los cambios de solo posición no generan peticiones.
- **[Confirmado]** La sesión de librespot también lee playlists (rootlist), canciones populares, artistas relacionados, novedades, radio y actividad de amigos mediante endpoints **privados** (Pathfinder `api-partner.spotify.com`, spclient, `/device-auth/v1/refresh`, Login5). Con eso Fastpotify ahorra mucha cuota de Web API.
- **Aviso legal:** librespot es un cliente no oficial obtenido por ingeniería inversa. Se autentica con el Client ID del cliente de escritorio de Spotify (`PLAYBACK_CLIENT_ID` en `auth.rs`) y consume APIs privadas. Ni lo uno ni lo otro está cubierto por los Spotify Developer Terms, y sus respuestas pueden cambiar sin aviso. **No lo he adaptado**: equivale a usar un Client ID compartido y a eludir las restricciones de acceso, que es justo lo que pediste evitar. Tu equivalente legítimo es el **Web Playback SDK**, que ya usas.

## 3. Caché

- **[Confirmado]** `account_cache.rs` guarda instantáneas por cuenta (con versión y `account_id`) y las escribe de forma atómica. Al reiniciar se muestra lo último bueno y se revalida detrás. Si el archivo está corrupto o es de otra cuenta, se ignora.
- **[Confirmado]** Canciones favoritas en caché: sin petición si tienen menos de 15 min; después se refrescan página a página sin ocultar las filas. Letras: 1 mes. Made for you: 10 min en memoria. Novedades por Web API: 1 h. Los artistas se revalidan a los 10 min. Las portadas eligen el tamaño mínimo suficiente y se guardan con presupuesto LRU.
- **[Confirmado]** Se recuerdan las capacidades ausentes: si recomendaciones da 403/404, la estantería se oculta sin mostrar error.

## 4. Qué funciones usan Web API

- **[Confirmado]** Perfil, biblioteca (playlists, favoritos, álbumes, podcasts), búsqueda, páginas de álbum, artista y playlist, edición de playlists y portadas, cola, control de dispositivos remotos (`/me/player`), recomendaciones y, como respaldo, todo lo que la sesión de librespot no consigue resolver.

## 5. Client IDs y autenticación

- **[Confirmado]** Authorization Code + PKCE con listener en loopback (`127.0.0.1:8989`). Desde la versión 0.14.2 solo se admite el Client ID propio del usuario: el compartido (de ncspot) dejó de aceptar inicios de sesión en octubre de 2026. Los grants se guardan en el llavero del sistema, en un hilo dedicado y con marcas de revocación. Si Spotify rechaza el refresh token, se borra y se pide autorización otra vez.
- **[Confirmado]** El token se refresca 90 s antes de caducar, el acceso está serializado con un mutex, y tras un 401 solo se refresca si el token rechazado sigue siendo el actual.
- **[Confirmado]** Antes de abrir el navegador, comprueba el Client ID enviando al token endpoint un código que no puede ser válido: `invalid_client` significa ID desconocido; `invalid_grant`, ID válido.

## 6. 429 y cuotas

- **[Confirmado]** `ApiClient` usa un semáforo de 6 peticiones en primer plano y otro de 4 en segundo plano, con presupuestos separados por grant y por sesión. Un `Retry-After` pausa solo ese grant mediante `cooldown_until`, que se comprueba después de obtener el permiso. Hasta 3 reintentos, con espera máxima de 30 s. Las escrituras en la cola esperan el `Retry-After` completo y conservan su orden. Un 429 con `QUOTA_EXCEEDED` deja de reintentar al momento. Ante un 5xx en GET, un único reintento a los 800 ms. Las mutaciones nunca se repiten tras una respuesta dudosa: cliente sin reintentos ni redirecciones.
- **[Confirmado]** Sondeo adaptativo: 15 s normal, 20 s con reproducción local, 30 s con la ventana oculta y sin reproducción, y 4 s durante 10 s después de una orden.
- **[Confirmado]** Las tareas de fondo se ejecutan en un ámbito `unseen` (variable *task-local*) que no activa el indicador de "Esperando a Spotify".

## 7. Qué se puede adaptar legalmente

Ideas de arquitectura (no son código protegido, y además la licencia es MIT):

| Se puede adaptar | No se adapta |
| --- | --- |
| Estado por eventos en vez de sondeo (con el **evento oficial del SDK**) | librespot con el Client ID de escritorio |
| Prioridad y presupuesto separados para el segundo plano | Endpoints privados (Pathfinder, spclient, Login5, device-auth) |
| Refresco de token compartido; 401 solo con el token actual | Client IDs compartidos de otros proyectos |
| Stale-while-revalidate por cuenta | Enviar tokens a receptores Cast o Connect de terceros |
| `Retry-After` sin acortar y `QUOTA_EXCEEDED` sin reintentos | |
| Recordar capacidades ausentes (403/404) | |

## Qué ya tenía SpotifyMac

Tu `SpotifyRequestManager` ya cubría buena parte: caché persistente por cuenta con TTL, ETag y `Cache-Control`; deduplicación de GET; ritmo de 1,5 s y 12 peticiones por 30 s; bloqueo persistente por 429 y cuota; datos vencidos durante el 429; memoria de endpoints no disponibles; y diagnóstico. En algunos puntos es **más conservador** que Fastpotify: no reintenta 429 y guarda el bloqueo entre reinicios. Lo he mantenido así.

## Cambios implementados (todos [Propuesta], inspirados en lo [Confirmado])

1. **Estado local por eventos.** `WebPlayer.swift` escucha `player_state_changed` del Web Playback SDK y lo convierte en `PlaybackState` (`LocalPlayerState`). Mientras suena en este Mac, `Store` no sondea `GET /me/player`. Tampoco hace las lecturas de confirmación después de reproducir, pausar o saltar: espera el evento hasta 2,5 s y solo si no llega consulta la API. Cuando la reproducción se va a otro dispositivo, hace una única lectura. Es el equivalente oficial a los *pushes* de Dealer que usa Fastpotify.
2. **Prioridades.** El orden de paso es: órdenes de reproducción, después lecturas que espera el usuario, después segundo plano. El segundo plano (`spotifyBackground { … }`, un `@TaskLocal` como `unseen`) cede el turno y no puede ocupar los últimos 4 huecos de la ventana de 30 s. Lo usan los artistas de una playlist, los nombres auxiliares, las fotos de perfil y las recomendaciones por género.
3. **Refresco de token único.** `AuthManager` comparte una sola tarea de refresco entre peticiones simultáneas. Así se evita el doble uso de un refresh token rotado, que acababa en 400 y cierre de sesión. Un 401 solo refresca si el token rechazado sigue siendo el actual. La sesión solo se cierra con `invalid_grant` o `invalid_client`. El margen de refresco pasa de 30 a 60 s.
4. **`Retry-After` con fecha HTTP.** Antes, una fecha se interpretaba como 60 s. Ahora se calcula el tiempo real y nunca se acorta.
5. **Stale-while-revalidate al iniciar.** El nombre y las playlists aparecen al instante desde la copia guardada (aunque haya vencido) y se revalidan detrás. La copia es por cuenta y no gasta peticiones.
6. **Sondeo más lento en segundo plano.** El intervalo se duplica cuando la app no está activa.

Archivos: `SpotifyRequestManager.swift`, `Auth.swift`, `API.swift`, `WebPlayer.swift`, `Store.swift`, pruebas en `Tests/SpotifyMacTests/`. Las copias previas están en `backups/*.before-ratelimit-v2.swift.txt`.

## Recomendaciones pendientes (no implementadas)

- **Validar el Client ID antes de abrir el navegador** con el método de Fastpotify (`invalid_client` frente a `invalid_grant`), que usa el endpoint oficial.
- **Liked Songs incremental:** guardar el `total` de la primera página y volver a descargar las demás solo si cambió.
- **`api.artists(ids)`** sigue haciendo una petición por artista (hasta 16). Se podrían usar los artistas simplificados que ya vienen en las pistas, y pedir las fotos solo de las tarjetas visibles.
