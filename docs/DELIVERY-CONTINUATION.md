# Estado de entrega y continuación — 6 de octubre de 2026

La rama principal del repositorio se llama `master`. La reversión pedida en
septiembre quedó integrada mediante `bc5768d`; la base remota revisada para
este pase es `f0458dd1dae08b3aca080a914042deb5a9454cf3`. Este pase agrega
perfiles aislados, diagnóstico de paquetes, actualización verificada,
instalador Windows, validaciones de distribución y limpieza de dependencias.
Online e invitaciones no reciben cambios funcionales en este pase.

## Verificación realizada en esta PC

- Rust/Cargo/Clippy 1.98.0, Windows x86_64, máximo dos jobs de compilación y
  dos threads de pruebas. No se usó WSL ni se inició un servidor persistente.
- Formato y Clippy de todos los targets, con warnings como errores.
- Suite completa de todos los targets: **872 pruebas aprobadas y dos ignoradas**,
  incluyendo **836 pruebas de librería aprobadas y dos ignoradas**, más los tres
  benchmarks en modo smoke. El código comprobado termina en `f7893e4`; la
  documentación de relevo se agregó después.
- Pruebas de perfil mediante procesos independientes: layout, configuración,
  historial de dos hojas, notas y memoria conservan datos entre ejecuciones.
  Los argumentos CLI inválidos no abren la app ni generan archivos.
- Se agregaron pruebas con un servidor HTTPS local y certificado propio para
  descarga completa, progreso, checksum incorrecto, respuesta incompleta,
  límites, redirección rechazada y cancelación de una respuesta detenida.
  Usan caches temporales independientes y no modifican el perfil habitual.
- Los comandos del instalador tienen plazo y límite de salida. Windows exige
  que el registro de Inno nombre la copia que está ejecutándose y entrega esa
  carpeta mediante `/DIR`; una copia portable firmada usa actualización manual.
  Las regresiones incluyen Unicode, espacios y rutas UNC.
- 14 pruebas del validador de paquetes y 9 de la extensión.
- En el pase anterior se extrajo y comprobó un ZIP **debug sin firma**, con arquitectura, manifiesto,
  SHA256, versión, diagnóstico, helper de memoria y handshake/listado MCP.
  Esto no valida un instalador de producción ni una firma.
- `cargo-audit 0.22.2`: cero vulnerabilidades conocidas, cuatro avisos de
  mantenimiento (`bincode`, `paste`, `rustls-pemfile`, `ttf-parser`), sin ignores.

Los logs del último cierre se guardan en `dist/validation-clippy-final.log` y
`dist/validation-tests-final.log`; la auditoría previa está en
`dist/validation-audit.json`. No se deben confundir
los resultados locales con una nueva CI multiplataforma del candidato.

## Pendientes concretos

1. Subir este pase. La cuenta `ainiagent` autenticada en esta PC tiene lectura,
   sin permiso `push`; no se puede publicar con esas credenciales. Autenticar
   GitHub con una cuenta con escritura y permiso para actualizar workflows.
   No pegar tokens, certificados ni contraseñas en una conversación.
2. Ejecutar la CI del candidato en Windows, Linux, macOS Intel y Apple Silicon,
   incluyendo el daemon en Unix. Corregir cualquier fallo sin bajar aserciones.
3. Ejecutar el nuevo `Release` por `workflow_dispatch`: construye paquetes de
   prueba, ejecuta Inno en el runner y verifica instalación/desinstalación.
   Esa ejecución no publica y no exige certificados de producción.
4. Probar interfaz, cierre/reapertura, recuperación tras crash y agentes reales
   en instalaciones nativas. Comprobar además que los datos previos se conservan.
5. Configurar las credenciales reales documentadas en `RELEASE.md`: Windows
   Authenticode con timestamp; macOS Developer ID, notarización y Team ID.
   Probar upgrade desde una versión firmada por el mismo editor/certificado.
   Comprobar cancelación, error de disco, publisher distinto y sesiones vivas.
   Comprobar el árbol de procesos en Windows: la asociación al Job ocurre
   después del lanzamiento y las pruebas locales de plazo sólo comprueban
   terminación del hijo directo. No afirmar contención total de descendientes.
6. Publicar sólo después de validar el dry-run y el candidato. Los tags deben
   coincidir con Cargo; el workflow impide publicación sin firmas verificadas.
   Descargar y verificar los paquetes que se publicaron realmente.
7. Reemplazar placeholders de versión/SHA256 del cask con los DMG finales y
   publicar el tap con acceso real. Revisar protección de rama y permisos de
   archivos en instalaciones reales. No afirmar que ya existe una release/tap.
8. Revisar los cuatro avisos de mantenimiento. Eliminarlos exige migraciones
   de dependencias y validación multiplataforma; no sustituirlos por ignores.

## Prompt para continuar en otra máquina

> Continuá Terminal Canvas (`MauroProto/terminal-canvas`) desde el pase local
> de entrega de octubre de 2026. Leé `docs/DELIVERY-CONTINUATION.md`,
> `docs/RELEASE.md`, `docs/PORTABLE.md` y `docs/SUPPORT.md`. Conservá todos los
> microcommits e integrá mediante fast-forward en la rama principal `master`.
> Antes de trabajar comprobá el HEAD, el remoto, los permisos y si el pase
> local fue subido; si no, importá el bundle entregado, que requiere la base
> remota `f0458dd`. No descartes cambios ni reescribas historial. No modifiques
> Online/invitaciones. Cerrá los pendientes enumerados con evidencia real:
> CI multiplataforma, dry-run de release, instaladores, preservación de datos,
> pruebas con agentes reales, firma/notarización, publicación y Homebrew.
> Los tests locales y el ZIP debug ya probados no equivalen a una entrega
> firmada. No inventes secretos ni resultados. Usá Rust 1.98.0 y mantené baja
> la carga local: builds secuenciales con dos jobs, sin servidores ni procesos
> innecesarios. Corregí fallos en commits pequeños y reportá por separado lo
> comprobado, lo publicado y lo que necesita credenciales o hardware externo.

Si el pase aún no está en GitHub, el bundle de continuación guarda los commits
de este pase sobre `origin/master`. El archivo actualizado es
`terminal-canvas-delivery-20261006.bundle`; el bundle anterior conserva el pase
previo y no incluye estas últimas correcciones. En la otra máquina, dentro del checkout:

```bash
git fetch origin
git bundle verify /ruta/terminal-canvas-delivery-20261006.bundle
git fetch /ruta/terminal-canvas-delivery-20261006.bundle master
git switch master
git merge --ff-only FETCH_HEAD
```

Revisá el candidato y ejecutá sus comprobaciones antes de hacer `git push`.
