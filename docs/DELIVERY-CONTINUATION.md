# Estado de entrega y continuación — 6 de octubre de 2026

La rama principal del repositorio se llama `master`. La reversión pedida en
septiembre quedó integrada mediante `bc5768d`; la base remota revisada para
este pase es `f0458dd1dae08b3aca080a914042deb5a9454cf3`. Este pase agrega
perfiles aislados, diagnóstico de paquetes, actualización verificada,
instalador Windows, validaciones de distribución, limpieza de dependencias y
correcciones de persistencia al cerrar. El checkpoint de entrega contiene
29 microcommits publicados sobre esa base remota. El checkpoint publicado es
`d79f0fa856ca24a873920b47134000fd9df22262`; los commits locales originales
se conservaron antes de sincronizar el checkout con GitHub.
Después se publicaron dos microcommits de infraestructura, hasta `9f0e84d`,
para usar acciones Node 24 fijadas por SHA y Ubuntu 24.04. No cambian código
de la app, permisos de publicación ni controles de firma.
El siguiente microcommit, `5107f41`, limita los metadatos ZIP antes de construir
el parser del validador de paquetes. El checkpoint `5107f41` contiene
**32 microcommits publicados** sobre la base revisada; el código Rust conserva
el mismo árbol que la CI del checkpoint `d79f0fa`.
Online e invitaciones no reciben cambios funcionales en este pase.

## Verificación realizada en esta PC

- Rust/Cargo/Clippy 1.98.0, Windows x86_64, máximo dos jobs de compilación y
  dos threads de pruebas. El último pase usó un solo job con prioridad baja.
  No se usó WSL ni se inició un servidor persistente.
- Formato y Clippy de todos los targets, con warnings como errores.
- Suite completa de todos los targets: **889 pruebas aprobadas y tres ignoradas**,
  incluyendo **850 pruebas de librería aprobadas y tres ignoradas**, más los tres
  benchmarks en modo smoke. El código comprobado termina en el commit local
  `c2faf67`, publicado como `1588988`. Los cambios posteriores afectan
  scripts de validación, infraestructura y documentación.
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
- **27 pruebas del validador de paquetes**. Un TAR con 5.006 entradas se rechaza
  tras leer sólo ocho encabezados, sin extraer archivos. Un ZIP con 3.005
  entradas se rechaza antes de construir `ZipFile`; también se comprueban
  conteos falsificados, ZIP64, offsets, comentarios y límites de metadatos.
  Las nueve pruebas de la extensión pasaron también en GitHub.
- Se extrajo y comprobó un nuevo ZIP **debug sin firma**, con arquitectura, manifiesto,
  SHA256, versión, diagnóstico, helper de memoria y handshake/listado MCP.
  El mismo ZIP real volvió a pasar el nuevo preflight ZIP y los smoke tests,
  sin recompilar ni abrir la interfaz.
  Esto no valida un instalador de producción ni una firma.
- `cargo-audit 0.22.2`: cero vulnerabilidades conocidas, cuatro avisos de
  mantenimiento (`bincode`, `paste`, `rustls-pemfile`, `ttf-parser`), sin ignores.

