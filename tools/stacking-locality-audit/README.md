# Auditoría de apilamiento Xorg

Sonda pasiva para estudiar si los tiles de Maverick forman pocos bloques en el
orden raíz. **No cambia Maverick ni implementa superficies segmentadas.**
Base inspeccionada: `e6c67fdaaddbe1093dcd0914bc536ecb86ecc41d`.

## Instalar y comprobar

Linux con Python >=3.9, compilador C, headers/libX11, Maverick y maverickctl.
Para iniciar desde consola: Xorg, `startx`/xinit, xauth y una sesión local logind
con acceso al dispositivo DRM. Matplotlib solo es necesario para analizar gráficos.
En Debian/Ubuntu los paquetes de herramientas son `build-essential libx11-dev
python3 python3-matplotlib xinit xauth xserver-xorg`; las pruebas usan además `xvfb`.
La herramienta no instala paquetes ni usa sudo.

```sh
git clone --branch tools/xorg-stacking-audit --single-branch https://github.com/Azytar/Maverick.git
cd Maverick
# Si no tienes Maverick/maverickctl instalados:
cargo build --release -p maverick -p maverickctl --locked
export PATH="$PWD/target/release:$PATH"
A=./tools/stacking-locality-audit/run
```

Dentro de tu sesión Xorg existente:

```sh
maverickctl list
"$A" check --instance default
```

Usa el nombre real que muestra `list`; `default` es el predeterminado del binario
inspeccionado. Puedes pasar `--wm /ruta/maverick` y `--ctl /ruta/maverickctl`.
`check` compila la sonda con comprobaciones estrictas y la cachea fuera del repositorio.
No abre ventanas. Debe autenticar en el DISPLAY seleccionado y comprobar que la
instancia Maverick pertenece a esa misma pantalla.

## Sesión limpia desde TTY, captura de diez minutos

1. Guarda tu trabajo y **cierra la sesión gráfica anterior**. Cambiar de VT no es
   cerrar sesión. Si el gestor de login mantiene un greeter Xorg/Wayland activo,
   utiliza el modo de arranque en consola de tu distribución antes de continuar;
   la herramienta no detiene ese servicio ni otros servidores.
2. Inicia sesión como usuario normal en una TTY física local activa. No uses
   terminal SSH, sudo ni una pseudoterminal dentro del escritorio.
3. Desde el clon, con los binarios en PATH, ejecuta:

```sh
./tools/stacking-locality-audit/run tty --preview
./tools/stacking-locality-audit/run tty --seconds 600
```

El modo `tty` comprueba VT, asiento logind, ausencia de sesión gráfica en él,
servidores existentes, acceso DRM y disponibilidad de display/lock. Elige otro
número libre con `--display-number 1`; **no borres locks para forzar el inicio**.

Inicia Xorg con `startx`, un nombre nuevo de Maverick y la captura como cliente
principal de xinit. No toca `~/.xinitrc` ni crea terminales o ventanas de prueba.
Usa los atajos normales de tu configuración para abrir aplicaciones y trabajar;
puedes pasar `--config /ruta/a/tu/config.toml`. La configuración/autostart que
elijas puede abrir tus aplicaciones; la sonda no las genera.

Al cumplirse el tiempo, termina solo el WM y el Xorg de esa ejecución y vuelve
a la TTY. Si sales antes de Maverick, se detiene y conserva la captura. Ctrl+C
desde la TTY interrumpe la ejecución; si Xorg cae antes de finalizar la escritura,
los datos parciales permanecen con `complete=false`. No se presentan como sesión completa.

**No se ha probado este arranque en hardware físico en el entorno de desarrollo.**
Se probaron sus condiciones de rechazo y el ciclo del recolector con Xvfb.

## Capturar una sesión existente

```sh
"$A" capture --instance default --seconds 600 --preview
"$A" capture --instance default --seconds 600
# Opcional: salida explícita nueva, siempre fuera del clon.
"$A" capture --instance default --seconds 600 --out /ruta/privada/captura-01
```

No inicia ni detiene el WM o el servidor de esa sesión. SIGINT/SIGTERM detienen el
recolector ordenadamente. Se conserva la salida aunque falle la captura.

La confirmación física exige que las credenciales del socket X11 apunten al
proceso Xorg y que este tenga un dispositivo DRM `card*` y la VT correspondiente.
Xvfb, Xwayland y Xephyr no cuentan como físicos. Si `/proc` o el driver impiden
verificarlo, se aborta. `--allow-unconfirmed` permite capturar con etiqueta
**`x11-unconfirmed`**; nunca convierte ese resultado en evidencia física.
En el modo TTY también se comprueba que el servidor sea nuevo y de la VT solicitada.

## Resultados, privacidad y análisis

Por defecto quedan bajo `$XDG_STATE_HOME/maverick-stacking-audit/`, o
`~/.local/state/maverick-stacking-audit/`, en una carpeta nueva 0700 y archivos 0600.
La caché compilada queda en `$XDG_CACHE_HOME/maverick-stacking-audit/` o `~/.cache/`.
La ruta final se imprime; no se sobrescriben capturas ni se suben resultados.

```sh
"$A" privacy /ruta/captura-01
"$A" analyze /ruta/captura-01 /ruta/captura-02 --out /ruta/nuevo-analisis
# Revisa la ruta exacta antes de borrar tus datos privados:
rm -r -- /ruta/captura-01
```

`privacy` valida el esquema de `raw.jsonl`, sus seudónimos y su hash cuando está
completa. No certifica otros archivos que añadas a esa carpeta: revísalos antes
de compartirlos. Los XID reales no se guardan; los alias son locales a cada
captura y no pueden correlacionarse automáticamente entre sesiones.

No se consulta título, clase, PID ni contenido mediante X11. La consulta IPC
existente `query tree` incluye títulos/clases/PID junto a los campos necesarios:
se reciben transitoriamente en memoria y se descartan antes de persistir. No hay
una consulta IPC con proyección de campos en la base inspeccionada. Si no aceptas
ese acceso transitorio, añade `--native-only` a `check`, `capture` o `tty`: no
consulta el árbol IPC de aplicaciones y deja desconocidas las columnas/políticas.
Sí comprueba la identidad de la instancia WM para no elegir otra pantalla.

Los snapshots son brackets no atómicos. El coalescing predeterminado de 250 ms
puede perder estados breves; el heartbeat es de 5 s. Se informa permanencia
estimada y transiciones, sin contar snapshots idénticos como observaciones
independientes. Pertenencia inferida y políticas desconocidas no prueban
compatibilidad; fullscreen, maximización, otros monitores y ventanas aparcadas
requieren cautela. El analizador no mezcla clases de entorno diferentes.

## Pruebas y procedencia

```sh
python3 tools/stacking-locality-audit/test_audit.py
python3 tools/stacking-locality-audit/test_launch.py
python3 tools/stacking-locality-audit/test_launch.py \
  --wm "$(command -v maverick)" --ctl "$(command -v maverickctl)" --out /tmp/nueva-prueba-lanzador
python3 tools/stacking-locality-audit/validate.py \
  --wm "$(command -v maverick)" --ctl "$(command -v maverickctl)" \
  --out /tmp/nueva-prueba-apilamiento --sessions 1
```

Las dos últimas pruebas crean únicamente sus propios Xvfb/clientes y requieren
un usuario normal. La confirmación positiva de hardware se simula en tests de
contrato; no equivale a una prueba física. `PROVENANCE.json` registra los hashes
de las fuentes definitivas importadas, sin capturas ni binarios de laboratorio.
