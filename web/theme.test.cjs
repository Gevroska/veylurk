"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");
const source = fs.readFileSync(path.join(__dirname, "theme.js"), "utf8");

function boot({ saved = null, readThrows = false, writeThrows = false, loading = false, systemDark = false } = {}) {
  const attributes = {};
  const storageData = new Map(saved === null ? [] : [["veylurk.theme", saved]]);
  const documentListeners = new Map();
  const windowListeners = new Map();
  const selectListeners = new Map();
  let systemListener;
  const preference = { matches: systemDark, addEventListener: (_, callback) => { systemListener = callback; } };
  const selector = { value: "auto", addEventListener: (name, callback) => selectListeners.set(name, callback) };
  const message = { hidden: true, textContent: "" };
  const storage = {
    getItem(key) { if (readThrows) throw new Error("Read blocked"); return storageData.get(key) ?? null; },
    setItem(key, value) { if (writeThrows) throw new Error("Write blocked"); storageData.set(key, value); },
  };
  const context = vm.createContext({
    document: {
      readyState: loading ? "loading" : "complete",
      documentElement: { setAttribute: (key, value) => { attributes[key] = value; } },
      getElementById: (id) => id === "theme-select" ? selector : message,
      addEventListener: (name, callback) => documentListeners.set(name, callback),
    },
    window: { localStorage: storage, matchMedia: () => preference, addEventListener: (name, callback) => windowListeners.set(name, callback) },
  });
  vm.runInContext(source, context);
  return {
    attributes, selector, message, storageData,
    ready() { documentListeners.get("DOMContentLoaded")?.(); },
    choose(value) { selector.value = value; selectListeners.get("change")(); },
    external(value, key = "veylurk.theme") { windowListeners.get("storage")({ key, newValue: value, storageArea: storage }); },
    system(dark) { preference.matches = dark; systemListener(); },
  };
}

test("Auto is the default and stored explicit themes apply before the page body is ready", () => {
  assert.equal(boot().attributes["data-theme"], "auto");
  for (const theme of ["light", "dark"]) {
    const app = boot({ saved: theme, loading: true });
    assert.equal(app.attributes["data-theme"], theme);
    app.ready();
    assert.equal(app.selector.value, theme);
  }
});

test("theme choices persist, including returning to Auto", () => {
  const app = boot();
  for (const theme of ["dark", "light", "auto"]) {
    app.choose(theme);
    assert.equal(app.attributes["data-theme"], theme);
    assert.equal(app.storageData.get("veylurk.theme"), theme);
    assert.equal(boot({ saved: app.storageData.get("veylurk.theme") }).attributes["data-theme"], theme);
  }
});

test("Auto follows live system changes while explicit Light and Dark override them", () => {
  const app = boot({ systemDark: true });
  assert.equal(app.attributes["data-color-scheme"], "dark");
  app.system(false);
  assert.equal(app.attributes["data-color-scheme"], "light");
  app.choose("dark"); app.system(false);
  assert.equal(app.attributes["data-color-scheme"], "dark");
  app.choose("light"); app.system(true);
  assert.equal(app.attributes["data-color-scheme"], "light");
  app.choose("auto");
  assert.equal(app.attributes["data-color-scheme"], "dark");
});

test("corrupt or unavailable storage falls back safely and a theme still works for this tab", () => {
  for (const value of ["sepia", '{"theme":"dark"}', "<script>", "DARK", ""]) assert.equal(boot({ saved: value }).attributes["data-theme"], "auto");
  const app = boot({ readThrows: true, writeThrows: true });
  assert.equal(app.attributes["data-theme"], "auto");
  app.choose("dark");
  assert.equal(app.attributes["data-theme"], "dark");
  assert.equal(app.message.hidden, false);
  assert.match(app.message.textContent, /Browser storage is unavailable/);
});

test("another tab's preference synchronizes without accepting unknown modes or unrelated storage keys", () => {
  const app = boot({ saved: "light" });
  app.external("dark");
  assert.equal(app.selector.value, "dark");
  app.external("light", "unrelated.key");
  assert.equal(app.selector.value, "dark");
  app.external("corrupt");
  assert.equal(app.attributes["data-theme"], "auto");
  app.external(null, null);
  assert.equal(app.selector.value, "auto");
});
