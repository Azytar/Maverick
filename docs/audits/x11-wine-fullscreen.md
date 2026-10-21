# Auditoría X11/Wine/fullscreen de Maverick

Fecha de investigación: 20 de octubre de 2026 (America/Lima). Entorno Debian 13,
Rust 1.99.0, Xvfb privado 1280×800×24. No se ejecutó Wine ni Blood Strike.

## Resultado técnico y alcance

Se reprodujo un defecto general de progreso en el bucle X11, sin Wine: una
secuencia de map/unmap/remap puede dejar propiedades EWMH pendientes de publicar
y eventos pendientes de despachar mientras el WM espera otra señal del socket.
Las consultas X11 de otro cliente no despiertan al WM. Una ventana desmapeada
conservó su recurso X11; la traza mostraba que Maverick ya había eliminado su
`Client`, mientras `_NET_CLIENT_LIST` seguía incluyéndola durante el plazo de cinco
segundos de la prueba. Otra ejecución de la corrección incompleta se detuvo en
remap. **No se ha demostrado que Blood Strike active esta ruta.**

El arreglo publica las listas antes de la espera y consulta no bloqueantemente
la cola de eventos antes de confiar en la legibilidad del descriptor. No cambia
las políticas de fullscreen, foco o retirada ICCCM. El cliente de regresión
comprueba las propiedades y los atributos del servidor, no solo flags Rust.

Las transferencias sintéticas a hija, popup override-redirect, diálogo,
ventana ajena y raíz no borraron fullscreen en la versión corregida. Eso no
prueba qué hace Wine, su foco interno ni un fullscreen exclusivo real.

El trabajo reside en el worktree `/workspace/work/maverick-audit`, rama
`audit/x11-wine-fullscreen`, basado en `fix/vertical-focus-and-float-stacking`.
La rama `work` del checkout original permanece seleccionada y sus archivos fuente
no se modificaron. Se reutilizó su directorio de compilación como caché.

## Ramas, ancestros y cambios

| Referencia remota recuperada | Commit exacto |
|---|---|
| main | `1772dfd0e4f368855d00b03df39b1e100af6f1f3` |
| feat/restore-layout | `f5d7edba50c1e9f78a3f9e60237aa51cefa707b0` |
| fix/ewmh-state-and-sessions | `3b3b2a176b1d576ddad5a106279ae5074dcfafcc` |
| fix/vertical-focus-and-float-stacking | `3fd91d3c242af5bfc89741d621000b50ebd57e7a` |

Las cuatro referencias forman una cadena de ancestros, en el orden de la tabla.
Por tanto el merge-base de cada par es la referencia anterior. El `origin/main`
local estaba inicialmente en `72b846d`; `fetch` lo actualizó. La configuración
inicial de fetch solo traía main; se recuperaron las demás referencias con
refspecs explícitos. Las conclusiones se refieren a estos hashes, no a nombres
mutables de ramas.

Cambios pertinentes:

- `5d5a2cd`: preserva los dos pasos de restauración de un flotante maximizado que
  entra en fullscreen; separación de snapshot fullscreen y geometría normal.
- `f5d7edb`: agrega `RestoreLayout`, `apply_fullscreen` y
  `SyncWindowPresentation`. Restaurar es una acción explícita y no un manejador
  de FocusOut.
- `7cadfa2`: rechaza acciones EWMH mayores que 2; el defecto de interpretarlas
  como TOGGLE está en main/restore, ya corregido en ewmh y vertical.
- `4a217e8`: aplica/publica ADD, REMOVE y TOGGLE de atención.
- `f8aebc8`: absorbe maximización sin cambios; evita trabajo y cambios de cámara
  por ADD repetido o REMOVE sobre un eje ya normal.
- `7cedcaf`: corrige metadatos de desktops/workareas; `5526826` y `3b3b2a1`
  tratan sesiones y procesos. No atribuir a estos commits una destrucción X11.
