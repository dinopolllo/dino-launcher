# Contratos de Dino 2.0 (etapa 3)

Fecha: 2026-11-23 · Estado: **revisado con la sesión de diseño** el mismo día (cruzado con lo que
lee `src/index.html` en `windows-port`, 3e16a5e). Sus 12 puntos están aplicados; ver «Revisión».

La fuente de verdad son los tipos de `crates/dino-core` (`schema`, `events`, `error`, `api`).
Este documento los resume y dice qué pasa con cada comando de 1.x.

## Reglas

- Cada comando devuelve `Result<T, DinoError>`. `DinoError` lleva `code` (estable),
  `retryable`, `action` (botón que ofrecer) y `detail` (solo para el registro).
- **La interfaz redacta los textos.** El núcleo no manda frases en español: manda códigos
  y fases. (En 1.x el progreso llegaba como `{paso: "Descargando mods · 3 de 120", frac}`.)
- **Un solo canal de eventos**, `dino://event`, con `type` como etiqueta. Nada de sondeo:
  `session_state` cada segundo desaparece.
- Los nombres de comandos van en `snake_case` y agrupados por prefijo.

## Comandos

| 2.0 | Recibe | Devuelve | 1.x |
| --- | --- | --- | --- |
| `app_info` | — | `AppInfo` | `app_paths`, `window_glass`, `memory_info` |
| `window_minimize`, `window_toggle_maximize`, `window_is_maximized`, `window_close` | — | igual que 1.x | igual |
| `library_list` | — | `Vec<InstanceView>` | `list_packs`, `get_pack`, `pack_state` |
| `library_select` | `id` | — | `select_pack` |
| `instance_details` | `id` | `InstanceDetails` (`path`, `worlds`, `disk_bytes`, `mods`, `loader`, `minecraft`) | `instance_info` |
| `library_remove` | `id`, `delete_files` | — | `remove_pack` |
| `catalog_lookup` | `code` (o link) | `PackPreview` | `catalog_lookup` |
| `catalog_add` | `code` | `InstanceView` | `catalog_install` |
| `mrpack_inspect` | `path` | `PackPreview` | `inspect_mrpack` |
| `mrpack_import` | `path`, `server?` | `InstanceView` | `set_pack` + `play{install_only}` |
| `play` | `PlayRequest` | `PlayOutcome` | `play` |
| `play_cancel` | — | — | `abort_launch` |
| `game_stop` | — | — | `stop_game` |
| `instance_settings_get` | `id` | `InstanceSettings` (con `available_java`) | `memory_info`, `java_runtimes` |
| `instance_settings_set` | `id`, `memory`, `jvm_args`, `java` | — | `set_pack_memory` (y los args JVM, que en 1.x no se aplicaban) |
| `instance_verify` | `id` | — | botón «Verificar archivos» (`play{install_only}`) |
| `open_folder` | `target`: `instance` \| `logs` \| `data` | — | `open_folder` (que recibía una ruta cualquiera) |
| `server_status` | `addr` | `ServerStatus` | `server_status` |
| `accounts_list` | — | `Vec<AccountView>` | `active_account` |
| `account_select` | `uuid` | — | (no existía) |
| `account_offline_check` | `name` | `Option<String>` (aviso) | `offline_check` |
| `account_offline_create` | `name` | `AccountView` | `offline_create` |
| `account_microsoft_add` | — | `MicrosoftLoginView { only_microsoft_window }`; al terminar llegan `AccountsChanged` y `MicrosoftLogin { account }` (`null` = se cerró sin entrar) | `open_engine_login` |
| `account_remove` | `uuid` | — | (no existía) |
| `update_check`, `update_install` | — | igual que 1.x | igual |
| `invite_pending` | — | `Option<String>` | `invitacion_pendiente` |
| `admin_unlocked`, `admin_unlock`, `admin_read`, `admin_published`, `admin_publish` | igual que 1.x | igual | igual |
| `diagnostics_export` | — | ruta del zip | (no existía) |

**Desaparecen** porque solo existían para vigilar el motor desde fuera:
`engine_windows`, `engine_question`, `close_engine`, `session_state` (pasa a evento) y
`boot_ping` (pasa al registro con `tracing`).

`open_folder` deja de aceptar una ruta arbitraria: abre solo carpetas que el núcleo conoce.

## Eventos (`dino://event`)

| `type` | Campos | Sustituye a |
| --- | --- | --- |
| `progress` | `task`, `instance`, `task_bytes_done/total`, `phase`, `files_done/total`, `bytes_done/total`, `finished` | evento `install` |
| `session` | `state`: `idle` \| `preparing` \| `playing` \| `stopping`, `instance` | sondeo de `session_state` |
| `library_changed` | — | recargar a mano tras cada cambio |
| `accounts_changed` | — | — |
| `microsoft_login` | `account`: nombre de la cuenta con la que se entró (ya activa), o `null` si se cerró sin entrar | adivinarlo mirando la cuenta activa |
| `invite` | `code` | evento `invitacion` |
| `update_available` | `version`, `notes` | evento `update` |

