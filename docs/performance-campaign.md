# Campaña de rendimiento X11

Rama: `perf/x11-performance`. Base: `807218896c05e3ffdaa565799ab523fca84e418e`.
Directorio de trabajo: `/workspace/work/maverick-performance`.

Objetivo del usuario: superar a i3, bspwm, Openbox y dwm en las nueve métricas
existentes, con resultados reales y reproducibles. Es una meta de investigación,
no una conclusión ni una garantía. Esta rama se dedica a optimizaciones y a las
pruebas necesarias para validarlas.

## Punto de partida verificado

El benchmark independiente se conserva en `benchmarks/x11/` y su evidencia
original en `benchmarks/results/2026-10-22-final/`. La fuente exacta de aquella
medición también está archivada dentro de ese directorio. Los candidatos se
miden en directorios nuevos; nunca se sobrescribe la línea base.

- Revisión medida: `807218896c05e3ffdaa565799ab523fca84e418e`.
- SHA-256 del binario Maverick:
  `819547a9080939ef4c4749faa70eee7edf5f196c7b6465eec431e2edd058e82c`.
- SHA-256 de `raw.json`:
  `7af70c2d820528975f1104a40625b210d8c7659b895e9a9d05b78248779a07e8`.
- 80 ensayos completos: 10 calentamientos y 70 mediciones; siete repeticiones
  por gestor y población. El manifiesto contiene los hashes de los otros
  binarios, configuraciones y herramientas.

Los valores siguientes son medianas de siete estadísticas por ejecución.
En todas las filas, menos es mejor. Los valores históricos sirven para orientar
el trabajo; para declarar una victoria se repite la comparación completa.

| Métrica | Ventanas | Maverick inicial | Mejor valor inicial | Unidad |
| --- | ---: | ---: | ---: | --- |
| Gestionar ventanas: p50 | 10 | 1.177 | 0.764 (bspwm) | ms |
| Gestionar ventanas: p50 | 50 | 1.740 | 1.622 (bspwm) | ms |
| Gestionar ventanas: p95 | 50 | 3.690 | 2.631 (bspwm) | ms |
| Cambiar foco: p50 | 50 | 1.676 | 0.141 (bspwm) | ms |
| Cambiar foco: p95 | 50 | 2.465 | 0.273 (bspwm) | ms |
| Transición fullscreen: p50 | 50 | 1.358 | 0.422 (dwm) | ms |
| Retirar ventanas: p50 | 50 | 1.301 | 0.684 (bspwm) | ms |
| Memoria RSS del WM | 50 | 2.559 | 1.273 (bspwm) | MiB |
| CPU del WM al gestionar 100 ventanas | 50 | 40.000 | 20.000 (bspwm) | ms de CPU |

## Condiciones de aceptación

1. Conservar clientes, resolución, configuraciones, operaciones y criterios de
   finalización del benchmark. Si se descubre un defecto de medición, documentar
   la corrección y volver a medir todos los gestores con ella.
2. Comparar la base y cada candidato en el mismo entorno, con compilaciones
   release equivalentes, alternando su orden. No compilar ni ejecutar regresiones
   mientras se mide. Registrar revisiones, cambios pendientes y hashes de los
   binarios; conservar todas las muestras y ensayos fallidos.
3. Mantener 10 y 50 ventanas, 500 cambios de foco, 40 transiciones fullscreen,
   dos segundos de reposo, siete repeticiones y un calentamiento por combinación.
   Repetir la matriz de cinco gestores al evaluar un candidato para el ranking.
4. Publicar las nueve métricas, su variabilidad y las regresiones, aunque alguna
   empeore. Una diferencia pequeña con distribuciones solapadas es inconclusa;
   una victoria numérica en una sola serie no demuestra superioridad estable.
   Añadir series independientes si la variabilidad impide decidir, sin borrar
   las anteriores. La resolución de los ticks limita la comparación de CPU.
5. Preservar cámara, geometría, bordes, stacking, publicación EWMH, foco ICCCM,
   Wine y ciclo de vida fullscreen. No desactivar trabajo funcional ni adelantar
   una confirmación para obtener un número menor. No introducir rutas especiales
   para reconocer clientes del benchmark.
