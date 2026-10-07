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

## Exportaciones y copia

Los exports de texto usan fecha e identificador único. Un único worker lazy
escribe texto y comprime diagnósticos; acepta un trabajo hasta consumir su
resultado. La captura del grid permanece en el hilo de UI. El polling funciona
también con la ventana oculta, y el cierre normal espera el trabajo aceptado.
Los errores de creación del worker y escritura se muestran al usuario.

Copiar una selección entre líneas del visor excluye los números del gutter y
conserva el texto Unicode. Esta corrección no resuelve el coste de renderizar o
resaltar una línea extraordinariamente larga: el límite de archivo de 2 MiB
todavía permite ese caso.

## Verificación

Las regresiones cubren colisiones de nombres, worker ocupado/cierre/fallo,
polling oculto, aviso persistente, sharing de lectura en Windows, permisos de
historial en Unix, ACK, generaciones y rechazo/fallo de replay. La prueba de
clipboard usa eventos de puntero y copia sobre galleys reales de egui.

El pase necesita CI completa en Windows, Linux y ambos macOS, incluyendo daemon
en Unix, y ensayo de paquetes del mismo SHA. Los recibos externos de entrega
registran esos resultados; esta descripción no acredita su ejecución por sí
sola. No se ejecutaron builds completos, Cargo tests ni una GUI en esta PC para
este pase. La validación nativa con perfiles y agentes reales sigue pendiente.
