# Dino Launcher

Modpacks y servidor, listos de una vez. Tus amigos abren un ejecutable, inician sesión y juegan —
sin instalar mods, sin buscar la IP, sin configurar nada.

## Cómo está hecho

App de [Tauri v2](https://tauri.app) con la interfaz en HTML/CSS y la lógica en Rust.
Por debajo usa [ElyPrismLauncher](https://github.com/ElyPrismLauncher/Launcher) como motor:
él resuelve autenticación, Java, mod loaders y descarga de mods; Dino Launcher pone la cara,
el catálogo de modpacks y la sincronización.

El motor se descarga de sus releases oficiales y **no se modifica**. Corre siempre con `--dir` apuntando a la carpeta de datos de Dino
Launcher, de modo que no toca ningún Prism que ya tengas instalado.

Los detalles de la orquestación están en [docs/arquitectura.md](docs/arquitectura.md).

## Desarrollo

Requiere [Rust](https://rustup.rs) y Node 18+.

En **Windows** hacen falta además las Build Tools de C++ (Rust compila con el enlazador
de MSVC) y el runtime de WebView2, que ya viene con Windows 11:

```bash
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install --id Rustlang.Rustup -e
```

Quien no quiera esos gigabytes puede saltárselo: `.github/workflows/build.yml`
compila el instalador de Windows en cada push y lo deja como artefacto.

```bash
npm install
npm run dev
```

Para compilar los instaladores de la plataforma actual:

```bash
npm run build
```

Los tests de la lógica del motor no necesitan tener ElyPrism instalado:

```bash
cd src-tauri && cargo test
```

## Estructura

```
src/                interfaz (HTML/CSS/JS, sin framework)
src-tauri/
  src/engine.rs     orquestación de ElyPrism: argumentos e instance.cfg
  src/lib.rs        ventana, vidrio nativo y comandos expuestos a la interfaz
  icons/            generados a partir del sprite del logo
docs/               arquitectura y decisiones
reference/          copia de solo lectura de ElyPrism (gitignorada)
.github/workflows/  tests en macOS y Windows, e instalador de Windows
```

## Publicar una versión

Las descargas viven en **[dino-launcher-releases](https://github.com/dinopolllo/dino-launcher-releases)**,
un repo público aparte. Este es privado, y en un repo privado hasta las releases
devuelven 404: ni el auto-update ni pasarle el link a alguien funcionarían desde aquí.

Publicar son dos pasos:

```bash
npm version 0.2.0 --no-git-tag-version && git commit -am "v0.2.0"
```

```bash
git tag v0.2.0 && git push --tags
```

La etiqueta dispara `.github/workflows/release.yml`, que compila Windows y macOS
—universal, Intel y Apple Silicon en un archivo— y publica ambos instaladores.
Antes de compilar comprueba que la etiqueta coincida con la versión de
`tauri.conf.json`: si no coincidieran, el launcher ofrecería una actualización que al
instalarse sigue diciendo que es la vieja, y avisaría de ella para siempre.

Hace falta un secreto: **`RELEASES_TOKEN`**, un token personal con permiso de escritura
en el repo de descargas. El `GITHUB_TOKEN` normal no alcanza a otros repos.

### Auto-actualización

Es **opcional y se añade encima**. Sin claves de firma el workflow publica igual los
instaladores y omite el `latest.json`; esas versiones se descargan a mano y no se
auto-actualizan. Para activarla:

1. Generar la clave y pegar la **pública** en `src-tauri/tauri.release.conf.json`:
   ```bash
   npm run tauri signer generate -- -w ~/.tauri/dino.key
   ```
2. Añadir `TAURI_SIGNING_PRIVATE_KEY` y `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` como
   secretos.

A partir del siguiente tag, el workflow firma y publica el manifiesto, y los launchers
ya instalados empiezan a verse las versiones nuevas solos.

`createUpdaterArtifacts` y la configuración del updater están en
`tauri.release.conf.json` y no en `tauri.conf.json` porque esa bandera hace fallar la
compilación cuando no hay clave de firma: en el config principal habría tumbado el CI
de cada push.

## Licencia

Dino Launcher es software libre bajo la **GNU General Public License v3.0**: puedes usarlo,
estudiarlo, modificarlo y redistribuirlo con las condiciones de [LICENSE](LICENSE). Quien
reparta una versión, modificada o no, tiene que ofrecer también su código fuente.

Copyright © Yahir. ElyPrismLauncher y Prism Launcher son de sus respectivos autores bajo
GPL-3.0. Dino Launcher no está afiliado ni respaldado por Prism Launcher, ElyPrismLauncher,
Ely.by, Modrinth ni Mojang.
