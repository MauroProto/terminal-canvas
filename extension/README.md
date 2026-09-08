# TerminalCanvas Design Mode (extensión MV3)

Activá la selección desde el icono de la extensión y elegí un elemento de la
página. Su HTML, CSS computado, rectángulo e imagen recortada se envían a
TerminalCanvas para el agente enfocado.

## Instalación (descomprimida)

1. Abrí TerminalCanvas. Al arrancar escribe el endpoint file con la URL y el
   token del servidor local:

   ```sh
   cat "$HOME/Library/Application Support/terminal-app/agent-hooks/endpoint.sh"
   # export TC_HOOK_URL=http://127.0.0.1:54321
   # export TC_HOOK_TOKEN=c08b36fe78884a7487c88be578e35a82
   ```

   El puerto es efímero: **cambia en cada arranque de la app**.

2. En Chrome/Edge/Brave: `chrome://extensions` → activá **Modo de
   desarrollador** → **Cargar descomprimida** → elegí esta carpeta
   (`extension/`).

3. Clickeá el icono de la extensión y pegá `TC_HOOK_URL` y `TC_HOOK_TOKEN`.
   Guardá.

## Uso

- En el popup, pulsá **Seleccionar elemento**. Aparece una indicación en esa
  pestaña; hacé clic sobre el elemento que querés cambiar. **Esc** cancela.
- La confirmación aparece cuando la app local acepta la captura. Los errores
  de token, conexión y cambio de pestaña se muestran en la página. Si falta
  la imagen, la confirmación lo indica explícitamente.
- El prompt le llega al **agente enfocado** con este formato exacto:

  ```
  Element: div.card > button
  Rect: x=10 y=20 w=100 h=40
  HTML: <button class="primary">Ok</button>
  CSS: display: inline-flex; background-color: rgb(240, 200, 110); …
  Screenshot: /Users/vos/Library/Application Support/terminal-app/agent-hooks/captures/design-abc.png
  ```

## Notas

- Se envía un conjunto acotado de propiedades de diseño computadas, sin
  insertar elementos de prueba que alteren el documento o hereden su CSS.
- La copia del HTML limita profundidad, cantidad de nodos y longitud. Omite
  scripts, iframes, manejadores de eventos, valores de formularios y atributos
  que indiquen credenciales. Revisá qué elemento seleccionás: su texto e imagen
  forman parte del contexto enviado.
- HTML y CSS se recortan también a 32 KB cada uno del lado de la app.
- Si el screenshot falla, la captura se manda igual sin la línea `Screenshot:`.
- La imagen pertenece a la ventana de origen y se recorta al área visible del
  elemento. Un cambio de pestaña o URL aborta el envío.
- El endpoint solo admite `http://127.0.0.1:puerto`, exige `X-TC-Token` y no
  sigue redirecciones. La extensión no solicita acceso persistente a todas las
  páginas: usa el permiso temporal de la acción del usuario.

## Verificación

```sh
node --test extension/tests/*.test.cjs
```

Las pruebas cubren aceptación y rechazo HTTP, cambio de pestaña, origen local
y recorte de imágenes. La activación sigue el contrato oficial de
[activeTab](https://developer.chrome.com/docs/extensions/develop/concepts/activeTab)
y la captura usa el `windowId` requerido por
[captureVisibleTab](https://developer.chrome.com/docs/extensions/reference/api/tabs#method-captureVisibleTab).
