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
- ⚡ Consumo de memoria extremadamente bajo (~3–4 MB).
- 🔲 Tres modos de layout: Column (estable), Monocle & Grid (experimental).
- 🖥 Multi-monitor real vía RandR.
- 🧩 Soporte de ventanas flotantes y pantalla completa.
- 📐 Gaps, bordes, barra y split bias configurables.
- 🔧 Reglas de ventanas declarativas.
- 🚀 Autostart de programas.
- 📋 Compatible con EWMH.

---

## 🚀 Instalación

### Compilar desde fuente

```bash
git clone https://github.com/azytar/Maverick.git
cd Maverick
cargo build --release
cp target/release/maverick ~/.local/bin
```

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

maverick incluye tres modos de layout intercambiables en tiempo de ejecución:

*Nota: Los modos `Monocle` y `Grid` son actualmente experimentales y están en desarrollo activo.*

| Modo     | Atajo         | Descripción                                                                |
| -------- | ------------- | -------------------------------------------------------------------------- |
| **Column**  | `Super+T`     | Columnas desplazables. Cada ventana vive en su propia columna por defecto. |
| **Monocle** | `Super+M`     | Una ventana a la vez ocupando toda el área de trabajo.                     |
| **Grid**    | `Super+G`     | Todas las ventanas en rejilla uniforme.                                    |

Ciclar entre los tres: `Super+Space`.

> El layout es global en todos los monitores. Al cambiarlo, se reorganizan todos simultáneamente.

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
| `Super+B`                | Mostrar / ocultar barra      |

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

> Los tags de la barra también son **clicables**.

### WM (Control del Gestor)

| Atajo                    | Acción                            |
| ------------------------ | --------------------------------- |
| `Super+Shift+Q`          | Salir de maverick              |
| `Super+Shift+R`          | Reiniciar maverick en caliente    |
| `Super+F5`               | Reiniciar maverick en caliente    |
| `Super+Space`            | Ciclar modos de layout            |
| `Super+T`                | Establecer layout Column          |
| `Super+M`                | Establecer layout Monocle         |
| `Super+G`                | Establecer layout Grid            |

### Ratón (ventanas flotantes)

| Acción                              | Resultado                  |
| ----------------------------------- | -------------------------- |
| `Super+Arrastrar botón izquierdo`   | Mover ventana flotante     |
| `Super+Arrastrar botón derecho`     | Redimensionar ventana flotante |
| Clic en el tag de la barra          | Ir a ese workspace         |

---

## 🔧 Configuración

**Nota:** maverick se configura completamente en `src/config.rs`. **Debes recompilar el proyecto después de cualquier cambio para que se apliquen.**

```bash
cargo build --release
# Luego reinicia maverick
```

### Opciones principales

```rust
border_w:      2,      // ancho del borde en píxeles
gaps:          6,      // espacio entre ventanas y bordes de pantalla (px)
bar_height:    22,     // altura de la barra de estado (px)
top_bar:       true,   // barra arriba (false = abajo)
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
col_bar_bg:  0x1e1e2e,  // fondo de la barra         (Base)
col_bar_fg:  0xcdd6f4,  // texto de la barra         (Text)
col_bar_sel: 0x89b4fa,  // workspace seleccionado    (Blue)
col_bar_occ: 0xa6e3a1,  // workspace ocupado         (Green)
```

### Nombres de workspaces

```rust
tag_names: ["1", "2", "3", "4", "5", "6", "7", "8", "9"].to_vec(),
```

### Inicio (Startup)

```rust
compositor: vec!["picom", "--vsync"],            // compositor lanzado antes del WM
compositor_delay_ms: 180,                        // ms de espera tras lanzar el compositor
startup_sound: None,                             // sonido opcional WAV/OGG al iniciar
autostart: vec![
    vec!["feh", "--bg-fill", "/ruta/a/wallpaper.png"],
    vec!["alacritty"],
],
```