- Entre ewmh y vertical: `a4c6dc5` cambia navegación vertical; `8990f01` ordena
  floats por foco y dependencia transitoria; `aa8f939` publica el stacking real;
  `3fd91d3` cambia la espera del hilo de control. Estos cambios no añaden una
  transición fullscreen a FocusOut/UnmapNotify.
- Ya en main: `9db7d4c` evita contrafoco a no gestionadas; `52fe35d` ofrece
  WM_TAKE_FOCUS a clientes Globally Active; `8c151e3` usa tiempo de servidor;
  `25f76f3` acepta familias ICCCM; `496a00d` limita las intenciones pendientes;
  `e5a9c27` trata docks/fullscreen entre Views; `ee40e0e` restaura sesiones X11.

Los diffs íntegros y el historial recuperado se conservan en la evidencia.
El historial ubica el drenaje anterior a la espera en `37d8f12` y la espera sin
heartbeat en cambios anteriores a las ramas estudiadas. Un blame no establece
por sí mismo el primer commit defectuoso; el resultado de la búsqueda histórica
se registra al final.

## Mapa de estados y contratos

```text
NORMAL / MAXIMIZED_V / MAXIMIZED_H / MAXIMIZED_V+H
  -- EWMH ADD fullscreen o toggle explícito --> FULLSCREEN + snapshot
FULLSCREEN
  -- EWMH REMOVE / TOGGLE / toggle explícito --> snapshot previo
  -- RestoreLayout explícito ----------------> sin fullscreen ni maximización
  -- inicio de drag que exige salir ----------> toggle explícito desde pointer.rs
  -- navegación horizontal explícita --------> conserva FULLSCREEN;
                                                policy True pasa a Normal
  -- foco/popup/stack/arrange/configure --------> conserva FULLSCREEN
  -- cambio de View/monitor/RandR -------------> cambia presentación/ubicación;
                                                no debe borrar identidad/flag
  -- UnmapNotify de retirada -----------------> Client eliminado, XID puede vivir
  -- DestroyNotify ---------------------------> Client eliminado, recurso destruido
  -- reinicio --------------------------------> scan + propiedades privadas/restauración
REMAP tras retirada
  --> nuevo Client del mismo XID; reglas/estado inicial se vuelven a evaluar
```

No hay garantía de conservar un toggle del usuario a través de una retirada
ICCCM legítima. Con `honor_initial_state=false`, manage elimina los estados
fullscreen/maximize iniciales; con true los considera. El cliente puede solicitar
ADD nuevamente al remapear. Esto es una política explícita del producto y debe
distinguirse de una salida accidental mientras el cliente sigue gestionado.

`ToggleFullscreen` llama al único mutador lógico `apply_fullscreen`; este usa
`apply_fullscreen_topology`, snapshot/bordes y `apply_fullscreen_geom_restore`.
`SetFullscreen` es el efecto X11 que publica el estado. `SyncWindowPrefs`
persiste preferencias privadas; `SyncWindowPresentation` refleja los flags
actuales tras RestoreLayout. `FullscreenPolicy::True` es un overlay exclusivo
del WM, **no** fullscreen exclusivo de Wine/DXGI. Maximización mantiene ejes
independientes y geometría de restauración propia.

ADD y REMOVE son idempotentes. TOGGLE se aplica una vez por mensaje válido:
dos TOGGLE repetidos producen dos transiciones legítimas. La implementación
considera ambos átomos con OR, de modo que duplicar fullscreen en los dos campos
no lo alterna dos veces. No promete reordenar mensajes entre clientes: se aplican
en orden de recepción; no existe un identificador transaccional EWMH para
deduplicar solicitudes diferentes. Las pruebas verifican el segundo campo,
átomos duplicados y secuencias ADD/REMOVE/TOGGLE.

## Rutas de foco y ciclo de vida

