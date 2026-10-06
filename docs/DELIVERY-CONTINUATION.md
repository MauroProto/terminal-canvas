# Estado de entrega y continuación — 6 de octubre de 2026

La rama principal del repositorio se llama `master`. La reversión pedida en
septiembre quedó integrada mediante `bc5768d`; la base remota revisada para
este pase es `f0458dd1dae08b3aca080a914042deb5a9454cf3`. Este pase agrega
perfiles aislados, diagnóstico de paquetes, actualización verificada,
instalador Windows, validaciones de distribución, limpieza de dependencias y
correcciones de persistencia al cerrar. El checkpoint de entrega contiene
29 microcommits locales sobre esa base remota.
Online e invitaciones no reciben cambios funcionales en este pase.

## Verificación realizada en esta PC

- Rust/Cargo/Clippy 1.98.0, Windows x86_64, máximo dos jobs de compilación y
  dos threads de pruebas. El último pase usó un solo job con prioridad baja.
  No se usó WSL ni se inició un servidor persistente.
- Formato y Clippy de todos los targets, con warnings como errores.
- Suite completa de todos los targets: **889 pruebas aprobadas y tres ignoradas**,
  incluyendo **850 pruebas de librería aprobadas y tres ignoradas**, más los tres
  benchmarks en modo smoke. El código comprobado termina en el commit local
  `c2faf67`; la
  documentación de relevo se agregó después.
  Dos pruebas ignoradas son fixtures que las regresiones normales invocan
  explícitamente en procesos hijos; la tercera depende de sesiones Claude reales.
- Pruebas de perfil mediante procesos independientes: layout, configuración,
  historial de dos hojas, notas y memoria conservan datos entre ejecuciones.
  Una salida con `process::exit` antes del cleanup conserva el marcador y los
  datos para la siguiente ejecución; esto no simula corte de energía ni crash
  nativo. El CLI de consulta no altera bytes, fechas ni permisos de un perfil
  poblado con marcador pendiente. Rutas bloqueadas por archivos fallan sin
  generar datos de respaldo en el directorio de trabajo.
- El cierre final y el cierre de workspace esperan los autosaves aceptados
  antes de publicar su layout. Los fallos de configuración, layout o historial
  mantienen el marcador de recuperación; se rescatan los datos que pueden
  guardarse. La poda espera el layout actual confirmado y conserva el historial
  anterior si falla publicarlo. Nueve regresiones usan perfiles y procesos
  independientes, sin abrir interfaz ni terminales reales.
- Los ZIP de diagnóstico se terminan antes de publicarlos atómicamente y usan
  nombres únicos. Las regresiones comprueban dos exports con la misma fecha y la
  conservación del destino y limpieza temporal ante fallos. La aserción de
  permisos Unix está incluida, pero requiere ejecución en Unix.
- Se agregaron pruebas con un servidor HTTPS local y certificado propio para
  descarga completa, progreso, checksum incorrecto, respuesta incompleta,
  límites, redirección rechazada y cancelación de una respuesta detenida.
  Usan caches temporales independientes y no modifican el perfil habitual.
- Los comandos del instalador tienen plazo y límite de salida. Windows exige
  que el registro de Inno nombre la copia que está ejecutándose y entrega esa
  carpeta mediante `/DIR`; una copia portable firmada usa actualización manual.
  Las regresiones incluyen Unicode, espacios y rutas UNC.
- Windows configura el Job para terminar los helpers asociados cuando el SO
  cierra su último handle, incluso si la app sale sin ejecutar `Drop`. Dos
  pruebas reales comprueban cierre del handle y salida abrupta del padre.
  La ventana entre lanzamiento y asociación al Job sigue pendiente.
- 15 pruebas del validador de paquetes y 9 de la extensión (éstas del pase
  anterior, sin cambios posteriores en la extensión). Un TAR con 5.006
  entradas se rechaza tras leer sólo ocho encabezados, sin extraer archivos.