El compositor se inicia **antes** que el WM para que todas las ventanas tengan compositing desde el primer fotograma. Los programas de autostart se lanzan después de que tanto el compositor como el WM estén listos. `startup_sound` acepta una ruta a un archivo `.wav` u `.ogg`; prueba `pw-play → paplay → canberra-gtk-play → mpv → aplay` en ese orden.

---

## 📋 Reglas de ventanas

Las reglas asignan workspaces o fuerzan flotante automáticamente, por clase WM o título.

```rust
rules: vec![
    Rule { class: Some("xdg-desktop-portal"), title: None,                    float: true,  ws: None },
    Rule { class: Some("gpick"),              title: None,                    float: true,  ws: None },
    Rule { class: Some("pinentry"),           title: None,                    float: true,  ws: None },
    Rule { class: None, title: Some("file upload"),    float: true,  ws: None },
    Rule { class: None, title: Some("open file"),      float: true,  ws: None },
    Rule { class: None, title: Some("save file"),      float: true,  ws: None },
    Rule { class: None, title: Some("qt file dialog"), float: true,  ws: None },
],
```

**Campos de las reglas:**

| Campo   | Tipo            | Descripción                                                |
| ------- | --------------- | ---------------------------------------------------------- |
| `class` | `Option<&str>`  | Coincide con `WM_CLASS` (subcadena, sin mayúsculas)        |
| `title` | `Option<&str>`  | Coincide con el título de la ventana (subcadena, sin mayúsculas) |
| `float` | `bool`          | Forzar modo flotante                                       |
| `ws`    | `Option<usize>` | Enviar al workspace índice (0-based)                       |

---

## 🏗 Detalles Técnicos

maverick evita capas de abstracción innecesarias siempre que es posible:

- **X11 / XLibre vía `x11rb 0.13`** — bindings del protocolo con tipado seguro, sin libx11.
- **Renderizado de barra con X11 puro** — Barra de estado dibujada con `image_text8` y `poly_fill_rectangle`, sin librerías de fuentes externas.
- **Mapa de clientes `HashMap`** — búsquedas de ventana O(1) por XID.
- **Batching en la barra** — la cola se vacía antes de cada `flush()` para evitar redibujados O(N).
- **Layout de columnas O(N)** — las alturas de las filas se precalculan en una sola pasada.
- **Detección de monitores RandR** — cálculo correcto del área de trabajo para la barra de cada monitor.
- **Soporte EWMH** — `_NET_WM_STATE`, `_NET_WM_DESKTOP`, `_NET_ACTIVE_WINDOW`, etc.
- **Reinicio basado en `exec`** — reemplaza el proceso en caliente, sin condición de carrera (race condition) al atrapar X11.
- **Aislamiento `override_redirect`** — barras y overlays son invisibles para el propio WM.
- **Protección de centrado flotante** — evita que la heurística de centrado falle en herramientas de captura a pantalla completa (≥90% de cobertura del área = sin centrado).

---

## 📂 Estructura del proyecto

```text
maverick/
├── src/
│   ├── main.rs          punto de entrada, señales, autostart
│   ├── config.rs        configuración, atajos, reglas de ventanas
│   ├── types.rs         tipos principales: State, Monitor, Workspace, Column, Client
│   ├── log.rs           logging ligero
│   ├── core/
│   │   ├── mod.rs
│   │   ├── engine.rs    capa de lógica pura (layout engine)
│   │   ├── layout.rs    arrange_columns / arrange_monocle / arrange_grid
│   │   ├── events.rs    enum AppEvent
│   │   ├── commands.rs  enum Command (MoveResize, SetBorderColor, …)
│   │   └── tests.rs     tests unitarios
│   └── backend/
│       ├── mod.rs
│       ├── atoms.rs     caché de átomos EWMH / ICCCM
│       ├── bar.rs       barra de estado
│       └── x11.rs       bucle de eventos X11, gestión de ventanas, RandR
├── Cargo.toml
├── Cargo.lock
├── LICENSE
├── README.md
└── README.es.md
```

---

## 📜 Licencia

GPL-3.0 license 
