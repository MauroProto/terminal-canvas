const urlInput = document.getElementById("url");
const tokenInput = document.getElementById("token");
const status = document.getElementById("status");

chrome.storage.local.get({ url: "", token: "" }).then((stored) => {
  urlInput.value = stored.url;
  tokenInput.value = stored.token;
});

document.getElementById("save").addEventListener("click", async () => {
  await chrome.storage.local.set({
    url: urlInput.value.trim().replace(/\/$/, ""),
    token: tokenInput.value.trim(),
  });
  status.textContent = "Guardado.";
});
