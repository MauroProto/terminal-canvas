const urlInput = document.getElementById("url");
const tokenInput = document.getElementById("token");
const status = document.getElementById("status");

chrome.storage.local.get({ url: "", token: "" }).then((stored) => {
  urlInput.value = stored.url;
  tokenInput.value = stored.token;
});

document.getElementById("save").addEventListener("click", async () => {
  try {
    await save();
    status.textContent = "Guardado.";
  } catch (error) { status.textContent = error.message; }
});

async function save() {
  const url = new URL(urlInput.value.trim());
  if (url.protocol !== "http:" || url.hostname !== "127.0.0.1" || url.username || url.password ||
      url.pathname !== "/" || url.search || url.hash || !tokenInput.value.trim()) {
    throw new Error("Ingresá la dirección http://127.0.0.1:puerto y el token de la app.");
  }
  await chrome.storage.local.set({ url: url.origin, token: tokenInput.value.trim() });
}

document.getElementById("activate").addEventListener("click", async () => {
  try {
    await save();
    const result = await chrome.runtime.sendMessage({ kind: "tc-design-activate-tab" });
    if (!result?.ok) throw new Error(result?.error || "No se pudo activar la selección.");
    window.close();
  } catch (error) { status.textContent = error.message; }
});
