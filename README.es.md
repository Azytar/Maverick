# Maverick

Maverick es un gestor de ventanas con mosaico para X11, escrito en Rust. Ordena
ventanas en una pantalla X11, publica las propiedades EWMH que un escritorio
espera, y hasta ahí llega. Sigue las convenciones de Unix y es deliberadamente
estrecho de alcance: sin compositor, sin panel, sin lanzador, sin demonio de
notificaciones, sin subsistema de wallpaper y sin sistema de animación.

Su modelo se articula en torno a las **Views lógicas**. Cada View contiene un
conjunto de columnas en mosaico y sus propias ventanas flotantes; un **Carousel**
selecciona qué View es la actual; el **layout Scroll** decide dónde van los
clientes en mosaico de la View actual; y un par `DesiredState`/`Reconciler`
convierte esa decisión en peticiones a X11. Una View es un contenedor lógico, no
una ventana X11: crearla, seleccionarla y eliminarla es una transición de
estado, y nada de eso requiere un viaje de ida y vuelta al servidor.

Maverick distribuye dos binarios. `maverick` es el gestor de ventanas.
`maverickctl` es un cliente de control separado que nunca enlaza el gestor de
ventanas ni toca X11; habla con una instancia en marcha a través del socket de
control Unix de esa instancia. La instalación es desde código fuente: no hay
archivos de release ni binarios precompilados.

