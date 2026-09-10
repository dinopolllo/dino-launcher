# Dino Launcher

Modpacks y servidor, listos de una vez. Tus amigos abren un ejecutable, inician sesión y juegan —
sin instalar mods, sin buscar la IP, sin configurar nada.

## Cómo está hecho

App de [Tauri v2](https://tauri.app) con la interfaz en HTML/CSS y la lógica en Rust.
Por debajo usa [ElyPrismLauncher](https://github.com/ElyPrismLauncher/Launcher) como motor:
él resuelve autenticación, Java, mod loaders y descarga de mods; Dino Launcher pone la cara,
el catálogo de modpacks y la sincronización.

El motor se descarga de sus releases oficiales y **no se modifica**, así que su GPL-3 no se
propaga a este código. Corre siempre con `--dir` apuntando a la carpeta de datos de Dino
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

El launcher se actualiza solo: comprueba al abrir y ofrece instalar con un botón. No
se instala sin permiso — reemplazar el ejecutable cierra la app, y hacerlo por sorpresa
sería peor que la molestia que esto evita.

Las descargas viven en un **repo público aparte**, porque este es privado y el updater
pide la URL sin autenticación: contra un release privado recibiría un 404.

Preparación, una sola vez:

1. Crear el repo público de descargas (`dino-launcher-releases`).
2. Generar la clave de firma y pegar la **pública** en `src-tauri/tauri.release.conf.json`:
   ```bash
   npm run tauri signer generate -- -w ~/.tauri/dino.key
   ```
3. Añadir tres secretos al repo privado:
   - `TAURI_SIGNING_PRIVATE_KEY` — el contenido de la clave privada.
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — su contraseña.
   - `RELEASES_TOKEN` — un token personal con permiso de escritura en el repo de
     descargas. El `GITHUB_TOKEN` normal no sirve: no alcanza a otros repos.

Después, cada versión son dos pasos:

```bash
npm version 0.2.0 --no-git-tag-version && git commit -am "v0.2.0"
```

```bash
git tag v0.2.0 && git push --tags
```

La etiqueta dispara `.github/workflows/release.yml`, que compila con
`tauri.release.conf.json`, firma, arma el `latest.json` y publica todo en el repo de
descargas. Antes de compilar comprueba que la etiqueta coincide con la versión de
`tauri.conf.json`: si no coincidieran, el launcher ofrecería una actualización que al
instalarse sigue diciendo que es la vieja, y avisaría de ella para siempre.

Hoy solo se publica el instalador de Windows. Un macOS que consulte el manifiesto no
encuentra su plataforma y sencillamente no ve actualizaciones; añadirlo es sumar un job
que aporte su `.app.tar.gz` firmado al mismo `latest.json`.

## Licencia

Este proyecto es de Yahir. ElyPrismLauncher y Prism Launcher son de sus respectivos autores bajo
GPL-3.0 y se usan sin modificar. Dino Launcher no está afiliado ni respaldado por Prism Launcher,
ElyPrismLauncher, Ely.by ni Mojang.