Fases de `progress`: `checking_updates`, `downloading_pack`, `reading_versions`,
`downloading_libraries`, `downloading_assets`, `installing_java`, `preparing_loader`,
`downloading_mods`, `verifying_files`, `renewing_session`, `launching`, `done`.

La barra lee `task_bytes_done / task_bytes_total`, el total de la tarea, que nunca
retrocede aunque cambie la fase o corran varias a la vez. La fase y sus `files_*`/`bytes_*`
son para el texto. **Un total de 0 significa «no se sabe»**: barra indeterminada.

## Códigos de error

`offline`, `download_failed`, `hash_mismatch`, `disk_full`, `disk_permission`,
`pack_not_found`, `pack_invalid`, `unsupported_loader`, `java_unavailable`,
`account_required`, `session_expired`, `microsoft_rejected`, `game_crashed`, `busy`,
`not_installed`, `update_failed`, `internal`.

`instance_details` con una instancia sin instalar devuelve `not_installed`.

Acciones que la interfaz puede ofrecer: `retry`, `free_space`, `choose_folder`, `sign_in`,
`renew_session`, `open_logs`, `none`.

## Archivos en disco

Ver `schema.rs`. Todos llevan `schema`, se escriben de forma atómica y no se sobrescriben
si son de una versión más nueva.

| Archivo | Sustituye a |
| --- | --- |
| `config.json` (`download_concurrency`, `download_limit`, `close_ui_while_playing`, `update_channel`, `admin_unlocked`) | marca `admin` |
| `library.json` | `packs.json` (y el `pack.json` antiguo) |
| `accounts.json` (sin secretos) | lectura del `accounts.json` del motor |
| `instances/<id>/instance.json` | `instance.cfg` + `mmc-pack.json` + `dino-archivos.json` |
| `migration-v1.json` | — (diario de la migración, permite volver a 1.x) |
| `cache/files/<aa>/<sha1>` | — (caché compartida de mods entre packs, enlaces duros) |

## Migración

`dino_core::migrate`: plan puro → `apply` (mueve, no copia, con diario) → `rollback`.
Probado en seco con los datos reales de 1.x el 2026-11-23: 3 instancias, 4 cuentas, 4,5 GB
a mover, sin avisos.

## Cambios de forma frente a 1.x que la interfaz tiene que adaptar

| Dato | 1.x | 2.0 |
| --- | --- | --- |
| `loader` | texto (`"fabric 0.19.5"`) | objeto `{kind, version}`; el texto lo redacta la interfaz |
| `icon` | data URL | URL lista para `<img src>` (protocolo propio `dino-icon`): `http://dino-icon.localhost/<id>` en Windows, `dino-icon://localhost/<id>` en macOS y Linux; la construye el núcleo (`icon_url`), sin `convertFileSrc` ni scope |
| `AccountView.skin` | — | URL del PNG de la skin de ese nombre por el mismo protocolo (`…/skin/<nombre>`); 404 si el nombre no tiene skin propia. El núcleo la busca en Mojang y la guarda un día en `cache/skins/` |
| Tamaño en la vista previa | `bytes`, ambiguo | `pack_bytes` (el archivo) y `download_bytes` (lo que se baja; `None` = no se sabe) |
| «No se sabe» | `0` | `None` en todos los datos de comandos (también `PrimaryAction.bytes`); `0` solo en los totales de `progress` |
| `ServerStatus` | `players`, `max_players` | igual (se mantienen los nombres de 1.x) |
| Mensajes de error | texto bajo el aviso (`.alert-raw`) | `code` + `action`; `detail` va a una revelación «Detalles» |

## Revisión de la sesión de diseño (2026-11-23)

| # | Punto | Resolución |
| --- | --- | --- |
| 1 | `PackPreview` sin `code`, `icon`, `server` | Añadidos, opcionales (un archivo local no tiene código) |
| 2 | `InstanceView` sin `mods` ni tamaño en disco | Añadidos `mods` y `disk_bytes` |
| 3 | `instance_info` sin equivalente (incluye `worlds` para el diálogo de quitar) | Nuevo `instance_details(id)` |
| 4 | La barra retrocedía con bytes por fase | `task_bytes_done/total`, monótono |
| 5 | `ServerStatus` cambiaba nombres | Se mantienen `players` y `max_players` |
| 6 | `loader` pasa a objeto | Aceptado y anotado |
| 7 | `icon` como ruta exigía scope | URL de protocolo propio, sin scope |
| 8 | `bytes` ambiguo | `pack_bytes` + `download_bytes` |
| 9 | Total 0 | = «no se sabe», escrito en el contrato |
| 10 | Verificar no es descargar | Fase `verifying_files` |
| 11 | Sin lista de Javas | `available_java` en `InstanceSettings` |
| 12 | `detail` en «Detalles» | Cosa de la interfaz; de acuerdo |

### Segunda pasada (2026-11-23)

| # | Punto | Resolución |
| --- | --- | --- |
| 1 | En WebView2 un protocolo propio se sirve como `http://<esquema>.localhost` | `icon_url` construye la URL según la plataforma, con test de las dos formas |
| 2 | «No se sabe» escrito como `0` y como `None` | `PrimaryAction.bytes` pasa a `Option<u64>` |
| 3 | `JavaOption.origin` era texto | `enum JavaOrigin { mojang, system }` |
| 4 | `instance_details` sin instalar | Devuelve `not_installed` (código nuevo) |

