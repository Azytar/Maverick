# 🦅 Maverick

<p align="center">
  <img src="https://img.shields.io/badge/Rust-000000?style=for-the-badge&logo=rust&logoColor=white">
  <img src="https://img.shields.io/badge/Linux-111111?style=for-the-badge&logo=linux&logoColor=white">
  <img src="https://img.shields.io/badge/XLibre-222222?style=for-the-badge&logo=x.org&logoColor=white">
  <img src="https://img.shields.io/badge/x11rb_0.13-444444?style=for-the-badge">
</p>

<p align="center">
  <a href="README.md">
    <img src="https://img.shields.io/badge/Language-English-blue?style=for-the-badge&logo=translate&logoColor=white">
  </a>
</p>

<p align="center">
  <b>Gestor de ventanas de mosaico para X11 basado en un layout de columnas desplazables.</b>
</p>

<p align="center">
  🦅 columnar • 🦀 rust • 🖥 xlibre • 🧩 tiling • 🌙 minimal
</p>

---

## ✨ Acerca de

**maverick** es un gestor de ventanas de mosaico para X11 basado en un layout de columnas desplazables, inspirado en [niri](https://github.com/YaLTeR/niri).
Escrito íntegramente en Rust usando `x11rb 0.13` — sin Cairo, sin Pango, sin dependencias pesadas.

### Características Principales
- 🦅 Columnas desplazables horizontalmente (estilo niri).
- ⚡ Consumo ligero — sin cairo/pango/Xft, sin runtime async, binario estático único.
- 🔲 Dos modos de layout: Column (desplazable, por defecto) & Grid.
- 🖼 Maximizar de verdad (llena el área de trabajo, conserva el borde) además de pantalla completa.
- 🖥 Multi-monitor real vía RandR.
- 🧩 Soporte de ventanas flotantes y pantalla completa.
- 🧱 Soporte de docks/barras externas (Waybar, Polybar, …) vía struts EWMH.
- 🔌 Socket de control `maverickctl` — list/state/dispatch/restart/reload/quit sobre cualquier instancia corriendo.
- 📐 Gaps, bordes y split bias configurables.
- 🔧 Reglas de ventanas declarativas.
- 🚀 Autostart de programas.
- 📋 Compatible con EWMH.

---

## 🚀 Instalación

### Compilar desde fuente

Maverick es un workspace de Cargo con tres binarios: `maverick` (el WM),
`maverickctl` (CLI de control) y `maverick-dialog` (el diálogo de
confirmación al salir). Compílalos todos juntos:

```bash
git clone https://github.com/azytar/Maverick.git
cd Maverick
cargo build --release --workspace
```

(`--workspace` es necesario porque el `Cargo.toml` raíz ES el paquete
`maverick` — sin esa bandera, Cargo solo compila `maverick` y se salta
los binarios de `maverick-sys`/`maverick-dialog`.)

### Añadir al PATH

```bash
cp target/release/maverick target/release/maverickctl target/release/maverick-dialog ~/.local/bin/
```

`maverick-dialog` solo hace falta en el `PATH` si quieres que aparezca
el diálogo de confirmación de `Super+Shift+Q`; sin él, `maverickctl`
recurre a `zenity`/`kdialog`/un prompt de terminal.

### Iniciar con `.xinitrc`

```bash
//.xinitrc
exec maverick
```

### Display manager — `maverick.desktop`

Crear `/usr/share/xsessions/maverick.desktop`:

```ini
[Desktop Entry]
Name=maverick
Comment=Columnar tiling WM
Exec=maverick
Type=XSession
```

---

## 🔲 Layouts

maverick incluye dos modos de layout intercambiables en tiempo de ejecución:

| Modo     | Atajo         | Descripción                                                                |
| -------- | ------------- | -------------------------------------------------------------------------- |
| **Column**  | `Super+T`     | Columnas desplazables. Cada ventana vive en su propia columna por defecto. |
| **Grid**    | `Super+G`     | Todas las ventanas en rejilla uniforme.                                    |

Ciclar entre los dos: `Super+Space`.

> El layout se establece **por workspace**, no globalmente — cambiarlo solo reorganiza el workspace activo del monitor seleccionado.

---

## ⌨️ Atajos

`Super` = tecla Windows (`Mod4`)

### Lanzar programas

| Atajo                   | Acción                          |
| ----------------------- | ------------------------------- |
| `Super+Return`          | Abrir terminal (`alacritty`)    |
| `Super+P`               | Lanzador de apps (`rofi -show drun`) |
| `Super+Shift+P`         | Ejecutar comando (`rofi -show run`)  |

### Ventanas

| Atajo                    | Acción                       |
| ------------------------ | ---------------------------- |
| `Super+Shift+C`          | Cerrar ventana enfocada      |
| `Super+Shift+Space`      | Alternar flotante            |
| `Super+Shift+F`          | Alternar pantalla completa   |

### Foco

| Atajo           | Acción                                     |
| --------------- | ------------------------------------------ |
| `Super+H`       | Foco a la columna izquierda                |
| `Super+L`       | Foco a la columna derecha                  |
| `Super+K`       | Foco a la ventana de arriba (dentro de columna) |
| `Super+J`       | Foco a la ventana de abajo (dentro de columna)  |
| `Super+Tab`     | Foco al siguiente monitor                  |

### Mover ventanas

| Atajo                  | Acción                                          |
| ---------------------- | ----------------------------------------------- |
| `Super+Shift+H`        | Mover ventana a la izquierda                    |
| `Super+Shift+L`        | Mover ventana a la derecha                      |
| `Super+Shift+K`        | Intercambiar ventana hacia arriba (dentro de columna) |
| `Super+Shift+J`        | Intercambiar ventana hacia abajo (dentro de columna)  |
| `Super+Shift+Tab`      | Mover ventana al siguiente monitor              |

> **Semántica de movimiento:** si la columna tiene una sola ventana, `Shift+H/L` intercambia la columna entera con su vecina (100% reversible). Si tiene varias ventanas, extrae la ventana enfocada a su propia columna adyacente.

### Columnas

| Atajo                    | Acción                                |
| ------------------------ | ------------------------------------- |
| `Super+Shift+Return`     | Mover ventana a una nueva columna     |
| `Super+Ctrl+H`           | Reducir columna (−50 px)              |
| `Super+Ctrl+L`           | Ampliar columna (+50 px)              |
| `Super+Ctrl+J`           | Colapsar columna en la de su izquierda|

### Workspaces

| Atajo                              | Acción                           |
| ---------------------------------- | -------------------------------- |
| `Super+1` … `Super+9`              | Ir al workspace 1–9              |
| `Super+Shift+1` … `Super+Shift+9`  | Mover ventana al workspace 1–9   |

### WM (Control del Gestor)

| Atajo                    | Acción                            |
| ------------------------ | --------------------------------- |
| `Super+Shift+Q`          | Pide confirmación y luego sale de maverick |
| `Super+Shift+R`          | Reiniciar maverick en caliente    |
| `Super+F5`               | Reiniciar maverick en caliente    |
| `Super+Space`            | Ciclar modos de layout            |
| `Super+T`                | Establecer layout Column          |
| `Super+G`                | Establecer layout Grid            |

> `Super+Shift+Q` lanza `maverickctl quit --confirm` (recurre a `zenity`/`kdialog`/terminal si `maverick-dialog` no está instalado), así una tecla apretada por error no puede matar la sesión. Todo el WM también es controlable desde fuera por un socket Unix vía `maverickctl` — ver [Detalles Técnicos](#-detalles-técnicos).

### Ratón (ventanas flotantes)

| Acción                              | Resultado                  |
| ----------------------------------- | -------------------------- |
| `Super+Arrastrar botón izquierdo`   | Mover ventana flotante     |
| `Super+Arrastrar botón derecho`     | Redimensionar ventana flotante |

---

## 🔧 Configuración

Maverick se configura en **`$XDG_CONFIG_HOME/maverick/config.toml`** (o
`~/.config/maverick/config.toml` si `XDG_CONFIG_HOME` no está definida). El
archivo es **completamente opcional** — si falta, maverick arranca con los
valores por defecto compilados, sin quejarse. Los campos ausentes caen a esos
valores, así que solo escribes lo que quieras cambiar.

La carga es **a prueba de fallos por diseño**: un archivo con sintaxis
inválida se rechaza completo y se usan los valores compilados; un valor o
entrada individual — tipo equivocado, clave desconocida o acción mal escrita —
se descarta con una advertencia y el resto del
archivo sigue cargando igual. Maverick nunca deja de arrancar por culpa de una
config mala.

Hay un ejemplo completo y comentado en [`examples/config.toml`](examples/config.toml):

```bash
mkdir -p ~/.config/maverick
cp examples/config.toml ~/.config/maverick/config.toml
```

Aplica cambios sin reiniciar:

```bash
maverickctl reload
```

Si prefieres quedarte con todo compilado, simplemente no crees el archivo —
nada cambia respecto a antes.

### Opciones principales

```rust
border_w:      2,      // ancho del borde en píxeles
gaps:          6,      // espacio entre ventanas y bordes de pantalla (px)
n_tags:        9,      // número de workspaces
default_col_w: 700,    // ancho por defecto de una columna nueva (px)
split_bias:    0.6,    // cuánta altura extra recibe la ventana enfocada en split
focus_mouse:   false,  // enfocar ventana al pasar el ratón por encima
warp_cursor:   false,  // teletransportar cursor al centro de la ventana enfocada
```

**`split_bias`** controla cuánto más alta es la ventana enfocada respecto a sus vecinas dentro de la misma columna. `0.0` = alturas iguales, `1.0` = máximo bias.

### Colores

Paleta por defecto: Catppuccin Mocha. Todos los valores son hex `0xRRGGBB`.

```rust
col_normal:  0x45475a,  // borde ventana sin foco    (Surface1)
col_focused: 0x89b4fa,  // borde ventana con foco    (Blue)
col_urgent:  0xf38ba8,  // borde ventana urgente     (Red)
```

### Nombres de workspaces

```rust
tag_names: (1..=9).map(|n| n.to_string()).collect(),
```

### Inicio (Startup)

```rust
autostart: vec![
    vec!["/usr/lib/xdg-desktop-portal-gtk"],
    vec!["/usr/lib/xdg-desktop-portal"],
    vec!["picom", "--vsync"],                    // compositor, si quieres uno
    vec!["polybar", "main"],                     // barra externa
    vec!["feh", "--bg-fill", "/ruta/a/wallpaper.png"],
    vec!["alacritty"],
],
```

maverick no orquesta ningún programa externo de forma especial — compositor,
barra, wallpaper, portales, todo son entradas de `autostart`, lanzadas en
cuanto el WM está listo. No hay lógica de orden/delay que configurar; si una
herramienta necesita un momento antes de estar lista, eso es cosa suya.

> El `autostart` por defecto también lanza `/usr/lib/xdg-desktop-portal` y `/usr/lib/xdg-desktop-portal-gtk` — sin ellos, los selectores de archivos basados en GTK/portal (subir archivos en el navegador, etc.) nunca aparecen.

### Usar una barra externa

maverick **no incluye barra de estado** — dibujarla no es trabajo del WM. Usa
polybar, waybar, eww o similar; el WM reserva espacio en pantalla para cualquier
dock que publique `_NET_WM_STRUT_PARTIAL`/`_NET_WM_STRUT`, así que las ventanas
en mosaico nunca se superponen a la barra (ver `backend/x11/struts.rs`). Lanza tu
barra desde `autostart`:

```rust
autostart: vec![
    vec!["polybar".into(), "main".into()],
    // …
],
```

Para el texto de estado, maverick lee el `WM_NAME` de la ventana raíz (definido
con `xsetroot -name "…"` o `xsetroot -name "$(date)"`) y lo expone vía
`maverickctl state` / `maverickctl subscribe`, así que una barra o script puede
leerlo sin raspar propiedades X por su cuenta.

---

## 📋 Reglas de ventanas

Las reglas asignan workspaces o fuerzan flotante automáticamente, por clase WM o título. Se configuran con `[[rules]]` en `config.toml` (ver [Configuración](#-configuración)) o, para la base compilada, en `config.rs`:

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

**Campos de las reglas:**

| Campo   | Tipo             | Descripción                                                |
| ------- | ---------------- | ---------------------------------------------------------- |
| `class` | `Option<String>` | Coincide con `WM_CLASS` (subcadena, sin mayúsculas)        |
| `title` | `Option<String>` | Coincide con el título de la ventana (subcadena, sin mayúsculas) |
| `float` | `bool`           | Forzar modo flotante                                       |
| `ws`    | `Option<usize>`  | Enviar al workspace índice (0-based)                       |

---

## 🏗 Detalles Técnicos

maverick evita capas de abstracción innecesarias siempre que es posible:

- **X11 / XLibre vía `x11rb 0.13`** — bindings del protocolo con tipado seguro, sin libx11. Solo el WM y `maverick-dialog` enlazan `x11rb`; el resto del workspace es `std` puro.
- **Un único punto de despacho** — `Engine::dispatch(Action) -> Vec<Effect>` es el ÚNICO camino de un atajo o comando IPC hacia la mutación de estado. `Effect` es un vocabulario semántico (`ArrangeMonitor`, `FocusWindow`, `SetFullscreen`, …); `execute()` del backend X11 es el único lugar que convierte eso en llamadas al protocolo. Un futuro backend no-X11 implementaría `execute()` contra los mismos efectos sin tocar el núcleo.
- **Fullscreen/maximizar como presentación, no como bloqueo de estado** — `core/present.rs` reescribe solo el rect de la ventana *enfocada* (fullscreen → pantalla completa, maximizar → área de trabajo, ambos con precedencia sobre el layout normal) y reorganiza en cada cambio de foco, en vez de bloquear la entrada mientras una ventana está en fullscreen.
- **Posicionamiento propio de ventanas flotantes** — `manage()` nunca confía en la geometría X cruda que reporta una ventana nueva; las flotantes se centran sobre la geometría real guardada de su padre transient (o el área de trabajo del monitor asignado, para diálogos de portal sin padre real) y se recortan (clamp) dentro de ella. Solo el ancho/alto vienen de la solicitud original.
- **Plano de control de instancias** — `maverick-sys` le da a cada instancia corriendo una identidad de PID/display/tty y un protocolo por socket Unix (`ping`/`identify`/`state`/`dispatch`/`restart`/`reload`/`subscribe`/`quit`). `maverickctl` habla con él: `list`, `state`, `msg <acción>`, `subscribe`, `quit[--confirm]`, `quit-all`, `restart`, `reload`, `prune`. Soporta varias instancias en distintos displays/ttys.
- **Capa opcional de config TOML** — `userconfig.rs` parsea `config.toml` y lo combina campo a campo sobre `config::compiled_config()`; un archivo que no parsea se rechaza completo, una entrada individual mala se descarta con aviso. `maverickctl reload` lo relee en caliente, sin reiniciar.
- **Struts de docks/barras externas** — Los docks se detectan vía `_NET_WM_WINDOW_TYPE_DOCK`/`_DESKTOP` (nunca por nombre de proceso) y reservan espacio leyendo `_NET_WM_STRUT_PARTIAL`/`_NET_WM_STRUT` heredado, rastreados por monitor y liberados al destruirse/desmapearse. maverick no incluye barra de estado — usa Waybar/Polybar/eww y deja que el WM reserve el espacio.
- **Mapa de clientes `HashMap`** — búsquedas de ventana O(1) por XID.
- **Layout de columnas O(N)** — las alturas de las filas se precalculan en una sola pasada.
- **Detección de monitores RandR** — cálculo correcto del área de trabajo para la barra de cada monitor.
- **Soporte EWMH** — `_NET_WM_STATE`, `_NET_WM_DESKTOP`, `_NET_ACTIVE_WINDOW`, etc.
- **Reinicio basado en `exec`** — reemplaza el proceso en caliente, sin condición de carrera (race condition) al atrapar X11.
- **Aislamiento `override_redirect`** — las barras externas y los overlays son invisibles para el propio WM.

---

## 📂 Estructura del proyecto

```text
Maverick/                    # workspace de Cargo
├── src/                     # `maverick` — el binario del WM
│   ├── main.rs                punto de entrada, señales, autostart, conexión al plano de control
│   ├── config.rs               config base compilada: Cfg, Rule, atajos, colores
│   ├── userconfig.rs            config.toml opcional: parseo, carga a prueba de fallos, merge
│   ├── types.rs                  modelo de datos principal: State, Monitor, Workspace, Column, Client
│   ├── log.rs                     logger ligero por stderr
│   ├── core/                       capa de lógica pura — sin X11
│   │   ├── engine.rs                 Engine::dispatch(Action) -> Vec<Effect>
│   │   ├── effect.rs                  enum Effect (la unión entre core y backend)
│   │   ├── present.rs                  capa de presentación fullscreen/maximizar
│   │   ├── layout.rs                    arrange_columns / arrange_grid
│   │   ├── ipc.rs                        state_json / parse_action para el socket de control
│   │   └── tests.rs                       tests unitarios
│   └── backend/                    backend X11 — el único lugar que habla el protocolo
│       ├── atoms.rs                  caché de átomos EWMH / ICCCM
│       └── x11/                        el WindowManager en ejecución, dividido por tema
│           ├── mod.rs                    WindowManager, bucle de eventos, RandR
│           ├── manage.rs                  descubrimiento de ventanas, lectura de propiedades
│           ├── events.rs                   tabla de despacho de eventos X
│           ├── ewmh.rs                      mantenimiento de propiedades EWMH
│           ├── actions.rs                    do_action / execute (ejecuta los Effects del core), reload
│           ├── input.rs                       keymap, agarre de teclas
│           ├── pointer.rs                      arrastrar para mover/redimensionar, foco por clic
│           ├── render.rs                        aplicación de geometría, foco, restack
│           ├── struts.rs                         reserva de espacio para docks externos
├── maverick-sys/             # FFI libc + identidad de instancia/socket de control/hub/discover
│   └── src/
│       ├── identity.rs         "ficha" de PID/display/tty por instancia
│       ├── control.rs           ControlServer — el protocolo por socket Unix
│       ├── hub.rs                 ControlHub — puente hacia el bucle de eventos del WM
│       ├── discover.rs             list/find/quit de instancias
│       └── bin/maverickctl.rs       el CLI `maverickctl`
├── maverick-dialog/           # ventana X11 standalone de confirmación sí/no al salir
│   └── src/main.rs
├── examples/
│   └── config.toml            ejemplo completo y comentado de config de usuario
├── CHANGELOG.md
├── Cargo.toml                 # raíz del workspace + el paquete `maverick`
├── Cargo.lock
├── LICENSE
├── README.md
└── README.es.md
```

---

## 📜 Licencia

GPL-3.0 license