Los logs del último cierre se guardan en `dist/validation-clippy-job-cleanup.log`,
`dist/validation-tests-job-cleanup.log` y `dist/validation-package-job-cleanup.log`;
la auditoría previa está en
`dist/validation-audit.json`. No se deben confundir
los resultados locales con una CI multiplataforma del candidato.
La [CI del checkpoint publicado](https://github.com/MauroProto/terminal-canvas/actions/runs/37544616708)
y el [ensayo manual de distribución](https://github.com/MauroProto/terminal-canvas/actions/runs/37545144493)
apuntan exactamente a `d79f0fa`. La auditoría y las nueve pruebas de la extensión
ya pasaron; el ensayo pasó sus 15 regresiones del validador de paquetes.
La CI de ese checkpoint y la
[CI de la revisión actual](https://github.com/MauroProto/terminal-canvas/actions/runs/37549072652)
(`5107f41`, con Node 24 y el validador ZIP corregido) terminaron completamente
en verde, con estos mismos resultados:

| Plataforma | Suite normal | Suite con daemon | Regresiones de seguridad |
| --- | --- | --- | --- |
| Windows x86_64 | 889 aprobadas, 3 ignoradas | No aplica | 17 aprobadas |
| Linux x86_64 | 956 aprobadas, 3 ignoradas | 996 aprobadas, 3 ignoradas | 20 aprobadas |
| macOS Intel | 955 aprobadas, 3 ignoradas | 995 aprobadas, 3 ignoradas | 20 aprobadas |
| macOS Apple Silicon | 955 aprobadas, 3 ignoradas | 995 aprobadas, 3 ignoradas | 20 aprobadas |

Las suites normal y daemon son configuraciones separadas; no sumar sus
recuentos como pruebas distintas. El benchmark comparativo del workflow se
omitió porque sólo corre para pull requests.

La [CI de las acciones actualizadas](https://github.com/MauroProto/terminal-canvas/actions/runs/37546104540)
apunta a `9f0e84d`: Windows, Linux, extensión y auditoría pasaron. Se canceló
cuando macOS seguía esperando runners, para dar paso al candidato ZIP.
Su [ensayo de distribución](https://github.com/MauroProto/terminal-canvas/actions/runs/37546248753)
se canceló mientras esperaba, sin jobs ni artefactos, para reemplazarlo por
la revisión ZIP. La [CI de la revisión ZIP](https://github.com/MauroProto/terminal-canvas/actions/runs/37549072652)
y el [ensayo actualizado](https://github.com/MauroProto/terminal-canvas/actions/runs/37549143310)
apuntan exactamente a `5107f4155d7899968753e2fa3b215f4f137d04d4`.
CI y Release tienen grupos separados de concurrencia. Tanto la CI actual como
el ensayo actualizado terminaron completamente en verde. El ensayo actual validó
el ZIP Windows con el nuevo verificador, construyó/instaló/desinstaló Inno,
montó y comprobó ambos DMG y verificó el tarball Linux, incluidos sus helpers.
`verify-set` comprobó los cinco paquetes y cinco checksums, sin extras; `publish`
quedó omitido. El ensayo base también terminó completamente en verde:
ZIP Windows, instalador Inno construido/instalado/desinstalado en el runner,
tarball Linux y ambos DMG macOS montados y comprobados. Los helpers pasaron
sus smoke tests. `verify-set` descargó los cuatro grupos de artefactos y
validó exactamente cinco paquetes y sus cinco SHA256, sin extras. `publish`
quedó omitido. El ensayo actualizado ya pasó las 27 regresiones Python.
El ensayo manual no crea tags ni releases. Sus artefactos `dry-run-*` se conservan
14 días en Actions y, en este repositorio público, son descargables por lectores
autenticados. No hay una release firmada publicada.
Los logs y herramientas dentro de `dist` son locales y no forman parte del bundle.

Los últimos commits de este cierre sólo actualizan documentación, incluida
`PORTABLE.md`, que se copia dentro de los paquetes. Los artefactos del ensayo
`5107f41` se verifican con ese mismo checkout: el validador compara también
LICENSE y PORTABLE con su fuente. No usar una revisión documental posterior
para comprobar byte por byte esos archivos anteriores. El workflow de una
release real reconstruye los paquetes y su documentación desde el tag exacto.

## Publicación y respaldo

La publicación del checkpoint `5107f41` está realizada: se verificaron los
32 árboles, mensajes y padres antes de avanzar `master`, sin force push.
El conector autorizado de `MauroProto` pudo publicar también los workflows.
Git Data API asignó nuevos IDs, fechas y metadatos de autor; los originales
permanecen en la rama local `codex/local-delivery-20261006` y el bundle original.
Usar la rama publicada para continuar. Los originales de los siguientes pases
quedan en `codex/ci-runtime-delivery-20261006` y
`codex/zip-preflight-delivery-20261006`. El CLI de esta PC conserva su cuenta
de sólo lectura. Revalidar la CI cuando se cambie código, sin bajar aserciones.

## Pendientes concretos

1. Probar interfaz, cierre/reapertura, recuperación tras crash y agentes reales
   en instalaciones nativas. Comprobar además que los datos previos se conservan.
2. Cerrar la ventana entre lanzamiento del helper y asociación al Job Windows
   de los comandos del actualizador (`src/update_install.rs`).
   Las regresiones comprueban el helper asociado y el cierre abrupto del padre;
   no demuestran contención de procesos que escaparon antes de asociar el Job.
   Resolverlo con lanzamiento contenido antes de ejecutar código del hijo y
   probar también nietos creados inmediatamente, timeout y cancelación.
3. Antes de publicar, configurar las credenciales reales documentadas en `RELEASE.md`: Windows
   Authenticode con timestamp; macOS Developer ID, notarización y Team ID.
   Probar upgrade desde una versión firmada por el mismo editor/certificado.
   Comprobar cancelación, error de disco, publisher distinto y sesiones vivas.
4. Publicar sólo después de validar el dry-run y el candidato. Los tags deben
   coincidir con Cargo; el workflow impide publicación sin firmas verificadas.
   Descargar y verificar los paquetes que se publicaron realmente.
5. Reemplazar placeholders de versión/SHA256 del cask con los DMG finales y
   publicar el tap con acceso real. Revisar protección de rama y permisos de
   archivos en instalaciones reales. No afirmar que ya existe una release/tap.
6. Ejecutar las migraciones de mantenimiento descritas en
   [DEPENDENCY-MAINTENANCE.md](DEPENDENCY-MAINTENANCE.md), respetando la exclusión
   de Online/invitaciones. Requieren validación multiplataforma, sin ignores.

Para la validación nativa, usar una copia de los datos y un perfil aislado:
crear dos workspaces con divisiones, escribir historial distinto, agregar notas
y memoria, cerrar y reabrir; comparar layout y datos. Repetir un cierre con
autosave pendiente y una terminación abrupta, comprobando el aviso de recuperación
y los archivos conservados. En Unix, verificar la reconexión al daemon y el
cierre explícito de sus sesiones; en Windows, verificar ConPTY y las terminales
nuevas al reabrir. Probar los agentes que se usan realmente, incluyendo Unicode,
paste, clipboard y sesiones activas al intentar actualizar. Registrar plataforma,
versión y resultados sin adjuntar contenido privado del perfil.

## Prompt para continuar en otra máquina

> Continuá Terminal Canvas (`MauroProto/terminal-canvas`) desde la rama publicada
> `master`; el checkpoint Rust y de entrega es `d79f0fa`, seguido por acciones
> Node 24 y validación ZIP acotada hasta `5107f41`. Usá el HEAD publicado actual.
> Leé `docs/DELIVERY-CONTINUATION.md`,
> `docs/RELEASE.md`, `docs/PORTABLE.md`, `docs/SUPPORT.md` y
> `docs/DEPENDENCY-MAINTENANCE.md`. Conservá todos los
> microcommits e integrá mediante fast-forward en la rama principal `master`.
> Antes de trabajar comprobá el HEAD, el remoto, los permisos y los resultados
> de la CI 37544616708 y del ensayo Release 37545144493 sobre `d79f0fa`.
> Verificá también CI 37549072652 y Release 37549143310 sobre `5107f41`.
> Las dos CI y los dos ensayos completos quedaron aprobados; los ensayos
> omitieron publicación y generaron paquetes de prueba sin firma del proveedor.
> Los últimos commits posteriores a `5107f41` sólo corrigen documentación.
> Para verificar archivos empaquetados del ensayo, usá su mismo checkout.
> No repitas builds completos sin cambios ni una falla que investigar.
> Las corridas 9f se reemplazaron: CI 37546104540 se canceló con Windows/Linux,
> extensión y auditoría aprobados y macOS aún en cola; Release 37546248753
> se canceló antes de ejecutar.
> Los IDs del respaldo original difieren de los publicados por Git Data API:
> usá la rama publicada; no fuerces la mezcla de los dos historiales.
> No descartes cambios ni reescribas historial. No modifiques
> Online/invitaciones. Cerrá los pendientes enumerados con evidencia real:
> preservación de datos e interfaz en sistemas nativos, pruebas con agentes reales,
> contención de helpers Windows desde el lanzamiento, firma/notarización,
> upgrade firmado, publicación, Homebrew y las migraciones
> de mantenimiento compatibles con la exclusión de Online/invitaciones.
> Los tests locales y el ZIP debug ya probados no equivalen a una entrega
> firmada. No inventes secretos ni resultados. Usá Rust 1.98.0 y mantené baja
> la carga local: builds secuenciales con un job y dos threads de pruebas,
> sin servidores ni procesos
> innecesarios. Corregí fallos en commits pequeños y reportá por separado lo
> comprobado, lo publicado y lo que necesita credenciales o hardware externo.

El pase ya está en GitHub. En la otra máquina, preferir un clon actualizado
de `master`. También se verificó `terminal-canvas-published-source-20261006.bundle`,
que contiene exactamente los 32 commits publicados hasta `5107f41` y requiere
la base `f0458dd`. No incluye cambios posteriores a ese checkpoint ni esta
actualización final de documentación. El bundle anterior
`terminal-canvas-published-20261006.bundle` conserva los primeros 29 commits
publicados hasta `d79f0fa`.
El archivo `terminal-canvas-delivery-20261006-verified.bundle` conserva el
historial local original con otros IDs; no mezclarlo con la rama publicada.
Los bundles anteriores conservan sus pases previos.
Para recuperar el checkpoint publicado en un checkout compatible:

```bash
git fetch origin
git bundle verify /ruta/terminal-canvas-published-source-20261006.bundle
git fetch /ruta/terminal-canvas-published-source-20261006.bundle master
git switch master
git merge --ff-only FETCH_HEAD
```

Después, integrar con `git merge --ff-only origin/master` cualquier avance
publicado posterior. Revisar el candidato y ejecutar sus comprobaciones antes
de publicar nuevos cambios. No pegar tokens, certificados ni contraseñas en
una conversación.
