# ADR 001 — Usar app-lib de Modrinth por piezas, no como dependencia

Fecha: 2026-11-23 · Estado: propuesta (etapa 2)

## Contexto

Dino 2.0 pasa a GPL-3.0-only, así que puede reutilizar código de Modrinth App
(`packages/app-lib` en `modrinth/code`, GPL-3.0-only). La pregunta es si usarla entera
como dependencia o adaptar sus piezas.

## Hallazgos (revisado `modrinth/code@f7df182`)

- 85.475 líneas de Rust y unas 80 dependencias directas (`sqlx`/SQLite, `image`, `zbus`,
  Discord, etc.).
- `build.rs` compila un agente Java con Gradle y exige 7 URLs de Modrinth en `.env`.
- Todo pasa por `State::get()` y su base SQLite: no hay funciones sueltas que llamar.
- El núcleo de lanzamiento (`src/launcher/`) son 3.490 líneas y es lo que Dino necesita.
- Los perfiles de NeoForge salen de `launcher-meta.modrinth.com`, ya preprocesados.
- El login de Microsoft está ligado al `client_id` de Modrinth: no se puede reutilizar.

## Decisión

Adaptar piezas, citando el origen en cada módulo:

- fusión de perfiles de loader con la versión de Mojang (`merge_partial_version`);
- ejecución de los *processors* de NeoForge (`install_minecraft_inner`);
- el formato de perfiles de launcher-meta.

Todo lo demás (descargas, Java, assets, `.mrpack`, lanzamiento) se escribe propio y pequeño
en `dino-download` y `dino-minecraft`.

## Consecuencias

- Dino no arrastra SQLite, Gradle ni la marca de Modrinth.
- Dependencia de servicio: los perfiles de NeoForge vienen de launcher-meta de Modrinth.
  Si un día no está disponible, la alternativa es ejecutar el instalador oficial de
  NeoForge (`--install-client`) y leer su `version.json`.
- Cada pieza adaptada lleva su atribución; el workspace entero es GPL-3.0-only.