6. Ejecutar formato, clippy, pruebas Rust y regresiones Xvfb relevantes antes
   de aceptar cambios. Foco requiere, como mínimo, WM_HINTS, WM_TAKE_FOCUS,
   ventanas hijas, captura de entrada, Wine, flotantes, stacking y fullscreen.
7. Medir sin trazas para publicar tiempos y memoria. Las trazas y perfiles se
   usan en ejecuciones diagnósticas separadas; su sobrecoste no se mezcla con el
   ranking. No afirmar mejoras de FPS ni latencia hasta la pantalla a partir de
   Xvfb: este experimento no mide esas magnitudes.

El foco de dwm 6.5 sigue siendo no compatible con esta petición EWMH cuando
solo marca urgencia; no cuenta como cero, victoria ni derrota de latencia.
Los nueve paneles incluyen estadísticas relacionadas, no nueve capacidades
independientes. Si una métrica sigue peor o inconclusa, la meta sigue pendiente.

## Orden de investigación

1. **Foco y cámara.** Separar coste de despacho, proyección, peticiones de
   geometría y reconciliación del foco. El foco de Maverick retargetea la cámara
   y marca un arrange; una diferencia respecto a otro gestor no identifica por
   sí sola un cuello de botella. Buscar trabajo repetido conservando el resultado
   visible y la reparación de foco.
2. **Fullscreen.** Examinar geometría, bordes, máscaras y stacking durante
   entrada y salida. Verificar restauración exacta y ventanas transitorias.
3. **Gestión y retirada de ventanas.** Medir lecturas X11 seriales, publicación
   de propiedades, invalidaciones y asignaciones; conservar manejo de errores.
4. **CPU y memoria.** Perfilar asignaciones y buffers del backend. Evaluar cada
   caché por su ahorro y su RSS; no intercambiar una regresión oculta por otra
   métrica favorable.

## Primera optimización: gestión de ventanas

La lectura de geometría y `WM_TRANSIENT_FOR` se envía junto con las demás
lecturas de propiedades durante `manage()`, antes de esperar respuestas. Esto
elimina dos esperas seriales del camino de apertura. La rama conserva las
comprobaciones de errores y el tratamiento de ventanas transitorias y Wine.

Comparación alternada en el mismo entorno: siete parejas medidas y una de
calentamiento, 50 ventanas, versión inicial frente a la optimizada. La tabla
muestra medianas de estadísticas por ensayo; menor es mejor. Los números de
esta sesión no se comparan directamente con los de la sesión histórica.

| Métrica | Inicial | Optimizada | Parejas favorables / 7 |
| --- | ---: | ---: | ---: |
| Apertura p50 (ms) | 2.622 | 2.047 | 7 |
| Apertura p95 (ms) | 6.080 | 3.077 | 6 |
| Foco p50 (ms) | 2.131 | 2.209 | 2 |
| Foco p95 (ms) | 3.573 | 3.643 | 5 |
| Fullscreen p50 (ms) | 1.517 | 1.711 | 1 |
| Retirada p50 (ms) | 1.632 | 1.689 | 2 |
| RSS (MiB) | 2.621 | 2.574 | 5 |
| CPU al abrir 100 ventanas (ms) | 60 | 60 | 4 |

La apertura p50 mejora un 22 % en esta comparación; foco, fullscreen y retirada
no mejoran. El coste de CPU está cuantizado en intervalos de 10 ms. Los datos
completos, la matriz de cinco gestores y los hashes están en
[`benchmarks/results/2026-10-23-optimized/`](../benchmarks/results/2026-10-23-optimized/REPORT.md).

Se probó también agrupar la escritura de `_NET_CLIENT_LIST` con la consulta del
árbol X11. Aunque pasó las regresiones funcionales, la apertura p50 pasó de
2.247 a 2.658 ms (solo dos de siete parejas favorables); se descartó. También
se descartó reutilizar buffers del backend tras empeorar foco, fullscreen y
retirada. Sus parches, muestras y manifiestos están en los directorios
`rejected-ewmh/` y `rejected-buffers/` del mismo conjunto de resultados.
Otra prueba eliminó la confirmación síncrona de `WM_STATE`: mejoró la mediana
de apertura en esa serie, pero solo ganó 3/7 parejas y empeoró apertura p95 y
foco p95. Se descartó y quedó archivada en `rejected-wmstate/`.
La meta de ganar las nueve métricas sigue pendiente.
