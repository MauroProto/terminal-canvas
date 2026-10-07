# Resaltado del visor de código

El pase del 7 de octubre de 2026 corrige la detección de archivos JSX y de
módulos JavaScript. La selección de gramática es compartida por el indicador
de lenguaje y el worker que colorea el archivo.

## Correcciones

Los assets `fancy-regex` de `two-face 0.5.2` no contienen la gramática Babel
que registra `jsx`, `mjs` y `cjs`. Tener TypeScriptReact en el catálogo no
hacía que esas extensiones se reconocieran automáticamente. La exclusión
está documentada en el [README de two-face](https://docs.rs/crate/two-face/0.5.2/source/README.md).

Ahora `.jsx` usa la gramática TypeScriptReact embebida y muestra `JSX` como
lenguaje; `.mjs` y `.cjs` usan JavaScript. Los alias aceptan mayúsculas y
sólo se aplican al último sufijo de un nombre con punto. Las definiciones
nativas conservan prioridad, seguidas de tokens de nombre y shebang.
`Dockerfile`, `Makefile`, sufijos desconocidos y archivos sin extensión
conservan sus fallbacks anteriores. Las extensiones nativas ya son
insensibles a mayúsculas en syntect; no se incorporó una normalización
adicional ni un cambio de prioridad para C/C++.

La [gramática React incorporada](https://raw.githubusercontent.com/microsoft/TypeScript-Sublime-Plugin/ba45efd058df5111837e30fb9598cfc8cbd51095/TypeScriptReact.tmLanguage)
permite colorear markup y expresiones JSX. Esta corrección no agrega Babel
ni una nueva gramática y no cambia el contenido visible del archivo.

## Comprobaciones del componente

Las pruebas del código comparan caracteres y colores de JSX con TSX, y de
los módulos ESM/CommonJS con JavaScript. Cubren comentarios, expresiones,
template strings, Unicode, LF, CRLF, terminadores mixtos y EOF sin salto
final. Después del cierre de los tags JSX, la línea JavaScript siguiente
también se compara con una línea independiente para detectar estado de
markup que no se haya cerrado. Se comprueba la prioridad de extensiones
nativas, nombres conocidos y shebang, además de cancelación, límite de
líneas y descarte de resultados obsoletos del worker.

El harness aislado ejecutó 29 pruebas correctamente en `0bcbf0a`; la
aserción adicional de cierre JSX pasó en `b35d0b8`. Clippy del componente
pasó en `6ad734c`, cuyo archivo del resaltador es idéntico al de `b35d0b8`.
Los tres tests propios del harness revisan catálogos y corpus sintéticos;
los otros 26 importan el archivo real de la app. El corpus actualizado
tiene 65 casos. Esto no equivale a probar todos los patrones posibles de
las gramáticas ni la interfaz gráfica de la app.

Se preservó un ensayo fallido de extensión en mayúsculas en `524ad39`:
su expectativa sobre `.C` era incorrecta para el matcher nativo de
syntect. El cambio y ese test se revirtieron íntegramente en `6ad734c`.
No se cuenta esa ejecución fallida como validación del código final.

La integración de la app completa y los paquetes debe acreditarse con
la CI y Release del SHA publicado, registrados en el recibo externo del
checkpoint. Las corridas del checkpoint gráfico anterior no validan
estos cambios de producción.

## Límites y continuidad

La ejecución local se limitó al componente puro: sin app, PTY, GPU,
ventanas ni servidores, con un job, un hilo de tests, prioridad Idle y
proceso oculto dentro de un Windows Job limitado a dos CPUs, 20% de CPU,
1 GiB de memoria total y diez minutos. Los logs y recibos se conservan
en `dist` y en la entrega externa; no son parte de los binarios.

El serializador de producción y `Cargo.lock` no cambiaron. La investigación
de compatibilidad de assets, los dos avisos de mantenimiento y las razones
para conservar el prototipo separado están en
[DEPENDENCY-MAINTENANCE.md](DEPENDENCY-MAINTENANCE.md).
Online e invitaciones no recibieron cambios funcionales.