`on_focus_in/out` ignora modos de grab/ungrab y detalles Inferior/Pointer.
FocusOut borra el espejo gestionado si corresponde y arma una consulta al final
del lote. **El comentario que dice que se reimpone foco también contra ventanas
no gestionadas está obsoleto**: `settle_focus` sale sin contrafoco cuando el
destino real no es un Client. Tampoco decide fullscreen.

`reconcile_focus` consulta XGetInputFocus; `settle_focus` compara con la selección
lógica, acepta transferencias transitorias/de grupo en el View y monitor visibles
y conserva la oferta Globally Active pendiente cuando la raíz es su eco.
Entre dos clientes gestionados ajenos puede reparar el foco hacia el lógico.
Un destino raíz sin oferta pendiente también puede provocar reparación.

`accept_client_focus` exige topología válida, visibilidad, elegibilidad de entrada
y parentesco/grupo; si existe overlay exige además su relación transitoria.
Los grupos provienen de WM_HINTS.window_group y WM_CLIENT_LEADER, no de WM_CLASS
ni PID. Esas declaraciones no autentican al emisor. `pending_focus` describe una
intención diferida por un overlay y no equivale al foco real.

WM_HINTS.input=false con WM_TAKE_FOCUS usa el protocolo Globally Active;
input=true admite foco directo y ofertas si procede. Las marcas temporales se
obtienen del servidor. El espejo `x11_input_focus` **solo contiene gestionadas**;
None no prueba que X tenga foco en raíz: puede tenerlo una hija o popup. La traza
`focus_probe raw_real` y el observador externo conservan el XID real para esos
casos. La ventana que recibe teclas se verifica con las pruebas XTest/XInput2
existentes; no se infiere únicamente del foco lógico.

`on_unmap` ignora la copia dirigida a raíz y desgestiona la copia dirigida a la
ventana. `unmanage(false)` limpia topología, AppliedState, protocolos, stacking,
ofertas, grabs y WM_STATE; no destruye el recurso del cliente. Destroy es
idempotente después de unmap. El código ignora también un UnmapNotify sintético
con event=root: no se ha validado aquí toda la matriz de retirada sintética
ICCCM y no se propone cambiar este filtro a ciegas.

`hide_offscreen` configura la geometría `parked_rect` y `wm_hidden` para ventanas
fuera del View: no las desmapea. ConfigureRequest de fullscreen no adopta su
geometría solicitada; ConfigureNotify se compara con AppliedState y no es una
solicitud de abandonar fullscreen. El caché AppliedState describe lo solicitado,
no demuestra por sí mismo lo aceptado por el servidor.

## Hallazgos priorizados

### F1 — Progreso y publicación antes de la espera (CONFIRMADO)

Invariante: no bloquear esperando legibilidad del socket cuando hay trabajo
EWMH por publicar o un evento disponible en la cola de la conexión.

Ruta demostrada: el drenaje anterior a la espera ejecuta on_unmap; el Client se
elimina y `client_list_dirty` queda activo. El bucle publicaba solo al inicio del
siguiente turno; podía dormir antes de alcanzarlo. Además, una lectura de
respuesta dentro de reconciliación puede consumir del socket eventos que quedan
en la cola de x11rb. Una espera de descriptor no detecta esa cola.

Evidencia original instrumentada: `lifecycle-honor`, en el turno 21, registra
`unmap_receipt`, `unmanage_enter`, `managed=false`, `applied=None` y después
`wait boundary=begin`; la espera termina 5,009 s después, cuando el cliente
continúa tras agotar su plazo. Durante el plazo, el servidor aún publica el XID
en `_NET_CLIENT_LIST`, con map_state=IsUnmapped. No se trató de DestroyNotify.

El escenario de 128 ciclos reprodujo la falta de progreso sin instrumentación.
La primera corrección, solo flush de listas, no bastó: se detuvo en remap en el
ciclo 83. Se conservan tanto el intento fallido como la versión original.

Corrección mínima: flush_client_list antes de esperar; poll_for_event antes de
la espera de fd y timeout cero si devuelve un evento. Se despacha ese evento y
se finaliza el turno normal, incluido control y reconciliación. No se añade un
heartbeat, espera arbitraria ni nueva consulta bloqueante de diagnóstico.