## Tweaks de UX (2026-11-24)

Todo lo nuevo es **opcional y apagado por omisión**: un pack que no publica nada se comporta
igual que antes.

### Comandos nuevos o cambiados

| 2.0 | Recibe | Devuelve | Nota |
| --- | --- | --- | --- |
| `instance_settings_set` | `id`, `patch: {memory?, jvm_args?, java?, window?}` | — | **Cambia**: ahora es un parche y se guarda en el **perfil del jugador activo** (`profiles.json`), no en la instancia. Sin cuenta, escribe la instancia como en 1.x |
| `instance_settings_get` | `id` | `InstanceSettings` | Devuelve lo que **vale** para el jugador activo, más `*_from` (`profile` \| `pack`), `window` y `profile` (nombre) |
| `instance_settings_reset` | `id` | — | Borra lo que el jugador activo eligió para esa instancia |
| `profile_get` | — | `ProfileView` | `raw` (lo elegido, `null` = sin tocar), `prefs` (resuelto con valores de fábrica), `window` |
| `profile_set` | `launcher: LauncherPrefs`, `window?` | `ProfileView` | Reemplaza; `account_required` si no hay cuenta |
| `profile_reset` | — | `ProfileView` | Deja al jugador como uno nuevo |
| `account_microsoft_cancel` | — | — | Cancela el login en curso y cierra su motor |

`InstanceView` gana `theme`, `notes` y `notes_version` (ya saneados). `Config` gana
`discord_client_id`.

### Archivos nuevos

| Archivo | Qué es |
| --- | --- |
| `profiles.json` | Por UUID de cuenta: `launcher` (cabeza, sonidos, volumen, temas, Discord), `window` y `instances/<id>` (memoria, JVM, Java, ventana). Todo `Option`: `None` = «no lo ha tocado» |
| `packs/<id>.extras.json` | Copia saneada de lo que publicó el pack (Discord, notas, tema) y de qué versión, para usarlo sin red |

Resolución al lanzar (`dino_core::profile::resolve`): lo que ese jugador eligió para esa
instancia → lo que eligió para todas (solo ventana) → lo de `instance.json` → automático.
Un jugador nuevo tiene un perfil vacío y juega con 3 y 4.

### Catálogo: extras de un pack

`PackRemoto` acepta, además de lo de siempre, tres bloques opcionales (`presence`, `theme`,
`notes`; ver `dino_core::pack_meta`). El catálogo es un archivo remoto, así que todo pasa
por `sanitized()`: colores solo `#hex`, enlaces solo `https`, textos acotados. Lo
desactivado se descarta. En el editor, los archivos elegidos en el equipo (logo, imágenes de
las notas, banner, sonidos) viajan como `data:` y `publicar` los sube a `packs/<id>-<hash>.<ext>`
y los cambia por su enlace fijado a commit.

- **Discord** (`presence`): `details`/`state` con `{pack} {server} {version} {minecraft} {player}`,
  logo (`https` o clave de asset), botón de unirse (sin `url` apunta a la invitación del pack),
  `client_id` propio. La aplicación sale de `presence.client_id`, `Config.discord_client_id` o
  `DINO_DISCORD_CLIENT_ID` al compilar; **sin ninguna no se manda nada**. Se pone al abrirse la
  ventana del juego y se quita al terminar. El jugador la puede apagar en su perfil.
- **Notas** (`notes`): `title`, `summary`, `sections[{kind: feature|change|fix|note, title, items}]`,
  `images[{url, caption}]`. La interfaz las enseña antes de actualizar (y desde la píldora de la ficha).
- **Tema** (`theme`): `colors` (`primary accent deep ink surface border text`), `sounds`
  (`click hover done play`: preset `soft|blip|chime|void` o `https`, más `volume`) y `decor`
  (`banner badge frame tagline`).

### Interfaz (`ui/modules/`)

Cada pieza es un archivo sin dependencias de las demás: `perf.js` (qué corre y cuándo),
`head.js` (cabeza que mira al cursor), `sfx.js`, `theme.js`, `notes.js` y `pubextras.js`
(formulario del editor). `Dino.perf` pausa todo lo no esencial con la ventana sin foco,
mientras se abre el juego (`launching`) y durante la partida (`playing`/`stopping`, donde además
se congelan las animaciones CSS), y lo reanuda al volver a `idle`.

### Login de Microsoft sin destellos

El motor sigue siendo imprescindible (su `client_id` es el único aprobado), pero ya no se le ve
pasar: `msa_window::Concealer` pone a cada ventana nueva del motor opacidad 0, estilo de
herramienta (sin botón en la barra) y fuera de pantalla **antes** de que Qt la muestre; solo la de
Microsoft se devuelve a la normalidad, ya centrada. Un solo intento a la vez: reintentar, cancelar
(`account_microsoft_cancel`), pasar 10 minutos o salir de la app cierran el motor del intento.
