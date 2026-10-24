# Validación del paquete

Base: `e6c67fdaaddbe1093dcd0914bc536ecb86ecc41d`. Solo se añaden herramientas;
no se cambia producción, dependencias Rust ni el prototipo del viewport.

Ejecutado en Linux, como usuario normal, con Xvfb privado:

- Compilación C con `-std=c11 -O2 -Wall -Wextra -Werror -Wshadow -Wformat=2`.
- 14 tests del modelo/analizador: corridas, límites con pertenencia desconocida,
  mundos distintos, privacidad, esquema y ponderación por permanencia.
- 12 contratos del lanzador: endpoint, VT/arranque reciente, locks, limpieza
  del WM propio ante errores, salida externa y privacidad.
- Integración con Xvfb autenticado: rechazo sin confirmación, DISPLAY ausente,
  cookie incorrecta, maverickctl incompatible, preview sin crear captura,
  SIGTERM con conservación de datos, modo native-only y permisos 0700/0600.
  El WM y Xvfb ajenos al alcance del recolector sobreviven al terminarlo.
- Campaña funcional propia con ventanas reales: tiles, floats, diálogos,
  intercalado OR, fullscreen exclusivo/cedido al ribbon, maximización, dock
  con strut, cambio de View y ciclo de vida.
- Controles negativos nominales: inversión del árbol detectada por la prueba
  de dirección; selección de redirección rechazada por la guardia en ejecución.
- Compilación de Python, sintaxis del lanzador shell y `cargo fmt --all -- --check`.

Los tests positivos de reconocimiento físico usan un `/proc` simulado. **No se
ha arrancado ni capturado un Xorg físico aquí.** Tampoco se demuestra ausencia de
perturbación por consultas IPC. El arranque con startx, logind y DRM debe validarse
en el hardware de destino; cuando no se puede verificar, se aborta o se etiqueta
explícitamente `x11-unconfirmed`.

No se añaden capturas, cookies, logs, resultados privados, ejecutables compilados
ni carpetas de evidencias al repositorio. Los hashes de importación en
`PROVENANCE.json` se verificaron contra la entrega definitiva de la auditoría;
las adaptaciones de publicación se describen allí y en el diff del commit.
