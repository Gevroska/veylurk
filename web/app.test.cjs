"use strict";

// Offline behavioral fixtures: no browser, server, or network connection is opened.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");

const html = fs.readFileSync(path.join(__dirname, "index.html"), "utf8");
const script = fs.readFileSync(path.join(__dirname, "app.js"), "utf8");
const received = "2026-10-23T12:00:00Z";

class Element {
  constructor(tag = "div") {
    this.tagName = tag.toUpperCase(); this.children = []; this.listeners = new Map();
    this.attributes = {}; this.dataset = {}; this.className = ""; this.value = "";
    this.hidden = false; this.disabled = false; this._text = ""; this.scrollLeft = 0;
    this.classList = {
      add: (name) => this.classList.toggle(name, true),
      remove: (name) => this.classList.toggle(name, false),
      contains: (name) => this.className.split(/\s+/).includes(name),
      toggle: (name, enabled) => {
        const names = new Set(this.className.split(/\s+/).filter(Boolean));
        const include = enabled === undefined ? !names.has(name) : enabled;
        if (include) names.add(name); else names.delete(name);
        this.className = [...names].join(" ");
      },
    };
  }
  set textContent(text) { this._text = String(text); this.children = []; }
  get textContent() { return this._text + this.children.map((child) => child.textContent).join(""); }
  set innerHTML(_) { throw new Error("HTML injection sink used"); }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) {
    if (this.ownerDocument && this.contains(this.ownerDocument.activeElement)) this.ownerDocument.activeElement = null;
    this._text = ""; this.children = children;
  }
  contains(element) { return descendants(this).includes(element); }
  querySelector(selector) { assert.equal(selector, ".table-wrap"); return descendants(this).slice(1).find((element) => element.classList.contains("table-wrap")) || null; }
  querySelectorAll(selector) { assert.equal(selector, "[data-channel-action]"); return descendants(this).slice(1).filter((element) => element.dataset.channelAction); }
  focus() { if (this.ownerDocument) this.ownerDocument.activeElement = this; }
  setAttribute(key, value) { this.attributes[key] = value; }
  removeAttribute(key) { delete this.attributes[key]; }
  addEventListener(name, callback) { this.listeners.set(name, [...(this.listeners.get(name) || []), callback]); }
  async dispatch(name) { await Promise.all((this.listeners.get(name) || []).map((callback) => callback({ preventDefault() {} }))); }
  scrollIntoView() {}
}

function descendants(element) { return [element, ...element.children.flatMap(descendants)]; }
async function settle() { for (let i = 0; i < 5; i += 1) await new Promise(setImmediate); }
function deferred() { let resolve; const promise = new Promise((done) => { resolve = done; }); return { promise, resolve }; }
function event(id, account = "sample_account") { return { id, session_id: 1, account, channel: "sample_channel", kind: "join", received_at: received, source: "twitch_irc" }; }

