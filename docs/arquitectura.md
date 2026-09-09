# Dino Launcher — arquitectura

App de **Tauri v2** (Rust + webview) que usa **ElyPrismLauncher** como motor.
Dino Launcher es la única interfaz que ve el usuario; ElyPrism corre oculto por debajo.

## Por qué no forkeamos

ElyPrism es C++/Qt Widgets: 406 `.cpp`, 438 `.h`, 63 `.ui`, cero QML/JS y sin QtWebEngine.
La interfaz Liquid Glass es HTML/CSS y no se puede portar ahí. Al usarlo como motor en vez de
forkearlo conservamos el diseño y **la GPL-3 se queda con ellos**: no modificamos su código,
lo descargamos de sus releases oficiales y lo invocamos como proceso aparte.

## Contrato de orquestación

Verificado en `launcher/Application.cpp` del upstream:

| Bandera | Uso |
|---|---|
| `-d, --dir <ruta>` | Raíz de datos propia. **Aísla el motor** de cualquier Prism que el usuario ya tenga. |
| `-l, --launch <id>` | Lanza la instancia por ID. |
| `-s, --server <ip>` | Entra directo al servidor. Solo junto con `--launch`. |
| `-a, --profile <perfil>` | Elige la cuenta por nombre de perfil. |
| `-o, --offline <nombre>` | Lanza offline con ese nombre. Solo junto con `--launch`. |
| `-I, --import <url>` | Importa **recursos sueltos** (mods, resourcepacks, mundos). **No sirve para `.mrpack`.** |
| `--alive` | Escribe un archivo de vida al arrancar; sirve para detectar que levantó. |
| `--show-window` | Muestra la ventana de ElyPrism. **La omitimos siempre.** |

Omitir `--show-window` es lo que elimina el parpadeo de ventana ajena: el usuario
le da a Jugar en Dino Launcher y lo siguiente que ve es Minecraft.

### `-I` no importa modpacks

Probado contra el binario 11.1.0: `--import` con un `.mrpack` responde
`Can't Identify ... Ignoring it.` y no crea nada. En `MainWindow::processURLs` esa bandera
enruta solo a `ModPlatform::ResourceType` (mods, resourcepacks, mundos, shaders); el manejo
de `.mrpack` vive en `ImportPage.cpp`, que es un diálogo gráfico.

Por eso **Dino Launcher construye la instancia** (`src-tauri/src/pack.rs`) y el motor solo la
lanza. Nos conviene: controlar la instalación es lo que permite respetar los archivos del
jugador entre actualizaciones, cosa que el importador del motor no haría.

## Instancias

Cada instancia es una carpeta con `instance.cfg` (INI) y `mmc-pack.json`.

`mmc-pack.json` es `formatVersion: 1` y lista componentes por `uid`
(`PackProfile.cpp`). Las dependencias del `.mrpack` se traducen así:
`minecraft` → `net.minecraft`, `fabric-loader` → `net.fabricmc.fabric-loader`,
`quilt-loader` → `org.quiltmc.quilt-loader`, `forge` → `net.minecraftforge`,
`neoforge` → `net.neoforged`.
Claves de `instance.cfg` que Dino Launcher escribe desde el catálogo:

```ini
OverrideMemory=true
MinMemAlloc=2048
MaxMemAlloc=6144
OverrideJavaArgs=true
JvmArgs=-XX:+UseG1GC -XX:MaxGCPauseMillis=50 ...
JoinServerOnLaunch=true
JoinServerOnLaunchAddress=mc.dinolab.gg
ManagedPackID=dino-smp
ManagedPackVersionID=4.3.1
```

El binario de macOS es universal (x86_64 + arm64), firmado *adhoc* y sin TeamIdentifier:
habrá que quitarle la cuarentena al descargarlo o Gatekeeper lo bloquea.

`ManagedPack*` es de upstream y encaja con nuestro catálogo: guarda de qué pack y versión
salió la instancia, así que la detección de actualizaciones sale casi gratis.

### seed vs managed

- **managed** — `mods/`, `config/` del admin: los reescribe cada `.mrpack` nuevo.
- **seed** — `options.txt`, controles, `saves/`: solo en la primera instalación.

Los `overrides/` del `.mrpack` son *managed* por definición. Los archivos *seed* se copian
una única vez y después se respetan; ese es el trabajo propio de Dino Launcher.

## Cuentas

ElyPrism trae **Microsoft y Ely.by**, ambos con flujo de código de dispositivo
(`ElyDeviceCodeStep`, `ElyYggdrasilTokenStep`). Microsoft es el predeterminado en la interfaz
y Ely.by la alternativa.

`ElyPatchTask` inyecta los parches oficiales de authlib de Ely.by: **las skins y capas se ven
en cualquier servidor sin plugin ni mod**. Eso elimina CustomSkinLoader, la API propia de skins
y el hosting de texturas que estaban en el plan anterior.

## Distribución

Catálogo en GitHub (`catalog.json` + los `.mrpack` como release assets). El launcher lo consulta
al arrancar; el panel de admin publica por la API de GitHub. Sin costo de infraestructura.

## Disposición en disco

```
<datos de la app>/
  engine/          <- ElyPrism con --dir apuntando aquí (descargado, nunca modificado)
    instances/
  packs/           <- .mrpack cacheados
  catalog.json     <- última copia del catálogo
```