Esto puede explicar una ventana remapeada que no llega a presentarse, pero esa
cadena concreta en Blood Strike sigue siendo **HIPÓTESIS**, no conclusión.

### F2 — Acciones EWMH inválidas (CONFIRMADO POR CÓDIGO; ya corregido en ramas)

main y restore interpretan action>2 como toggle. `7cadfa2` ya lo corrige antes
de la rama ewmh. Se incorpora la aserción de servidor en la nueva prueba y se
registran las diferencias de la matriz. No se duplica ese arreglo en este patch.
No hay evidencia de que Wine envíe action=99.

### F3 — Duplicación en el plan de stacking (CONFIRMADO POR CÓDIGO)

`float_stack_order` elimina duplicados de la capa de floats, pero `stack_overlay`
vuelve a agregar propietarios fullscreen/presentados, floats enfocados y
transitorios que ya aparecen en las capas base. El bucle raise recorre cada
aparición; por ello hay operaciones repetidas. La última aparición determina la
capa efectiva. XQueryTree confirmó el diálogo por encima del fullscreen en el
escenario ejecutado; no se demostró que estos duplicados produzcan un orden final
incorrecto. No se cambia la política para ocultar este detalle.

### F4 — Interpretación del hint de compositor (CONFIRMADO POR CÓDIGO)

`set_fullscreen` escribe `_NET_WM_BYPASS_COMPOSITOR=2`, aunque el comentario lo
describe como solicitud de bypass. EWMH define 1 como preferir bypass y 2 como
preferir no bypass. No cambia el flag fullscreen y no prueba el fallo del juego.
No se altera ese comportamiento sin una prueba independiente de compositor.

### Hipótesis y límites

- **HIPÓTESIS:** Wine reacciona a una pérdida de foco enviando REMOVE; requiere
  correlacionar foco real y ClientMessage en el caso real.
- **HIPÓTESIS:** Wine se retira y remapea/reemplaza la ventana; se distingue con
  Unmap/Destroy, atributos, identidad del XID y PID/grupos, no por desaparición visual.
- **DESCARTADO en los escenarios ejecutados, no universalmente:** que una
  transferencia a hija/popup/dialogue o ConfigureRequest necesariamente borre
  FULLSCREEN en Maverick.
- **NO VERIFICABLE aquí:** foco interno de Wine, juego/antitrampas, fullscreen
  exclusivo DXGI, hardware multimonitor, cambios RandR físicos y compositor real
  sobre el juego. No se ejecutó el juego ni se modificaron sus archivos.
- Las simulaciones no prueban la ausencia de otros defectos. La prueba generada
  del modelo no sustituye a una prueba de servidor ni modela todo X11.

## Herramientas y reproducción

Se reutiliza `tests/xvfb-wine-focus.py`: añade selección de probe, estado inicial,
directorio de evidencia, observador externo opcional y manifest con revisión,
hash del binario y del fuente de prueba, comando y duración. Xvfb utiliza
`-displayfd`, resolución fija, `-nolisten tcp`, PIDs propios y teardown inverso.
No toca DISPLAY anfitrión. El cliente C espera condiciones con plazos monotónicos
y poll; la ventana de observación estable de 200 ms es una duración de prueba,
no una sincronización de arranque. El harness conserva logs incluso al fallar.

```bash
cargo build --locked -p maverick -p maverickctl
python3 tests/xvfb-wine-focus.py --wm target/debug/maverick \
  --probe tests/fullscreen-lifecycle-probe.c --complete fullscreen-lifecycle
AUDIT_PUBLICATION_ROUNDS=128 python3 tests/xvfb-wine-focus.py \
  --wm target/debug/maverick --probe tests/fullscreen-lifecycle-probe.c \
  --complete fullscreen-lifecycle --honor-initial-state
# Aislar progreso del bucle de las políticas históricas de fullscreen:
AUDIT_PUBLICATION_ROUNDS=128 AUDIT_PUBLICATION_ONLY=1 \
  python3 tests/xvfb-wine-focus.py --wm target/debug/maverick \
  --probe tests/fullscreen-lifecycle-probe.c --complete fullscreen-lifecycle
PROPTEST_RNG_SEED=20261020 cargo test -p maverick forensic_passive_sequences_preserve_fullscreen
```

