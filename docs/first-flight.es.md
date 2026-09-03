# Maverick First Flight

La primera experiencia con un gestor de ventanas define todo: o aterrizas en un
entorno usable, o terminas frente a una pantalla vacia sin saber si algo salio
mal.

**Maverick First Flight** es la propuesta para que Maverick no solo compile e
instale correctamente, sino que tambien prepare una sesion real para la maquina
donde se esta instalando. Detecta lo que ya existe, pregunta solo cuando hay una
decision importante y genera una configuracion pequena, clara y especifica para
ese sistema.

La meta no es esconder como funciona Maverick. La meta es eliminar la friccion
inicial: que el primer arranque sea digno, funcional y facil de diagnosticar.

## Objetivo

First Flight debe convertir este flujo:

```text
compilar -> copiar binarios -> escribir config a mano -> probar suerte
```

en este:

```text
instalar -> detectar entorno -> generar config -> validar -> arrancar
```

El usuario no deberia terminar con una instalacion "casi lista". Deberia
terminar con una sesion arrancable.

## Instalador

`maverick-installer` debe ser el camino recomendado para instalar Maverick desde
fuentes. Su trabajo no es ser un gestor de paquetes universal; su trabajo es
tomar el workspace actual, compilarlo, instalar las piezas correctas y dejar el
sistema listo para iniciar sesion.

Flujo esperado:

```text
1. Compilar el workspace en modo release.
2. Instalar maverick, maverickctl, maverick-msg y maverick-dialog.
3. Instalar o proponer la entrada maverick.desktop.
4. Verificar que el directorio de instalacion este en PATH.
5. Ejecutar First Flight para preparar la configuracion.
6. Generar ~/.config/maverick/config.toml.
7. Validar la configuracion con maverick --check-config.
8. Mostrar un resumen final de decisiones, advertencias y siguientes pasos.
```

El instalador debe ser estricto con lo esencial y flexible con lo opcional. Si
falla la compilacion o no puede instalar los binarios, debe parar. Si falta una
barra, un launcher o una herramienta decorativa, debe avisar y continuar.

## Deteccion Del Sistema

First Flight debe inspeccionar el entorno antes de preguntar. Las preguntas
deben aparecer solo cuando la maquina ofrece varias respuestas validas o cuando
falta una pieza importante.

Debe detectar:

- terminales instaladas;
- launchers disponibles;
- barras compatibles;
- cantidad y geometria de monitores;
- soporte GLX/OpenGL suficiente para el compositor;
- sesion X11 actual;
- directorios XDG relevantes;
- herramientas opcionales como `rofi`, `dmenu`, `waybar`, `polybar`, `feh` o
  conversores de imagen;
- posibles conflictos, como otro compositor ya activo.

El resultado de la deteccion debe poder verse sin escribir archivos:

```bash
maverick-setup --detect
maverick-setup --dry-run
```

## Terminales

La terminal es una dependencia practica para cualquier gestor de ventanas. Si
`Super+Return` no puede abrir nada, Maverick se siente roto aunque el WM este
funcionando.

First Flight debe buscar terminales conocidas, por ejemplo:

```text
alacritty
kitty
ghostty
wezterm
xterm
urxvt
st
gnome-terminal
konsole
xfce4-terminal
lxterminal
```

Politica recomendada:

- si encuentra una terminal, la usa automaticamente;
- si encuentra varias, pregunta cual debe ser la principal;
- si no encuentra ninguna, genera una configuracion segura, pero deja una
  advertencia clara y accionable.

Mensaje sugerido:

```text
No encontre una terminal instalada.
Maverick puede arrancar, pero necesitas instalar una terminal para usar
Super+Return.

Recomendadas: alacritty, kitty, ghostty o xterm.
```

Si hay varias terminales, First Flight no deberia llenar la configuracion con
bindings duplicados. Debe elegir una principal y mantener la config limpia.

## Launchers Y Barras

Para launchers, el camino recomendado en X11 deberia ser `rofi` o `dmenu`. Si
ninguno esta instalado, Maverick puede arrancar, pero el resumen final debe
decirlo claramente.

Orden sugerido:

```text
rofi
dmenu
bemenu
fuzzel
wofi
```