[Panorámica](#panorámica) · [Lo que Maverick no es](#lo-que-maverick-no-es) ·
[Arquitectura](#arquitectura) · [Estructura del proyecto](#estructura-del-proyecto) ·
[Requisitos](#requisitos) · [Instalación](#instalación) ·
[Ejecutar Maverick](#ejecutar-maverick) · [Atajos](#atajos) ·
[Configuración](#configuración) · [maverickctl](#maverickctl) ·
[Sesiones](#sesiones) · [Diagnóstico de problemas](#diagnóstico-de-problemas) ·
[Compilar desde el código](#compilar-desde-el-código) ·
[Pruebas](#pruebas) · [Estado](#estado) · [Licencia](#licencia)

Léelo en inglés: [README.md](README.md).

## Panorámica

- **Un gestor de ventanas con mosaico para X11, escrito en Rust.** Linux, X11,
  ICCCM y EWMH para lo que un gestor de ventanas necesita. No hay backend de
  Wayland.
- **Construido alrededor de Views lógicas.** Una View es un contenedor de
  ventanas, no una ventana X11. El tipo de Rust detrás de una View es
  `Workspace`.
- **Navegado con un Carousel.** Cada monitor posee un `Carousel` que registra qué
  View es `current` y cuál es `origin`, y se mueve entre ellas paso a paso. La
  navegación es lógica e instantánea.
- **Ordenado por layouts.** Un layout convierte los clientes en mosaico de una
  View en rectángulos. Scroll es el único layout que se proporciona.
- **Los clientes flotantes quedan fuera del layout.** Una ventana flotante
  conserva su propia geometría en espacio de pantalla; el layout en mosaico ni
  la coloca ni la mueve.
- **Materializado con un DesiredState y un Reconciler.** El motor calcula una
  intención pura, el reconciler la compara con lo que X11 ya sostiene, y sólo la
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

### Views e identidad de View

- Cada monitor posee una lista de Views. Un monitor arranca con `n_tags` de ellas
  (9 por defecto, 9 como máximo; el valor se recorta a 9).
- Cada View lleva un `ViewId`: un `u32` acuñado por el Carousel de ese monitor,
  estrictamente creciente y nunca reutilizado. Una View eliminada libera su id
  de forma permanente, así que un `ViewId` obsoleto es *detectable* en lugar de
  apuntar en silencio a lo que heredó la posición anterior.
- Una View está o bien en mosaico (sus columnas) o bien flotante (su propia lista
  de floats). Cada ventana gestionada se referencia desde exactamente una de las
  dos, en exactamente un monitor.
- `view_create` añade una View, hasta 9 por monitor. `view_remove` se rechaza
  mientras la View que nombra siga teniendo clientes: adónde irían es una
  decisión de política, así que la eliminación se rechaza en lugar de adivinarse.
- `view_create` y `view_remove` están expuestos como acciones y por
  `maverickctl view`; no vienen enlazados por defecto.

### Navegación del Carousel

- `view_next` / `view_prev` avanzan un puesto en un carousel **circular**: desde
  la última View, `next` es la primera; desde la primera, `previous` es la
  última.
- `view_return` selecciona el `origin` — la View a la que quedó anclado el
  carousel cuando se creó la primera View de ese monitor. Es una operación total:
  `origin` se repara en cada eliminación, así que nunca puede nombrar una View
  borrada.
- `view <n>` selecciona una View por su posición en la lista del monitor,
  resuelta a través del Carousel, de modo que el cambio es de identidad de View y
  no de índice posicional.
- Crear una View mientras ya existen otras **no** cambia la View actual: sólo la
  transición de vacío a no-vacío convierte una View nueva en actual y ancla el
  origin.
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
- Overview (`toggle_overview`, `overview_nav`, `overview_enter`) es un viewport
  de navegación de escala fija sobre la View actual para elegir una columna: al
  entrar, los tiles se reducen visiblemente a la escala configurada
  (`general.overview_scale`, `0.76` por defecto) y la navegación desplaza el
  viewport en vez de reescalar. Cambia la proyección, no el layout ni la
  pertenencia a una View: al salir, la geometría asentada se recupera exacta.
- `grow_col` redimensiona la columna enfocada en píxeles; `maverickctl resize`
  expresa la misma operación como porcentaje. `new_column` y `collapse_column`
  añaden y quitan columnas.
- Fullscreen y maximize son **presentación**, aplicada después del layout
  (`present::present_into`), no layouts aparte. Una ventana que toma un
  fullscreen exclusivo real también recibe `_NET_WM_BYPASS_COMPOSITOR`
  publicado, para que un compositor externo se aparte de ella.
- `LayoutKind` tiene una sola variante, `Column`. `set_layout` sólo acepta
  `column`, y no existe un segundo layout en este repositorio.

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
  recuerdan estar maximizadas abren como un tile normal. `honor_initial_state` lo
  permite, globalmente o por regla.

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

## Lo que Maverick no es

Maverick es un gestor de ventanas, no un entorno de escritorio. No contiene, no
distribuye y no arranca:

- ningún entorno de escritorio;
- ningún compositor, renderer ni camino por GPU — no hay bucle de frames, ni GL,
  ni Vulkan;
- ningún subsistema de wallpaper — el fondo de la ventana raíz no es una ventana
  que gestionar;
- ningún demonio de notificaciones;
- ningún system tray;
- ningún sistema de animación o transiciones — la geometría se escribe una sola
  vez, en su posición final;
- ningún modo Monocle;
- ningún layout distinto de Scroll.

La composición, un panel, un lanzador, las notificaciones y el wallpaper son
trabajo de otros programas. Maverick arranca los que aparecen en `[autostart]`,
lee los struts que publican y no vuelve a hablar con ellos.

El instalador refleja ese mismo alcance: no hay ningún interruptor para un
compositor, un wallpaper, un componente de animación o un asset de demostración, y
`--with-compositor` y `--no-default-features` se rechazan con estado de salida 2
porque no hay nada que seleccionar.

## Arquitectura

El flujo de alto nivel, desde la View activa hasta la geometría que X11 sostiene:

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

| crate o ruta | responsabilidad |
| --- | --- |
| `maverick` (paquete raíz) | El binario del gestor de ventanas: CLI, selección de configuración, señales, identidad de instancia, arranque del backend |
| `maverick-core/` | Tipos de dominio sin dependencias: `State`, `Monitor`, `Workspace` (una View), `ViewId`, `Carousel`, `Column`, `Client`, `Rect`, `Action` |
| `src/core/` | Motor, acciones/comandos/efectos/eventos, layout, presentación, `DesiredState` |
| `src/backend/x11/` | Eventos, gestión de clientes, entrada, EWMH, struts, reconciliación |
| `src/config.rs`, `src/userconfig.rs` | Defaults compilados, fusión de configuración, validación |
| `maverick-x11/` | Arranque compartido de la conexión Xlib/XCB; enlaza `X11` y `X11-xcb` |
| `maverick-sys/` | Frontera del SO/FFI, identidad de instancia, servidor del protocolo de control, JSON mínimo |
| `maverick-toml/` | Parser de un subconjunto de TOML sin dependencias |
| `maverickctl/` | Cliente de control: CLI, cliente IPC por socket, descubrimiento, ciclo de vida de sesiones |
| `installer/` | El instalador, su biblioteca shell y su suite de tests |
| `tests/` | Sondas sobre X11 real, scripts de integración, test de humo del instalador |

`maverick-core` no depende de X11, del reloj, del sistema de ficheros ni del
entorno, y por eso la mayor parte de la suite de tests no necesita display. La
implementación sí depende de las bibliotecas nativas de X11, y `x11rb` se usa con
una conexión FFI de XCB en lugar de como una pila de protocolo enteramente pura
en Rust. No hay runtime asíncrono ni toolkit de GUI.

`docs/architecture.md` describe las mismas fronteras con anclajes `file:line`.

## Estructura del proyecto

```text
Maverick/
├── Cargo.toml            raíz del workspace, y el binario `maverick`
├── Cargo.lock
├── config/
│   └── config.toml       configuración de ejemplo comentada
├── docs/
│   ├── architecture.md   fronteras entre módulos con anclajes file:line
│   └── sessions.md       el modelo de sesión y su frontera de seguridad
├── installer/
│   ├── install.sh        el instalador
│   ├── lint.sh           bash -n, más shellcheck cuando está disponible
│   ├── lib/              bibliotecas shell de i18n, setup y UI de terminal
│   ├── tests/            suite de comportamiento del instalador
│   └── golden/           salida esperada del instalador, inglés y español
├── maverick-core/        tipos de dominio puros; sin X11, reloj ni sistema de ficheros
├── maverick-sys/         FFI de libc, identidad de instancia, servidor de control
├── maverick-toml/        parser de un subconjunto de TOML sin dependencias
├── maverick-x11/         arranque de la conexión Xlib/XCB
├── maverickctl/          el cliente de control `maverickctl`
├── src/
│   ├── main.rs           CLI, arranque, señales, cableado del backend
│   ├── config.rs         defaults compilados
│   ├── userconfig.rs     parseo, fusión y validación de la configuración
│   ├── types.rs          reexportaciones de tipos del gestor de ventanas
│   ├── log.rs            manejo del nivel de log
│   ├── core/             motor, acciones, layout, presentación, IPC
│   └── backend/x11/      eventos, clientes, entrada, EWMH, struts, reconciliación
├── tests/                sondas sobre X11 real y scripts de integración
├── CHANGELOG.md
├── README.md
├── README.es.md
└── LICENSE
```

## Requisitos

Linux, un servidor X11, un enlazador C y Rust 1.82 o posterior (`rust-version` en
`Cargo.toml`). Maverick enlaza `libX11` y `libX11-xcb`.

```bash
# Arch Linux
sudo pacman -S --needed base-devel rust libx11 libxcb
# Debian / Ubuntu
sudo apt install --no-install-recommends build-essential cargo \
  libx11-dev libx11-xcb-dev libxcb1-dev
# Fedora
sudo dnf install -y cargo gcc libX11-devel libxcb-devel
```

`libX11-xcb` es un paquete de desarrollo aparte en Debian y Ubuntu:
`libx11-dev` no depende de él, así que sin él falta `-lX11-xcb`. En Arch el
único paquete `libx11` incluye tanto `libX11.so` como `libX11-xcb.so`, y por eso
esa línea no nombra ninguno de los dos.

Para una sesión X11 arrancada con `startx` también hacen falta `xorg-server` y
`xorg-xinit` (Arch: `xorg-server xorg-xinit`).

Los atajos compilados por defecto lanzan `alacritty` y `rofi`, y el autostart
compilado lanza `/usr/lib/xdg-desktop-portal` y `/usr/lib/xdg-desktop-portal-gtk`
mediante ruta absoluta. Son conveniencias, no requisitos: los atajos pueden
sobrescribirse y la lista `[autostart] commands` reemplazarse. El motor de layout
no necesita ninguna de ellas.

Variables de entorno relevantes:

| variable | efecto |
| --- | --- |
| `DISPLAY` | la pantalla X a la que Maverick se conecta |
| `XDG_RUNTIME_DIR` | padre del directorio del socket de control; si falta, recurre a `/run/user/$UID`, nunca a `/tmp` |
| `XDG_CONFIG_HOME` | padre de `maverick/config.toml` |
| `MAVERICK_INSTANCE` | selector de instancia por defecto para `maverickctl` |
| `MAVERICK_SESSION` | selector de sesión usado por las herramientas de sesión |
| `MAVERICK_LOG` | nivel de log; `--debug` equivale a `MAVERICK_LOG=debug` |

## Instalación

La instalación es desde código fuente. El instalador compila ambos binarios desde
este workspace con `cargo build --release -p maverick -p maverickctl`.

```bash
git clone https://github.com/azytar/Maverick.git
cd Maverick
./installer/install.sh
```

El prefijo por defecto es `$HOME/.local`, que no necesita privilegios. El
instalador se niega a ejecutarse como root, nunca invoca `sudo` y nunca habilita
ningún servicio.

```text
--system               instalar en /usr/local en lugar del prefijo por defecto
--prefix DIR           instalar en DIR
--xsessions-dir DIR    instalar además el fichero de sesión en DIR (la única
                       escritura que sale del prefijo; desactivada por defecto
                       porque los gestores de pantalla suelen leer sólo
                       ubicaciones del sistema)
--lang LANG            forzar el idioma del instalador: en | es | auto
--yes, -y              omitir las confirmaciones del instalador
--no-config            no crear fichero de configuración
--no-build             omitir la compilación y usar el $CARGO_TARGET_DIR/release
                       existente
--add-path             añadir el directorio bin a PATH sin preguntar
--no-path              no modificar ficheros de inicio del shell; imprimir la
                       línea de export en su lugar
--no-anim              desactivar la animación de terminal del propio instalador
--keep-log             conservar el log de compilación incluso si hay éxito
-h, --help             mostrar todas las opciones
```

Lo que instala, y dónde:

| ruta | qué |
| --- | --- |
| `<prefix>/bin/maverick` | el gestor de ventanas |
| `<prefix>/bin/maverickctl` | el cliente de control |
| `<prefix>/share/xsessions/maverick.desktop` | la entrada de sesión X11 |

Fuera del prefijo el instalador sólo escribe ficheros del usuario que lo invoca,
y sólo tras confirmación:

- `${XDG_CONFIG_HOME:-$HOME/.config}/maverick/config.toml`, sembrado desde
  [`config/config.toml`](config/config.toml) salvo que ya exista uno
  (`--no-config` omite este paso; rechazar la sobrescritura conserva el
  fichero existente);
- un bloque marcado y autoprotegido en un fichero de inicio del shell bajo
  `$HOME`, que se ofrece sólo cuando el directorio bin falta en `PATH`, y nunca
  con `--no-path`.

El prefijo es una frontera estricta: fuera de él no se crea ni se modifica nada
salvo esos dos ficheros. Un prefijo en el que no se puede escribir se informa como
error de permisos en lugar de escalarse con `sudo`, así que una instalación en
todo el sistema necesita acceso de escritura a `/usr/local` previsto de antemano.

Cada paso falla de forma ruidosa. El instalador ejecuta los binarios que acaba de
instalar — `maverick --version`, `maverickctl --help`,
`maverickctl session --help` — y un conjunto parcial, obsoleto o roto se informa
como fallo y no como instalación correcta. Es seguro ejecutarlo repetidamente:
una segunda pasada converge, corrige permisos hostiles al umask y no duplica el
bloque de `PATH`.

`CARGO_TARGET_DIR` se respeta tal cual. Cuando no está definido la compilación
ocurre en un directorio de caché bajo `$XDG_CACHE_HOME`, y el checkout nunca se
usa como directorio de compilación. El primer intento de compilación pasa
`-C target-cpu=native` y cae a una compilación normal si falla, de modo que el
binario instalado queda ajustado para la máquina que lo compiló; conviene un
`cargo build` normal cuando se necesiten artefactos para otra CPU.

Verificar una instalación con:

```bash
maverick --version
maverickctl --version
```

Para desinstalar, borrar los dos binarios y el fichero de sesión del prefijo, y
eliminar el bloque delimitado por las marcas `# >>> maverick (install.sh) >>>` de
cualquier fichero de inicio que el instalador haya tocado.

Ver [`installer/README.md`](installer/README.md) para la documentación propia del
instalador y su suite de tests.

## Ejecutar Maverick

Para una sesión `startx`, esto va al final de `~/.xinitrc`:

```sh
exec maverick
```

Como alternativa, seleccionar la sesión Maverick instalada en el gestor de
pantalla.

```bash
maverick --check-config ~/.config/maverick/config.toml   # validar, no arrancar nada
maverick --config ~/.config/maverick/config.toml --name desktop
maverick --help
```

| flag | efecto |
| --- | --- |
| `--name <id>` | etiquetar la instancia para control e identificación |
| `--session-id <id>` | publicar la instancia con un id de sesión fijo (`[A-Za-z0-9_-]`), que es lo que usa `maverickctl session`; por defecto es aleatorio |
| `--replace` | tomar el relevo de un gestor de ventanas en marcha, adoptando sus ventanas |
| `--debug` | log en nivel debug, equivalente a `MAVERICK_LOG=debug` |
| `--log-level <nivel>` | `off`, `error`, `warn`, `info`, `debug` o `trace`; tiene precedencia sobre `--debug` |
| `--config <ruta>` | leer la configuración de `<ruta>` en lugar de `$XDG_CONFIG_HOME/maverick/config.toml`; reutilizado por reload y restart |
| `--check-config [ruta]` | validar una configuración y salir: `0` limpio, `1` con avisos o errores. No arranca ningún gestor de ventanas ni abre display |
| `-v`, `--version` | imprimir la versión y salir |
| `-h`, `--help` | mostrar la ayuda incorporada |

## Atajos

`Super` es Mod4, normalmente la tecla Windows. La tabla siguiente es el conjunto
**compilado por defecto**: 33 atajos explícitos más un `Super+<dígito>` y un
`Super+Shift+<dígito>` por View, 51 en total.

| acción | atajo | comportamiento |
| --- | --- | --- |
| Lanzar una terminal | `Super+Return` | ejecuta `alacritty` |
| Lanzador, ejecutar un comando | `Super+Shift+P` | ejecuta `rofi -show run` |
| Lanzador, ejecutar una entrada de escritorio | `Super+P` | ejecuta `rofi -show drun` |
| Cerrar la ventana enfocada | `Super+Shift+C` | pide al cliente que cierre |
| Conmutar flotante | `Super+Shift+Space` | mueve la ventana dentro o fuera del layout |
| Conmutar fullscreen | `Super+Shift+F` | overlay de fullscreen exclusivo real |
| Conmutar maximize | `Super+Shift+M` | maximize sólo de presentación |
| Enfocar izquierda / abajo / arriba / derecha | `Super+H` / `J` / `K` / `L` | mueve el foco dentro de la View |
| Mover ventana izquierda / abajo / arriba / derecha | `Super+Shift+H` / `J` / `K` / `L` | mueve el cliente a una columna vecina |
| Nueva columna | `Super+Shift+Return` | añade una columna al ribbon |
| Encoger columna | `Super+Ctrl+H` | `grow_col:-50` |
| Agrandar columna | `Super+Ctrl+L` | `grow_col:50` |
| Plegar columna | `Super+Ctrl+J` | elimina la columna enfocada |
| Fijar el layout | `Super+T` | `layout:column`; el único valor aceptado |
| Salir | `Super+Shift+Q` | apagado ordenado: pide cerrar a los clientes, espera, fuerza el cierre del resto y limpia |
| Reiniciar | `Super+Shift+R`, `Super+F5` | se reejecuta en el sitio con los mismos argumentos |
| Enfocar el monitor siguiente | `Super+Tab` | cicla por el orden de enumeración de monitores |
| Mover la ventana al monitor siguiente | `Super+Shift+Tab` | cicla por el orden de enumeración de monitores |
| Overview: alternar / entrar / siguiente / anterior | `Super+O` / `Super+E` / `Super+N` / `Super+Shift+O` | viewport de escala fija para elegir columna |
| Zoom del viewport dentro / fuera | `Super+=` / `Super+-` | agranda o restaura el ribbon |
| Page-snap derecha / izquierda | `Super+]` / `Super+[` | mueve la cámara una pantalla |
| Seleccionar View | `Super+1` … `Super+9` | generados por View, hasta `n_tags` |
| Enviar ventana a una View | `Super+Shift+1` … `Super+Shift+9` | generados por View |

Los atajos de dígitos generados siguen a `n_tags`. Fijar
`auto_workspace_binds = false` en `[general]` los suprime, dejando la fila de
dígitos completamente sin gestionar.

Los pasos del carousel (`view_next`, `view_prev`, `view_return`) y el ciclo de
vida de las Views (`view_create`, `view_remove`) **no** vienen enlazados por
defecto. Son accesibles por `maverickctl view`, o mediante una tabla
`[[keybindings]]`.

Los atajos se resuelven a través del layout XKB activo, así que una combinación
coincide con lo que el teclado produce realmente y no con el código nominal. Una
combinación escrita en un fichero de configuración usa nombres de keysym de X con
modificadores tipo `Mod4`/`Mod1`, por ejemplo `Mod4+Shift+Return` o
`Mod4+Control+h`.

La configuración de ejemplo de [`config/config.toml`](config/config.toml) es un
preset y no una copia de los defaults compilados: enlaza el zoom del viewport y
el page-snap a otras teclas y añade reglas, un tema y una lista de autostart, así
que usarla como punto de partida reemplaza los atajos compilados.

## Configuración

La configuración es opcional. Maverick lee
`$XDG_CONFIG_HOME/maverick/config.toml`, con recurso a
`~/.config/maverick/config.toml`, y usa los defaults compilados para todo lo que
el fichero no fije.

Un fichero mínimo que funciona:

```toml
[general]
column_width = 0.5
gaps_inner = 10
gaps_outer = 14
focus_mouse = false
```

Defaults compilados de las claves `[general]` más relevantes:

| clave | default | significado |
| --- | --- | --- |
| `n_tags` | `9` | Views por monitor al arrancar; recortado a un máximo de 9 |
| `column_width` | `0.6` | ancho de columna como fracción del workarea del monitor; debe estar entre `0.1` y `1.0` |
| `gaps_inner` | `4` | hueco entre tiles adyacentes |
| `gaps_outer` | `8` | hueco entre los tiles y el borde de la pantalla |
| `border_width` | `1` | grosor del borde del tile en píxeles |
| `focus_mouse` | `false` | si mover el puntero cambia el foco |
| `honor_initial_state` | `false` | respetar el estado maximizado/fullscreen pedido al mapearse, en lugar de normalizarlo |
| `auto_workspace_binds` | `true` | generar los atajos de View `Super+<dígito>` |
| `smart_gaps` | `false` | suprimir los huecos exteriores cuando sólo hay un tile visible |
| `corner_radius` | `0` | radio de esquina del tile en píxeles |
| `theme` | `catppuccin-mocha` | nombre del tema integrado |
| `tag_names` | `["1"]` … `["9"]` | etiquetas de View publicadas como `_NET_DESKTOP_NAMES` |

Otras claves `[general]` aceptadas: `gaps`, `accordion_boost`,
`overview_scale`, `overview_zoom_min` y `warp_cursor`. `border_w` es un alias de
`border_width`. `overview_scale` es la escala a la que se entra en Overview
(`0.05`–`1.0`, por defecto `0.76`); `overview_zoom_min` es el suelo por debajo
del cual esa escala no baja cuando el tile enfocado no cabría ni siquiera
reducido.

Dos claves son alias obsoletos conservados por compatibilidad. Ambas se siguen
cargando, y ambas emiten un aviso:

| clave obsoleta | alias | tipo | sustituida por |
| --- | --- | --- | --- |
| `default_col_width` | `default_col_w` | píxeles | `column_width`, convirtiendo contra un workarea fijo de 1920px |
| `split_bias` | — | fracción, `0.0`–`1.0` | `column_width` |

`[colors]` acepta `normal`, `focused` y `urgent`, legibles también como
`col_normal`, `col_focused` y `col_urgent`.

### Cómo se combina un fichero con los defaults

- Los ajustes ordinarios se fusionan campo a campo.
- `[[keybindings]]` y `[[rules]]` **reemplazan** la lista compilada por completo
  cuando se declaran. Declarar un solo `[[rules]]` descarta la política compilada
  de float por aplicación, así que hay que repetir las entradas necesarias.

### Comportamiento ante un fichero mal formado o incompleto

- Un TOML mal formado recurre a los defaults compilados.
- Una entrada individual inválida se diagnostica y se ignora; el resto del
  fichero se carga igualmente.
- Una tabla desconocida se salta en silencio, de modo que un fichero escrito para
  otro Maverick no produce ruido.
- Una clave desconocida dentro de una tabla que Maverick sí conoce se informa como
  aviso y se ignora.

Conviene validar antes de depender de cualquiera de estos casos:

```bash
maverick --check-config ~/.config/maverick/config.toml
```

[`config/config.toml`](config/config.toml) es una muestra comentada que cubre el
vocabulario más amplio. Es un preset, no una copia de los defaults compilados.

### Reglas de aplicación

`class`, `instance` y `title` coinciden con subcadenas sin distinción de mayúsculas
de las cadenas propias de la ventana; `window_type` (alias `type`) coincide con un
nombre `_NET_WM_WINDOW_TYPE` normalizado y completo. Todos los criterios
presentes en una regla deben coincidir.

```toml
[[rules]]
class = "calculator"
float = true
size = [480, 360]
position = [120, 100]
```

| clave de regla | alias | efecto |
| --- | --- | --- |
| `class`, `instance`, `title` | — | coincidencia por subcadena sin distinguir mayúsculas |
| `window_type` | `type` | un nombre `_NET_WM_WINDOW_TYPE` normalizado y completo |
| `float` | — | gestionar la ventana como cliente flotante |
| `sticky` | — | mostrar la ventana en todas las Views de su monitor |
| `workspace` | `ws` | colocar la ventana en esta View, numerada desde 1 |
| `size` | — | `[ancho, alto]` para una ventana flotante |
| `position` | — | `[x, y]` respecto al origen del workarea |
| `opacity` | — | opacidad de la ventana |
| `border_width` | `border_w` | grosor de borde por ventana |
| `honor_initial_state` | — | eximir esta ventana de la normalización del estado inicial |
| `ignore_initial_state` | `no_initial_state`, `no_maximize` | lo contrario de lo anterior |
| `deny_fullscreen` | `no_fullscreen` | rechazar las peticiones de fullscreen del propio cliente, no el atajo `Super+Shift+F` |
| `true_fullscreen` | `exclusive_fullscreen` | pedir un overlay realmente exclusivo; tiene precedencia sobre `deny_fullscreen` |

### Autostart

`[autostart] commands` es una lista de listas de argumentos:

```toml
[autostart]
commands = [["polybar", "main"], ["picom", "--vsync"]]
```

Una lista no vacía reemplaza la compilada. Aquí es donde pertenece un compositor,
un panel o un programa de wallpaper: Maverick arranca el comando y no vuelve a
hablar con él. Los docks que publican struts reservan workarea automáticamente.
El arranque de sesión y el reinicio no son un supervisor general de servicios.

## maverickctl

`maverickctl` es un cliente de control separado. Nunca enlaza el gestor de
ventanas ni abre un display X; habla con una instancia en marcha a través del
socket de control Unix de esa instancia, verificado contra pares con `SO_PEERCRED`
dentro de un directorio de runtime privado `0700`.

```bash
maverickctl --version
maverickctl list
maverickctl state --name desktop
maverickctl query tree --name desktop
maverickctl msg view 3 --name desktop
maverickctl subscribe --name desktop
maverickctl reload --name desktop
maverickctl restart --name desktop
maverickctl quit --name desktop --confirm
```

Las opciones globales pueden aparecer en cualquier punto de la línea y nunca se
reenvían a la instancia como parte de una acción:

| opción | efecto |
| --- | --- |
| `-v`, `--version` | imprimir la versión y salir |
| `-j`, `--json` | salida legible por máquina donde el comando produce un documento |
| `-y`, `--yes` | omitir una pregunta de confirmación |
| `-s`, `--session <sid>` | id de sesión explícito, obtainable con `list` |
| `-n`, `--name <id>` | etiqueta de instancia, o id de sesión |

La selección de instancia prefiere `--session`, luego `--name`, luego
`$MAVERICK_INSTANCE`, luego la única instancia del `DISPLAY`/TTY actual. Cuando
coinciden varios candidatos, el descubrimiento se niega a adivinar.

Las Views y las ventanas se direccionan semánticamente, y toda operación es la
misma acción que ejecuta un atajo de teclado:

```bash
maverickctl view debug next
maverickctl view debug goto 3
maverickctl window list debug --json
maverickctl window focus debug firefox
maverickctl window float debug 0x42003
maverickctl resize debug +10%
maverickctl process list debug --json
maverickctl inspect debug
```

Una ventana se direcciona por su id X11 (`0x42003`) o por un nombre que coincide
con la clase, el nombre de instancia y el título — primero se intenta una
coincidencia exacta y después una coincidencia por subcadena, y un nombre ambiguo
se rechaza indicando los ids candidatos en lugar de adivinar. Omitir la ventana
actúa sobre la enfocada.

Cualquier palabra que `maverickctl` no reconozca como comando se reenvía tal cual
al gestor de ventanas, que es lo único que puede distinguir una acción de un tema
de consulta de una errata.

`maverickctl <grupo> --help` documenta un grupo completo: `session`, `window`,
`process`. `maverickctl --help` ofrece la lista completa de comandos.

## Sesiones

`maverickctl session` gestiona sesiones gráficas completas: un servidor X
anidado, un Maverick, los programas lanzados en ella, una cookie, logs y un
ciclo de vida.

```bash
maverickctl session create work --resolution 1920x1080
maverickctl session status work --json
maverickctl session logs work -f
maverickctl session stop work
maverickctl session remove work
```

Cada sesión ejecuta un servidor X anidado —`xephyr` por defecto, que es visible, o
`xvfb` con `--backend xvfb`. La identidad, el socket y los logs de la sesión
viven bajo el directorio de runtime privado, y una sesión se niega a arrancar en
una display que no haya reclamado de forma exclusiva.

Detener una sesión pide a sus clientes que cierren mediante `WM_DELETE_WINDOW` y
fuerza el cierre de los supervivientes tras una espera acotada, así que conviene
guardar el trabajo pendiente antes.

Ver [`docs/sessions.md`](docs/sessions.md) para el modelo de sesión completo, sus
limitaciones y su frontera de seguridad.

## Diagnóstico de problemas

**`maverickctl` no encuentra la instancia.** El descubrimiento se niega a adivinar
cuando más de un candidato coincide con el display. Listar los candidatos con
`maverickctl list` y seleccionar de forma explícita con `--name` o `--session`.

**Un cambio de configuración no surtió efecto.** `reload` es un no-op cuando el
binario en marcha se compiló únicamente con configuración compilada; en ese caso
conviene reiniciar la instancia. `--check-config <ruta>` informa de si un fichero
parsea y si tiene claves desconocidas.

**Un atajo no se dispara.** Los atajos se resuelven a través del layout XKB
activo. Una tabla `[[keybindings]]` reemplaza la lista compilada por completo, así
que declarar una elimina todos los defaults que no se repitan en ella.

**Una ventana abre maximizada o a pantalla completa cuando no debería.** El
`_NET_WM_STATE` pedido al mapearse se normaliza por defecto. Fijar
`honor_initial_state = true` en `[general]`, o por regla, conserva el estado
propio del cliente.

**Se informa de un `ViewId` obsoleto.** Los ids de View nunca se reutilizan, así
que una referencia a una View eliminada es detectable en lugar de redirigirse en
silencio. Volver a seleccionar la View por posición con
`maverickctl view <sesión> goto <n>`.

**El instalador sale con estado 2.** `--with-compositor`,
`--without-compositor`, `--no-compositor` y `--no-default-features` se rechazan:
Maverick no tiene compositor y la compilación no tiene característica por
defecto, así que no hay nada que seleccionar.

**El instalador falla en un prefijo en el que no se puede escribir.** El prefijo
es una frontera estricta y nunca se escala con `sudo`. Usar `--prefix` con una
ubicación escribible, o previsto de antemano el acceso de escritura a
`/usr/local` para `--system`.

**Hace falta más detalle sobre la entrada o el estado de las ventanas.**
Compilar con las características opt-in `input-trace` y `window-trace` para añadir
trazas estructuradas. Ambas están desactivadas por defecto y sólo añaden log:

```bash
cargo build -p maverick --features input-trace,window-trace
./target/debug/maverick --config /ruta/a/config.toml 2> /tmp/maverick-debug.log
```

Ejecutar eso sólo sobre un `DISPLAY` previsto para entregarse a un gestor de
ventanas.

**La animación del instalador embarulla la salida canalizada.** Usar `--no-anim`,
o exportar `MAVERICK_NO_ANIM=1`. No alcanza a ninguna compilación de cargo:
Maverick dibuja a través de X11 y no tiene subsistema de animación.

## Compilar desde el código

```bash
cargo build --release -p maverick -p maverickctl
cargo check --workspace --all-targets
cargo fmt --all -- --check      # forma de solo lectura; `cargo fmt --all` aplica
```

## Pruebas

```bash
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

El layout, el Carousel y el trabajo de comandos está cubierto por tests de estado
puros, que no necesitan display. El comportamiento de protocolo, apilado y foco
usa un servidor X aislado.

El instalador tiene sus propias comprobaciones:

```bash
bash installer/lint.sh                  # bash -n, y shellcheck cuando está disponible
python3 installer/tests/partition.py    # la suite de comportamiento del instalador
```

Los harnesses sobre X11 real viven en `tests/`. `tests/xvfb-stacking.py` es la
regresión automatizada, y los scripts `tests/xephyr-*.sh` son escenarios de
integración manuales. **No** están todos aislados con el mismo rigor —algunos
helpers antiguos de `tests/common.sh` matan procesos por nombre o usan displays
fijos—, así que conviene leer un script antes de ejecutarlo y reservar la suite
legacy para una sesión gráfica desechable.

CI (`.github/workflows/ci.yml`) ejecuta tres trabajos: el workspace con Clippy
estricto y ambos conjuntos de características, las comprobaciones del instalador,
y un test de humo de apilado con Xvfb.

## Estado

La versión canónica se declara una sola vez, en `[workspace.package]` en
`Cargo.toml`, y cada paquete la hereda. El árbol lleva actualmente **1.1.1**,
que es la release actual. Los cambios hechos desde la última release están en
`[Unreleased]` dentro de [`CHANGELOG.md`](CHANGELOG.md). La release más
reciente es **1.1.1**; el historial completo está ahí también.

Maverick está en preview. No se declara lista para producción, y los scripts de
integración son comprobaciones de regresión y no una certificación de compatibilidad
de aplicaciones.

- **Alcance:** sólo Linux y X11. Sin backend de Wayland, compositor, subsistema de
  animación, shell de escritorio, desenfoque ni sombras.
- **Layouts:** Scroll es el único layout. `LayoutKind` tiene una variante; un
  segundo layout no está implementado y no se documenta como si lo estuviera.
- **Views:** como máximo 9 por monitor. `n_tags` fija cuántas existen al arrancar, y
  la fila de dígitos no tiene décima tecla.
- **Compatibilidad:** ICCCM y EWMH están implementados para lo que el gestor de
  ventanas necesita, lo que no equivale a una cobertura completa del protocolo o
  de las aplicaciones.
- **Monitores:** el foco y el movimiento ciclan por el orden de enumeración de
  monitores, no por dirección física. La recuperación de topología usa rectángulos
  e índices, no identidades estables de conector, así que un hotplug o
  reordenamiento arbitrario no preserva las asignaciones.
- **Geometría:** X11 tiene un único espacio global de coordenadas raíz con límites
  de tamaño y coordenada de protocolo; la proyección de Scroll y los workareas
  multimonitor los respetan.
- **Interfaces:** la configuración, las APIs internas y la política de
  presentación pueden cambiar. El parser TOML del repo soporta un subconjunto de
  TOML, no la especificación completa.
- **Nomenclatura:** el vocabulario de acciones dice `view`; la configuración sigue
  diciendo `n_tags` y `workspace` para los mismos objetos. Ambas grafías están
  vivas.

Antes de usar Maverick como único gestor de ventanas para trabajo importante,
validar una sesión X11 desechable en la máquina destino: inicio y salida limpia,
lanzamiento y cierre de aplicaciones, foco y entrada, fullscreen, flotantes y
diálogos transitorios, cambio de View, suspensión/reactivación de display y
cambios de monitor. Conviene conservar una vía de vuelta a la sesión anterior.

## Licencia

GPL-3.0. Ver [LICENSE](LICENSE).