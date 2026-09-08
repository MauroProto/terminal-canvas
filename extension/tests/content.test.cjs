const { test } = require("node:test");
const assert = require("node:assert/strict");
const vm = require("node:vm");
const fs = require("node:fs");
const path = require("node:path");

function text(value) { return { nodeType: 3, textContent: value }; }
function element(tagName, childNodes = [], attributes = []) {
  return {
    nodeType: 1, tagName, childNodes, attributes, isContentEditable: false,
    style: {}, id: "", className: "", parentElement: null, isConnected: true,
    setAttribute(name, value) { this.attributes.push({ name, value }); },
    appendChild(node) { this.childNodes.push(node); },
    remove() { this.isConnected = false; },
    getBoundingClientRect() { return { x: 0, y: 0, width: 100, height: 100 }; },
    get outerHTML() {
      const attributes = this.attributes.map(a => ` ${a.name}="${a.value}"`).join("");
      return `<${tagName.toLowerCase()}${attributes}>${this.childNodes.map(n => n.outerHTML ?? n.textContent).join("")}</${tagName.toLowerCase()}>`;
    },
  };
}

function contentScript() {
  const listeners = {};
  const messages = [];
  let runtimeListener;
  const inert = { createElement: name => element(name.toUpperCase()), createTextNode: text };
  const context = vm.createContext({
    clearTimeout() {}, setTimeout() {}, CSS: { escape: value => value },
    document: {
      ...inert, documentElement: element("HTML"),
      implementation: { createHTMLDocument: () => inert },
      addEventListener(name, listener) { listeners[name] = listener; },
    },
    window: {
      getComputedStyle: () => ({ getPropertyValue: () => "" }),
      scrollX: 0, scrollY: 0, devicePixelRatio: 1,
    },
    chrome: { runtime: {
      onMessage: { addListener(listener) { runtimeListener = listener; } },
      async sendMessage(message) { messages.push(message); return { ok: true }; },
    } },
  });
  // Expose the serializer only inside this isolated VM; production stays closed.
  const source = fs.readFileSync(path.join(__dirname, "../content.js"), "utf8")
    .replace(/\}\)\(\);\s*$/, "globalThis.captureHtmlForTest = captureHtml;})();");
  vm.runInContext(source, context);
  return {
    messages,
    html: context.captureHtmlForTest,
    activate: () => runtimeListener({ kind: "tc-design-activate" }),
    click: (target, isTrusted) => listeners.click({
      target, isTrusted, preventDefault() {}, stopImmediatePropagation() {},
    }),
  };
}

test("a page-generated click cannot consume the user's armed selection", async () => {
  const script = contentScript();
  script.activate();
  await script.click(element("DIV", [text("synthetic-private-element")]), false);
  assert.equal(script.messages.length, 0);
  await script.click(element("BUTTON", [text("chosen-by-user")]), true);
  assert.equal(script.messages.length, 1);
  assert.match(script.messages[0].html, /chosen-by-user/);
  assert.doesNotMatch(script.messages[0].html, /synthetic-private-element/);
});

test("HTML and SVG executable elements are excluded regardless of tag casing", () => {
  const script = contentScript();
  const root = element("DIV", [text("public-label")]);
  for (const name of ["SCRIPT", "script", "STYLE", "style", "IFRAME", "iframe", "object", "embed"]) {
    root.childNodes.push(element(name, [text("synthetic-private-code")]));
  }
  const html = script.html(root);
  assert.match(html, /public-label/);
  assert.doesNotMatch(html, /synthetic-private-code|script|style|iframe|object|embed/i);
});

test("form values, editable text and private attributes stay out of serialized HTML", () => {
  const script = contentScript();
  const editable = element("DIV", [text("private-editable")]);
  editable.isContentEditable = true;
  const html = script.html(element("FORM", [
    element("LABEL", [text("public-field-label")]),
    element("INPUT", [], [{ name: "value", value: "private-input" }]),
    element("TEXTAREA", [text("private-textarea")]),
    element("SELECT", [element("OPTION", [text("private-selection")])]),
    editable,
    element("DIV", [], [
      { name: "data-token", value: "private-token" },
      { name: "onclick", value: "private-handler" },
      { name: "class", value: "public-style" },
    ]),
  ]));
  assert.match(html, /public-field-label|public-style/);
  assert.doesNotMatch(html, /private-/);
});

test("deep and wide subtrees stop before exceeding capture limits", () => {
  const script = contentScript();
  let deep = element("DIV", [text("too-deep-to-copy")]);
  for (let depth = 0; depth < 50; depth++) deep = element("DIV", [deep]);
  assert.doesNotMatch(script.html(deep), /too-deep-to-copy/);
  const wide = script.html(element("DIV", Array.from({ length: 600 }, (_, index) =>
    element("SPAN", [text(`item-${index}`)]))));
  assert.match(wide, /item-0/);
  assert.doesNotMatch(wide, /item-599/);
  assert.ok(wide.length <= 32768);
});
