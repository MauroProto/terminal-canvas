// Injected after an explicit extension action; selection is cancelled by Escape.
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
    if (node.id) return `${node.tagName.toLowerCase()}#${CSS.escape(node.id)}`;
    const classes = (node.className && typeof node.className === "string")
      ? "." + node.className.trim().split(/\s+/).filter(Boolean).slice(0, 3).map(name => CSS.escape(name)).join(".")
      : "";
    return node.tagName.toLowerCase() + classes;
  };
  const self = describe(element);
  const parent = describe(element.parentElement);
  return parent ? `${parent} > ${self}` : self;
}

// Selected computed properties; no probe in the page, which would inherit its CSS.
function meaningfulStyles(element) {
  const computed = window.getComputedStyle(element);
  const out = [];
  for (const prop of USEFUL_PROPS) {
    const value = computed.getPropertyValue(prop);
    if (value) {
      out.push(`${prop}: ${value.slice(0, 512)};`);
    }
  }
  return out.join(" ");
}

// Clone a bounded subtree, omitting form contents and executable/private attributes.
function captureHtml(element) {
  let remaining = 24000;
  let nodes = 0;
  function copy(node, depth) {
    if (++nodes > 512 || remaining <= 0 || depth > 32) return null;
    if (node.nodeType === 3) {
      const text = node.textContent.slice(0, Math.min(remaining, 2048));
      remaining -= text.length;
      return document.createTextNode(text);
    }
    if (node.nodeType !== 1 || /^(SCRIPT|STYLE|IFRAME|OBJECT|EMBED|NOSCRIPT)$/.test(node.tagName)) return null;
    const clone = document.createElement(node.tagName.toLowerCase());
    for (const attribute of Array.from(node.attributes).slice(0, 32)) {
      if (/^on|value|srcdoc|token|secret|password|credential|nonce/i.test(attribute.name)) continue;
      const value = attribute.value.slice(0, Math.min(remaining, 512));
      remaining -= attribute.name.length + value.length + 4;
      if (remaining < 0) break;
      clone.setAttribute(attribute.name, value);
    }
    if (!/^(INPUT|TEXTAREA|SELECT)$/.test(node.tagName) && !node.isContentEditable) {
      for (const child of node.childNodes) {
        if (nodes >= 512 || remaining <= 0) break;
        const result = copy(child, depth + 1);
        if (result) clone.appendChild(result);
      }
    }
    remaining -= node.tagName.length * 2 + 5;
    return clone;
  }
  return (copy(element, 0)?.outerHTML || "").slice(0, 32768);
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
      html: captureHtml(element),
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
