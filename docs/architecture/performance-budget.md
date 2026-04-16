# Performance Budget

Este documento define el budget operativo de la app mientras se consolida el runtime desacoplado.

No es un benchmark público. Es una restricción de ingeniería para que el producto se mantenga estable como **desktop nativo de terminales y agentes**.

## Baseline target

- `20` terminales abiertas
- `6` paneles visibles
- `3` terminales con output sostenido

## Acceptance criteria

El baseline sólo se considera sano si, sobre hardware moderado:

- las sesiones restauradas arrancan `detached`
- las terminales minimizadas u offscreen no fuerzan trabajo inútil
- el output bursty se coalescea, no dispara repaint por cada evento
- drag/resize difieren scans caros de orquestación
- la terminal enfocada mantiene latencia de input aceptable aun con otras sesiones activas

## Importante: contrato actual de render

La infraestructura de runtime sigue modelando tiers como `Full`, `ReducedLive`, `Preview` y `Hidden`.

Pero la UX actual prioriza fidelidad visual:

- paneles normales y renderables usan `Full`
- `Preview` queda como backstop para tamaños mínimos o estados no renderables
- `ReducedLive` sigue siendo una herramienta del runtime, no el comportamiento visual por defecto del desktop

El budget y los smoke tests deben leerse con esa decisión en mente. No se debe reintroducir “low power mode” visible sin una decisión explícita de producto y una validación real de UX.

## Instrumentation

En builds de desarrollo debe seguir siendo posible medir:

- frame time
- cantidad de paneles visibles
- sesiones attached vs detached
- cache hits vs misses
- motivo de repaint
- duración de scans de orquestación

## Scenario coverage

La etapa no se considera cerrada sin smoke tests repetibles para:

- `1` abierta / `1` visible / `0` con output sostenido
- `4` abiertas / `2` visibles / `1` con output sostenido
- `20` abiertas / `6` visibles / `3` con output sostenido

## Acceptance rule

Una implementación es aceptable sólo si mantiene el baseline sin cambiar la experiencia principal del producto:

- desktop acotado
- ventanas/paneles reales
- taskbar
- workspaces
- colaboración
- orquestación

## Notes

- El budget es conservador a propósito.
- Si la implementación actual no usa un tier más bajo en background, la documentación y los tests deben reflejarlo.
- Las optimizaciones futuras deben ser medibles y no degradar la legibilidad del contenido terminal sin decisión explícita.
