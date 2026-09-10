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

### El motor en cada sistema

Los nombres salen de leer el directorio central de cada zip del release 11.1.0, no de
suponerlos. Equivocarse aquí no rompe la compilación: descarga el motor entero y recién
entonces falla, que es el peor momento para enterarse.

| | asset | ejecutable dentro |
|---|---|---|
| macOS | `PineconeMC-macOS-11.1.0.zip` | `ElyPrismLauncher.app/Contents/MacOS/elyprismlauncher` |
| Windows x64 | `PineconeMC-Windows-MSVC-Portable-11.1.0.zip` | `elyprismlauncher.exe` |
| Windows arm64 | `PineconeMC-Windows-MSVC-arm64-Portable-11.1.0.zip` | `elyprismlauncher.exe` |
| Linux | `PineconeMC-Linux-<arch>.AppImage` | el AppImage mismo |

El portable de Windows descomprime **sin carpeta contenedora** y trae un `portable.txt`
que le diría al motor que guarde sus datos junto al ejecutable. `--dir` manda por encima,
pero el archivo se borra al instalar: si algún día dejara de mandar, los datos irían a
`bin/` y `accounts.json` quedaría donde nadie lo busca — el launcher pediría iniciar
sesión para siempre sin explicar por qué. Upstream lo documenta como borrable.

### Lo que no se traduce solo

Tres cosas del sistema no tienen equivalente directo y se resuelven aparte en cada uno:

- **Huérfanos.** Unix reparenta a `launchd`/`init` y basta con `ppid == 1`. Windows no
  reparenta nada: el ppid sigue apuntando a un proceso que ya murió, así que el huérfano
  se reconoce porque su padre no está entre los vivos. La decisión de matar o no
  (`is_killable`) es la misma función en los dos; solo cambia quién calcula "huérfano".