El caso C registra XID, monotónico, eventos, solicitudes, foco X11, map_state,
geometría, pertenencia a `_NET_CLIENT_LIST` y fullscreen real. Comprueba stacking
con XQueryTree. Los tests input-probe existentes cubren protocolo, teclado,
XInput2 y captura. La propiedad Rust usa 128 secuencias de hasta 159 pasos:
foco, cambio de View, proyección, geometría de monitor y restauración sin destino
válido; preserva FULLSCREEN/snapshot e invariantes y termina con RestoreLayout
idempotente. No modela remap real mediante una mera mutación Rust.

La instrumentación reutiliza `MAVERICK_TRACE=1`, buffer acotado, sin leer X11
solo por hacer logs. Registros before/after comparten XID, fase y turno; el orden
de líneas es la secuencia local. Incluye mensaje EWMH bruto, unmap/destroy,
foco lógico/real conocido, monitor/View, fullscreen, policy, hidden, geometría,
AppliedState y efectos. Los orígenes describen la ruta inmediata, no autentican
al usuario ni prueban causalidad entre procesos.

Limitación del buffer preexistente: retiene un prefijo de 262144 registros y
descarta nuevos al llenarse; no es un ring que conserve siempre el final.
El dump se escribe en salida ordenada del WM, no garantiza sobrevivir SIGKILL.
El observador externo escribe y vacía su salida durante la sesión para conservar
una segunda fuente independiente. Revisar dropped/truncated en el encabezado.

## Captura real en la sesión del usuario

1. Compilar el probe con `cc -Wall -Wextra -Werror tests/fullscreen-lifecycle-probe.c
   -o /tmp/mav-x11-observer -lX11`. Ejecutar esta captura **solo manualmente** en
   la sesión gráfica que se va a observar; no usar el harness Xvfb para el juego.
2. Para la próxima sesión Maverick, habilitar `MAVERICK_TRACE=1` y
   `MAVERICK_TRACE_PATH=/ruta/privada/wm.trace` al iniciarlo normalmente. Opcional:
   compilar con `--features input-trace,window-trace` y guardar stderr. No reiniciar
   ni reemplazar automáticamente el WM de una sesión de juego ya abierta.
3. Antes de abrir el juego ejecutar `/tmp/mav-x11-observer --observe 180 >
   /ruta/privada/x11.log 2>&1`. El modo observador solo selecciona eventos y consulta
   recursos; no mapea, enfoca, configura, cierra ni destruye ventanas. Tiempo en
   segundos configurable; Ctrl-C detiene solo este observador.
4. Registrar `ps -eo pid,ppid,lstart,args` y, para los XID relevantes,
   `xprop -id XID WM_CLASS WM_CLIENT_LEADER WM_TRANSIENT_FOR WM_HINTS _NET_WM_PID
   _NET_WM_STATE WM_STATE _NET_WM_DESKTOP`. PID y propiedades son declaraciones
   del cliente, no identidad autenticada. El observador incluye PID, líder,
   transitorio, foco, geometría y ventanas creadas durante la captura.
5. Anotar las acciones humanas y el momento de entrada fullscreen; reproducir el
   fallo sin cambiar juego, Wine, antitrampas ni configuración durante la captura.
   Guardar también versión Wine/Proton, comando de lanzamiento y hash del WM.
