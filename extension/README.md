# TerminalCanvas Design Mode (extensión MV3)

Alt+click en cualquier elemento de una página y su HTML, CSS computado
relevante, rect y un screenshot recortado se van al agente enfocado de
TerminalCanvas.

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

- **Alt+click** sobre el elemento que querés cambiar. Un flash naranja confirma
  la captura y en TerminalCanvas aparece el toast "Elemento capturado".
- El prompt le llega al **agente enfocado** con este formato exacto:

  ```
  Element: div.card > button
  Rect: x=10 y=20 w=100 h=40
  HTML: <button class="primary">Ok</button>
  CSS: display: inline-flex; background-color: rgb(240, 200, 110); …
  Screenshot: /Users/vos/Library/Application Support/terminal-app/agent-hooks/captures/design-abc.png
  ```

## Notas

- El CSS que se manda son solo las props que **difieren del default** del
  navegador para ese tag (~40 candidatas): mandar las 340 de
  `getComputedStyle` haría el prompt inservible.
- HTML y CSS se recortan a 32 KB cada uno del lado de la app.
- Si el screenshot falla, la captura se manda igual sin la línea `Screenshot:`.
- El endpoint solo escucha en `127.0.0.1` y exige el header `X-TC-Token`.
