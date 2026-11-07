# Maverick frente a otros gestores X11

Mediciones reales realizadas con el mismo cliente nativo y un Xvfb nuevo por ejecución. La fecha de la figura usa America/Lima; el manifiesto conserva la hora UTC original.

**80 ejecuciones correctas: 10 calentamientos y 70 repeticiones medidas, con 35.000 operaciones observadas.** Hay siete repeticiones por gestor y cantidad de ventanas, en orden aleatorio reproducible.

## Resultado con 50 ventanas

| Gestor | Gestión p50 (ms) | Foco p50 (ms) | Fullscreen p50 (ms) | Retirada p50 (ms) | RSS (MiB) | CPU / 100 ventanas (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Maverick | 1.740 | 1.676 | 1.358 | 1.301 | 2.559 | 40.00 |
| i3 | 3.375 | 0.831 | 2.309 | 4.964 | 10.48 | 120.00 |
| bspwm | 1.622 | 0.141 | 1.469 | 0.684 | 1.273 | 20.00 |
| Openbox | 2.881 | 0.467 | 0.990 | 0.742 | 13.44 | 100.00 |
| dwm | 2.334 | No compatible¹ | 0.422 | 2.261 | 5.828 | 60.00 |

¹ dwm 6.5 estándar marca urgencia ante `_NET_ACTIVE_WINDOW`; no cambia el foco. La capacidad se verificó por separado; estos casos se conservan como valores ausentes, nunca cero.

Maverick usa menos RSS que i3, Openbox y este dwm.default; bspwm usa menos que Maverick. Maverick gestiona ventanas antes que i3, Openbox y dwm en esta carga, pero su cambio de foco es más lento que en los otros gestores que aceptan la misma petición. El gráfico muestra ambos resultados.

Las barras son medianas de estadísticas por ejecución. Los bigotes son P25–P75 entre las siete ejecuciones; no son intervalos de confianza. No se eliminaron valores extremos. CPU tiene resolución de 10 ms y está normalizada a 100 ventanas; no debe interpretarse con precisión submilisegundo.

## Entorno y alcance

- Debian 13, AMD EPYC 9V74 virtualizado, tres CPU visibles; cuota cgroup equivalente a dos CPU y 8 GiB de RAM.
- Xvfb 1280×800×24, sin compositor, con procesos y directorios aislados.
- Maverick release/default features, revisión `807218896c05e3ffdaa565799ab523fca84e418e`.
- Versiones y SHA-256 de los cinco binarios en `manifest.json`; bibliotecas y paquetes en `provenance.json`.
- Openbox apila ventanas flotantes; los demás usan sus layouts. dwm.default conserva barra y bordes compilados.
- Latencias hasta estado observable en X11: incluyen servidor, observador y planificación; no son FPS ni latencia a pantalla.
- Pilotos y una serie abortada durante la preparación de bspwm no se mezclaron con esta matriz completa.

## Repetir y verificar

Las instrucciones, condiciones de finalización y limitaciones están en `benchmarks/x11/README.md` en el paquete. `raw.json` contiene todas las muestras; `runs.csv` las estadísticas por ejecución; `summary.csv` y `summary.json` los valores de la figura. El generador rechaza matrices incompletas y datos alterados.

La fuente exacta utilizada al medir está en `source-at-measurement/`. El generador posterior solo corrige etiquetas y fecha local; sus hashes y los de las figuras están en `chart-manifest.json`.
