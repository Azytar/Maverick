# 🦅 Maverick

<p align="center">
  <img src="https://img.shields.io/badge/Rust-000000?style=for-the-badge&logo=rust&logoColor=white">
  <img src="https://img.shields.io/badge/Linux-111111?style=for-the-badge&logo=linux&logoColor=white">
  <img src="https://img.shields.io/badge/XLibre-222222?style=for-the-badge&logo=x.org&logoColor=white">
  <img src="https://img.shields.io/badge/x11rb_0.13-444444?style=for-the-badge">
</p>

<p align="center">
  <a href="README.md">
    <img src="https://img.shields.io/badge/Language-English-lightgrey?style=for-the-badge&logo=translate&logoColor=white">
  </a>
</p>

<p align="center">
  <b>Gestor de ventanas en mosaico columnar con diseño desplazable tipo niri, escrito en Rust</b>
</p>

<p align="center">
  🦅 columnar • 🦀 rust • 🖥 xlibre • 🧩 tiling • 🌙 minimal
</p>

---

## ✨ Acerca de

**maverick** es un gestor de ventanas en mosaico ligero y columnar escrito en
Rust. Presenta un diseño de columnas desplazables horizontalmente inspirado en
[niri](https://github.com/YaLTeR/niri), y se construye directamente sobre
`x11rb 0.13` para minimizar dependencias y peso.

### Características principales
- 🦅 Diseño de columnas desplazable horizontalmente.
- ⚡ Huella reducida — sin cairo/pango/Xft, sin runtime asíncrono, un único binario estático.
- 🔲 Dos modos de diseño: Column (desplazable, por defecto) y Grid (reescrito como motor puro y determinista).
- 🎨 Compositor OpenGL integrado (opcional, activado por defecto con fallback automático) con detección de daño consciente de oclusión — no hace falta `picom`/`xcompmgr`.
- 🖼️ Fondo de pantalla nativo — una imagen (PNG decodificado sin dependencias; otros formatos vía un conversor externo) o un shader GLSL en vivo, configurable en `config.toml` o al vuelo con `maverick-msg wallpaper …`.
- 💾 Persistencia de sesión — el layout de columnas/workspace y el foco sobreviven a un reinicio en caliente o recarga; nada de estado en tiempo real (geometría, animaciones, estado del compositor) se confía nunca al disco.
- 🔒 Aislamiento por sesión — cada instancia en ejecución tiene su propio directorio/socket de runtime, así una sesión real y una instancia de prueba en Xephyr nunca chocan.
- 🖼 Maximización real (llena el área de trabajo, conserva el borde) además de pantalla completa.
- 🖥 Soporte multi-monitor vía RandR.
- 🧩 Soporte de ventanas flotantes y a pantalla completa.
- 🧱 Soporte de docks/barras externas (Waybar, Polybar, …) vía struts EWMH.
- 🔌 Socket de control `maverickctl` — listar/estado/despachar/reiniciar/recargar/salir de cualquier instancia en ejecución.
- 📐 Altamente configurable (ancho de columna, gaps, bordes, colores, binds de escritorio).
- 🔧 Reglas declarativas de ventanas.
- 🚀 Autostart de programas.
- 📋 Compatible con EWMH.

---

## 🚀 Instalación

### Compilar desde las fuentes

Maverick es un workspace de Cargo: el binario `maverick` (el propio gestor),
`maverickctl` (CLI de control), `maverick-dialog` (el diálogo de
confirmación de salida), `maverick-installer`, y los crates de librería
`maverick-gl` (primitivas GL/GLX del compositor), `maverick-img` (decode de
PNG sin dependencias para el wallpaper) y `maverick-toml` (el parser de
config sin dependencias). Compílalos todos juntos:

```bash
git clone https://github.com/azytar/Maverick.git
cd Maverick
cargo build --release --workspace
```

(`--workspace` es necesario porque el `Cargo.toml` raíz es en sí el paquete
`maverick` — sin él, Cargo solo compila `maverick` y omite los binarios
`maverick-sys`/`maverick-dialog`.)

### Añadir al PATH

```bash
cp target/release/maverick target/release/maverickctl target/release/maverick-dialog ~/.local/bin/
```

`maverick-dialog` solo necesita estar en el `PATH` si quieres que aparezca el
aviso de salida con `Super+Shift+Q`; sin él, `maverickctl` recurre a
`zenity`/`kdialog`/un aviso en TTY.

### Arranque con `.xinitrc`

```bash
exec maverick
```

### Gestor de sesión — `maverick.desktop`

Crea `/usr/share/xsessions/maverick.desktop`:

```ini
[Desktop Entry]
Name=maverick
Comment=Columnar tiling WM
Exec=maverick
Type=XSession
```

---

## 🖥 Opciones de línea de comandos

`maverick` acepta un pequeño conjunto de flags (en cualquier orden):

| Flag | Descripción |
| --- | --- |
| `--config <ruta>` | Carga el TOML de configuración desde `<ruta>` en lugar de `$XDG_CONFIG_HOME/maverick/config.toml`. La misma ruta se reutiliza en `maverickctl reload`, así que una configuración personalizada sobrevive a un reinicio en caliente. |
| `--check-config [ruta]` | Analiza la configuración (la ruta de `--config` si se indica, si no la ubicación por defecto) y sale. Código de salida `0` = limpia (sin avisos ni errores), `1` = se reportaron avisos o errores. Nunca inicia el WM — útil para CI/lint. |
| `--replace` / `-r` | Reemplaza a un WM ya en ejecución, adoptando sus ventanas. |
| `--name <id>` | Nombre de instancia usado para control/identificación (para que `maverickctl` apunte a la instancia correcta). |
| `-v` / `--version` | Imprime la versión y sale. |
| `-h` / `--help` | Imprime el uso y sale. |

Valida una configuración antes de arrancar:

```bash
maverick --check-config ~/.config/maverick/config.toml
maverick --config ~/.config/maverick/config.toml
```

---

## 🔲 Diseños

Maverick trae dos modos de diseño conmutables en tiempo de ejecución.

| Modo | Atajo | Descripción |
| --- | --- | --- |
| **Column** | `Super+T` | Columnas desplazables (por defecto). Cada ventana vive en su propia columna. |
| **Grid** | `Super+G` | Todas las ventanas en una cuadrícula uniforme. |

Cicla por todos los modos con `Super+Space`.

> El diseño se define **por escritorio**, no globalmente — cambiarlo solo reordena el escritorio activo en el monitor seleccionado.

---

## ⌨️ Atajos de teclado

`Super` = tecla Windows (`Mod4`)

### Lanzar

| Atajo | Acción |
| --- | --- |
| `Super+Return` | Abrir terminal (`alacritty`) |
| `Super+P` | Lanzador de apps (`rofi -show drun`) |
| `Super+Shift+P` | Ejecutor de comandos (`rofi -show run`) |

### Operaciones de ventana

| Atajo | Acción |
| --- | --- |
| `Super+Shift+C` | Cerrar ventana enfocada |
| `Super+Shift+Space` | Conmutar flotante |
| `Super+Shift+F` | Conmutar pantalla completa |

### Navegación de foco

| Atajo | Acción |
| --- | --- |
| `Super+H` | Enfocar columna a la izquierda |
| `Super+L` | Enfocar columna a la derecha |
| `Super+K` | Enfocar ventana de arriba (dentro de la columna) |
| `Super+J` | Enfocar ventana de abajo (dentro de la columna) |
| `Super+Tab` | Enfocar monitor siguiente |

### Movimiento de ventanas

| Atajo | Acción |
| --- | --- |
| `Super+Shift+H` | Mover ventana a la izquierda |
| `Super+Shift+L` | Mover ventana a la derecha |
| `Super+Shift+K` | Intercambiar ventana hacia arriba dentro de la columna |
| `Super+Shift+J` | Intercambiar ventana hacia abajo dentro de la columna |
| `Super+Shift+Tab` | Mover ventana al monitor siguiente |

> **Semántica de movimiento:** si la columna enfocada tiene una ventana, `Shift+H/L`
> intercambia toda la columna con su vecina (totalmente reversible). Si la columna
> tiene varias ventanas, la ventana enfocada se extrae a su propia columna adyacente.

### Operaciones de columna

| Atajo | Acción |
| --- | --- |
| `Super+Shift+Return` | Mover ventana a una columna nueva |
| `Super+Ctrl+H` | Encoger columna actual (−50 px) |
| `Super+Ctrl+L` | Agrandar columna actual (+50 px) |
| `Super+Ctrl+J` | Colapsar columna en la de su izquierda |

### Escritorios

| Atajo | Acción |
| --- | --- |
| `Super+1` … `Super+9` | Cambiar al escritorio 1–9 |
| `Super+Shift+1` … `Super+Shift+9` | Mover ventana enfocada al escritorio 1–9 |

### Control del WM

| Atajo | Acción |
| --- | --- |
| `Super+Shift+Q` | Pide confirmación y luego sale de maverick |
| `Super+Shift+R` | Reinicio en caliente en sitio |
| `Super+F5` | Reinicio en caliente en sitio |
| `Super+Space` | Ciclar modos de diseño |
| `Super+T` | Poner diseño Column |
| `Super+G` | Poner diseño Grid |

> `Super+Shift+Q` lanza `maverickctl quit --confirm` (recurre a
> `zenity`/`kdialog`/TTY si `maverick-dialog` no está instalado) para que una
> pulsación accidental no mate la sesión. Todo el WM también es controlable
> desde fuera vía un socket Unix con `maverickctl` — véase
> [Detalles técnicos](#-detalles-técnicos).

### Ratón (ventanas flotantes)

| Acción | Resultado |
| --- | --- |
| `Super+Left-drag` | Mover ventana flotante |
| `Super+Right-drag` | Redimensionar ventana flotante |

---

## 🔧 Configuración

Maverick se configura en **`$XDG_CONFIG_HOME/maverick/config.toml`** (o
`~/.config/maverick/config.toml` cuando `XDG_CONFIG_HOME` no está definido). El
archivo es **totalmente opcional** — si falta, maverick arranca con los valores
compilados por defecto sin quejas. Los campos ausentes recaen en esos valores,
así que solo escribes lo que quieres sobreescribir.

La carga es **a prueba de fallos por diseño**: un archivo con sintaxis inválida
se rechaza por completo y se usan los valores por defecto, mientras que un
valor de tipo incorrecto, un nombre de clave desconocido o una cadena de acción
rota se descartan con un aviso y el resto del archivo se carga igual. Maverick
nunca falla al arrancar por una configuración errónea.

Hay un ejemplo completo y comentado en
[`config/config.toml`](config/config.toml):

```bash
mkdir -p ~/.config/maverick
cp config/config.toml ~/.config/maverick/config.toml
```

```toml
# ~/.config/maverick/config.toml

[general]
border_width = 2
gaps = 6
n_tags = 9

[colors]
normal  = 0x45475a
focused = 0x89b4fa
urgent  = 0xf38ba8

[[keybindings]]
key = "super+return"
action = "spawn:alacritty"

[[keybindings]]
key = "super+shift+q"
action = "kill"

[[rules]]
class = "mpv"
float = true

[autostart]
commands = [["nm-applet"]]
```

Aplica los cambios sin reiniciar:

```bash
maverickctl reload
```

Si prefieres mantener todo compilado, simplemente no crees el archivo — nada
cambia respecto a antes.

### Opciones principales

```rust
border_w:       2,        // grosor del borde en píxeles
gaps:           6,        // separación entre ventanas y bordes de pantalla (px)
n_tags:         9,        // número de escritorios
column_width:   0.6,      // ancho de una columna recién creada, como
                          //   fracción (0.1–1.0) del ancho del área de trabajo
accordion_boost: 0.0,     // factor de expansión de foco para la columna enfocada (0.0–0.9)
overview_zoom_min: 0.25,  // zoom mínimo de la tira Overview (0.05–1.0)
focus_mouse:    false,    // enfocar ventana al entrar el ratón
warp_cursor:    false,    // llevar el cursor al centro de la ventana enfocada
auto_workspace_binds: true, // auto-generar Super+1..9 / Super+Shift+1..9
```

`column_width` es la fracción del área de trabajo que recibe una columna recién
creada (0.1–1.0). Sustituye a las antiguas claves `default_col_w` (píxeles) y
`split_bias`, que ahora son alias obsoletos que se mapean sobre ella.

### Colores

Paleta por defecto: Catppuccin Mocha. Todos los colores son hex de 24 bits `0xRRGGBB`:

```rust
col_normal:  0x45475a,  // borde de ventana sin foco   (Surface1)
col_focused: 0x89b4fa,  // borde de ventana enfocada    (Blue)
col_urgent:  0xf38ba8,  // borde de ventana urgente     (Red)
```

### Nombres de escritorio

```rust
tag_names: (1..=9).map(|n| n.to_string()).collect(),
```

### Autostart

```rust
autostart: vec![
    vec!["/usr/lib/xdg-desktop-portal-gtk"],
    vec!["/usr/lib/xdg-desktop-portal"],
    vec!["polybar", "main"],                     // barra externa
    vec!["alacritty"],
],
```

El compositor y el wallpaper ya no son entradas de autostart — están
integrados al WM y se controlan con `[compositor]`/`[wallpaper]` en
`config.toml` (ver [Compositor y fondo de pantalla](#-compositor-y-fondo-de-pantalla)).
Las barras y portales siguen siéndolo: maverick no las orquesta de forma
especial, simplemente se lanzan una vez que el WM está listo, sin lógica de
orden/retardo configurable — si una herramienta necesita un momento antes de
estar usable, eso depende de ella. (Si preferís usar un compositor externo en
vez del integrado, poné `[compositor] enabled = false` y agregalo a
`autostart` como antes.)

> El `autostart` por defecto también lanza `/usr/lib/xdg-desktop-portal` y
> `/usr/lib/xdg-desktop-portal-gtk` — sin ellos, los selectores de archivos
> basados en GTK/portales (diálogos de subida de navegador, etc.) nunca aparecen.

### Usar una barra externa

maverick **no incluye barra de estado** — dibujarla no es trabajo del WM. Usa
polybar, waybar, eww o similar; el WM reserva espacio en pantalla para cualquier
dock que publique `_NET_WM_STRUT_PARTIAL`/`_NET_WM_STRUT`, así que las ventanas
en mosaico nunca lo solapan (véase `backend/x11/struts.rs`). Lanza tu barra
desde `autostart`:

```rust
autostart: vec![
    vec!["polybar".into(), "main".into()],
    // …
],
```

Para el texto de estado, maverick lee el `WM_NAME` de la ventana raíz (fijado
con `xsetroot -name "…"` o `xsetroot -name "$(date)"`) y lo expone vía
`maverickctl state` / `maverickctl subscribe`, para que una barra o script lo
lean sin rastrear propiedades X.

---

## 🎨 Compositor y fondo de pantalla

Maverick levanta su propio compositor OpenGL/GLX sobre
`CompositeGetOverlayWindow` cuando la pantalla lo soporta — no hace falta
`picom`/`xcompmgr`. Si falta GL, no se puede crear un contexto 3.3, o ya hay
otro compositor dueño de la pantalla, avisa por log y cae en silencio al
camino clásico `ConfigureWindow` (las esquinas redondeadas vía `Shape` de
X11 siguen funcionando sin GL). Se puede desactivar explícitamente con
`[compositor] enabled = false` en `config.toml`, o con la variable de
entorno `MAVERICK_NO_COMPOSITOR`.

El compositor también gestiona un fondo de pantalla nativo, configurado
bajo `[wallpaper]`:

```toml
[wallpaper]
path = "~/Pictures/wallpaper.png"   # o un shader .glsl/.frag
mode = "fill"                       # fill | fit | stretch | center
```

- **Imágenes** (`.png/.jpg/.jpeg/.webp/.avif/.bmp/.qoi/.ppm/.ff`): el PNG se
  decodifica de forma nativa (`maverick-img`, sin dependencias); el resto de
  formatos cae a un conversor externo.
- **Shaders GLSL** (`.glsl/.frag/.vert/.shader/.fs`): se compilan una vez en
  la GPU y se redibujan cada frame, recibiendo `u_time`, `u_resolution` y
  `u_delta_time`.
- `path = null` (por defecto) lo desactiva — sin fondo dibujado por el
  compositor.

Se controla en vivo, sin reiniciar:

```bash
maverick-msg wallpaper set ~/Pictures/wallpaper.png
maverick-msg wallpaper mode fit
maverick-msg wallpaper clear
```

## 💾 Sesiones

El layout de columnas/workspace, los pesos por columna, el workspace activo
y el foco sobreviven a un reinicio en caliente (`maverickctl restart`) o una
recarga de config. Solo se persiste esa topología *lógica* — la geometría,
el estado de animación de cámara/scroll, el estado de zoom/overview, la
caché del layout Grid y el estado del compositor siempre se reconstruyen
desde cero contra las ventanas realmente vivas, nunca se confían al disco.

Como cada instancia en ejecución tiene ahora su propio directorio de
runtime y socket de control (con clave un id de sesión aleatorio por
proceso), una sesión real y una instancia de prueba en `Xephyr` corriendo
al mismo tiempo ya no comparten — ni pelean por — el mismo socket.

---

## 📋 Reglas de ventanas

Las reglas permiten asignar ventanas a escritorios concretos o forzarlas a
flotar automáticamente, coincidiendo por subcadena de WM_CLASS o título. Se
definen con `[[rules]]` en `config.toml` (véase
[Configuración](#-configuración)) o, para la base compilada, en `config.rs`:

```rust
rules: vec![
    Rule { class: Some("xdg-desktop-portal".into()), title: None,                             float: true, ws: None },
    Rule { class: Some("gpick".into()),              title: None,                             float: true, ws: None },
    Rule { class: Some("pinentry".into()),           title: None,                             float: true, ws: None },
    Rule { class: None, title: Some("file upload".into()),    float: true, ws: None },
    Rule { class: None, title: Some("open file".into()),      float: true, ws: None },
    Rule { class: None, title: Some("save file".into()),      float: true, ws: None },
    Rule { class: None, title: Some("qt file dialog".into()), float: true, ws: None },
],

```

**Campos de regla:**

| Campo | Tipo | Descripción |
| --- | --- | --- |
| `class` | `Option<String>` | Coincide con `WM_CLASS` (subcadena, distingue mayúsculas/minúsculas) |
| `title` | `Option<String>` | Coincide con el título de ventana (subcadena, distingue mayúsculas/minúsculas) |
| `float` | `bool` | Forzar modo flotante |
| `ws` | `Option<usize>` | Enviar al índice de escritorio (base 0) |

---

## 🏗 Detalles técnicos

maverick minimiza las capas de abstracción evitando dependencias innecesarias:

* **X11 / XLibre vía `x11rb 0.13`** — bindings de protocolo seguros en tipos, sin libx11. Solo el WM y `maverick-dialog` enlazan `x11rb`; el resto del workspace es `std` puro.
* **Una sola costura de despacho** — `Engine::dispatch(Action) -> Vec<Effect>` es el *único* camino de un atajo o comando IPC a la mutación de estado. `Effect` es un vocabulario semántico (`ArrangeMonitor`, `FocusWindow`, `SetFullscreen`, …); el `execute()` del backend X11 es el único sitio que los convierte en llamadas de protocolo. Un backend no-X11 futuro implementaría `execute()` contra los mismos efectos sin tocar el núcleo.
* **Pantalla completa/maximizar como presentación, no como bloqueo de máquina de estados** — `core/present.rs` reescribe solo el rectángulo de la ventana *enfocada* (pantalla completa → toda la pantalla, maximizar → área de trabajo, ambos con precedencia sobre el diseño simple) y reordena en cada transición de foco, en lugar de bloquear la entrada mientras una ventana está a pantalla completa.
* **Colocación flotante autocalculada** — `manage()` nunca confía en la geometría X bruta que reporta una ventana nueva; las ventanas flotantes se centran sobre la geometría real almacenada del padre transitorio (o el área de trabajo del monitor asignado, para diálogos de portales sin padre real) y se recortan dentro de ella. Solo el ancho/alto vienen de la petición original.
* **Pipeline de estado deseado explícito, un solo dueño de la geometría aplicada** — `State -> layout::arrange -> present::present_into -> DesiredState -> Reconciler -> AppliedState -> X11`. El `Reconciler` (`backend/x11/reconciler.rs`) es el único lugar que decide si hace falta un `configure_window`, reemplazando detección de cambios que antes vivía duplicada en `render`, `manage` y `events`.
* **Detección de daño consciente de oclusión en el compositor** — el compositor rastrea *por qué* un frame está sucio (bitflags `DirtyReason`) y se salta el redibujado de ventanas/regiones totalmente tapadas por una ventana opaca encima, en vez de repintar todo el frame ante cualquier cambio.
* **Plano de control por instancia** — `maverick-sys` da a cada instancia en ejecución una identidad por sesión (un id de sesión aleatorio, independiente de la reutilización de PID) y un protocolo de socket Unix (`ping`/`identify`/`state`/`dispatch`/`restart`/`reload`/`subscribe`/`quit`), cada una en su propio directorio de runtime aislado. `maverickctl` habla con él: `list`, `state`, `msg <acción>`, `subscribe`, `quit[--confirm]`, `quit-all`, `restart`, `reload`, `prune`. Maneja varias instancias en distintos displays/ttys/sesiones sin choques.
* **Capa TOML de configuración opcional** — `userconfig.rs` analiza `config.toml` y lo fusiona campo a campo sobre `config::compiled_config()`; un archivo que falla al analizar se rechaza entero, una entrada errónea se descarta con aviso. `maverickctl reload` lo relee en vivo, sin reiniciar.
* **Struts de docks/barras externas** — Los docks se detectan vía `_NET_WM_WINDOW_TYPE_DOCK`/`_DESKTOP` (nunca por nombre de proceso) y reservan espacio leyendo `_NET_WM_STRUT_PARTIAL`/el legado `_NET_WM_STRUT`, seguidos por monitor y liberados al destruir/desmapear. maverick no incluye barra de estado — usa Waybar/Polybar/eww y deja que el WM reserve espacio para ella.
* **Mapa de clientes `HashMap`** — Búsquedas O(1) por XID.
* **Diseño de columnas O(N)** — Alturas de fila precalculadas en una sola pasada.
* **Detección de monitores RandR** — Contabilidad de área de trabajo correcta por monitor.
* **Soporte EWMH** — Incluye `_NET_WM_STATE`, `_NET_WM_DESKTOP`, `_NET_ACTIVE_WINDOW`, etc.
* **Reinicio basado en `exec`** — Reemplaza el proceso en sitio, evitando condiciones de carrera en el grab de X11.
* **Aislamiento `override_redirect`** — Barras y overlays externos permanecen invisibles para el WM.

---

## 📂 Estructura del proyecto

```text
Maverick/                    # Cargo workspace
├── src/                     # `maverick` — el binario del WM
│   ├── main.rs               punto de entrada, señales, autostart, cableado del plano de control
│   ├── config.rs              config base compilada: Cfg, CompositorCfg, WallpaperCfg, Rule, keybinds
│   ├── userconfig.rs           config.toml opcional: análisis, carga a prueba de fallos, fusión
│   ├── types.rs                modelo de datos central: State, Monitor, Workspace, Column, Client, tipos Grid/Fullscreen
│   ├── log.rs                   logger ligero a stderr
│   ├── core/                    capa de lógica pura — sin X11
│   │   ├── engine.rs              Engine::dispatch(Action) -> Vec<Effect>
│   │   ├── effect.rs               enum Effect (la costura núcleo/backend)
│   │   ├── present.rs               capa de presentación fullscreen/maximize
│   │   ├── layout.rs                 arrange_columns / arrange_grid
│   │   ├── grid.rs                    motor de layout Grid puro (determinista, sin X11/State)
│   │   ├── desired.rs                 DesiredState — el traspaso explícito hacia el Reconciler
│   │   ├── session.rs                  persistencia/recuperación de sesión (guarda/restaura topología)
│   │   ├── wallpaper.rs                 modelo de dominio del wallpaper (fuente/modo, sin tipos GL/X11)
│   │   ├── invariants.rs                 State::check_invariants() chequeos internos de consistencia
│   │   ├── ipc.rs                         state_json / parse_action para el socket de control
│   │   ├── action.rs                       vocabulario unificado de nombre/análisis de Action (TOML + IPC)
│   │   ├── commands.rs                      manejadores de comando por cada Action
│   │   └── tests.rs                          tests unitarios
│   └── backend/                 backend X11 — el único sitio que habla el protocolo
│       ├── atoms.rs               caché de átomos EWMH / ICCCM
│       └── x11/                     el WindowManager en ejecución, dividido por preocupación
│           ├── mod.rs                 WindowManager, bucle de eventos, RandR
│           ├── manage.rs                descubrimiento de ventanas, lectura de propiedades, setup
│           ├── events.rs                 tabla de despacho de eventos X
│           ├── ewmh.rs                    mantenimiento de propiedades EWMH
│           ├── actions.rs                  do_action / execute (ejecuta Effects del núcleo), reload
│           ├── input.rs                     keymap, grabs de teclas, suscripción a cambios XKB
│           ├── pointer.rs                    drag-to-move/resize, click focus
│           ├── render.rs                      recorte de geometría flotante, foco, restack
│           ├── reconciler.rs                   único dueño de "qué hay realmente en X11"
│           ├── struts.rs                        reserva de docks externos
│           ├── compositor.rs                     detección de daño GL + dibujo GPU del wallpaper
│           ├── framesched.rs                      scheduler puro de "necesito frame, cuándo"
│           └── hubevents.rs                        puente de eventos del hub de control
├── maverick-sys/             # libc FFI + identidad por sesión/socket de control/hub/discover
│   └── src/
│       ├── identity.rs         id de sesión por instancia, directorio de runtime aislado, "ficha"
│       ├── control.rs           ControlServer — el protocolo de socket Unix
│       ├── hub.rs                 ControlHub — puente al bucle de eventos del WM
│       ├── discover.rs             list/find/quit instancias (por id de sesión)
│       └── bin/maverickctl.rs       la CLI `maverickctl`
├── maverick-gl/               # contexto GLX + primitivas de textura/shader del compositor
│   └── src/
├── maverick-img/                # decode de PNG sin dependencias para el wallpaper nativo
│   └── src/lib.rs
├── maverick-toml/                # parser de TOML sin dependencias usado por userconfig.rs
│   └── src/lib.rs
├── maverick-dialog/           # ventana X11 autónoma de confirmación de salida
│   └── src/main.rs
├── maverick-installer/         # instalador opcional (miembro del workspace)
│   └── src/main.rs
├── config/
│   └── config.toml            ejemplo de configuración de usuario completo y comentado
├── tests/                      # suite de integración basada en Xephyr + clientes C de prueba
├── CHANGELOG.md
├── Cargo.toml                 # raíz del workspace + el paquete `maverick`
├── Cargo.lock
├── LICENSE
├── README.md
└── README.es.md
```

---

## 📜 Licencia

Licencia GPL-3.0