function createApp(override, origin = "http://127.0.0.1:3131") {
  const elements = new Map([...html.matchAll(/<([a-z]+)\b[^>]*\bid="([^"]+)"/g)].map((match) => [match[2], new Element(match[1])]));
  elements.get("search-limit").value = "50";
  const navigation = ["channels", "history", "broadcasts", "health"].map((section) => {
    const element = new Element("a"); element.dataset.section = section; return element;
  });
  const location = new URL(origin); location.hash = "#channels";
  const calls = [];
  const document = {
    hidden: false,
    activeElement: null,
    getElementById(id) { assert.ok(elements.has(id), `Unknown HTML id: ${id}`); return elements.get(id); },
    createElement(tag) { const element = new Element(tag); element.ownerDocument = document; return element; },
    querySelectorAll(selector) { assert.equal(selector, "[data-section]"); return navigation; },
    addEventListener() {},
  };
  for (const element of elements.values()) element.ownerDocument = document;
  const defaultResponse = (target) => {
    if (target.pathname === "/api/status") return { state: "collecting", started_at: received, storage_path: "local.sqlite", enabled_channels: 1, connected_channels: 1, observation_count: 2, session_count: 1, storage_bytes: 1024 };
    if (target.pathname === "/api/channels") return { channels: [{ login: "sample_channel", tracked: true, enabled: true, active_session_id: 1, last_observed_at: received }] };
    if (target.pathname === "/api/metadata/status") return { configured: false, state: "unavailable", channels: [] };
    if (target.pathname === "/api/relationships") return { relationships: [], has_more: false, next_offset: null };
    if (target.pathname.endsWith("/history")) return { sessions: [], next_before: null };
    if (target.pathname === "/api/broadcasts") return { broadcasts: [], next_before: null };
    return { observations: [], next_before: null };
  };
  const context = vm.createContext({
    document, location, Node: Element, URL, URLSearchParams, Intl, Date, Set, Number, Error, TypeError, AbortController,
    window: { addEventListener() {} },
    setInterval() { return 1; }, setTimeout() { return 1; }, clearTimeout() {},
    fetch: async (address, options) => {
      const target = new URL(address); calls.push({ target, options });
      const custom = override ? await override(target, options) : undefined;
      if (custom instanceof Error) throw custom;
      const payload = custom === undefined ? defaultResponse(target) : custom;
      const status = payload && payload.httpStatus || 200;
      return { ok: status >= 200 && status < 300, status, async json() { return payload; } };
    },
  });
  vm.runInContext(script, context, { filename: "app.js" });
  return { elements, calls, document, get: (id) => elements.get(id) };
}

test("untrusted account, channel, title, and error text stays literal", async () => {
  const hostile = '<img src=x onerror="throw 1">';
  const app = createApp((target) => {
    if (target.pathname === "/api/search") return { observations: [event(1, hostile)], next_before: null };
    if (target.pathname === "/api/broadcasts") return { broadcasts: [{ id: 1, channel: hostile, title: hostile, started_at: received, last_live_at: received }], next_before: null };
    if (target.pathname === "/api/metadata/status") return { configured: true, state: "error", last_error: hostile, channels: [] };
  });
  await settle();
  app.get("search-account").value = "account";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.match(app.get("search-results").textContent, /<img src=x/);
  await app.get("broadcasts-form").dispatch("submit"); await settle();
  assert.match(app.get("broadcasts-results").textContent, /<img src=x/);
  assert.match(app.get("metadata-message").textContent, /<img src=x/);
  for (const element of app.elements.values()) assert.equal(descendants(element).some((child) => child.tagName === "IMG"), false);
});

test("late search replies cannot replace a newer account query", async () => {
  const old = deferred();
  const app = createApp((target) => target.pathname === "/api/search" ? target.searchParams.get("account") === "old_account" ? old.promise : { observations: [event(2, "new_account")], next_before: null } : undefined);
  await settle();
  app.get("search-account").value = "old_account"; await app.get("search-form").dispatch("submit");
  app.get("search-account").value = "new_account"; await app.get("search-form").dispatch("submit"); await settle();
  old.resolve({ observations: [event(1, "old_account")], next_before: null }); await settle();
  assert.match(app.get("search-results").textContent, /new_account/);
  assert.doesNotMatch(app.get("search-results").textContent, /old_account/);
});

test("editing filters during a request keeps the eventual response visibly stale", async () => {
  const pending = deferred();
  const app = createApp((target) => target.pathname === "/api/search" ? pending.promise : undefined);
  await settle();
  app.get("search-account").value = "old_account"; await app.get("search-form").dispatch("submit");
  app.get("search-account").value = "new_account"; await app.get("search-account").dispatch("input");
  pending.resolve({ observations: [event(1, "old_account")], next_before: 1 }); await settle();
  assert.equal(app.get("search-results").classList.contains("stale"), true);
  assert.match(app.get("search-message").textContent, /Filters changed/);
  assert.equal(app.get("search-next").disabled, true);
});

test("failed pagination keeps the successful page and reports that it may be stale", async () => {
  const app = createApp((target) => target.pathname === "/api/search" ? target.searchParams.has("before") ? { httpStatus: 500, error: { message: "Storage unavailable" } } : { observations: [event(8)], next_before: 8 } : undefined);
  await settle();
  app.get("search-account").value = "sample_account"; await app.get("search-form").dispatch("submit"); await settle();
  await app.get("search-next").dispatch("click"); await settle();
  assert.match(app.get("search-results").textContent, /sample_account/);
  assert.match(app.get("search-message").textContent, /Storage unavailable.*previous page/);
  assert.equal(app.get("search-page").textContent, "Page 1");
  assert.equal(app.get("search-next").disabled, false);
});

test("bulk channels normalize and deduplicate, and removal retains history semantics", async () => {
  const app = createApp(); await settle();
  app.get("channel-logins").value = "Sample_Channel, #sample_channel\nOTHER_CHANNEL";
  await app.get("add-channels-form").dispatch("submit"); await settle();
  const post = app.calls.find((call) => call.options.method === "POST");
  assert.deepEqual(JSON.parse(post.options.body), { logins: ["sample_channel", "other_channel"], enabled: true });
  const remove = descendants(app.get("channels-results")).find((element) => element.tagName === "BUTTON" && element.textContent === "Remove tracking");
  await remove.dispatch("click"); await settle();
  const deletion = app.calls.find((call) => call.options.method === "DELETE");
  assert.equal(deletion.target.pathname, "/api/channels/sample_channel");
  assert.equal(deletion.options.headers["Content-Type"], "application/json");
  assert.match(app.get("channel-message").textContent, /stored history retained/);
  for (const call of app.calls) {
    assert.equal(call.target.origin, "http://127.0.0.1:3131");
    assert.equal(call.options.mode, "same-origin"); assert.equal(call.options.redirect, "error");
  }
});

test("a remotely hosted copy never makes API requests", async () => {
  const app = createApp(undefined, "https://example.invalid"); await settle();
  assert.equal(app.calls.length, 0);
  assert.match(app.get("app-message").textContent, /local address/);
  assert.equal(app.get("connection-status").textContent, "Status unavailable");
});

test("stale live metadata is Unknown and never displayed as a current live state", async () => {
  const app = createApp((target) => target.pathname === "/api/metadata/status" ? { configured: true, state: "ready", channels: [{ channel: "sample_channel", state: "unknown", last_reported_state: "live", checked_at: received, stale: true, viewer_count: 999 }] } : undefined);
  await settle(); app.get("search-channel").value = "sample_channel";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.equal(app.get("channel-broadcast-state").textContent, "Unknown");
  assert.match(app.get("broadcast-message").textContent, /report is stale/);
  assert.doesNotMatch(app.get("broadcast-message").textContent, /999/);
});

test("interrupted sessions keep their end unknown and label the recovery time separately", async () => {
  const endBound = "2026-10-23T09:14:00Z";
  const app = createApp((target) => target.pathname.endsWith("/history") ? { sessions: [{ id: 1, channel: "sample_channel", started_at: "2026-10-23T09:00:00Z", ended_at: endBound, end_reason: "process_interrupted", end_uncertain: true, closed_recorded_at: received, observation_count: 2 }], next_before: null } : undefined);
  await settle(); app.get("search-channel").value = "sample_channel";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.match(app.get("sessions-results").textContent, /Unknown — interrupted/);
  assert.match(app.get("sessions-results").textContent, /Recovery recorded:/);
  const times = descendants(app.get("sessions-results")).filter((element) => element.tagName === "TIME");
  assert.equal(times.some((element) => element.dateTime === new Date(endBound).toISOString()), false);
  assert.equal(times.some((element) => element.dateTime === new Date(received).toISOString()), true);
});

test("channel availability overrides an open session and health explains partial readiness", async () => {
  const warning = "The channel did not return a NAMES terminator before its deadline.";
  const app = createApp((target) => {
    if (target.pathname === "/api/channels") return { channels: [
      { login: "unavailable_room", tracked: true, enabled: true, active_session_id: 1, collection_state: "unavailable", collection_error: warning },
      { login: "healthy_room", tracked: true, enabled: true, active_session_id: 1, collection_state: "collecting", collection_error: null },
      { login: "joining_room", tracked: true, enabled: true, active_session_id: 1, collection_state: "joining", collection_error: null },
    ] };
    if (target.pathname === "/api/status") return { state: "collecting", enabled_channels: 3, connected_channels: 1, last_error: "One or more channels have unavailable membership; healthy rooms continue collecting." };
  });
  await settle();
  const channelNodes = descendants(app.get("channels-results"));
  const unavailable = channelNodes.find((element) => element.tagName === "SPAN" && element.textContent === "Membership unavailable");
  assert.ok(unavailable);
  assert.equal(unavailable.classList.contains("bad"), true);
  assert.equal(unavailable.classList.contains("good"), false);
  assert.match(app.get("channels-results").textContent, /Collecting chat/);
  assert.match(app.get("channels-results").textContent, /Joining chat/);
  assert.ok(app.get("channels-results").textContent.includes(warning));
  assert.doesNotMatch(app.get("channels-results").textContent, /Session open/);
  assert.match(app.get("health-results").textContent, /some channels are joining or unavailable/);
  assert.match(app.get("health-results").textContent, /healthy rooms continue collecting/);
});

test("channel refresh preserves action focus and horizontal position without rebuilding unchanged content", async () => {
  let revision = 0;
  const app = createApp((target) => target.pathname === "/api/channels" ? { channels: [{ login: "sample_channel", tracked: true, enabled: true, active_session_id: 1, last_observed_at: new Date(Date.parse(received) + revision * 1000).toISOString() }] } : undefined);
  await settle();
  const container = app.get("channels-results");
  const original = container.children[0];
  const action = descendants(container).find((element) => element.tagName === "BUTTON" && element.textContent === "Pause");
  original.scrollLeft = 120; action.focus();
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.equal(container.children[0], original);
  assert.equal(app.document.activeElement, action);
  revision += 1;
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.notEqual(container.children[0], original);
  assert.equal(app.document.activeElement.dataset.channelAction, action.dataset.channelAction);
  assert.equal(app.document.activeElement.textContent, "Pause");
  assert.equal(container.children[0].scrollLeft, 120);
});

test("an in-flight channel action keeps keyboard focus and rejects duplicate activation", async () => {
  const pending = deferred();
  const app = createApp((target, options) => options.method === "PATCH" ? pending.promise : undefined);
  await settle();
  const pause = descendants(app.get("channels-results")).find((element) => element.tagName === "BUTTON" && element.textContent === "Pause");
  pause.focus();
  const request = pause.dispatch("click"); await settle();
  assert.equal(app.document.activeElement.textContent, "Pause");
  assert.equal(app.document.activeElement.attributes["aria-disabled"], "true");
  assert.equal(app.document.activeElement.disabled, false);
  await app.document.activeElement.dispatch("click");
  assert.equal(app.calls.filter((call) => call.options.method === "PATCH").length, 1);
  pending.resolve({ channel: { login: "sample_channel", enabled: false } });
  await request; await settle();
  assert.equal(app.document.activeElement.dataset.channelAction, pause.dataset.channelAction);
});
