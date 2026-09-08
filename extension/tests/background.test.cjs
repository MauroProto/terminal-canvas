const { test } = require("node:test");
const assert = require("node:assert/strict");
const vm = require("node:vm");
const fs = require("node:fs");
const path = require("node:path");

function worker(overrides = {}) {
  const requests = [];
  const source = { id: 7, windowId: 3, url: "https://example.test/" };
  let listener;
  const context = vm.createContext({
    URL, AbortSignal, Uint8Array, btoa,
    chrome: {
      storage: { local: { get: async () => ({ url: "http://127.0.0.1:5000", token: "local-secret" }) } },
      tabs: {
        query: async () => [source],
        captureVisibleTab: async (windowId) => { requests.push({ windowId }); throw new Error("image unavailable"); },
        sendMessage: async () => {},
        ...overrides.tabs,
      },
      scripting: { executeScript: async () => {} },
      runtime: { onMessage: { addListener: callback => { listener = callback; } } },
    },
    fetch: async (url, init) => { requests.push({ url, init }); return { ok: true, status: 200 }; },
    ...overrides.globals,
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../background.js"), "utf8"), context);
  return { context, requests, source, send: message => new Promise(resolve => {
    assert.equal(listener(message, { tab: source, frameId: 0 }, resolve), true);
  }) };
}

test("capture responds only after the local HTTP result and reports a missing image", async () => {
  const w = worker();
  const result = await w.send({ kind: "tc-design-capture", html: "<button>OK</button>" });
  assert.equal(result.ok, true);
  assert.match(result.warning, /sin imagen/);
  assert.equal(w.requests[0].windowId, 3);
  assert.equal(w.requests[1].url, "http://127.0.0.1:5000/design/capture");
  assert.equal(w.requests[1].init.redirect, "error");
  assert.equal(JSON.parse(w.requests[1].init.body).screenshot_b64, null);
});

test("HTTP rejection never reports success", async () => {
  const w = worker({ globals: { fetch: async () => ({ ok: false, status: 401 }) } });
  const result = await w.send({ kind: "tc-design-capture" });
  assert.equal(result.ok, false);
  assert.match(result.error, /401/);
});

test("switching tabs during capture prevents submission", async () => {
  let queries = 0;
  const w = worker({ tabs: { query: async () => [++queries <= 2
    ? { id: 7, windowId: 3, url: "https://example.test/" }
    : { id: 8, windowId: 3, url: "https://other.test/" }] } });
  const result = await w.send({ kind: "tc-design-capture" });
  assert.equal(result.ok, false);
  assert.match(result.error, /pestaña cambió/);
  assert.equal(w.requests.filter(request => request.url).length, 0);
});

test("configuration refuses remote destinations, credentials and URL suffixes", () => {
  const { context } = worker();
  for (const url of ["https://example.test", "http://localhost:1234", "http://127.0.0.1.evil.test", "http://user@127.0.0.1", "http://127.0.0.1/path", "http://127.0.0.1/?token=x"]) {
    context.candidate = { url, token: "private" };
    assert.throws(() => vm.runInContext("validateEndpoint(candidate)", context));
  }
});

test("crop clips offscreen elements and rejects nonfinite coordinates", () => {
  const { context } = worker();
  const clipped = vm.runInContext("cropRect({x:-20,y:10,width:80,height:100},2,100,100)", context);
  assert.deepEqual(JSON.parse(JSON.stringify(clipped)), { x: 0, y: 20, width: 100, height: 80 });
  assert.throws(() => vm.runInContext("cropRect({x:NaN,y:0,width:1,height:1},1,100,100)", context));
  assert.throws(() => vm.runInContext("cropRect({x:101,y:0,width:1,height:1},1,100,100)", context));
});