6. Al desaparecer, distinguir existencia/map_state/geometría/stacking/View.
   XID existente fuera del rectángulo visible indica posible parking; IsUnmapped
   indica unmap; ausencia de Client con recurso existente indica desgestión;
   Destroy seguido de otro XID con propiedades equivalentes sugiere reemplazo.
   DestroyNotify por sí solo no distingue destrucción explícita y desconexión.
   Para esa distinción registrar también si el proceso/conexión sigue vivo.
7. Finalizar el observador y cerrar la sesión Maverick por el mecanismo normal
   **cuando sea conveniente para el usuario**, para volcar su traza. No ejecutar
   killall/pkill ni terminar el WM desde el capturador. Preservar los logs incluso
   si el juego sigue funcionando. Adjuntar wm.trace, stderr, x11.log, propiedades,
   árbol de procesos, configuración y acciones anotadas.

Un ClientMessage no identifica de forma autenticada al proceso emisor. Un REMOVE
tras FocusOut demuestra orden temporal, no que FocusOut causó la decisión de Wine.
Cruzar la transición que emite Maverick con la solicitud observada y los cambios
de recursos. Si el buffer del WM se llenó antes del fallo, repetir una captura
más corta conservando el observador externo; no rellenar huecos con suposiciones.

## Respuestas al caso Blood Strike

No se conoce aún el evento que hace salir al juego de fullscreen, su iniciador,
si hubo un cambio de foco causal ni por qué desaparece después. Se demostró sin
Wine una violación general de progreso del bucle, con una prueba de servidor y
una corrección acotada. El invariante relevante exige despachar eventos disponibles
y publicar la topología antes de dormir. Quedan pendientes la traza real y la
confirmación de que el juego recorre esa ruta. **El problema real no se declara
resuelto.**

## Registro ejecutado

Las tablas finales de comandos, duraciones, resultados, matriz y búsqueda
histórica se incorporan abajo desde los registros de ejecución. Los códigos no
cero de intentos previos y limitaciones del entorno se conservan como evidencia.

### Matriz ejecutada sin parches

| Rama | Ciclo funcional | Estrés 128 ciclos | Tiempo total build + pruebas |
|---|---|---|---|
| main | FALLA (acción inválida) | FALLA (progreso) | 53.480 s |
| feat/restore-layout | FALLA (acción inválida) | FALLA (progreso) | 17.903 s |
| fix/ewmh-state-and-sessions | PASA | FALLA (progreso) | 18.500 s |
| fix/vertical-focus-and-float-stacking | PASA | FALLA (progreso) | 18.379 s |

El caso funcional contiene la acción inválida como control: ese fallo no prueba
que el recorrido válido de fullscreen falle en main/restore. En las cuatro ramas
el estrés falla consultando el servidor sin inyectar eventos para despertar al WM.
Esto demuestra que vertical no introdujo F1. Cada fila tiene hash del binario y
comandos completos en `matrix.jsonl`; los manifests antiguos llaman `commit` a la
revisión del harness, no a la del binario: usar la revisión/hash de la fila de
matriz para atribuir resultados. El harness final distingue ambos campos.

### Versión corregida y validación final

| Comando | Resultado | Duración |
|---|---|---|
| `cargo fmt --all -- --check` | PASA | 1.756 s |
| `cargo check --workspace --all-targets --locked --offline` | PASA | 1.908 s |
| `cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings` | PASA | 2.070 s |
| `cargo test --workspace --all-targets --all-features --locked --offline` | PASA | 45.083 s |
| `cargo test --workspace --all-targets --locked --offline` | PASA | 38.412 s |
| `cargo build --release -p maverick -p maverickctl --locked --offline` | PASA | 17.048 s |

Semilla de las propiedades: `PROPTEST_RNG_SEED=20261020`. Revisión base de estas
pruebas: `3fd91d3` más el patch de auditoría. No se probaron commits de producción
modificados en el checkout original.

