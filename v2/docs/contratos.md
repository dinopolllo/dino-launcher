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
| `account_microsoft_add` | — | — (llega `AccountsChanged`) | `open_engine_login` |
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
`update_failed`, `internal`.

Acciones que la interfaz puede ofrecer: `retry`, `free_space`, `choose_folder`, `sign_in`,
`renew_session`, `open_logs`, `none`.

## Archivos en disco

Ver `schema.rs`. Todos llevan `schema`, se escriben de forma atómica y no se sobrescriben
si son de una versión más nueva.

| Archivo | Sustituye a |
| --- | --- |
| `config.json` | marca `admin` |
| `library.json` | `packs.json` (y el `pack.json` antiguo) |
| `accounts.json` (sin secretos) | lectura del `accounts.json` del motor |
| `instances/<id>/instance.json` | `instance.cfg` + `mmc-pack.json` + `dino-archivos.json` |
| `migration-v1.json` | — (diario de la migración, permite volver a 1.x) |

## Migración

`dino_core::migrate`: plan puro → `apply` (mueve, no copia, con diario) → `rollback`.
Probado en seco con los datos reales de 1.x el 2026-11-23: 3 instancias, 4 cuentas, 4,5 GB
a mover, sin avisos.

## Cambios de forma frente a 1.x que la interfaz tiene que adaptar

| Dato | 1.x | 2.0 |
| --- | --- | --- |
| `loader` | texto (`"fabric 0.19.5"`) | objeto `{kind, version}`; el texto lo redacta la interfaz |
| `icon` | data URL | URL lista para `<img src>` (protocolo propio de `dino-app`): sin `convertFileSrc` ni scope |
| Tamaño en la vista previa | `bytes`, ambiguo | `pack_bytes` (el archivo) y `download_bytes` (lo que se baja; `None` = no se sabe) |
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
