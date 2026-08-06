// Content script: alt+click captura el elemento y lo manda al service worker.
//
// Solo se recolectan las props computadas que difieren del default del
// navegador (~40 útiles): mandar las 340 de getComputedStyle haría un prompt
// inservible.

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
  (event) => {
    if (!event.altKey) return;
    const element = event.target;
    if (!element || element.nodeType !== 1) return;
    event.preventDefault();
    event.stopPropagation();

    const rect = element.getBoundingClientRect();
    chrome.runtime.sendMessage({
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

    // Feedback inmediato: un flash sobre el elemento capturado.
    const previous = element.style.outline;
    element.style.outline = "2px solid #f0c86e";
    setTimeout(() => { element.style.outline = previous; }, 350);
  },
  true,
);