| Integración release | Resultado | Duración |
|---|---|---|
| release-stacking | PASA | 23.065 s |
| release-restart | PASA | 1.871 s |
| release-reload | PASA | 3.072 s |
| harness-isolation | PASA | 5.131 s |
| observer-lifecycle | PASA | 2.775 s |
| verified-protocol | PASA | 0.468 s |
| verified-ownership | PASA | 2.070 s |
| verified-child | PASA | 0.265 s |
| verified-capture | PASA | 4.426 s |
| verified-capture-exclusive | PASA | 4.477 s |

El estrés corregido completó 1024 ciclos (9,794 s en su manifest) sin fallos.
Los recorridos con `honor_initial_state` true y false pasaron en la integración
corregida. Las pruebas de floats focus, layers, vertical, transients y metadata
pasaron en debug; stacking, restart/reload, aislamiento del harness y protocolo,
ownership, child, capture y capture exclusivo pasaron en release. El observador
se probó junto al cliente sintético en Xvfb y recogió eventos/snapshots reales.

Los intentos iniciales registran proxy inaccesible sin permiso de red, ausencia
de xcursor en la caché, sockets prohibidos por el sandbox y `/run` no escribible.
Se resolvieron con acceso de red para esos comandos, dependencias extraídas sin
instalación privilegiada y XDG_RUNTIME_DIR privado en /tmp. Un clone de un tipo
Copy en el nuevo test produjo un aviso clippy y se retiró. Los logs previos se
conservan; no se cuentan como pases. No se probó un compositor externo: picom no
está instalado; tampoco Wine. No se usó sudo ni se manipuló la sesión gráfica real.

### Búsqueda histórica y bisect

No se ejecutó `git bisect` con un extremo bueno inventado. Se intentó establecer
un límite histórico: `c1f11f9` compila y falla el estrés; `42a076d` requiere config
por XDG y no desgestiona en unmap según el contrato actual, por lo que su fallo no
es un resultado homologable; `7354a56` no compila (módulo grid y varias APIs faltan).
Un intento de probe posterior a ese último fallo de build se descarta: no prueba
ese commit. No se atribuye un primer commit causante. Sin un extremo bueno
verificado del mismo contrato, la bisección no daría una conclusión defendible.
La regresión queda automatizada para continuar la búsqueda cuando se identifique
ese extremo. Esto limita la localización histórica, no la reproducción F1 en las
cuatro ramas actuales ni su comparación antes/después.

### Tabla de resultados del caso reportado

| Caso | Estado | Evidencia | Commit afectado | Corrección |
|---|---|---|---|---|
| Wine pierde foco | NO VERIFICABLE | Wine/juego no disponibles | Sin atribuir | Pendiente de traza real |
| Fullscreen se normaliza en el juego | NO VERIFICABLE | Sin ejecución real | Sin atribuir | No se declara resuelto |
| Foco/popup/configure sintéticos preservan fullscreen | CONFIRMADO (escenarios limitados) | probe + propiedades/atributos X11 | 3fd91d3 + patch | Conserva política existente |
| Unmap elimina Client sin destruir XID | CONFIRMADO | resource exists, IsUnmapped, traza managed=false | 3fd91d3 | Retirada legítima; no parche de retención |
| Listas/eventos se atascan esperando | CONFIRMADO | estrés falla en cuatro ramas | 1772dfd, f5d7edb, 3b3b2a1, 3fd91d3 | Publicar y comprobar cola antes de esperar |
| Remap recupera ventana y acepta ADD | CONFIRMADO sintético | propiedades/geometría de servidor | 3fd91d3 + patch | Regresión de progreso y lifecycle |
| Stacking de floats/transitorios | CONFIRMADO en pruebas ejecutadas | XQueryTree y suites layers/transients | 3fd91d3 + patch | Sin cambio de política en este patch |
| Causa raíz completa de Blood Strike | HIPÓTESIS pendiente | Necesita captura de usuario | Sin atribuir | Pendiente |

### Registro de cambios

- Corrige exclusivamente el progreso/publicación del bucle antes de dormir.
- Agrega observabilidad optativa sobre el buffer existente y registra foco bruto
  ya consultado, sin consultas X11 nuevas solo para diagnosticar.