Para barras, Maverick ya soporta docks externos mediante struts EWMH. First
Flight deberia detectar barras comunes y ofrecer una integracion recomendada:

```text
waybar
polybar
xfce4-panel
lxpanel
```

Si detecta una barra compatible, puede proponer autostart. Si no detecta
ninguna, no debe inventarla ni bloquear la instalacion.

## Perfiles

First Flight debe ofrecer pocos perfiles, pero utiles:

```text
Minimal  - Lo esencial: ligero, sobrio, sin adornos innecesarios.
Daily    - Escritorio diario con compositor, launcher y barra si existen.
Laptop   - Atajos y defaults pensados para bateria, brillo y movilidad.
Gaming   - Fullscreen real, reglas para juegos y menos interferencia visual.
Custom   - Pregunta mas detalles y genera una configuracion mas personal.
```

Cada perfil debe modificar solo decisiones concretas:

- compositor activado o desactivado;
- gaps y bordes;
- opacidad;
- reglas de fullscreen;
- autostart;
- foco con mouse;
- cursor warp;
- wallpaper;
- tema visual.

El perfil no debe esconder la configuracion generada. El archivo final debe
seguir siendo legible.

## Configuracion Generada

La configuracion generada no debe ser una copia enorme del archivo de ejemplo.
Debe contener solo lo que First Flight decidio para esa maquina.

Ejemplo:

```toml
[general]
terminal = "alacritty"
launcher = "rofi -show drun"
compositor_enabled = true
n_tags = 9
focus_mouse = false
warp_cursor = false
gaps_inner = 8
gaps_outer = 12
border_width = 2
theme = "catppuccin-mocha"

[wallpaper]
source = "image"
path = "/home/user/Pictures/wallpaper.png"

[[autostart]]
cmd = "waybar"

[[rules]]
window_type = "dialog"
floating = true

[[rules]]
class = "Steam"
true_fullscreen = true
```

La regla principal: lo que no se decidio explicitamente no se escribe. Los
defaults compilados de Maverick deben seguir haciendo su trabajo.

## Primera Sesion

Maverick no debe aparecer como una caja negra en el primer arranque.

La primera sesion puede tener una animacion breve, limpia y util:

- el wallpaper entra con un fade suave;
- la primera columna se desliza a su posicion con la fisica de Maverick;
- el borde de foco pulsa una vez para indicar donde esta la atencion;
- si no hay ventanas abiertas, aparece una ayuda temporal minima.

Ayuda sugerida:

```text
Super+Return   Terminal
Super+P        Launcher
Super+Shift+Q  Salir
```

Esa ayuda no debe ser un tutorial permanente. Debe ser una senal de vida:
aparece una vez, desaparece sola y no estorba.

Si no hay terminal instalada, Maverick no debe fallar en silencio. Debe mostrar
un aviso claro mediante `maverick-dialog` o un fallback X11 simple.

## Diagnostico

First Flight debe complementarse con un comando de diagnostico:

```bash
maverickctl doctor
```

`doctor` debe revisar la sesion actual y explicar problemas comunes:

- config cargada y ruta usada;
- errores o advertencias de configuracion;
- compositor activo o fallback;
- extension GLX faltante;
- otro compositor detectado;
- socket de control activo;
- binds rechazados por otro cliente;
- terminal/launcher/barra no encontrados;
- monitores detectados;
- reglas aplicadas a ventanas comunes.

El diagnostico debe ser entendible para usuarios, pero suficientemente preciso
para reportar bugs.

## Comandos Propuestos

```bash
maverick-setup --interactive
maverick-setup --profile daily
maverick-setup --detect
maverick-setup --dry-run
maverick-setup --write
maverickctl doctor
```

`--dry-run` muestra lo que se generaria sin tocar archivos.

`--write` escribe la configuracion despues de validar que el resultado es TOML
aceptable para Maverick.

`--interactive` debe ser corto: detectar primero, preguntar poco y cerrar con
un resumen.

## Filosofia

Maverick debe sentirse ligero, pero no crudo.

Un gestor de ventanas minimalista no tiene que abandonar al usuario. Puede tomar
buenas decisiones, explicar lo importante y dejar control total cuando el
usuario quiera afinarlo.

First Flight existe para eso: que Maverick pase de "compilo correctamente" a
"puedo usarlo hoy".