- **Rutas en la línea de comandos.** Qt arma el classpath de Java con `/` aunque `--dir`
  vaya con `\`, y el disco de Windows no distingue mayúsculas. Comparar literal contra
  nuestra raíz no reconocería nada nuestro, así que se normaliza antes.
- **Subprocesos.** En una app sin terminal, cada `Command` hacia una consola abre una
  ventana negra encima. La RAM se consulta por API (`GetPhysicallyInstalledSystemMemory`,
  el equivalente de `hw.memsize`) y los procesos se terminan por API; lo único que queda
  en PowerShell es listar procesos con su línea de comandos, con `CREATE_NO_WINDOW`.

### Sin precalentado en Windows

`warm_engine` arranca el motor sin `--launch` para que refresque los tokens mientras el
usuario mira la interfaz. En macOS eso ocurre sin ventana y sin icono en el Dock.

En Windows **no**: corrido contra el release 11.1.0, con la config muda ya escrita y
tanto en la primera corrida como en las siguientes, el motor abre su ventana principal
(«PineconeMC - PineconeMC 11.1.0», con su botón en la barra de tareas). Sin `LSUIElement`
ni equivalente, precalentar dejaría esa ventana encima del launcher toda la sesión:
`QuitAfterGameStop` la mantiene viva hasta que alguien juega. Así que en Windows se
renuncia al precalentado, y el primer Jugar paga el diálogo «Please wait...» de ~2.5 s.

Si algún día ese diálogo molesta lo bastante, la salida sería localizar la ventana del
proceso (`EnumWindows` + `GetWindowThreadProcessId`) y esconderla con `ShowWindow`. No se
hace hoy porque implica un parpadeo mientras aparece y desaparece, que es peor que esperar.

### La ventana sin decoración en Windows

Con `decorations: false` y `transparent: true`, Tauri deja igualmente el marco nativo
puesto: medido sobre la ventana en marcha, `GWL_STYLE` vale `0x15CF0000`, con
`WS_THICKFRAME`, `WS_MAXIMIZEBOX` y `WS_MINIMIZEBOX`. O sea que el borde de
redimensionado, el doble clic en la barra y el encaje con `Win`+flechas o arrastrando
contra un borde funcionan sin escribir nada.

Lo único que no sale gratis son los **Diseños de ajuste** de Windows 11 —el menú que
aparece al mantener el puntero sobre el botón de maximizar—: eso exige responder
`HTMAXBUTTON` a `WM_NCHITTEST` sobre nuestro botón de HTML, y el botón es un `<button>`,
no una zona no-cliente. Se vive sin ello.

El icono de maximizar sigue el estado real de la ventana y no lo que hizo el botón:
`window_is_maximized` se consulta tras cada `resize`. Comprobado maximizando desde fuera
del proceso —con `ShowWindow`— y el icono pasó solo al de restaurar.

### `--dir` gana sobre el modo portable

Comprobado: lanzado como `elyprismlauncher.exe --dir <ruta>` **con `portable.txt` presente**,
el motor escribe sus datos dentro de `--dir` y no toca la carpeta del ejecutable. Aun así
`portable.txt` se borra al instalar: el día que eso cambiara, el fallo sería silencioso y
se manifestaría como un launcher que pide iniciar sesión para siempre.

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

## Cuentas: qué resuelve el motor y qué resolvemos nosotros

El motor sigue siendo el dueño de las cuentas —las guarda, las refresca y las usa para
lanzar—, pero **no tiene ninguna bandera para abrir su diálogo de login** (comprobado
contra `--help` del release 11.1.0: `--dir`, `--launch`, `--server`, `--world`,
`--profile`, `--offline`, `--alive`, `--show-window`, `--import`, `--show`). Sin eso, la
única forma de que alguien iniciara sesión era abrirle el motor entero, que es
exactamente el launcher ajeno que este proyecto existe para no enseñar.

El setup de primera vez pregunta primero y de ahí salen dos caminos distintos:

- **Microsoft** — sigue pasando por el motor: hacerlo aquí dentro exigiría un `client_id`
  de Azure con acceso a la API de Minecraft **aprobado por Mojang**, trámite externo que
  puede negarse. Lo que sí cambia es cuánto se ve. Se lanza con `--launch <instancia>` y
  **nunca** con `--show-window`; medido enumerando las ventanas del proceso, así el motor
  no abre su ventana principal, solo el aviso «No Accounts» y de ahí su página de Cuentas.
  Pasar `--show-window` —que es lo que se hacía antes— era justamente lo que plantaba
  delante el launcher ajeno completo.

  Tras iniciar sesión el motor continúa solo y abre el juego, así que en ese camino **no**
  se le mata: hacerlo abortaría el lanzamiento. Si en cambio alguien cierra el login sin
  entrar, el motor pregunta «Play demo?» —con *Cancelar* por defecto, sin ajuste que lo
  desactive— y se va. No se intenta adivinar ese momento contando ventanas, porque al
  cerrar un diálogo abre el siguiente y el conteo no baja; para cortar antes está el botón
  de cancelar de nuestra ventana, que llama a `close_engine`.

  `--launch` exige que la instancia exista, así que si alguien abre el setup antes de
  instalar el modpack se cae a `--show-window`. `open_engine_login` devuelve cuál de los
  dos caminos consiguió para que la interfaz redacte el aviso en consecuencia.
- **Ely.by** — nativo, en `src-tauri/src/auth.rs`. Código de dispositivo contra
  `account.ely.by`, sin abrir el motor y sin que nadie escriba una contraseña aquí dentro.
  Necesita un `client_id` propio, que en Ely.by es self-service y no lleva `client_secret`
  (por eso puede vivir en el repo). Con `ELY_CLIENT_ID` vacío la rama se enseña
  deshabilitada con el motivo, en vez de fallar a mitad del flujo.

Ely.by **no tiene API de registro**: crear una cuenta pide correo, contraseña y
confirmación en su web. El botón de «crear una» abre `account.ely.by/register` en el
navegador y ahí termina nuestra parte.

### Escribir en `accounts.json` es la parte delicada

`auth.rs` escribe la cuenta en el archivo del motor. El formato está verificado leyendo
`AccountData.cpp` y `AccountList.cpp` del release fijado, y tiene dos trampas que no dan
error, solo se comen la cuenta en silencio:

- `formatVersion` **tiene que ser 3**. Con cualquier otro número el motor renombra el
  archivo a `accounts-old.json` y arranca con la lista vacía, llevándose lo que hubiera.
- `profile` debe traer `id`, `name`, un `skin` con `id`/`url`/`variant` (cadenas, aunque
  sean vacías) y `capes` como array. Si falta cualquiera, `profileFromJSONV3` descarta el
  perfil entero y la cuenta queda inválida: el launcher pediría iniciar sesión para
  siempre sin decir por qué.

Las pruebas de `auth.rs` replican esas reglas de upstream a propósito, para que un perfil
mal formado falle en `cargo test` y no en la máquina de alguien.

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
