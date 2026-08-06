// Service worker: recorta el screenshot y postea a la app local.

const DEFAULTS = { url: "", token: "" };

async function endpoint() {
  const stored = await chrome.storage.local.get(DEFAULTS);
  return { url: (stored.url || "").trim(), token: (stored.token || "").trim() };
}

/// Recorta el screenshot de la pestaña al rect del elemento.
async function croppedScreenshot(tabId, viewportRect, dpr) {
  try {
    const dataUrl = await chrome.tabs.captureVisibleTab({ format: "png" });
    if (!viewportRect || viewportRect.width < 1 || viewportRect.height < 1) return dataUrl;
    const blob = await (await fetch(dataUrl)).blob();
    const bitmap = await createImageBitmap(blob);
    const canvas = new OffscreenCanvas(
      Math.round(viewportRect.width * dpr),
      Math.round(viewportRect.height * dpr),
    );
    const context = canvas.getContext("2d");
    context.drawImage(
      bitmap,
      Math.round(viewportRect.x * dpr),
      Math.round(viewportRect.y * dpr),
      canvas.width,
      canvas.height,
      0, 0, canvas.width, canvas.height,
    );
    const cropped = await canvas.convertToBlob({ type: "image/png" });
    return await new Promise((resolve) => {
      const reader = new FileReader();
      reader.onloadend = () => resolve(reader.result);
      reader.readAsDataURL(cropped);
    });
  } catch (error) {
    // Sin screenshot la captura sigue siendo útil: no se aborta.
    console.warn("TerminalCanvas: no se pudo capturar la pantalla", error);
    return null;
  }
}

chrome.runtime.onMessage.addListener((message, sender) => {
  if (!message || message.kind !== "tc-design-capture") return;
  (async () => {
    const { url, token } = await endpoint();
    if (!url || !token) {
      console.warn("TerminalCanvas: falta configurar URL/token en el popup");
      return;
    }
    const screenshot = await croppedScreenshot(
      sender.tab?.id,
      message.viewportRect,
      message.dpr || 1,
    );
    try {
      await fetch(`${url}/design/capture`, {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-TC-Token": token },
        body: JSON.stringify({
          selector: message.selector,
          html: message.html,
          css: message.css,
          rect: message.rect,
          screenshot_b64: screenshot,
        }),
      });
    } catch (error) {
      console.warn("TerminalCanvas: no se pudo postear la captura", error);
    }
  })();
});
