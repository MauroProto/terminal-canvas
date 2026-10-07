# Recuperación y exportaciones — 7 de octubre de 2026

Este pase conserva los archivos anteriores cuando una lectura o una
restauración falla. No cambia Online ni invitaciones. El daemon descrito aquí
es el daemon local de terminales Unix.

## Layout e historial

- Un `layout.json` ausente permite crear un perfil. Un error de lectura de un
  candidato existente detiene el loader y desactiva las escrituras de layout e
  historial por la UI, incluso si hay un backup legible. No se puede asumir que
  ese backup representa el contenido del archivo bloqueado.
- La UI conserva una franja visible con el motivo; no depende del toast inicial.
  La pérdida de propiedad de escritura también mantiene ese aviso.
- Las consultas periódicas de terminales no actualizan por sí solas las fechas
  de actividad ni de tareas. La entrada aceptada, la salida nueva, los hooks
  recibidos y los cambios
  reales de identidad, estado o resumen sí las actualizan. El contador de
  actividad vive sólo en memoria, separado del repintado, replay y ACK; no
  agrega campos al formato del layout. El historial mantiene su propia
  cadencia de guardado, aunque el layout no cambie.
- Si el grid está ocupado, oculto por minimización o desconectado, la consulta
  conserva el último resumen disponible. No interpreta una lectura omitida
  como texto vacío. Los reports autoritativos disponibles y la salida del
  proceso siguen siendo observables sin leer el grid.
- Una alerta de conflicto conserva fecha, confirmación y archivo mientras
  persista el mismo riesgo. Si el riesgo desaparece, se retira; una aparición
  posterior abre una alerta nueva, aunque su identificador sea el mismo.
- El daemon valida checkpoint, generación y log antes de abrir un PTY o escribir
  historial. Un error no autoriza reemplazar archivos ni confirmar salida
  pendiente mediante ACK. Los logs de generaciones antiguas siguen excluidos,
  incluso al elegir una generación después del wraparound.
- La restauración del PTY informa si aceptó el trabajo. Un rechazo por worker
  ocupado o fallo al crear el hilo conserva la hoja pendiente para reintentar.
  Un fallo durante replay bloquea nuevos checkpoints del grid parcial; requiere
  una nueva sesión de PTY para restablecer esa guarda.

La tolerancia existente a un tail de log incompleto se mantiene: recupera el
prefijo válido. No equivale a aceptar headers inválidos o saltos de secuencia.
Ver [SUPPORT.md](SUPPORT.md) antes de investigar datos reales.

## Preferencias y notas

El worker de preferencias se crea con el primer pedido. Conserva los snapshots
aceptados hasta recibir confirmación y mantiene separados los errores de cada
repositorio y de configuración. Un fallo al iniciar o una desconexión no deja
la app esperando indefinidamente; el aviso permanece visible aunque se cierre
Settings o desaparezca el toast. **Reintentar guardado** usa el último snapshot
retenido, incluyendo integraciones y onboarding.

Un archivo de notas ausente permite empezar una colección. Un error de lectura
o JSON inválido conserva sus bytes y bloquea crear, editar, borrar y enviar
notas; el diff sigue disponible. **Reintentar lectura de notas** vuelve a leer
el mismo repositorio. Las respuestas de pedidos anteriores o de otro
repositorio no reemplazan la colección actual. Al reabrir un review, los
snapshots sin guardar deben resolverse antes de ofrecer notas editables del
disco.

La carga, importación legacy y guardado de notas comparten un máximo de 4 MiB
para el JSON completo en UTF-8. Un archivo que lo supera se rechaza sin
interpretar un prefijo como una colección válida. Una serialización demasiado
grande se rechaza antes de publicar o rotar el archivo guardado; conserva los
bytes anteriores y el snapshot aceptado de esa sesión.

Si un guardado sigue fallando después de cerrar y reabrir el review,
**Editar notas pendientes** permite recuperar el último snapshot retenido del
mismo repositorio. La acción conserva las notas y sus metadatos, habilita
editar esa copia y no inicia un guardado automático. El aviso de guardado
permanece hasta recibir la confirmación correspondiente a la colección actual.
Las respuestas de cargas o imports anteriores no reemplazan la copia recuperada.
Una colección que excede el máximo requiere reducirla antes de volver a guardar;
reintentar el mismo snapshot no cambia su tamaño. Esta recuperación no permite
editar un archivo corrupto si no existe un snapshot pendiente del mismo
repositorio, no incluye un editor sin guardar y no sobrevive al fin del proceso.

La barrera de cierre aplica las respuestas pendientes de lectura e importación
y espera también el guardado de las notas fusionadas. Los fallos de
preferencias no impiden rescatar layout e historial, pero impiden marcar el
cierre como limpio o continuar una instalación. Los snapshots fallidos siguen
en memoria para reintentar durante esa sesión: la marca de recuperación no
convierte esos snapshots en archivos durables si el almacenamiento sigue
fallando.

## Exportaciones y copia

Los exports de texto usan fecha e identificador único. Un único worker lazy
escribe texto y comprime diagnósticos; acepta un trabajo hasta consumir su
resultado. La captura del grid permanece en el hilo de UI. El polling funciona
también con la ventana oculta, y el cierre normal espera el trabajo aceptado.
Los errores de creación del worker y escritura se muestran al usuario.

La selección del visor conserva el texto Unicode, CRLF y el salto final, sin
los números del gutter. El visor fragmenta las líneas largas antes de
renderizarlas y aplica un presupuesto separado al resaltado, según
[LONG-LINE-VIEWER.md](LONG-LINE-VIEWER.md).

## Verificación

Las regresiones cubren colisiones de nombres, worker ocupado/cierre/fallo,
polling oculto, aviso persistente, sharing de lectura en Windows, permisos de
historial en Unix, ACK, generaciones y rechazo/fallo de replay. La prueba de
clipboard usa eventos de puntero y copia sobre galleys reales de egui.

La regresión del guardado oculto fuerza una actualización de la actividad de
sesión mientras hay escrituras pendientes. Espera la confirmación del estado
vigente y el drenaje de comandos e historial, y coteja `layout.json` con ese
mismo estado. Una captura anterior puede quedar obsoleta aunque el guardado
sea correcto. Se mantienen el plazo de tres segundos y el aislamiento en un
perfil temporal; no se abre un shell ni se ejecuta una pasada de UI.

La regresión de inactividad cuenta las publicaciones del worker real en otro
perfil temporal: después del guardado inicial y su ACK, ocho observaciones
idénticas no publican layouts. Una actividad nueva con el mismo texto, un
cambio de estado y un cambio de layout se guardan y se cotejan con el archivo
durable. Otras regresiones cubren salida idéntica, entrada rechazada, replay,
grid ocupado, tareas bloqueadas por dependencias y episodios de conflicto.
Este recuento headless no mide consumo de CPU/GPU ni tiempos de una GUI real.

El pase necesita CI completa en Windows, Linux y ambos macOS, incluyendo daemon
en Unix, y ensayo de paquetes del mismo SHA. Los recibos externos de entrega
registran esos resultados; esta descripción no acredita su ejecución por sí
sola. No se ejecutaron builds completos, Cargo tests ni una GUI en esta PC para
este pase. La validación nativa con perfiles y agentes reales sigue pendiente.
