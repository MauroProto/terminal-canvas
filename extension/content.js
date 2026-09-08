// Content script: alt+click captura el elemento y lo manda al service worker.
//
// Solo se recolectan las props computadas que difieren del default del
// navegador (~40 útiles): mandar las 340 de getComputedStyle haría un prompt
// inservible.

(() => {
if (globalThis.__tcDesignInstalled) return;
globalThis.__tcDesignInstalled = true;
let armed = false;
let sending = false;
let banner;
let hideTimer;

function feedback(text, hide = false) {
  clearTimeout(hideTimer);
  if (!banner?.isConnected) {
    banner = document.createElement("div");
    banner.setAttribute("role", "status");
    banner.style.cssText = "position:fixed;top:16px;right:16px;z-index:2147483647;background:#171717;color:#fff;padding:12px 18px;border:1px solid #f0c86e;border-radius:8px;font:14px system-ui;pointer-events:none;max-width:360px;box-shadow:0 4px 20px #0005";
    document.documentElement.appendChild(banner);
  }
  banner.textContent = text;
  if (hide) hideTimer = setTimeout(() => banner?.remove(), 5000);
}

chrome.runtime.onMessage.addListener(message => {
  if (message?.kind !== "tc-design-activate" || sending) return;
  armed = true;
  feedback("TerminalCanvas: hacé clic en un elemento. Esc cancela.");
});
document.addEventListener("keydown", event => {
  if (event.key === "Escape" && armed) {
    armed = false;
    banner?.remove();
    event.stopPropagation();
  }
}, true);

const USEFUL_PROPS = [
  "display", "position", "top", "right", "bottom", "left", "z-index",
  "width", "height", "min-width", "min-height", "max-width", "max-height",
  "margin", "padding", "border", "border-radius", "box-shadow", "outline",
  "color", "background-color", "background-image", "opacity",
  "font-family", "font-size", "font-weight", "line-height", "letter-spacing",
  "text-align", "text-transform", "text-decoration", "white-space",
  "flex-direction", "justify-content", "align-items", "gap", "flex-wrap",
  "grid-template-columns", "grid-template-rows",
  "overflow", "cursor", "transition", "transform",
];

/// Selector legible y corto: id si hay, si no tag + clases, con el padre
/// inmediato como contexto.
function readableSelector(element) {
  const describe = (node) => {
    if (!node || node.nodeType !== 1) return "";
    if (node.id) return `${node.tagName.toLowerCase()}#${node.id}`;
    const classes = (node.className && typeof node.className === "string")
      ? "." + node.className.trim().split(/\s+/).slice(0, 3).join(".")
      : "";
    return node.tagName.toLowerCase() + classes;
  };
  const self = describe(element);
  const parent = describe(element.parentElement);
  return parent ? `${parent} > ${self}` : self;
}

/// Props computadas que difieren del default de un elemento del mismo tag.
function meaningfulStyles(element) {
  const computed = window.getComputedStyle(element);
  const probe = document.createElement(element.tagName);
  document.body.appendChild(probe);
  const defaults = window.getComputedStyle(probe);
  const out = [];
  for (const prop of USEFUL_PROPS) {
    const value = computed.getPropertyValue(prop);
    if (value && value !== defaults.getPropertyValue(prop)) {
      out.push(`${prop}: ${value};`);
    }
  }
  probe.remove();
  return out.join(" ");
}

document.addEventListener(
  "click",
  async (event) => {
    if (!armed || sending) return;
    const element = event.target;
    if (!element || element.nodeType !== 1) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    armed = false;
    sending = true;
    banner?.remove();

    const rect = element.getBoundingClientRect();
    try {
    const result = await chrome.runtime.sendMessage({
      kind: "tc-design-capture",
      selector: readableSelector(element),
      html: element.outerHTML,
      css: meaningfulStyles(element),
      rect: {
        x: rect.x + window.scrollX,
        y: rect.y + window.scrollY,
        width: rect.width,
        height: rect.height,
      },
      // Coordenadas de viewport para recortar el screenshot.
      viewportRect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
      dpr: window.devicePixelRatio || 1,
    });

    if (!result?.ok) throw new Error(result?.error || "No se recibió confirmación de la app.");
    feedback(result.warning || "Elemento enviado a TerminalCanvas.", true);
    } catch (error) {
      feedback(error.message || "No se pudo enviar el elemento.", true);
    } finally { sending = false; }
  },
  true,
);
})();
