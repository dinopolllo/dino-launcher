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

## Licencia

Este proyecto es de Yahir. ElyPrismLauncher y Prism Launcher son de sus respectivos autores bajo
GPL-3.0 y se usan sin modificar. Dino Launcher no está afiliado ni respaldado por Prism Launcher,
ElyPrismLauncher, Ely.by ni Mojang.