- Extiende el harness existente, añade cliente/observador C, una propiedad Rust
  y los dos recorridos de regresión al CI.
- No cambia el contrato fullscreen/withdrawal, ni añade reglas por WM_CLASS/juego,
  retención de ventanas retiradas o restauración forzada de todo FocusOut.

### Referencias de implementación

Las líneas siguientes corresponden al worktree final del patch (base 3fd91d3).
Los commits de cada cambio de comportamiento se describen en la matriz inicial.

| Archivo | Símbolo | Línea |
|---|---|---|
| `src/core/commands.rs` | `apply_fullscreen_topology` | 187 |
| `src/core/commands.rs` | `apply_fullscreen_geom_restore` | 272 |
| `src/core/commands.rs` | `apply_fullscreen` | 284 |
| `src/core/commands.rs` | `ToggleFullscreen` | 1260 |
| `src/core/commands.rs` | `RestoreLayout` | 1308 |
| `src/core/commands.rs` | `decide_active_window` | 534 |
| `src/core/commands.rs` | `decide_manage_focus` | 484 |
| `src/backend/x11/events.rs` | `on_map_request` | 67 |
| `src/backend/x11/events.rs` | `on_unmap` | 104 |
| `src/backend/x11/events.rs` | `on_destroy` | 90 |
| `src/backend/x11/events.rs` | `on_configure_request` | 136 |
| `src/backend/x11/events.rs` | `on_configure_notify` | 287 |
| `src/backend/x11/events.rs` | `on_property` | 561 |
| `src/backend/x11/events.rs` | `on_client_message` | 687 |
| `src/backend/x11/events.rs` | `on_focus_in` | 972 |
| `src/backend/x11/events.rs` | `on_focus_out` | 1016 |
| `src/backend/x11/render.rs` | `parked_rect` | 392 |
| `src/backend/x11/render.rs` | `hide_offscreen` | 678 |
| `src/backend/x11/render.rs` | `float_stack_order` | 446 |
| `src/backend/x11/render.rs` | `stack_overlay` | 793 |
| `src/backend/x11/render.rs` | `reconcile_focus` | 1431 |
| `src/backend/x11/render.rs` | `settle_focus` | 1542 |
| `src/backend/x11/render.rs` | `accept_client_focus` | 1450 |
| `src/backend/x11/manage.rs` | `manage` | 115 |
| `src/backend/x11/manage.rs` | `unmanage` | 675 |
| `src/backend/x11/manage.rs` | `set_fullscreen` | 1283 |
| `src/backend/x11/manage.rs` | `write_net_wm_state` | 1366 |
| `src/backend/x11/manage.rs` | `sync_window_prefs` | 1057 |
| `src/backend/x11/mod.rs` | `run_once` | 750 |
| `src/backend/x11/mod.rs` | `forensic_state` | 540 |
| `src/backend/x11/ewmh.rs` | `flush_client_list` | 239 |
| `src/backend/x11/ewmh.rs` | `shares_focus_group` | 374 |
| `src/backend/x11/ewmh.rs` | `declared_focus_group` | 343 |
| `src/backend/x11/reconciler.rs` | `reconcile` | 191 |
| `src/core/tests.rs` | `forensic_passive_sequences_preserve_fullscreen` | 4036 |

Referencia normativa: [EWMH, propiedades de ventanas y bypass de compositor](https://specifications.freedesktop.org/wm-spec/latest/ar01s05.html).

### Control final sin instrumentación

Se repitió el mismo fuente C final con `--no-trace` y 128 ciclos: la versión
3fd91d3 sin patch falló (10,349 s), la corregida pasó en release (1,721 s) y
también en debug. El control debug antes/después evita atribuir el cambio solo
al perfil de optimización. Los manifests finales incluyen los hashes, revisión
explícita del WM, revisión independiente del harness y hash idéntico del probe.
Los resultados antes/después se adjuntan en `final-ab.jsonl` y sus manifests.
