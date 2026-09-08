// MV3 service worker. Capture only the page explicitly activated by the user.
const DEFAULTS = { url: "", token: "" };

function validateEndpoint(stored) {
  let url;
  try { url = new URL(stored.url); } catch { throw new Error("Ingresá la dirección local de TerminalCanvas."); }
  if (url.protocol !== "http:" || url.hostname !== "127.0.0.1" ||
      url.username || url.password || url.search || url.hash || url.pathname !== "/") {
    throw new Error("La dirección debe ser http://127.0.0.1:puerto.");
  }
  const token = String(stored.token || "").trim();
  if (!token || /[\r\n]/.test(token)) throw new Error("Ingresá un token válido de TerminalCanvas.");
  return { url: url.origin, token };
}

async function activateTab() {
  validateEndpoint(await chrome.storage.local.get(DEFAULTS));
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab?.id || !/^https?:/.test(tab.url || "")) throw new Error("Abrí una página web para seleccionar un elemento.");
  await chrome.scripting.executeScript({ target: { tabId: tab.id }, files: ["content.js"] });
  await chrome.tabs.sendMessage(tab.id, { kind: "tc-design-activate" }, { frameId: 0 });
  return { ok: true };
}

async function assertSourceActive(tab) {
  if (!Number.isInteger(tab?.id) || !Number.isInteger(tab?.windowId)) throw new Error("No se pudo identificar la pestaña de origen.");
  const [active] = await chrome.tabs.query({ active: true, windowId: tab.windowId });
  if (active?.id !== tab.id || active.url !== tab.url) throw new Error("La pestaña cambió. Volvé al elemento e intentá de nuevo.");
}

function cropRect(rect, dpr, width, height) {
  if (!rect || ![rect.x, rect.y, rect.width, rect.height, dpr].every(Number.isFinite) ||
      rect.width <= 0 || rect.height <= 0 || dpr <= 0 || dpr > 8) throw new Error("El elemento no tiene un área visible válida.");
  const x = Math.max(0, Math.floor(rect.x * dpr));
  const y = Math.max(0, Math.floor(rect.y * dpr));
  const right = Math.min(width, Math.ceil((rect.x + rect.width) * dpr));
  const bottom = Math.min(height, Math.ceil((rect.y + rect.height) * dpr));
  if (right <= x || bottom <= y) throw new Error("El elemento está fuera de la pantalla.");
  return { x, y, width: right - x, height: bottom - y };
}

async function croppedScreenshot(tab, viewportRect, dpr) {
  await assertSourceActive(tab);
  const dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId, { format: "png" });
  await assertSourceActive(tab);
  const bitmap = await createImageBitmap(await (await fetch(dataUrl)).blob());
  try {
    const rect = cropRect(viewportRect, dpr, bitmap.width, bitmap.height);
    const scale = Math.min(1, 2048 / Math.max(rect.width, rect.height));
    const canvas = new OffscreenCanvas(Math.max(1, Math.round(rect.width * scale)), Math.max(1, Math.round(rect.height * scale)));
    canvas.getContext("2d").drawImage(bitmap, rect.x, rect.y, rect.width, rect.height, 0, 0, canvas.width, canvas.height);
    const blob = await canvas.convertToBlob({ type: "image/png" });
    if (blob.size > 4 * 1024 * 1024) throw new Error("La captura es demasiado grande.");
    const bytes = new Uint8Array(await blob.arrayBuffer());
    let binary = "";
    for (let i = 0; i < bytes.length; i += 8192) binary += String.fromCharCode(...bytes.subarray(i, i + 8192));
    return "data:image/png;base64," + btoa(binary);
  } finally {
    bitmap.close();
  }
}

async function capture(message, sender) {
  if (sender.frameId !== 0) throw new Error("Seleccioná un elemento en la página principal.");
  const { url, token } = validateEndpoint(await chrome.storage.local.get(DEFAULTS));
  await assertSourceActive(sender.tab);
  let screenshot = null;
  let warning = "";
  try {
    screenshot = await croppedScreenshot(sender.tab, message.viewportRect, message.dpr || 1);
  } catch (error) {
    warning = "Enviado sin imagen: " + error.message;
  }
  await assertSourceActive(sender.tab);
  const response = await fetch(url + "/design/capture", {
    method: "POST",
    redirect: "error",
    signal: AbortSignal.timeout(10000),
    headers: { "Content-Type": "application/json", "X-TC-Token": token },
    body: JSON.stringify({
      selector: String(message.selector || "").slice(0, 2048),
      html: String(message.html || "").slice(0, 32768),
      css: String(message.css || "").slice(0, 32768),
      rect: message.rect,
      screenshot_b64: screenshot,
    }),
  });
  if (!response.ok) throw new Error("TerminalCanvas rechazó la captura (" + response.status + "). Revisá el token y la app.");
  return { ok: true, warning };
}

chrome.runtime.onMessage.addListener((message, sender, respond) => {
  let task;
  if (message?.kind === "tc-design-activate-tab" && !sender.tab) task = activateTab();
  else if (message?.kind === "tc-design-capture") task = capture(message, sender);
  else return false;
  task.then(respond, error => respond({ ok: false, error: error.message || "No se pudo conectar con TerminalCanvas." }));
  return true; // Keep the response channel open until the local app acknowledges.
});

