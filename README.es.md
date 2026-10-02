# Maverick

Maverick es un gestor de ventanas X11 con mosaico para Linux, escrito en Rust.
Es Unix-oriented y deliberadamente estrecho de alcance: ordena ventanas en una
pantalla X11, publica las propiedades EWMH que un escritorio espera, y hasta ahí
llega.

Su modelo se articula en torno a las **Views lógicas**. Cada View contiene un
conjunto de columnas en mosaico y sus propias ventanas flotantes; un **Carousel**
selecciona qué View es la actual; un **layout** decide dónde van los clientes en
mosaico de la View actual; y un par `DesiredState`/`Reconciler` convierte esa
decisión en peticiones a X11.

[Panorámica](#panorámica) · [Lo que Maverick no es](#lo-que-maverick-no-es) ·
[Funcionalidad](#funcionalidad) · [Arquitectura](#arquitectura) ·
[Instalación](#instalación) · [Ejecución](#ejecución) ·
[Configuración](#configuración) · [Control](#control) ·
[Desarrollo](#desarrollo) · [Estado](#estado) · [Licencia](#licencia)

Léelo en inglés: [README.md](README.md).

## Panorámica

- **Un gestor de ventanas X11 con mosaico, escrito en Rust.** Linux, X11,
  ICCCM/EWMH para lo que un gestor de ventanas necesita. No hay backend de
  Wayland.
- **Construido alrededor de Views lógicas.** Una View es un contenedor de
  ventanas, no una ventana X11: crearla, seleccionarla y eliminarla es una
  transición puramente lógica. El tipo de Rust detrás de una View es
  `Workspace`.
- **Navegado con un Carousel.** Cada monitor posee un `Carousel` que registra
  qué View es `current` y cuál es `origin`, y se mueve entre ellas paso a paso.
  La navegación es lógica e instantánea.
- **Ordenado por layouts.** Un layout convierte los clientes en mosaico de una
  View en rectángulos. Scroll es el único layout que Maverick ofrece hoy.
- **Los clientes flotantes quedan fuera del layout.** Una ventana flotante
  conserva su propia geometría en espacio de pantalla; el layout en mosaico ni
  la coloca ni la mueve.
- **Materializado con un DesiredState y un Reconciler.** El motor calcula una
  intención pura, el reconciler la compara con lo que X11 ya tiene, y sólo la
  diferencia se convierte en llamadas `ConfigureWindow`.

Cinco cosas se mantienen deliberadamente separadas, y todo el diseño gira en
mantenerlas separadas:

| concepto | qué es | qué no es |
|---|---|---|
| **Identidad de View** (`ViewId`) | Un nombre estable, acuñado de forma monótona y nunca reutilizado para una View de un monitor | una posición, un id de ventana X11, o una etiqueta de layout |
| **Orden de las Views** | La posición que ocupa una View en la lista de un monitor | la identidad de la View: eliminar una View desplaza todas las posteriores |
| **Selección del Carousel** | Qué View es `current`, y cuál es `origin` | estado del layout; el Carousel no sabe nada de layouts |
| **Geometría del layout** | Los rectángulos que reciben los clientes en mosaico de una View | la pertenencia a una View; ningún layout añade, quita ni reubica un cliente |
| **Materialización X11** | Las llamadas `ConfigureWindow` que hacen que el servidor coincida | una segunda fuente de verdad; el estado aplicado es una caché del backend |

## Lo que Maverick no es

Maverick es un gestor de ventanas, no un entorno de escritorio. No contiene, no
distribuye y no arranca:

- ningún entorno de escritorio;
- ningún compositor, renderer ni camino por GPU — no hay bucle de frames, ni GL,
  ni Vulkan;
- ningún subsistema de wallpaper — el fondo de la ventana raíz no es una
  ventana que gestionar;
- ningún demonio de notificaciones;
- ningún system tray;
- ningún sistema de animación o transiciones — la geometría se escribe una
  sola vez, en su posición final;
- ningún modo Monocle;
- ningún layout distinto de Scroll.

La composición, un panel, un lanzador, las notificaciones y el wallpaper son
programas de otro. Maverick los arranca si los listas en `[autostart]`, lee los
struts que publican y no vuelve a hablar con ellos.

No hay un segundo layout. `LayoutKind` es un enum de una sola variante,
`set_layout` sólo acepta `column`, y no existe ningún layout Mosaic en este
repositorio.

## Funcionalidad

Todo lo que sigue está implementado en este árbol. El estado lógico, la
geometría del layout, la capa de comandos y las superficies de control están
cubiertos por la suite de tests; el comportamiento de protocolo, apilado y foco
tiene sus propios harnesses sobre X11 real.

### Views e identidad de View

- Cada monitor posee una lista de Views. Un monitor arranca con `n_tags` de
  ellas (9 por defecto, 9 como máximo).
- Cada View lleva un `ViewId`: un `u32` acuñado por el Carousel de ese monitor,
  estrictamente creciente y nunca reutilizado. Una View eliminada libera su id
  para siempre, así que un `ViewId` obsoleto es *detectable* en lugar de apuntar
  en silencio a lo que heredó la posición anterior.
- Una View está o bien en mosaico (sus columnas) o bien flotante (su propia
  lista de floats). Cada ventana gestionada se referencia desde exactamente una
  de las dos, en exactamente un monitor.
- `view_create` añade una View, hasta 9 por monitor. `view_remove` se rechaza
  mientras la View que nombra siga teniendo clientes: adónde irían es una
  decisión de política, así que la eliminación no se hace por ti.
- `view_create` y `view_remove` están expuestos como acciones y por
  `maverickctl view`; no vienen enlazados por defecto.

### Navegación del Carousel

- `view_next` / `view_prev` avanzan un puesto en un carousel **circular**: desde
  la última View, `next` es la primera; desde la primera, `previous` es la
  última.
- `view_return` selecciona el `origin` — la View a la que quedó anclado el
  carousel cuando se creó la primera View de ese monitor. Es una operación
  total: `origin` se repara en cada eliminación, así que nunca puede nombrar una
  View borrada.
- `view <n>` selecciona una View por su posición en la lista del monitor,
  resuelta a través del Carousel, de modo que el cambio es de identidad de View
  y no de índice posicional.
- Crear una View mientras ya existen otras **no** te mueve: sólo la transición
  de vacío a no-vacío convierte una View nueva en actual y ancla el origin.
- Eliminar una View repara `current` y `origin` de forma independiente: cada uno
  adopta la sucesora de la posición liberada, o la nueva cola cuando se va la
  última View.
- Los dos punteros del Carousel son `Some` exactamente cuando el monitor tiene al
  menos una View, y ambos son `None` sólo cuando no tiene ninguna.

### El layout Scroll

Scroll es el único layout. Es un ribbon de columnas con desplazamiento
horizontal, cada columna una pila vertical de ventanas, con una cámara de scroll
que mantiene a la vista la columna enfocada.

- Las columnas tienen un ancho expresado como fracción del workarea del monitor.
  Añadir una columna extiende el ribbon en vez de encoger a sus vecinas.
- `ideal_scroll` deriva el desplazamiento de cámara que deja la columna enfocada
  completamente visible, y se recalcula tras cada cambio en el árbol de columnas,
  así que la cámara nunca puede quedar varada más allá del final de un ribbon más
  corto.
- El zoom del viewport (`viewport_zoom`) agranda el ribbon para inspeccionarlo de
  cerca, y `page_snap` mueve la cámara una pantalla cada vez.
- Overview (`toggle_overview`, `overview_nav`, `overview_enter`) es una
  proyección reducida de la View actual para elegir una columna. Cambia la
  proyección, no el layout ni la pertenencia a una View.
- `grow_col` redimensiona la columna enfocada en píxeles; `maverickctl resize`
  expresa la misma operación como porcentaje. `new_column` y `collapse_column`
  añaden y quitan columnas.
- Fullscreen y maximize son **presentación**, aplicada después del layout
  (`present::present_into`), no layouts aparte. Una ventana que toma un
  fullscreen exclusivo real también recibe `_NET_WM_BYPASS_COMPOSITOR`
  publicado, para que un compositor externo se aparte de ella.

### Clientes flotantes

- Una ventana flota porque es un transient o un diálogo, porque coincide con una
  heurística de flotación o con una regla, o porque se conmuta con
  `Super+Shift+Space`.
- Las ventanas flotantes se proyectan desde su propio `Client::geom` y el layout
  nunca las coloca. Desplazar el ribbon, redimensionar una columna y entrar en
  Overview las dejan donde están, en coordenadas X11 globales.
- Los floats sticky permanecen visibles en todas las Views de su monitor. Los
  floats ordinarios siguen la visibilidad de la View a la que pertenecen.
- `[[rules]]` puede forzar el tamaño y la posición de un float (relativos al
  origen del workarea), su opacidad, su grosor de borde, y si las peticiones de
  fullscreen del propio cliente se respetan, se normalizan o se rechazan.
- Arrastrar con `Super` mueve, y arrastrar con `Super` y el botón derecho
  redimensiona, una ventana que ya está flotante. Los clientes en mosaico
  conservan su rectángulo de layout: un cliente no puede fijar su propio tile
  mediante `ConfigureRequest`.
- El `_NET_WM_STATE_MAXIMIZED_*` / `_NET_WM_STATE_FULLSCREEN` pedido al mapearse
  se normaliza para todos los clientes por defecto, así que las aplicaciones que
  recuerdan estar maximizadas abren como un tile normal. `honor_initial_state`
  lo permite, globalmente o por regla.

### X11

- Un único `Display*` de Xlib cuya cola de eventos posee XCB, entregado al gestor
  de ventanas como la conexión sobre la que emite sus peticiones. No hay un
  segundo lector del socket.
- EWMH: `_NET_SUPPORTED`, `_NET_CLIENT_LIST`, `_NET_CLIENT_LIST_STACKING`,
  `_NET_NUMBER_OF_DESKTOPS`, `_NET_DESKTOP_NAMES`, `_NET_CURRENT_DESKTOP`,
  `_NET_DESKTOP_GEOMETRY`, `_NET_WORKAREA`, `_NET_ACTIVE_WINDOW`,
  `_NET_SUPPORTING_WM_CHECK`, `_NET_WM_DESKTOP`, `_NET_WM_STATE` (incluidos
  `MODAL`, `MAXIMIZED_VERT`, `MAXIMIZED_HORZ`, `FULLSCREEN`,
  `DEMANDS_ATTENTION`), `_NET_CLOSE_WINDOW`, `_NET_FRAME_EXTENTS`,
  `_NET_WM_PID`, `_NET_WM_BYPASS_COMPOSITOR` y `_NET_WM_WINDOW_OPACITY`.
- Las reservas de un dock mediante `_NET_WM_STRUT` y `_NET_WM_STRUT_PARTIAL`
  reducen el workarea, así que las ventanas en mosaico nunca cubren un panel que
  el gestor de ventanas no posee.
- Descubrimiento de monitores RandR y actualizaciones de topología. Cada monitor
  tiene sus propias Views y su propio Carousel.
- `_NET_DESKTOP_GEOMETRY` y `_NET_WORKAREA` permanecen físicos. Maverick no
  publica `_NET_DESKTOP_VIEWPORT`: el scroll es una transformación interna del
  layout sobre el escritorio físico.
- Una sesión inactiva no consume CPU. El bucle de eventos bloquea sobre X11 más
  el self-pipe de control, sin plazo de frame, sin latido y sin temporizador.
- `--replace` pide el relevo a un gestor de ventanas en marcha y adopta sus
  ventanas. El reinicio se reejecuta en el sitio con los mismos argumentos.

### maverickctl

`maverickctl` es un binario separado — un cliente de control fino que nunca
enlaza el gestor de ventanas ni toca X11. Habla con una instancia en marcha a
través del socket de control Unix de esa instancia.

- `maverickctl list`, `state`, `query <topic>`, `subscribe`, `msg <action>`,
  `reload`, `restart`, `quit`, `quit-all`, `prune`.
- `maverickctl view <session> goto <n> | next | prev | return | create |
  remove <n>` — el Carousel, codificado como las mismas acciones que ejecuta un
  atajo de teclado.
- `maverickctl window <session> list | inspect | focus | close | move | float |
  fullscreen`, más `camera`, `resize` y `layout` para el layout en sí.
- `maverickctl session …` gestiona sesiones gráficas completas: un servidor X
  anidado, un Maverick, los programas lanzados en ella, una cookie, logs y un
  ciclo de vida. Ver [`docs/sessions.md`](docs/sessions.md).
- Cada instancia tiene un directorio de runtime privado bajo
  `$XDG_RUNTIME_DIR/maverick/<session-id>/` (`0700`), un socket `0600`
  verificado contra pares con `SO_PEERCRED` y un registro de identidad. El
  descubrimiento prefiere `--session`, luego `--name`, luego
  `$MAVERICK_INSTANCE`, luego el contexto de display/TTY; se niega a adivinar
  cuando hay varios candidatos.
- Cualquier palabra que `maverickctl` no reconozca como comando se reenvía tal
  cual al gestor de ventanas, que es lo único que puede distinguir una acción de
  un tema de consulta de una errata.

### Configuración

Ver [Configuración](#configuración) más abajo.

## Arquitectura

El flujo de alto nivel, desde la View activa hasta los píxeles que X11 sostiene:

```text
        Carousel
           ↓
    View activa
           ↓
  clientes en mosaico
           ↓
        Layout            (Scroll: la única implementación)
           ↓
    DesiredState         (intención pura: ventana + rect + borde)
           ↓
      Reconciler          (compara DesiredState con AppliedState)
           ↓
           X11
```

Las fronteras que importan:

- **El Carousel no conoce los layouts.** Contiene dos `ViewId` y se mueve entre
  ellos. No hay ningún `match layout` dentro, y la navegación debe dar la misma
  respuesta esté instalado el layout que esté.
- **Scroll no es dueño de la navegación entre Views.** A `layout::arrange` se le
  entrega una View — la activa, resuelta a través del Carousel — y devuelve
  geometría. Nunca crea, elimina, selecciona ni reordena una View, y nunca muta
  la pertenencia a una View: lee `columns` y `floats` y calcula rectángulos.
- **El orden de las Views y su identidad son cosas separadas.** La pertenencia se
  indexa por `ViewId`, así que eliminar una View desplaza posiciones sin
  invalidar ninguna referencia de cliente.
- **Los clientes flotantes quedan fuera del layout.** Viven en la lista `floats`
  de la View, que es exactamente la razón por la que se excluyen de la entrada
  del layout; la etapa de presentación los proyecta desde su propia geometría.
- **Una sola proyección, y es la geometría.** No hay una vista interpolada al
  lado de la asentada. Un scroll reescribe la cámara y el siguiente arrange *es*
  la geometría final.
- **Un solo sumidero de geometría.** Cada `ConfigureWindow` de un cliente sale de
  la diferencia que calcula el reconciler; ningún otro código coloca una
  ventana. El estado aplicado es una caché del backend, no una afirmación de que
  las peticiones X11 asíncronas nunca puedan fallar.
- **El motor es puro.** `Engine::dispatch(Action)` ejecuta un `Command`, que muta
  `State` y emite `Effect`s. Sólo el backend ejecuta los efectos, y sólo el
  backend toca X11.

| Ubicación | Responsabilidad |
| --- | --- |
| `src/main.rs` | CLI, selección de configuración, señales, identidad de instancia, arranque del backend |
| `maverick-core/` | Tipos de dominio sin dependencias: `State`, `Monitor`, `Workspace` (una View), `ViewId`, `Carousel`, `Column`, `Client`, `Rect` |
| `src/core/` | Motor, acciones/comandos/efectos/eventos, layout, presentación, `DesiredState` |
| `src/backend/x11/` | Eventos, gestión de clientes, entrada, EWMH, struts, reconciliación |
| `src/config.rs`, `src/userconfig.rs` | Defaults compilados, fusión de configuración, validación |
| `maverick-x11/` | Arranque compartido de la conexión Xlib/XCB |
| `maverick-sys/` | Frontera del SO/FFI, identidad de instancia, socket de control y hub |
| `maverick-toml/` | Parser de un subconjunto de TOML |
| `maverickctl/` | Binario cliente de control: CLI, cliente IPC por socket, descubrimiento, ciclo de vida de sesiones |
| `tests/` | Sondas sobre X11 real, scripts de integración, tests de humo del instalador |
| `installer/` | El instalador y su suite de tests |

`maverick-core` no depende de X11, del reloj, del sistema de ficheros ni del
entorno, y por eso la mayor parte de la suite de tests no necesita display. La
implementación sí depende de las bibliotecas nativas de X11, y `x11rb` se usa con
una conexión FFI de XCB en lugar de como una pila de protocolo enteramente pura
en Rust. No hay runtime asíncrono ni toolkit de GUI.

`docs/architecture.md` describe las mismas fronteras con anclajes `file:line`.

## Instalación

### Requisitos

Linux, un servidor X11, un enlazador C y Rust 1.82 o posterior. Maverick enlaza
`libX11` y `libX11-xcb`, y el parser del repo es un subconjunto de TOML.

```bash
# Arch Linux
sudo pacman -S --needed base-devel rust libx11 libxcb
# Debian / Ubuntu
sudo apt install --no-install-recommends build-essential cargo libx11-dev libxcb1-dev
# Fedora
sudo dnf install -y cargo gcc libX11-devel libxcb-devel
```

Para una sesión X11 arrancada con `startx`, instala también `xorg-server` y
`xorg-xinit` (Arch: `xorg-server xorg-xinit`).

Los atajos compilados por defecto lanzan `alacritty` y `rofi`, y el autostart
compilado lanza `xdg-desktop-portal` y `xdg-desktop-portal-gtk`. Son
conveniencias, no requisitos: sobrescribe los atajos o proporciona tu propia
lista `[autostart] commands`. El motor de layout no necesita ninguna de ellas.

### Instalar

```bash
git clone https://github.com/Azytar/Maverick.git
cd Maverick
./installer/install.sh
```

El instalador compila los binarios de release y los instala en `$HOME/.local`
por defecto, lo cual no necesita privilegios. Se niega a ejecutarse como root,
nunca invoca `sudo` y nunca habilita ningún servicio.

```bash
./installer/install.sh --help          # todas las opciones
./installer/install.sh --system       # instalar en /usr/local en su lugar
./installer/install.sh --prefix DIR   # instalar en DIR en su lugar
./installer/install.sh --no-config    # no crear fichero de configuración
./installer/install.sh --no-build     # instalar los binarios existentes de $CARGO_TARGET_DIR/release
./installer/install.sh --no-path      # no editar nunca un fichero de inicio de shell
```

Qué instala, y dónde:

| ruta | qué |
| --- | --- |
| `<prefix>/bin/maverick` | el gestor de ventanas |
| `<prefix>/bin/maverickctl` | el cliente de control |
| `<prefix>/share/xsessions/maverick.desktop` | la entrada de sesión X11 |

Fuera del prefijo sólo escribe tus propios ficheros, y sólo si aceptas:

- `${XDG_CONFIG_HOME:-$HOME/.config}/maverick/config.toml`, sembrado desde
  [`config/config.toml`](config/config.toml) salvo que ya exista uno (`--no-config`
  omite este paso; responder "no" a la pregunta de sobrescribir conserva el
  tuyo);
- un bloque marcado y autoprotegido en un fichero de inicio de shell bajo
  `$HOME`, ofrecido sólo cuando el directorio de binarios no está en `PATH` y
  nunca con `--no-path`.

La única escritura que sale del prefijo deliberadamente es el fichero de sesión,
y sólo cuando nombras el directorio: `--xsessions-dir /usr/share/xsessions`. Los
gestores de sesión suelen leer sólo ubicaciones del sistema, así que una entrada
de sesión en el home del usuario no aparecerá por sí sola en un selector.

El prefijo es una frontera dura: no se crea ni modifica nada fuera de él salvo
los dos ficheros de arriba. Un prefijo que no puedas escribir se informa como
error de permisos en lugar de escalarse, así que una instalación en todo el
sistema necesita que tú dispongas de acceso de escritura a `/usr/local`.

Cada paso falla ruidosamente. El instalador ejecuta los binarios que acaba de
instalar — `maverick --version`, `maverickctl --help` y
`maverickctl session --help` — y un conjunto parcial, obsoleto o roto se informa
como fallo en lugar de como instalación correcta. Es seguro ejecutarlo
repetidamente: una segunda pasada converge, corrige permisos hostiles al umask y
no duplica el bloque de `PATH`.

`CARGO_TARGET_DIR` se respeta tal cual. Cuando no está definido, la compilación
ocurre en un directorio de caché bajo `$XDG_CACHE_HOME` y el checkout nunca se
usa como directorio de build. El primer intento de compilación pasa
`-C target-cpu=native` y vuelve a un build normal si ese falla, así que el
binario instalado queda ajustado para la máquina que lo compiló; usa un
`cargo build` normal cuando necesites artefactos para otra CPU.

Para desinstalar, borra los dos binarios y el fichero de sesión del prefijo, y
elimina el bloque entre los marcadores `# >>> maverick (install.sh) >>>` de
cualquier fichero de inicio que haya tocado.

Ver [`installer/README.md`](installer/README.md) para la documentación propia
del instalador y su suite de tests.

## Ejecución

Para una sesión con `startx`, pon esto al final de `~/.xinitrc`:

```sh
exec maverick
```

O selecciona la sesión de Maverick instalada en tu gestor de sesión.

```bash
maverick --check-config "$HOME/.config/maverick/config.toml"   # validar, no arrancar nada
maverick --config "$HOME/.config/maverick/config.toml" --name desktop
maverick --help
```

- `--check-config [path]` valida una configuración y sale: `0` si está limpia,
  `1` si hay avisos o errores. Nunca abre un display X.
- `--config <path>` sustituye la ubicación por defecto de la configuración y se
  reutiliza en el reload y el reinicio.
- `--name <id>` etiqueta la instancia; `--session-id <id>` la publica bajo un
  session id fijo, que es lo que usa `maverickctl session`.
- `--replace` releva a un gestor de ventanas en marcha y adopta sus ventanas.
- `--debug` / `--log-level <off|error|warn|info|debug|trace>` fijan el nivel de
  log. `--log-level` gana sobre `--debug`.

### Atajos compilados por defecto

`Super` es Mod4, normalmente la tecla de Windows. `Super+1…9` y
`Super+Shift+1…9` se generan para `n_tags`; pon
`auto_workspace_binds = false` para gestionarlos tú.

| atajo | acción |
| --- | --- |
| `Super+Return` | terminal (`alacritty`) |
| `Super+P` / `Super+Shift+P` | lanzador (`rofi`) |
| `Super+H/J/K/L` | foco izquierda / abajo / arriba / derecha |
| `Super+Shift+H/J/K/L` | mover ventana izquierda / abajo / arriba / derecha |
| `Super+Shift+Return` | poner la ventana en una columna nueva |
| `Super+Ctrl+H` / `Super+Ctrl+L` / `Super+Ctrl+J` | encoger / agrandar la columna / colapsarla |
| `Super+T` | fijar el layout (`column`) |
| `Super+Shift+Space` | conmutar flotante |
| `Super+Shift+F` / `Super+Shift+M` | conmutar fullscreen / maximize |
| `Super+1…9` | seleccionar View |
| `Super+Shift+1…9` | enviar la ventana a la View |
| `Super+Tab` / `Super+Shift+Tab` | enfocar el monitor siguiente / mover la ventana a él |
| `Super+O` / `Super+E` / `Super+N` / `Super+Shift+O` | Overview: conmutar / entrar / siguiente / anterior |
| `Super+=` / `Super+-` | zoom del viewport hacia dentro / hacia fuera |
| `Super+]` / `Super+[` | page-snap a la derecha / a la izquierda |
| `Super+Shift+C` | cerrar la ventana enfocada |
| `Super+Shift+R` / `Super+F5` | reiniciar en el sitio |
| `Super+Shift+Q` | salir |

El avance por el Carousel (`view_next`, `view_prev`, `view_return`) y el ciclo
de vida de las Views (`view_create`, `view_remove`) no vienen enlazados por
defecto; llámalos con `maverickctl view` o añade tus propios
`[[keybindings]]`.

### Diagnóstico

Compila con las features opcionales `input-trace` y/o `window-trace` para
añadir trazas estructuradas de entrada/foco y de estado deseado/aplicado/X11.
Ambas están apagadas por defecto y ambas sólo añaden logging.

```bash
cargo build -p maverick --features input-trace,window-trace
./target/debug/maverick --config /path/to/test.toml 2> /tmp/maverick-debug.log
```

Ejecuta eso sólo en un `DISPLAY` que estés dispuesto a entregar a un gestor de
ventanas.

## Configuración

La configuración es opcional. Maverick lee
`$XDG_CONFIG_HOME/maverick/config.toml`, con
`~/.config/maverick/config.toml` como alternativa, y usa los defaults
compilados para lo que el fichero no fije.

```toml
[general]
column_width = 0.5
gaps_inner = 10
gaps_outer = 14
focus_mouse = false
```

Dos reglas deciden cómo se combina tu fichero con los defaults:

- Los ajustes ordinarios se fusionan campo a campo.
- `[[keybindings]]` y `[[rules]]` **sustituyen** la lista compilada entera
  cuando las declaras. Aportar una sola `[[rules]]` descarta la política
  compilada de flotación por aplicación, así que repite las entradas que
  quieras.

Un TOML mal formado vuelve a los defaults compilados; una entrada individual
inválida se diagnostica y se ignora, y el resto del fichero sigue cargando. Una
tabla que Maverick no conoce (una escrita para otro Maverick) se salta en
silencio, mientras que una clave desconocida dentro de una tabla que *sí*
conoce se informa. Valida antes de confiar en ella:

```bash
maverick --check-config ~/.config/maverick/config.toml
```

[`config/config.toml`](config/config.toml) es un ejemplo comentado que cubre el
vocabulario más amplio. Es un preset, no una copia de los defaults compilados:
copiarlo cambia tus atajos, reglas y autostart.

### Reglas de aplicación

`class`, `instance` y `title` hacen coincidencia como subcadena sin distinguir
mayúsculas de los propios textos de la ventana; `window_type` coincide con un
nombre `_NET_WM_WINDOW_TYPE` normalizado completo. Todos los criterios presentes
en una regla deben coincidir.

```toml
[[rules]]
class = "calculator"
float = true
size = [480, 360]
position = [120, 100]
```

Las reglas también aceptan `sticky`, `workspace` (base 1), `opacity`,
`border_width`, `ignore_initial_state`, `honor_initial_state`,
`deny_fullscreen` y `true_fullscreen`. `deny_fullscreen` rechaza las peticiones
de fullscreen del propio cliente, no tu `Super+Shift+F`. `true_fullscreen` pide
un overlay realmente exclusivo y tiene prioridad sobre `deny_fullscreen`.

### Autostart

`[autostart] commands` es una lista de listas de argumentos:

```toml
[autostart]
commands = [["polybar", "main"], ["picom", "--vsync"]]
```

Una lista no vacía sustituye a la compilada. Aquí es donde pertenece un
compositor, un panel o un programa de wallpaper: Maverick arranca el comando y
no vuelve a hablar con él. Los docks que publican struts reservan workarea
automáticamente. El arranque de sesión y el reinicio no son un supervisor de
servicios de propósito general.

## Control

```bash
maverickctl list
maverickctl state --name desktop
maverickctl query tree --name desktop
maverickctl msg view 3 --name desktop
maverickctl subscribe --name desktop
maverickctl reload --name desktop
maverickctl restart --name desktop
maverickctl quit --name desktop --confirm
```

Las Views y las ventanas se direccionan semánticamente, por View o por id o
nombre de ventana, y cada operación es la misma acción que ejecuta un atajo de
teclado:

```bash
maverickctl view debug next
maverickctl window list debug --json
maverickctl window focus debug firefox
maverickctl window float debug 0x42003
maverickctl resize debug +10%
maverickctl process list debug --json
maverickctl inspect debug
maverickctl session stop debug
```

Cada listado tiene su forma `--json`. Salir de una sesión pide a sus clientes
que cierren mediante `WM_DELETE_WINDOW` y fuerza el cierre de los supervivientes
tras una espera acotada; guarda tu trabajo antes. Ver
[`docs/sessions.md`](docs/sessions.md) para el modelo de sesión, sus
limitaciones y la frontera de seguridad.

## Desarrollo

```bash
cargo fmt --all
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

`cargo fmt --all -- --check` es la forma de sólo lectura. Mantén el trabajo de
layout, de Carousel y de comandos cubierto por tests de estado puros, que no
necesitan display; usa un servidor X aislado para el comportamiento de
protocolo, apilado y foco. Al cambiar la interoperabilidad EWMH (struts, bypass
hints, opacidad), valida contra una sesión real.

El instalador tiene sus propias comprobaciones:

```bash
bash installer/lint.sh                  # bash -n, y shellcheck si está presente
python3 installer/tests/partition.py    # la suite de comportamiento del instalador
```

Los harnesses sobre X11 real viven en `tests/`: `tests/xvfb-stacking.py` es la
regresión automatizada, y los scripts `tests/xephyr-*.sh` son escenarios de
integración manuales. **No** están todos aislados con el mismo rigor —algunos
helpers antiguos en `tests/common.sh` matan procesos por nombre o usan displays
fijos—, así que lee un script antes de ejecutarlo, y ejecuta la suite legacy
sólo en una sesión gráfica desechable.

CI (`.github/workflows/ci.yml`) ejecuta tres trabajos: tests del workspace con
Clippy estricto, las comprobaciones del instalador, y un test de humo de apilado
con Xvfb.

## Estado

Maverick está en preview. No está declarado como listo para producción, y los
scripts de integración son comprobaciones de regresión, no una certificación de
compatibilidad de aplicaciones.

- **Alcance:** sólo Linux y X11. Sin backend de Wayland, compositor, subsistema
  de animación, shell de escritorio, blur ni sombras.
- **Layouts:** Scroll es el único layout. `LayoutKind` tiene una variante; un
  segundo layout no está implementado y no debe documentarse como si lo
  estuviera.
- **Views:** como mucho 9 por monitor. `n_tags` fija cuántas existen al arrancar,
  y la fila de números no tiene una décima tecla.
- **Compatibilidad:** ICCCM y EWMH están implementados para lo que el gestor de
  ventanas necesita, lo cual no es una afirmación de cobertura completa del
  protocolo o de las aplicaciones.
- **Monitores:** el foco y el movimiento recorren el orden de enumeración de los
  monitores, no la dirección física. La recuperación de topología usa rectángulos
  e índices, no identidades estables de conector, así que un hotplug o
  reordenamiento arbitrario no preserva las asignaciones.
- **Geometría:** X11 tiene un único espacio global de coordenadas raíz con los
  límites de tamaño y coordenadas del protocolo; la proyección de Scroll y los
  workareas multi-monitor los respetan.
- **Interfaces:** la configuración, las APIs internas y la política de
  presentación pueden cambiar. El parser de TOML del repo soporta un subconjunto
  de TOML, no toda la especificación.
- **Nomenclatura:** el vocabulario de acciones dice `view`; la configuración
  sigue diciendo `n_tags` y `workspace` para los mismos objetos. Ambas grafías
  están vivas.

Antes de usar Maverick como único gestor de ventanas para trabajo importante,
valida una sesión X11 desechable en la máquina destino: inicio de sesión y salida
limpia, lanzamiento y cierre de aplicaciones, foco y entrada, fullscreen,
diálogos flotantes y transient, cambio de View, suspensión/reanudación de
pantalla, y cambios de monitor. Ten siempre una forma de volver a la sesión
anterior.

## Licencia

GPL-3.0. Ver [LICENSE](LICENSE).
