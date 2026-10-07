# Visor con líneas extensas

Una sola línea de 2 MiB podía llegar completa al layout de egui y al parser de
syntect, aunque la lista de líneas estuviera virtualizada. Este pase prepara
filas de hasta 1.024 bytes UTF-8 en el lector y usa el texto original para la
selección y la copia. No cambia Online ni invitaciones.

## Implementación

`file_viewer_document` guarda un `Arc<str>`, rangos de líneas lógicas con sus
terminadores y fragmentos visuales. El lector aplica los límites existentes de
2 MiB de bytes originales y 100.000 líneas antes de publicar el documento
completo con su token. No conserva otra colección de strings por línea.
Los offsets se refieren al texto decodificado: UTF-8 inválido sigue usando el
fallback con reemplazos, por lo que no se promete una copia binaria del archivo.

Los graphemes habituales se mantienen juntos. Un cluster patológico de más de
1.024 bytes se divide sólo en fronteras UTF-8; el texto original permanece
íntegro. Los saltos CRLF/LF y EOF se guardan separados del contenido visual.
Las continuaciones no inventan números de línea ni terminadores.

`file_viewer_selection` dibuja sólo las filas visibles, con un único dueño de
foco de egui independiente de las filas. Selección, copia, arrastre, Shift+clic,
flechas, Inicio/Fin, barra y menú contextual trabajan sobre offsets originales.
La selección persiste al desplazarse y Ctrl+A incluye el contenido fuera de
pantalla. Los metadatos de accesibilidad se limitan a los fragmentos visibles.
La app libera el foco al abrir un modal o perder el foco de ventana y resuelve
el visor antes de enviar el resto de la entrada a la terminal.

El resaltador comparte el mismo buffer. Su worker comprueba las líneas antes
de inicializar gramáticas o llamar al parser: una línea de más de 1.024 bytes,
incluido su terminador, deja el prefijo visible completo en plano. No salta una
línea para continuar después con un estado de parser incompleto. La UI evita
pedir resaltado cuando el documento ya contiene líneas extensas. Los archivos
habituales conservan su resaltado y el límite de 20.000 líneas coloreadas.

## Verificación y límites

Las fixtures permanentes comprueban límites, Unicode, selección reversible,
terminadores, cancelación y tokens. Las pruebas de egui usan eventos reales
de puntero y carets públicos de los galleys; comprueban copia única, foco,
navegación y cantidades de texto/meshes de filas visibles. Se conservan las
regresiones de bounds y copia Unicode de la integración previa.

La ejecución de estas fuentes debe quedar asociada a su SHA exacto en CI.
El checkpoint de recuperación `d226a0ce` aprobó CI y paquetes, pero esos
resultados no acreditan este pase posterior. No se compila ni ejecuta una GUI,
PTY, WSL o servidor local en la PC de Mauro durante esta integración.

El límite por galley no es un presupuesto global de CPU, GPU o memoria.
La evaluación nativa de DPI, drivers, lectores de pantalla y selección con
ventana real sigue pendiente. La cancelación del lector no interrumpe una
syscall ya bloqueada y la del resaltador no interrumpe una regex ya activa.
El prototipo bajo `dist/long-line-viewer-prototype` se conserva como referencia
congelada; sus fixtures no cuentan como ejecuciones de la app.