- Se extrajo y comprobó un nuevo ZIP **debug sin firma**, con arquitectura, manifiesto,
  SHA256, versión, diagnóstico, helper de memoria y handshake/listado MCP.
  Esto no valida un instalador de producción ni una firma.
- `cargo-audit 0.22.2`: cero vulnerabilidades conocidas, cuatro avisos de
  mantenimiento (`bincode`, `paste`, `rustls-pemfile`, `ttf-parser`), sin ignores.

Los logs del último cierre se guardan en `dist/validation-clippy-job-cleanup.log`,
`dist/validation-tests-job-cleanup.log` y `dist/validation-package-job-cleanup.log`;
la auditoría previa está en
`dist/validation-audit.json`. No se deben confundir
los resultados locales con una nueva CI multiplataforma del candidato.
Al preparar este checkpoint, la última CI pública exitosa era la de `f0458dd`;
no había ejecuciones de `Release` ni releases publicadas. Comprobar los runs y
el HEAD actual después de publicar; la CI también permite `workflow_dispatch`.
Los logs y herramientas dentro de `dist` son locales y no forman parte del bundle.

## Pendientes concretos

1. Confirmar que este pase está publicado. La cuenta `ainiagent` del CLI tiene
   sólo lectura; el conector de la app está vinculado a `MauroProto` y permite
   escribir contenidos. Comprobar además permiso para actualizar workflows.
   La publicación mediante Git Data API conserva árboles, mensajes y secuencia,
   pero asigna nuevos IDs y fechas. Los commits locales originales se conservan
   en el bundle y una rama de respaldo. Verificar cada árbol y actualizar la
   rama principal sólo mediante fast-forward, con la base remota esperada.
   Si se usa el CLI, autenticar una cuenta con escritura y permiso de workflows.
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
   después del lanzamiento. Las pruebas comprueban el helper asociado y el
   cierre abrupto del padre; no demuestran contención de procesos que escaparon
   antes de asociar el Job.
6. Publicar sólo después de validar el dry-run y el candidato. Los tags deben
   coincidir con Cargo; el workflow impide publicación sin firmas verificadas.
   Descargar y verificar los paquetes que se publicaron realmente.
7. Reemplazar placeholders de versión/SHA256 del cask con los DMG finales y
   publicar el tap con acceso real. Revisar protección de rama y permisos de
   archivos en instalaciones reales. No afirmar que ya existe una release/tap.
8. Ejecutar las migraciones de mantenimiento descritas en
   [DEPENDENCY-MAINTENANCE.md](DEPENDENCY-MAINTENANCE.md), respetando la exclusión
   de Online/invitaciones. Requieren validación multiplataforma, sin ignores.

## Prompt para continuar en otra máquina

> Continuá Terminal Canvas (`MauroProto/terminal-canvas`) desde el pase local
> de entrega de octubre de 2026. Leé `docs/DELIVERY-CONTINUATION.md`,
> `docs/RELEASE.md`, `docs/PORTABLE.md`, `docs/SUPPORT.md` y
> `docs/DEPENDENCY-MAINTENANCE.md`. Conservá todos los
> microcommits e integrá mediante fast-forward en la rama principal `master`.
> Antes de trabajar comprobá el HEAD, el remoto, los permisos y si el pase
> local fue subido. Si se publicó por Git Data API, sus IDs pueden diferir de
> los locales: compará árboles y usá la rama publicada; no fuerces la mezcla
> de los dos historiales. Si aún no se publicó, importá el bundle, que requiere la base
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
`terminal-canvas-delivery-20261006-verified.bundle`; los bundles anteriores
conservan sus pases previos y no incluyen estas últimas correcciones.
En la otra máquina, dentro del checkout:

```bash
git fetch origin
git bundle verify /ruta/terminal-canvas-delivery-20261006-verified.bundle
git fetch /ruta/terminal-canvas-delivery-20261006-verified.bundle master
git switch master
git merge --ff-only FETCH_HEAD
```

Revisá el candidato y ejecutá sus comprobaciones antes de hacer `git push`.
