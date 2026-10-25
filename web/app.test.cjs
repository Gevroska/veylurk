"use strict";

// Offline behavioral fixtures: no browser, server, or network connection is opened.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");

const html = fs.readFileSync(path.join(__dirname, "index.html"), "utf8");
const script = fs.readFileSync(path.join(__dirname, "app.js"), "utf8");
const stylesheet = fs.readFileSync(path.join(__dirname, "style.css"), "utf8");
const received = "2026-10-23T12:00:00Z";

class Element {
  constructor(tag = "div") {
    this.tagName = tag.toUpperCase(); this.children = []; this.listeners = new Map();
    this.attributes = {}; this.dataset = {}; this.className = ""; this.value = "";
    this.hidden = false; this.disabled = false; this._text = ""; this.scrollLeft = 0;
    this.capturedPointers = new Set(); this.screenMatrix = { a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 };
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
  focus() {
    if (!this.ownerDocument || this.ownerDocument.activeElement === this) return;
    const previous = this.ownerDocument.activeElement;
    this.ownerDocument.activeElement = this;
    if (previous) void previous.dispatch("blur");
    void this.dispatch("focus");
  }
  blur() { if (this.ownerDocument?.activeElement === this) this.ownerDocument.activeElement = null; void this.dispatch("blur"); }
  setAttribute(key, value) { this.attributes[key] = String(value); if (key === "class") this.className = String(value); }
  removeAttribute(key) { delete this.attributes[key]; }
  addEventListener(name, callback) { this.listeners.set(name, [...(this.listeners.get(name) || []), callback]); }
  setPointerCapture(pointerId) { this.capturedPointers.add(pointerId); }
  hasPointerCapture(pointerId) { return this.capturedPointers.has(pointerId); }
  releasePointerCapture(pointerId) { this.capturedPointers.delete(pointerId); }
  getScreenCTM() {
    const { a, b, c, d, e, f } = this.screenMatrix;
    return { inverse() { const determinant = a * d - b * c; return { a: d / determinant, b: -b / determinant, c: -c / determinant, d: a / determinant, e: (c * f - d * e) / determinant, f: (b * e - a * f) / determinant }; } };
  }
  createSVGPoint() { return { x: 0, y: 0, matrixTransform(matrix) { return { x: this.x * matrix.a + this.y * matrix.c + matrix.e, y: this.x * matrix.b + this.y * matrix.d + matrix.f }; } }; }
  async dispatch(name, details = {}) {
    const event = { type: name, target: this, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; }, ...details };
    await Promise.all((this.listeners.get(name) || []).map((callback) => callback(event)));
    return event;
  }
  scrollIntoView() {}
}

function descendants(element) { return [element, ...element.children.flatMap(descendants)]; }
async function settle() { for (let i = 0; i < 5; i += 1) await new Promise(setImmediate); }
function deferred() { let resolve; const promise = new Promise((done) => { resolve = done; }); return { promise, resolve }; }
function event(id, account = "sample_account") { return { id, session_id: 1, account, channel: "sample_channel", kind: "join", received_at: received, source: "twitch_irc" }; }

function createApp(override, origin = "http://127.0.0.1:3131") {
  const elements = new Map([...html.matchAll(/<([a-z][a-z0-9]*)\b[^>]*\bid="([^"]+)"[^>]*>/g)].map((match) => {
    const element = new Element(match[1]); element.id = match[2];
    for (const attribute of match[0].matchAll(/\s([a-z][a-z0-9-]*)(?:="([^"]*)")?/g)) {
      element.setAttribute(attribute[1], attribute[2] ?? "");
      if (attribute[1] === "hidden" || attribute[1] === "disabled") element[attribute[1]] = true;
    }
    return [match[2], element];
  }));
  elements.get("search-limit").value = "50";
  const navigation = ["channels", "history", "broadcasts", "health"].map((section) => {
    const element = new Element("a"); element.dataset.section = section; return element;
  });
  const location = new URL(origin); location.hash = "#channels";
  const calls = [];
  const documentListeners = new Map();
  const windowListeners = new Map();
  const timers = new Map();
  let now = 0;
  let nextTimer = 0;
  const document = {
    hidden: false,
    activeElement: null,
    getElementById(id) { assert.ok(elements.has(id), `Unknown HTML id: ${id}`); return elements.get(id); },
    createElement(tag) { const element = new Element(tag); element.ownerDocument = document; return element; },
    createElementNS(_namespace, tag) { return this.createElement(tag); },
    querySelectorAll(selector) { assert.equal(selector, "[data-section]"); return navigation; },
    addEventListener(name, callback) { documentListeners.set(name, [...(documentListeners.get(name) || []), callback]); },
  };
  for (const element of elements.values()) element.ownerDocument = document;
  const defaultResponse = (target) => {
    if (target.pathname === "/api/status") return { state: "collecting", started_at: received, storage_path: "local.sqlite", enabled_channels: 1, connected_channels: 1, observation_count: 2, session_count: 1, storage_bytes: 1024 };
    if (target.pathname === "/api/channels") return { channels: [{ login: "sample_channel", tracked: true, enabled: true, active_session_id: 1, last_observed_at: received }] };
    if (target.pathname === "/api/metadata/status") return { configured: false, state: "unavailable", channels: [] };
    if (target.pathname === "/api/settings") return { collection_mode: "all", unknown_policy: "collect" };
    if (target.pathname === "/api/suggestions") return { kind: target.searchParams.get("kind"), suggestions: [] };
    if (target.pathname === "/api/timeline") return timeline();
    if (target.pathname === "/api/history") return { visits: [], has_more: false, next_offset: null };
    if (target.pathname === "/api/presence" || target.pathname.endsWith("/accounts")) return { accounts: [], has_more: false, next_offset: null };
    if (target.pathname.endsWith("/metadata/history")) return { observations: [], has_more: false, next_offset: null };
    if (target.pathname === "/api/relationships") return { relationships: [], has_more: false, next_offset: null };
    if (target.pathname.endsWith("/history")) return { sessions: [], next_before: null };
    if (target.pathname === "/api/broadcasts") return { broadcasts: [], next_before: null };
    return { observations: [], next_before: null };
  };
  const context = vm.createContext({
    document, location, Node: Element, URL, URLSearchParams, Intl, Date, Set, Number, Error, TypeError, AbortController,
    window: { addEventListener(name, callback) { windowListeners.set(name, [...(windowListeners.get(name) || []), callback]); } },
    setInterval() { return 1; },
    setTimeout(callback, delay) { const id = ++nextTimer; timers.set(id, { callback, at: now + delay }); return id; },
    clearTimeout(id) { timers.delete(id); },
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
  return {
    elements, calls, document, get: (id) => elements.get(id),
    navigate(section) { location.hash = "#" + section; for (const callback of windowListeners.get("hashchange") || []) callback(); },
    visibility(hidden) { document.hidden = hidden; for (const callback of documentListeners.get("visibilitychange") || []) callback(); },
    async advance(milliseconds) {
      const until = now + milliseconds;
      while (true) {
        const next = [...timers].filter(([, timer]) => timer.at <= until).sort((a, b) => a[1].at - b[1].at)[0];
        if (!next) break;
        now = next[1].at; timers.delete(next[0]); void next[1].callback(); await settle();
      }
      now = until; await settle();
    },
  };
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

test("local time ranges become UTC and invalid ranges never dispatch history requests", async () => {
  const app = createApp(); await settle();
  app.get("search-account").value = "sample_account";
  app.get("search-from").value = "2026-10-23T12:00:00";
  app.get("search-to").value = "2026-10-23T13:00:00";
  await app.get("search-form").dispatch("submit"); await settle();
  for (const pathname of ["/api/search", "/api/relationships", "/api/history"]) {
    const call = app.calls.find((item) => item.target.pathname === pathname);
    assert.equal(call.target.searchParams.get("from"), new Date("2026-10-23T12:00:00").toISOString());
    assert.equal(call.target.searchParams.get("to"), new Date("2026-10-23T13:00:00").toISOString());
  }
  const initialCount = app.calls.length;
  app.get("search-to").value = "2026-10-23T11:59:00";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.equal(app.calls.length, initialCount);
  assert.match(app.get("search-message").textContent, /end time must be later/);
  app.get("search-to").value = "2026-02-30T12:00:00";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.equal(app.calls.length, initialCount);
  assert.match(app.get("search-message").textContent, /does not exist/);
});

test("separate visits retain positive receipt bounds, gap reasons, and uncertain timelines", async () => {
  const visit = (id, end_basis) => ({ id, account: "sample_account", channel: "sample_channel", session_id: id, first_observed_at: received, last_observed_at: received, start_basis: "names", end_basis, positive_observation_count: 2, uncertain: true });
  const app = createApp((target) => target.pathname === "/api/history" ? { visits: [visit(1, "coverage_gap"), visit(2, "open")], has_more: false } : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit"); await settle();
  const result = app.get("visits-results");
  assert.equal(descendants(result).filter((item) => item.tagName === "SVG").length, 2);
  assert.match(result.textContent, /2 separate supported visits/);
  assert.match(result.textContent, /Coverage gap/);
  assert.match(result.textContent, /First positive receipt/);
  assert.match(result.textContent, /Last positive receipt/);
  assert.match(result.textContent, /not measured attendance/);
  assert.equal(descendants(result).filter((item) => item.tagName === "LINE").every((item) => item.attributes.class === "evidence-span"), true);
});

test("point-in-time evidence separates observed, inferred, and unknown without asserting absence", async () => {
  let emptyResponse = false;
  const app = createApp((target) => target.pathname === "/api/presence" ? { accounts: emptyResponse ? [] : [
    { account: "observed_account", state: "observed", observed_at: received },
    { account: "inferred_account", state: "inferred", observed_at: received, explanation: "Earlier receipt within an unclosed visit; uncertain." },
    { account: "unknown_account", state: "unexpected" },
  ], has_more: false } : undefined);
  await settle(); app.get("search-channel").value = "sample_channel";
  await app.get("search-form").dispatch("submit"); await settle();
  app.get("presence-at").value = "2026-10-23T12:00:00";
  await app.get("presence-form").dispatch("submit"); await settle();
  const call = app.calls.find((item) => item.target.pathname === "/api/presence");
  assert.equal(call.target.searchParams.get("at"), new Date("2026-10-23T12:00:00").toISOString());
  assert.match(app.get("presence-results").textContent, /Observed/);
  assert.match(app.get("presence-results").textContent, /Inferred/);
  assert.match(app.get("presence-results").textContent, /Unknown/);
  emptyResponse = true;
  await app.get("presence-form").dispatch("submit"); await settle();
  assert.match(app.get("presence-results").textContent, /does not mean the channel was empty/);
});

test("ambiguous delayed membership observations are not labeled as supported visits", async () => {
  const app = createApp((target) => target.pathname === "/api/history" ? { visits: [{ id: 4, account: "sample_account", channel: "sample_channel", session_id: 1, first_observed_at: received, last_observed_at: received, start_basis: "uncertain_names", end_basis: "open", positive_observation_count: 1, supported_visit: false, uncertain: true }], next_before: null } : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.match(app.get("visits-results").textContent, /0 separate supported visits/);
  assert.match(app.get("visits-results").textContent, /1 ambiguous observation segments/);
  assert.match(app.get("visits-results").textContent, /Observation only — ambiguous/);
  assert.match(app.get("visits-results").textContent, /no supported visit or continuity inference/);
  assert.equal(descendants(app.get("visits-results")).some((item) => item.classList.contains("evidence-span")), false);
});

test("metadata history keeps text literal and viewer samples unconnected with a table alternative", async () => {
  const hostile = '<svg onload="throw 1">';
  const app = createApp((target) => target.pathname.endsWith("/metadata/history") ? { observations: [
    { checked_at: received, state: "live", title: hostile, game_name: "Category A", viewer_count: 40 },
    { checked_at: "2026-10-23T13:00:00Z", state: "offline", title: null, viewer_count: null },
    { checked_at: "2026-10-23T14:00:00Z", state: "live", title: "Changed title", game_name: "Category B", viewer_count: 20 },
  ], has_more: false } : undefined);
  await settle(); app.get("search-channel").value = "sample_channel";
  await app.get("search-form").dispatch("submit"); await settle();
  const result = app.get("metadata-history-results");
  assert.match(result.textContent, /<svg onload/);
  assert.match(result.textContent, /Changed title/);
  assert.match(result.textContent, /Category B/);
  assert.match(result.textContent, /not connected across gaps/);
  assert.equal(descendants(result).filter((item) => item.tagName === "CIRCLE").length, 2);
  assert.equal(descendants(result).some((item) => ["POLYLINE", "PATH"].includes(item.tagName)), false);
  assert.ok(descendants(result).some((item) => item.tagName === "TABLE"));
});

test("broadcast detail loads account summaries and separate visits within selected UTC bounds", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/broadcasts") return { broadcasts: [{ id: 9, channel: "sample_channel", title: "A title", started_at: received, last_live_at: received }], next_before: null };
    if (target.pathname === "/api/broadcasts/9/accounts") return { accounts: [{ account: "sample_account", first_observed_at: received, last_observed_at: received, visit_count: 3, positive_observation_count: 8 }, { account: "inferred_account", state: "inferred", association_basis: "prior_connected_inference", first_observed_at: null, last_observed_at: null, prior_observed_at: "2026-10-23T09:00:00Z", positive_observation_count: 0, visit_count: 1 }], summary: { account_count: 2, observed_account_count: 1, inferred_only_account_count: 1, visit_count: 4, positive_observation_count: 8 }, has_more: false };
  });
  await settle(); app.get("broadcasts-from").value = "2026-10-23T10:00:00";
  app.get("broadcasts-to").value = "2026-10-23T18:00:00";
  await app.get("broadcasts-form").dispatch("submit"); await settle();
  const open = descendants(app.get("broadcasts-results")).find((item) => item.tagName === "BUTTON" && item.textContent === "View summary / visits / events");
  await open.dispatch("click"); await settle();
  assert.match(app.get("broadcast-accounts-results").textContent, /sample_account/);
  assert.match(app.get("broadcast-accounts-results").textContent, /Supported visits/);
  assert.match(app.get("broadcast-accounts-results").textContent, /Inferred — uncertain/);
  assert.match(app.get("broadcast-accounts-results").textContent, /No positive receipt in window/);
  assert.match(app.get("broadcast-accounts-results").textContent, /Supporting earlier receipt/);
  assert.match(app.get("broadcast-accounts-results").textContent, /Inferred-only accounts/);
  const history = app.calls.find((item) => item.target.pathname === "/api/history");
  assert.equal(history.target.searchParams.get("broadcast_id"), "9");
  for (const call of app.calls.filter((item) => item.target.pathname.startsWith("/api/broadcasts/9/") || item.target.pathname === "/api/history")) {
    assert.equal(call.target.searchParams.get("from"), new Date("2026-10-23T10:00:00").toISOString());
    assert.equal(call.target.searchParams.get("to"), new Date("2026-10-23T18:00:00").toISOString());
  }
});

test("live-only policy preserves an unsaved choice and labels fresh offline versus unknown metadata", async () => {
  const app = createApp((target, options) => {
    if (target.pathname === "/api/settings") return { collection_mode: options.method === "PATCH" ? JSON.parse(options.body).collection_mode : "live_only" };
    if (target.pathname === "/api/channels") return { channels: [{ login: "offline_room", enabled: true }, { login: "unknown_room", enabled: true }] };
    if (target.pathname === "/api/metadata/status") return { configured: true, state: "ready", channels: [{ channel: "offline_room", state: "offline", stale: false, checked_at: received }, { channel: "unknown_room", state: "unknown", stale: true, checked_at: received }] };
  });
  await settle();
  assert.match(app.get("channels-results").textContent, /Paused while offline/);
  assert.match(app.get("channels-results").textContent, /Unknown: chat observation continues/);
  assert.match(app.get("channels-results").textContent, /completeness|coverage unknown/);
  app.get("collection-mode").value = "all";
  await app.get("collection-mode").dispatch("change");
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.equal(app.get("collection-mode").value, "all");
  await app.get("settings-form").dispatch("submit"); await settle();
  const patch = app.calls.find((item) => item.target.pathname === "/api/settings" && item.options.method === "PATCH");
  assert.deepEqual(JSON.parse(patch.options.body), { collection_mode: "all" });
  assert.match(app.get("settings-message").textContent, /saved/);
});

test("disk warning remains visible and shutdown needs explicit confirmation with honest acknowledgement", async () => {
  let unavailable = false;
  const app = createApp((target) => {
    if (target.pathname === "/api/status") return unavailable ? new TypeError("unreachable") : { state: "collecting", disk_available_bytes: 100000, storage_warning: "Low disk space: collection may stop." };
    if (target.pathname === "/api/shutdown") return { state: "stopping", instance_id: "test-instance" };
  });
  await settle();
  assert.equal(app.get("storage-warning").hidden, false);
  assert.match(app.get("storage-warning").textContent, /Low disk space/);
  assert.match(app.get("health-results").textContent, /Available disk space/);
  await app.get("stop-app").dispatch("click");
  assert.equal(app.get("stop-confirmation").hidden, false);
  assert.equal(app.document.activeElement, app.get("confirm-stop"));
  assert.equal(app.calls.some((item) => item.target.pathname === "/api/shutdown"), false);
  await app.get("cancel-stop").dispatch("click");
  assert.equal(app.document.activeElement, app.get("stop-app"));
  await app.get("stop-app").dispatch("click"); await app.get("confirm-stop").dispatch("click"); await settle();
  const shutdown = app.calls.filter((item) => item.target.pathname === "/api/shutdown");
  assert.equal(shutdown.length, 1);
  assert.deepEqual(JSON.parse(shutdown[0].options.body), {});
  assert.equal(app.get("connection-status").textContent, "Stopping");
  assert.match(app.get("stopped-message").textContent, /accepted the stop request/);
  assert.doesNotMatch(app.get("stopped-message").textContent, /has stopped/);
  unavailable = true;
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.match(app.get("connection-status").textContent, /unreachable after stop/);
  unavailable = false;
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.equal(app.get("connection-status").textContent, "Collecting");
  assert.equal(app.get("stopped-message").hidden, true);
  assert.equal(app.get("stop-app").disabled, false);
});

test("fault details preserve the first cause, distinguish callback interruption, and render text literally", async () => {
  const hostile = '<img src=x onerror="injected()">';
  const app = createApp((target) => {
    if (target.pathname === "/api/status") return {
      state: "storage_error", storage_health: "unavailable",
      diagnostics: {
        first_failure: { sequence: 1, occurred_at_utc: received, operation: "query", job_class: "query", category: hostile, elapsed_ms: 5010, sqlite_primary_code: 9, sqlite_extended_code: 9, progress_interrupt_requested: true, autocommit_after: true },
        latest: { sequence: 2, occurred_at_utc: received, operation: "checkpoint", job_class: "durable_mutation", category: "checkpoint_busy", elapsed_ms: 2, sqlite_primary_code: null, sqlite_extended_code: null, progress_interrupt_requested: false, autocommit_after: null },
        persistence: { state: "failed", failure_count: 1, dropped_count: 2 },
      },
    };
  });
  await settle();
  const health = app.get("health-results");
  assert.match(health.textContent, /First storage failure/);
  assert.match(health.textContent, /Latest recorded fault/);
  assert.ok(health.textContent.indexOf("Query") < health.textContent.indexOf("Checkpoint"));
  assert.match(health.textContent, /Execution policyQuery/);
  assert.match(health.textContent, /Execution policyDurable mutation/);
  assert.match(health.textContent, /Query budget interruption requestedYes/);
  assert.match(health.textContent, /Query budget interruption requestedNo/);
  assert.match(health.textContent, /transaction active after operationNo/);
  assert.match(health.textContent, /transaction active after operationUnknown/);
  assert.ok(health.textContent.includes(hostile));
  assert.equal(descendants(health).some((element) => element.tagName === "IMG"), false);
  assert.match(health.textContent, /Diagnostic records not queued2/);
  assert.doesNotMatch(health.textContent, /successfully recovered|rolled back/);
});

test("health refresh keeps fault details expanded and preserves keyboard focus", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/status") return {
      state: "collecting",
      diagnostics: { first_failure: null, latest: null, persistence: { state: "not_attempted", failure_count: 0, dropped_count: 0 } },
    };
  });
  await settle();
  const original = descendants(app.get("health-results")).find((element) => element.tagName === "DETAILS");
  original.open = true;
  original.children[0].focus();
  await app.get("refresh-status").dispatch("click"); await settle();
  const refreshed = descendants(app.get("health-results")).find((element) => element.tagName === "DETAILS");
  assert.equal(refreshed.open, true);
  assert.equal(app.document.activeElement, refreshed.children[0]);
  assert.match(refreshed.textContent, /No fault recorded in this application run/);
});

test("per-room health overrides an open session and never equates recent receipts with completeness", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/status") return { state: "membership_unavailable", rooms: [{ channel: "sample_channel", state: "unavailable", last_positive_at: received, last_error: "Room response deadline elapsed", coverage: "unknown" }] };
  });
  await settle();
  assert.match(app.get("channels-results").textContent, /Membership unavailable/);
  assert.match(app.get("channels-results").textContent, /Room response deadline elapsed/);
  assert.match(app.get("channels-results").textContent, /Latest positive receipt/);
  assert.match(app.get("channels-results").textContent, /completeness unknown/);
  assert.doesNotMatch(app.get("channels-results").textContent, /Session open/);
});

const timelineFrom = "2026-10-23T10:00:00.000Z";
const timelineTo = "2026-10-23T14:00:00.000Z";
function timeline(intervals = [], extra = {}) {
  return { account: "sample_account", channel: null, broadcast: null, effective_range: { from: timelineFrom, to: timelineTo }, as_of: timelineTo, empty: false, intervals, has_more: false, next_before: null, confidence: null, confidence_status: "not_calibrated", ...extra };
}
function timelineVisit(extra = {}) {
  return {
    kind: "visit", id: 10, account: "sample_account", channel: "sample_channel", session_id: 1,
    first_observed_at: "2026-10-23T11:00:00Z", last_observed_at: received,
    supported_visit: true, out_of_order: false, start_basis: "join", end_basis: "part", boundary_at: "2026-10-23T13:00:00Z", boundary_basis: "part",
    inference_from: "2026-10-23T11:00:00Z", inference_to: "2026-10-23T13:00:00Z", inference_end_basis: "part",
    markers: [{ observation_id: 10, at: "2026-10-23T11:00:00Z", kind: "first_positive", basis: "join" }, { observation_id: 11, at: received, kind: "last_positive", basis: "positive_receipt" }, { observation_id: 12, at: "2026-10-23T13:00:00Z", kind: "part", basis: "part_receipt" }], ...extra,
  };
}

function timelineElement(app, className, prefix = "timeline") { return descendants(app.get(prefix + "-results")).find((item) => item.classList.contains(className)); }
function timelineChart(app, prefix = "timeline") { return descendants(app.get(prefix + "-results")).find((item) => item.tagName === "SVG"); }
function timelineButton(app, text, prefix = "timeline") { return descendants(app.get(prefix + "-results")).find((item) => item.tagName === "BUTTON" && item.textContent === text); }
function mouseAt(x, y = 78, extra = {}) { return { clientX: x, clientY: y, pointerId: 1, pointerType: "mouse", button: 0, isPrimary: true, ...extra }; }
async function searchTimeline(app) { await settle(); app.get("search-account").value = "sample_account"; await app.get("search-form").dispatch("submit"); await settle(); }
function metadataAt(extra = {}) {
  return { channel: "sample_channel", at: received, basis: "last_reported", reason: null,
    check: { id: 5, checked_at: "2026-10-23T11:55:00Z", state: "live", broadcast_id: 9 },
    next_check: { id: 6, checked_at: "2026-10-23T12:05:00Z", state: "live", broadcast_id: 9 },
    broadcast: { id: 9, channel: "sample_channel", started_at: timelineFrom, first_seen_at: timelineFrom, last_live_at: timelineTo, offline_observed_at: null },
    title: { title: "Stored broadcast title", seen_at: "2026-10-23T11:50:00Z", game_name: "Stored category", language: "en" }, ...extra };
}

test("timeline receipt hover debounces exact positive times and keeps historical titles and metadata times distinct", async () => {
  const hostile = '<img src=x onerror="injected()">';
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([timelineVisit()]);
    if (target.pathname.endsWith("/metadata/at")) return metadataAt({ title: { title: hostile, seen_at: "2026-10-23T11:50:00Z" } });
  });
  await searchTimeline(app); app.navigate("history");
  const point = descendants(timelineChart(app)).filter((item) => item.tagName === "CIRCLE").at(-1);
  await point.dispatch("pointerenter", mouseAt(580));
  await point.dispatch("pointermove", mouseAt(581)); await app.advance(179);
  assert.equal(app.calls.some((item) => item.target.pathname.endsWith("/metadata/at")), false);
  await app.advance(1);
  const request = app.calls.find((item) => item.target.pathname.endsWith("/metadata/at"));
  assert.equal(request.target.pathname, "/api/channels/sample_channel/metadata/at");
  assert.equal(request.target.searchParams.get("at"), new Date(received).toISOString());
  assert.equal(request.target.searchParams.has("broadcast_id"), false);
  const hover = timelineElement(app, "timeline-hover");
  assert.match(hover.textContent, /selected time:.*2026-10-23T12:00:00.000Z/);
  assert.match(hover.textContent, /Last reported title: <img src=x/);
  assert.match(hover.textContent, /Metadata checked:.*2026-10-23T11:55:00.000Z/);
  assert.match(hover.textContent, /Title first recorded:.*2026-10-23T11:50:00.000Z/);
  assert.match(hover.textContent, /does not establish continuity or video viewing/);
  assert.equal(descendants(app.get("timeline-results")).some((item) => item.tagName === "IMG"), false);
  assert.equal(hover.attributes.role, "status");
  assert.equal(point.attributes.tabindex, "0"); assert.equal(point.attributes["aria-describedby"], "timeline-hover");
});

test("timeline metadata reserves a bounded scrollable panel and keeps completed long results readable without replacing the chart", async () => {
  const hoverRule = stylesheet.match(/\.timeline-hover\s*\{([^}]+)\}/)[1];
  const styles = new Map(hoverRule.split(";").filter((declaration) => declaration.includes(":")).map((declaration) => declaration.split(":").map((part) => part.trim())));
  assert.match(styles.get("height"), /^\d+(?:\.\d+)?rem$/);
  assert.ok(parseFloat(styles.get("height")) >= 6 && parseFloat(styles.get("height")) <= 12);
  assert.equal(styles.has("min-height"), false);
  assert.equal(styles.get("overflow-y"), "auto"); assert.equal(styles.get("overflow-wrap"), "anywhere");
  assert.equal(styles.get("min-width"), "0"); assert.equal(styles.get("max-width"), "100%");
  assert.equal(styles.get("scrollbar-gutter"), "stable");
  const stale = deferred(), missing = deferred(), failed = deferred(); let requestNumber = 0;
  const longTitle = "UnbrokenHistoricalTitle".repeat(300);
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([timelineVisit()]);
    if (target.pathname.endsWith("/metadata/at")) return [stale.promise, metadataAt({ title: { title: longTitle, seen_at: received } }), missing.promise, failed.promise][requestNumber++];
  });
  await searchTimeline(app); app.navigate("history");
  const chart = timelineChart(app), hover = timelineElement(app, "timeline-hover");
  const viewportText = timelineElement(app, "timeline-viewport").textContent;
  const stable = () => { assert.equal(timelineChart(app), chart); assert.equal(timelineElement(app, "timeline-hover"), hover); assert.equal(timelineElement(app, "timeline-viewport").textContent, viewportText); };
  stable(); assert.match(hover.textContent, /Hover or focus/);
  const points = descendants(chart).filter((item) => item.tagName === "CIRCLE");
  await points[0].dispatch("pointerenter", mouseAt(380)); await app.advance(180);
  stable(); assert.match(hover.textContent, /Loading stored broadcast metadata/);
  const staleRequest = app.calls.find((item) => item.target.pathname.endsWith("/metadata/at"));
  points[1].focus(); await app.advance(180);
  assert.equal(staleRequest.options.signal.aborted, true);
  stable(); assert.ok(hover.textContent.includes(longTitle));
  hover.scrollTop = 70;
  hover.focus(); await settle(); await chart.dispatch("pointerleave");
  assert.equal(app.document.activeElement, hover); assert.equal(hover.tabIndex, 0);
  assert.match(hover.attributes["aria-label"], /scroll for full details/);
  assert.ok(hover.textContent.includes(longTitle)); assert.equal(hover.scrollTop, 70);
  stale.resolve(metadataAt({ title: { title: "STALE OVER RETAINED TITLE", seen_at: received } })); await settle();
  stable(); assert.ok(hover.textContent.includes(longTitle)); assert.doesNotMatch(hover.textContent, /STALE OVER RETAINED TITLE/);
  const inference = timelineElement(app, "timeline-inference");
  await inference.dispatch("pointerenter", mouseAt(600)); await app.advance(180);
  stable(); assert.match(hover.textContent, /Loading stored broadcast metadata/); assert.equal(hover.scrollTop, 0);
  missing.resolve(metadataAt({ basis: "unavailable", reason: "no_history", title: null })); await settle();
  await inference.dispatch("pointerleave"); stable(); assert.match(hover.textContent, /Unknown broadcast title.*No stored metadata history/);
  await inference.dispatch("pointerenter", mouseAt(650)); await app.advance(180);
  failed.resolve({ httpStatus: 500, error: { message: "Metadata unavailable" } }); await settle();
  await inference.dispatch("pointerleave"); stable(); assert.match(hover.textContent, /Unknown broadcast title.*could not be loaded/);
  await inference.dispatch("keydown", { key: "Escape" }); stable(); assert.match(hover.textContent, /Hover or focus/);
});

test("timeline inference hover maps transformed times, clamps the exclusive right edge and never queries PART or unknown background", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([timelineVisit({ inference_to: timelineTo })]);
    if (target.pathname.endsWith("/metadata/at")) return metadataAt({ basis: "exact_check", check: { id: 5, state: "live", checked_at: received, broadcast_id: 9 }, title: { title: "", seen_at: received } });
  });
  await searchTimeline(app); app.navigate("history");
  const chart = timelineChart(app); chart.screenMatrix = { a: .9, b: 0, c: 0, d: .9, e: -200, f: 50 };
  const inference = timelineElement(app, "timeline-inference");
  await inference.dispatch("pointerenter", mouseAt(575 * .9 - 200, 78 * .9 + 50)); await app.advance(180);
  let request = app.calls.filter((item) => item.target.pathname.endsWith("/metadata/at")).at(-1);
  assert.equal(request.target.searchParams.get("at"), new Date(received).toISOString());
  assert.match(timelineElement(app, "timeline-hover").textContent, /Uncertain chat inference; continuous presence is unknown/);
  assert.match(timelineElement(app, "timeline-hover").textContent, /Reported title: Empty reported title/);
  assert.doesNotMatch(timelineElement(app, "timeline-hover").textContent, /Last reported title/);
  await inference.dispatch("pointermove", mouseAt(970 * .9 - 200, 78 * .9 + 50)); await app.advance(180);
  request = app.calls.filter((item) => item.target.pathname.endsWith("/metadata/at")).at(-1);
  assert.equal(request.target.searchParams.get("at"), "2026-10-23T13:59:59.999Z");
  assert.equal(descendants(chart).find((item) => item.attributes.fill === "url(#timeline-hatch)").attributes["pointer-events"], "none");
  await inference.dispatch("pointerleave");
  const requests = app.calls.length;
  await timelineElement(app, "timeline-unknown").dispatch("pointerenter", mouseAt(200));
  await timelineElement(app, "timeline-departure").dispatch("pointerenter", mouseAt(772.5)); await app.advance(200);
  assert.equal(app.calls.length, requests);
});

test("timeline hover explains unavailable history without using the current broadcast title", async () => {
  for (const [reason, expected] of [["no_history", /No stored metadata history/], ["polling_gap", /polling gap/], ["missing_title_history", /No historical title/], ["metadata_unknown", /metadata state is unknown/], ["offline_report", /reported the channel offline/], ["transition_ambiguous", /transition is ambiguous/], ["broadcast_mismatch", /does not match/], ["outside_broadcast_window", /outside the recorded/], ["inconsistent_history", /history is inconsistent/]]) {
    const app = createApp((target) => {
      if (target.pathname === "/api/timeline") return timeline([timelineVisit()], { broadcast: { id: 9, title: "DO NOT BACKFILL CURRENT TITLE", started_at: timelineFrom, last_live_at: timelineTo } });
      if (target.pathname.endsWith("/metadata/at")) return metadataAt({ basis: "unavailable", reason, title: null });
    });
    await searchTimeline(app); app.navigate("history");
    const point = descendants(timelineChart(app)).find((item) => item.tagName === "CIRCLE"); point.focus(); await app.advance(180);
    const request = app.calls.find((item) => item.target.pathname.endsWith("/metadata/at"));
    assert.equal(request.target.searchParams.get("broadcast_id"), "9");
    const text = timelineElement(app, "timeline-hover").textContent;
    assert.match(text, /Unknown broadcast title/); assert.match(text, expected);
    assert.doesNotMatch(text, /DO NOT BACKFILL CURRENT TITLE/);
    assert.match(text, /does not establish absence or video viewing/);
    await point.dispatch("keydown", { key: "Escape" });
    assert.match(timelineElement(app, "timeline-hover").textContent, /Hover or focus/);
  }
});

test("moving timeline hover aborts pending lookups and rejects late successes and errors", async () => {
  const old = deferred(), newer = deferred(); let requestCount = 0;
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([timelineVisit()]);
    if (target.pathname.endsWith("/metadata/at")) return ++requestCount === 1 ? old.promise : newer.promise;
  });
  await searchTimeline(app); app.navigate("history");
  const inference = timelineElement(app, "timeline-inference");
  await inference.dispatch("pointerenter", mouseAt(500)); await app.advance(180);
  const first = app.calls.find((item) => item.target.pathname.endsWith("/metadata/at"));
  await inference.dispatch("pointermove", mouseAt(600));
  assert.equal(first.options.signal.aborted, true); await app.advance(180);
  old.resolve(metadataAt({ title: { title: "STALE TITLE", seen_at: received } })); await settle();
  assert.doesNotMatch(timelineElement(app, "timeline-hover").textContent, /STALE TITLE/);
  newer.resolve({ httpStatus: 500, error: { message: '<img src=x onerror="injected()">' } }); await settle();
  assert.match(timelineElement(app, "timeline-hover").textContent, /Unknown broadcast title.*could not be loaded/);
  assert.doesNotMatch(timelineElement(app, "timeline-hover").textContent, /<img/);
  await inference.dispatch("pointerleave");
  assert.match(timelineElement(app, "timeline-hover").textContent, /Unknown broadcast title.*could not be loaded/);
});

test("timeline hover cancels debounces and in-flight requests on drag, leave, zoom, filters, paging, navigation and hidden pages", async () => {
  for (const action of ["drag", "leave", "zoom", "filters", "page", "new-search", "navigation", "hidden"]) {
    const pending = deferred();
    const app = createApp((target) => {
      if (target.pathname === "/api/timeline") return timeline([timelineVisit()], { has_more: true, next_before: 10 });
      if (target.pathname.endsWith("/metadata/at")) return pending.promise;
    });
    await searchTimeline(app); app.navigate("history");
    let point = descendants(timelineChart(app)).find((item) => item.tagName === "CIRCLE");
    await point.dispatch("pointerenter", mouseAt(400));
    await point.dispatch("pointerleave"); await app.advance(180);
    assert.equal(app.calls.some((item) => item.target.pathname.endsWith("/metadata/at")), false);
    await point.dispatch("pointerenter", mouseAt(400)); await app.advance(180);
    const request = app.calls.find((item) => item.target.pathname.endsWith("/metadata/at"));
    if (action === "drag") await timelineChart(app).dispatch("pointerdown", mouseAt(400));
    if (action === "leave") await point.dispatch("pointerleave");
    if (action === "zoom") await timelineButton(app, "Zoom in").dispatch("click");
    if (action === "filters") await app.get("search-channel").dispatch("input");
    if (action === "page") { await app.get("timeline-next").dispatch("click"); await settle(); }
    if (action === "new-search") { await app.get("search-form").dispatch("submit"); await settle(); }
    if (action === "navigation") app.navigate("broadcasts");
    if (action === "hidden") app.visibility(true);
    assert.equal(request.options.signal.aborted, true, action);
    pending.resolve(action === "leave" ? { httpStatus: 500, error: { message: "STALE AFTER leave" } } : metadataAt({ title: { title: "STALE AFTER " + action, seen_at: received } })); await settle();
    assert.doesNotMatch(timelineElement(app, "timeline-hover").textContent, /STALE AFTER/);
    assert.match(timelineElement(app, "timeline-hover").textContent, /Hover or focus/);
  }
});

test("broadcast timeline hover requests the selected broadcast and keyboard focus inspects inference midpoint", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/broadcasts") return { broadcasts: [{ id: 9, channel: "sample_channel", title: "Current title", started_at: timelineFrom, last_live_at: timelineTo }], next_before: null };
    if (target.pathname === "/api/timeline") return timeline([timelineVisit()], { broadcast: { id: 9, channel: "sample_channel", started_at: timelineFrom, last_live_at: timelineTo } });
    if (target.pathname.endsWith("/metadata/at")) return metadataAt();
  });
  await settle(); app.navigate("broadcasts"); app.get("broadcasts-account").value = "sample_account";
  await app.get("broadcasts-form").dispatch("submit"); await settle();
  await descendants(app.get("broadcasts-results")).find((item) => item.tagName === "BUTTON" && item.textContent === "View summary / visits / events").dispatch("click"); await settle();
  const inference = timelineElement(app, "timeline-inference", "broadcast-timeline");
  inference.focus(); await app.advance(180);
  const request = app.calls.find((item) => item.target.pathname.endsWith("/metadata/at"));
  assert.equal(request.target.searchParams.get("broadcast_id"), "9");
  assert.equal(request.target.searchParams.get("at"), new Date(received).toISOString());
  assert.match(timelineElement(app, "timeline-hover", "broadcast-timeline").textContent, /Last reported title: Stored broadcast title/);
  inference.blur(); assert.match(timelineElement(app, "timeline-hover", "broadcast-timeline").textContent, /Last reported title: Stored broadcast title/);
});

test("timeline zoom buttons clip only loaded evidence, keep the query fixed and reset without fetching", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit()], { has_more: true, next_before: 10 }) : undefined);
  await searchTimeline(app);
  const requests = app.calls.length;
  const reset = timelineButton(app, "Reset zoom");
  assert.equal(reset.disabled, true); assert.equal(timelineButton(app, "Zoom out").disabled, true);
  const zoomIn = timelineButton(app, "Zoom in"); zoomIn.focus(); await zoomIn.dispatch("click");
  assert.equal(app.document.activeElement, zoomIn);
  assert.match(timelineElement(app, "timeline-viewport").textContent, /2026-10-23T11:00:00.000Z.*2026-10-23T13:00:00.000Z/);
  assert.match(timelineElement(app, "timeline-period").textContent, /query period.*2026-10-23T10:00:00.000Z.*2026-10-23T14:00:00.000Z/);
  assert.match(app.get("timeline-results").textContent, /additional evidence.*other pages/);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.tagName === "CIRCLE").length, 2);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.tagName === "POLYGON").length, 0);
  assert.equal(timelineElement(app, "timeline-inference").attributes.width, "790");
  assert.ok(descendants(timelineChart(app)).some((item) => item.attributes.fill === "url(#timeline-hatch)"));
  assert.equal(reset.disabled, false);
  await timelineButton(app, "Zoom out").dispatch("click");
  assert.match(timelineElement(app, "timeline-viewport").textContent, /2026-10-23T10:00:00.000Z.*2026-10-23T14:00:00.000Z/);
  await zoomIn.dispatch("click"); await reset.dispatch("click");
  assert.equal(reset.disabled, true);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.tagName === "POLYGON").length, 1);
  assert.equal(app.calls.length, requests);
});

test("timeline reverse selection uses transformed screen coordinates and never fabricates clipped endpoint dots", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit()]) : undefined);
  await searchTimeline(app);
  const requests = app.calls.length, chart = timelineChart(app);
  // A responsive chart after horizontal scrolling: local coordinates are scaled and translated on screen.
  chart.screenMatrix = { a: .9, b: 0, c: 0, d: .9, e: -150, f: 100 };
  const screen = (x) => mouseAt(x * .9 - 150, 78 * .9 + 100);
  const down = await chart.dispatch("pointerdown", screen(673.75));
  assert.equal(down.defaultPrevented, true); assert.equal(chart.hasPointerCapture(1), true);
  await chart.dispatch("pointermove", screen(476.25));
  const selection = timelineElement(app, "timeline-selection");
  assert.equal(selection.attributes.visibility, "visible");
  assert.ok(Math.abs(Number(selection.attributes.x) - 476.25) < .001);
  assert.ok(Math.abs(Number(selection.attributes.width) - 197.5) < .001);
  await chart.dispatch("pointerleave", screen(476.25));
  assert.equal(chart.hasPointerCapture(1), true);
  await chart.dispatch("pointerup", screen(476.25));
  assert.equal(chart.hasPointerCapture(1), false);
  assert.match(timelineElement(app, "timeline-viewport").textContent, /2026-10-23T11:30:00.000Z.*2026-10-23T12:30:00.000Z/);
  const points = descendants(timelineChart(app)).filter((item) => item.tagName === "CIRCLE");
  assert.equal(points.length, 1); assert.equal(points[0].attributes.cx, "575");
  assert.equal(timelineElement(app, "timeline-inference").attributes.x, "180");
  assert.equal(timelineElement(app, "timeline-inference").attributes.width, "790");
  assert.equal(descendants(timelineChart(app)).filter((item) => item.tagName === "POLYGON").length, 0);
  assert.match(app.get("timeline-results").textContent, /Unknown.*unloaded evidence/);
  assert.equal(app.calls.length, requests);
});

test("timeline drag clamps outside endpoints, handles both directions and ignores other pointer identities", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit()]) : undefined);
  await searchTimeline(app);
  let chart = timelineChart(app);
  await chart.dispatch("pointerdown", mouseAt(575));
  await chart.dispatch("pointerup", mouseAt(1300, 78, { pointerId: 2 }));
  assert.equal(chart.hasPointerCapture(1), true);
  await chart.dispatch("pointermove", mouseAt(1300));
  assert.equal(timelineElement(app, "timeline-selection").attributes.width, "395");
  await chart.dispatch("pointerup", mouseAt(1300));
  assert.match(timelineElement(app, "timeline-viewport").textContent, /2026-10-23T12:00:00.000Z.*2026-10-23T14:00:00.000Z/);
  const point = descendants(timelineChart(app)).find((item) => item.tagName === "CIRCLE");
  assert.equal(point.attributes.cx, "180"); assert.match(point.textContent, /Last positive/);
  await timelineButton(app, "Reset zoom").dispatch("click"); chart = timelineChart(app);
  await chart.dispatch("pointerdown", mouseAt(575)); await chart.dispatch("pointerup", mouseAt(30));
  assert.match(timelineElement(app, "timeline-viewport").textContent, /2026-10-23T10:00:00.000Z.*2026-10-23T12:00:00.000Z/);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.tagName === "CIRCLE").length, 1);
});

test("timeline canceled, hidden, navigated, lost, tiny and non-mouse drags leave the viewport unchanged and scrolling available", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit()]) : undefined);
  await searchTimeline(app);
  const original = timelineElement(app, "timeline-viewport").textContent;
  const chart = timelineChart(app);
  for (const rejected of [mouseAt(100), mouseAt(500, 30), mouseAt(500, 78, { pointerType: "touch" }), mouseAt(500, 78, { button: 2 }), mouseAt(500, 78, { isPrimary: false })]) {
    assert.equal((await chart.dispatch("pointerdown", rejected)).defaultPrevented, false);
    assert.equal(chart.hasPointerCapture(1), false);
  }
  for (const cancel of ["pointercancel", "lostpointercapture", "pointerleave", "navigation", "hidden"]) {
    await chart.dispatch("pointerdown", mouseAt(400)); await chart.dispatch("pointermove", mouseAt(800));
    assert.equal(chart.hasPointerCapture(1), true);
    if (cancel === "pointerleave") chart.capturedPointers.delete(1);
    if (cancel === "navigation") app.navigate("broadcasts");
    else if (cancel === "hidden") app.visibility(true);
    else await chart.dispatch(cancel, mouseAt(800));
    assert.equal(chart.hasPointerCapture(1), false);
    await chart.dispatch("pointerup", mouseAt(800));
    assert.equal(timelineElement(app, "timeline-selection").attributes.visibility, "hidden");
    assert.equal(chart.hasPointerCapture(1), false);
    assert.equal(timelineElement(app, "timeline-viewport").textContent, original);
    if (cancel === "navigation") app.navigate("history");
    if (cancel === "hidden") { app.visibility(false); await settle(); }
  }
  await chart.dispatch("pointerdown", mouseAt(400)); await chart.dispatch("pointerup", mouseAt(404));
  assert.equal(timelineElement(app, "timeline-viewport").textContent, original);
  chart.screenMatrix = { a: 0, b: 0, c: 0, d: 0, e: 0, f: 0 };
  assert.equal((await chart.dispatch("pointerdown", mouseAt(500))).defaultPrevented, false);
  assert.equal(timelineElement(app, "timeline-viewport").textContent, original);
});

test("timeline pages retain zoom and fixed query bounds while a new search resets the viewport", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit()], { has_more: !target.searchParams.has("before"), next_before: target.searchParams.has("before") ? null : 10 }) : undefined);
  await searchTimeline(app);
  await timelineButton(app, "Zoom in").dispatch("click");
  await app.get("timeline-next").dispatch("click"); await settle();
  assert.equal(timelineButton(app, "Reset zoom").disabled, false);
  assert.match(timelineElement(app, "timeline-viewport").textContent, /2026-10-23T11:00:00.000Z.*2026-10-23T13:00:00.000Z/);
  let request = app.calls.filter((item) => item.target.pathname === "/api/timeline").at(-1);
  assert.equal(request.target.searchParams.get("from"), timelineFrom); assert.equal(request.target.searchParams.get("to"), timelineTo);
  await app.get("timeline-previous").dispatch("click"); await settle();
  assert.equal(timelineButton(app, "Reset zoom").disabled, false);
  assert.match(timelineElement(app, "timeline-viewport").textContent, /2026-10-23T11:00:00.000Z.*2026-10-23T13:00:00.000Z/);
  await app.get("search-form").dispatch("submit"); await settle();
  assert.equal(timelineButton(app, "Reset zoom").disabled, true);
  request = app.calls.filter((item) => item.target.pathname === "/api/timeline").at(-1);
  assert.equal(request.target.searchParams.has("from"), false); assert.equal(request.target.searchParams.has("to"), false);
});

test("broadcast search keeps account first and omits an empty optional channel when listing account associations", async () => {
  const accountLabel = html.indexOf('<label for="broadcasts-account">Viewer / account login</label>');
  const channelLabel = html.indexOf('<label for="broadcasts-channel">Channel login (optional)</label>');
  assert.ok(accountLabel >= 0 && channelLabel > accountLabel);
  assert.match(html, /Leave Channel login blank to list this account across all channels/);
  const app = createApp(); await settle();
  app.get("broadcasts-account").value = "sample_account";
  await app.get("broadcasts-form").dispatch("submit"); await settle();
  let request = app.calls.filter((item) => item.target.pathname === "/api/broadcasts").at(-1);
  assert.equal(request.target.searchParams.get("account"), "sample_account");
  assert.equal(request.target.searchParams.has("channel"), false);
  app.get("broadcasts-channel").value = "sample_channel";
  await app.get("broadcasts-form").dispatch("submit"); await settle();
  request = app.calls.filter((item) => item.target.pathname === "/api/broadcasts").at(-1);
  assert.equal(request.target.searchParams.get("account"), "sample_account");
  assert.equal(request.target.searchParams.get("channel"), "sample_channel");
});

test("account timeline queries exact account, optional channel, and a bounded evidence page while retaining existing tables", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit(), timelineVisit({ id: 5, channel: "another_channel", session_id: 2 })]) : undefined);
  await settle();
  app.get("search-account").value = "@Sample_Account";
  await app.get("search-form").dispatch("submit"); await settle();
  let call = app.calls.find((item) => item.target.pathname === "/api/timeline");
  assert.equal(call.target.searchParams.get("account"), "sample_account");
  assert.equal(call.target.searchParams.has("channel"), false);
  assert.equal(call.target.searchParams.get("limit"), "25");
  assert.equal(call.target.searchParams.has("from"), false);
  assert.ok(app.calls.some((item) => item.target.pathname === "/api/search"));
  assert.ok(app.calls.some((item) => item.target.pathname === "/api/history"));
  const elements = descendants(app.get("timeline-results"));
  assert.equal(elements.filter((item) => item.classList.contains("timeline-unknown")).length, 2);
  assert.equal(elements.filter((item) => item.classList.contains("timeline-channel")).map((item) => item._text).join(","), "another_channel,sample_channel");
  assert.equal(elements.filter((item) => item.tagName === "SVG").length, 1);
  app.get("search-channel").value = "sample_channel";
  await app.get("search-form").dispatch("submit"); await settle();
  call = app.calls.filter((item) => item.target.pathname === "/api/timeline").at(-1);
  assert.equal(call.target.searchParams.get("channel"), "sample_channel");
});

test("timeline keyset pagination pins the effective axis, warns about unloaded evidence, and supports previous page", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit()], { has_more: !target.searchParams.has("before"), next_before: target.searchParams.has("before") ? null : 10 }) : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.match(app.get("timeline-results").textContent, /additional evidence.*other pages/i);
  const firstAxis = descendants(app.get("timeline-results")).filter((item) => item.classList.contains("timeline-axis-label")).map((item) => item.textContent);
  await app.get("timeline-next").dispatch("click"); await settle();
  let call = app.calls.filter((item) => item.target.pathname === "/api/timeline").at(-1);
  assert.equal(call.target.searchParams.get("before"), "10");
  assert.equal(call.target.searchParams.get("from"), timelineFrom);
  assert.equal(call.target.searchParams.get("to"), timelineTo);
  assert.deepEqual(descendants(app.get("timeline-results")).filter((item) => item.classList.contains("timeline-axis-label")).map((item) => item.textContent), firstAxis);
  assert.match(app.get("timeline-results").textContent, /Partial evidence page/);
  assert.equal(app.get("timeline-page").textContent, "Page 2");
  await app.get("timeline-previous").dispatch("click"); await settle();
  call = app.calls.filter((item) => item.target.pathname === "/api/timeline").at(-1);
  assert.equal(call.target.searchParams.has("before"), false);
  assert.equal(call.target.searchParams.get("from"), timelineFrom);
  assert.equal(app.get("timeline-page").textContent, "Page 1");
});

test("timeline clips inference and uses only actual half-open-window markers without synthetic edge points", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit({
    inference_from: "2026-10-23T09:00:00Z", inference_to: "2026-10-23T15:00:00Z",
    markers: [
      { observation_id: 1, at: "2026-10-23T09:00:00Z", kind: "first_positive", basis: "join" },
      { observation_id: 2, at: timelineFrom, kind: "part", basis: "part_receipt" },
      { observation_id: 3, at: timelineTo, kind: "last_positive", basis: "positive_receipt" },
    ],
  })]) : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit"); await settle();
  const elements = descendants(app.get("timeline-results"));
  assert.equal(elements.filter((item) => item.tagName === "CIRCLE").length, 0);
  assert.equal(elements.filter((item) => item.tagName === "POLYGON").length, 1);
  const inferred = elements.find((item) => item.classList.contains("timeline-inference"));
  assert.equal(inferred.attributes.x, "180"); assert.equal(inferred.attributes.width, "790");
  assert.match(app.get("timeline-results").textContent, /2026-10-23T10:00:00.000Z/);
  assert.match(app.get("timeline-results").textContent, /2026-10-23T14:00:00.000Z/);
});

test("separate sessions, ambiguous segments, out-of-order evidence and PART-only rows never gain connecting inference", async () => {
  const point = (at, id) => [{ observation_id: id, at, kind: "first_positive", basis: "join" }];
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([
    timelineVisit({ id: 4, session_id: 1, inference_from: "2026-10-23T10:30:00Z", inference_to: "2026-10-23T11:00:00Z", markers: point("2026-10-23T10:30:00Z", 4) }),
    timelineVisit({ id: 3, session_id: 2, inference_from: "2026-10-23T13:00:00Z", inference_to: "2026-10-23T13:30:00Z", markers: point("2026-10-23T13:00:00Z", 3) }),
    timelineVisit({ id: 2, supported_visit: false, markers: point(received, 2) }),
    timelineVisit({ id: 1, out_of_order: true, markers: point(received, 1) }),
    timelineVisit({ id: 0, kind: "departure", supported_visit: false, first_observed_at: null, last_observed_at: null, markers: [{ observation_id: 9, at: received, kind: "part", basis: "part_receipt" }] }),
  ]) : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit"); await settle();
  const elements = descendants(app.get("timeline-results"));
  assert.equal(elements.filter((item) => item.classList.contains("timeline-inference")).length, 2);
  assert.equal(elements.filter((item) => item.tagName === "CIRCLE").length, 4);
  assert.equal(elements.filter((item) => item.classList.contains("timeline-departure")).length, 1);
  const spans = elements.filter((item) => item.classList.contains("timeline-inference"));
  assert.ok(Number(spans[0].attributes.x) + Number(spans[0].attributes.width) < Number(spans[1].attributes.x));
  assert.match(app.get("timeline-results").textContent, /Ambiguous observation only/);
  assert.match(app.get("timeline-results").textContent, /Out-of-order evidence/);
  assert.match(app.get("timeline-results").textContent, /PART only/);
});

test("timeline empty evidence remains unknown and channel-only searches require an account", async () => {
  const app = createApp(); await settle();
  app.get("search-channel").value = "sample_channel";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.equal(app.calls.some((item) => item.target.pathname === "/api/timeline"), false);
  assert.match(app.get("timeline-results").textContent, /exact account login/);
  app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.match(app.get("timeline-results").textContent, /entire visible period remains unknown/);
  assert.match(app.get("timeline-results").textContent, /not.*absence/);
  assert.match(app.get("timeline-results").textContent, /No confidence percentage is calibrated/);
  assert.doesNotMatch(app.get("timeline-results").textContent, /99%|confirmed absence|verified absence|absent for/i);
  assert.equal(descendants(app.get("timeline-results")).filter((item) => item.classList.contains("timeline-unknown")).length, 1);
});

test("broadcast timeline requires an exact account and requests only the selected broadcast and time bounds", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/broadcasts") return { broadcasts: [{ id: 9, channel: "sample_channel", title: "A title", started_at: timelineFrom, last_live_at: timelineTo }], next_before: null };
    if (target.pathname === "/api/timeline") return timeline([timelineVisit()], { broadcast: { id: 9, channel: "sample_channel", started_at: timelineFrom, last_live_at: timelineTo } });
  });
  await settle();
  await app.get("broadcasts-form").dispatch("submit"); await settle();
  let open = descendants(app.get("broadcasts-results")).find((item) => item.tagName === "BUTTON" && item.textContent === "View summary / visits / events");
  await open.dispatch("click"); await settle();
  assert.equal(app.calls.some((item) => item.target.pathname === "/api/timeline"), false);
  assert.match(app.get("broadcast-timeline-results").textContent, /exact account login/);
  app.get("broadcasts-account").value = "sample_account";
  app.get("broadcasts-from").value = "2026-10-23T10:00:00";
  app.get("broadcasts-to").value = "2026-10-23T14:00:00";
  await app.get("broadcasts-form").dispatch("submit"); await settle();
  open = descendants(app.get("broadcasts-results")).find((item) => item.tagName === "BUTTON" && item.textContent === "View summary / visits / events");
  await open.dispatch("click"); await settle();
  const call = app.calls.find((item) => item.target.pathname === "/api/timeline");
  assert.equal(call.target.searchParams.get("broadcast_id"), "9");
  assert.equal(call.target.searchParams.get("account"), "sample_account");
  assert.equal(call.target.searchParams.get("from"), new Date("2026-10-23T10:00:00").toISOString());
  assert.equal(call.target.searchParams.get("to"), new Date("2026-10-23T14:00:00").toISOString());
  assert.match(app.get("broadcast-timeline-results").textContent, /Broadcast metadata window/);
  assert.equal(descendants(app.get("broadcast-timeline-results")).filter((item) => item.classList.contains("timeline-window")).length, 1);
});

test("timeline renders hostile text literally with an accessible shared chart and keyboard reachable exact table", async () => {
  const hostile = '<img src=x onerror="injected()">';
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit({ channel: hostile, start_basis: hostile })], { account: hostile }) : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit"); await settle();
  const elements = descendants(app.get("timeline-results"));
  assert.match(app.get("timeline-results").textContent, /<img src=x/);
  assert.equal(elements.some((item) => item.tagName === "IMG"), false);
  const graphic = elements.find((item) => item.tagName === "SVG");
  assert.equal(graphic.attributes.role, "img");
  assert.equal(graphic.attributes["aria-labelledby"], "timeline-chart-title timeline-chart-description");
  const scroll = elements.find((item) => item.classList.contains("timeline-scroll"));
  assert.equal(scroll.tabIndex, 0); assert.equal(scroll.attributes.role, "region");
  scroll.focus(); assert.equal(app.document.activeElement, scroll);
  const details = elements.find((item) => item.tagName === "DETAILS");
  assert.equal(details.children[0].tagName, "SUMMARY");
  assert.ok(descendants(details).some((item) => item.tagName === "TABLE"));
  assert.ok(descendants(details).some((item) => item.tagName === "TIME" && item.dateTime === new Date(received).toISOString()));
  assert.match(details.textContent, /positive receipt|Join|join/i);
  assert.match(details.textContent, /2026-10-23T12:00:00.000Z/);
});

test("a valid wide 25-character channel label stays within its slot while full identity remains accessible", async () => {
  const channel = "w".repeat(25);
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit({ channel })]) : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit"); await settle();
  const elements = descendants(app.get("timeline-results"));
  const label = elements.find((item) => item.classList.contains("timeline-channel"));
  assert.equal(label._text, "w".repeat(15) + "…");
  assert.equal(label.attributes.lengthAdjust, "spacingAndGlyphs");
  const plot = elements.find((item) => item.classList.contains("timeline-unknown"));
  assert.ok(Number(label.attributes.x) + Number(label.attributes.textLength) <= Number(plot.attributes.x) - 12);
  assert.equal(label.children.find((item) => item.tagName === "TITLE").textContent, channel);
  const evidenceTable = elements.find((item) => item.tagName === "TABLE");
  assert.ok(evidenceTable.textContent.includes(channel));
});

test("timeline request generations reject late replies and edited filters keep replies visibly stale", async () => {
  const old = deferred();
  const app = createApp((target) => target.pathname === "/api/timeline" ? target.searchParams.get("account") === "old_account" ? old.promise : timeline([timelineVisit()], { account: "new_account", has_more: true, next_before: 10 }) : undefined);
  await settle(); app.get("search-account").value = "old_account";
  await app.get("search-form").dispatch("submit");
  assert.equal(app.get("timeline-results").attributes["aria-busy"], "true");
  assert.match(app.get("timeline-message").textContent, /Loading/);
  app.get("search-account").value = "new_account";
  await app.get("search-form").dispatch("submit"); await settle();
  old.resolve(timeline([], { account: "old_account" })); await settle();
  assert.match(app.get("timeline-results").textContent, /new_account/);
  assert.doesNotMatch(app.get("timeline-results").textContent, /old_account/);
  app.get("search-account").value = "edited_account";
  await app.get("search-account").dispatch("input");
  assert.equal(app.get("timeline-results").classList.contains("stale"), true);
  assert.equal(app.get("timeline-next").disabled, true);
  const countBefore = app.calls.length;
  await app.get("timeline-next").dispatch("click"); await settle();
  assert.equal(app.calls.length, countBefore);
});

test("timeline failure retains the previous evidence page, rejects changed axes, and bounds overly wide requests", async () => {
  let changeAxis = false;
  const app = createApp((target) => target.pathname === "/api/timeline" ? target.searchParams.has("before") ? changeAxis ? timeline([], { effective_range: { from: "2026-10-22T10:00:00Z", to: timelineTo } }) : { httpStatus: 500, error: { message: "Timeline storage unavailable" } } : timeline([timelineVisit()], { has_more: true, next_before: 10 }) : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit"); await settle();
  await app.get("timeline-next").dispatch("click"); await settle();
  assert.match(app.get("timeline-message").textContent, /Timeline storage unavailable.*previous page/);
  assert.equal(app.get("timeline-page").textContent, "Page 1");
  assert.match(app.get("timeline-results").textContent, /sample_channel/);
  changeAxis = true;
  await app.get("timeline-next").dispatch("click"); await settle();
  assert.match(app.get("timeline-message").textContent, /period changed unexpectedly.*previous page/);
  const requests = app.calls.filter((item) => item.target.pathname === "/api/timeline").length;
  app.get("search-from").value = "2026-08-01T00:00:00"; app.get("search-to").value = "2026-10-23T00:00:00";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.match(app.get("timeline-message").textContent, /maximum of 31 days/);
  assert.equal(app.calls.filter((item) => item.target.pathname === "/api/timeline").length, requests);
});

test("editing timeline filters in flight preserves a stale warning after the pending response", async () => {
  const pending = deferred();
  const app = createApp((target) => target.pathname === "/api/timeline" ? pending.promise : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit");
  app.get("search-channel").value = "new_channel";
  await app.get("search-channel").dispatch("input");
  pending.resolve(timeline([timelineVisit()], { has_more: true, next_before: 10 })); await settle();
  assert.equal(app.get("timeline-results").classList.contains("stale"), true);
  assert.match(app.get("timeline-message").textContent, /Filters changed/);
  assert.equal(app.get("timeline-next").disabled, true);
  assert.equal(app.get("timeline-results").attributes["aria-busy"], "false");
});

test("a channel-only search clears an in-flight account timeline without a late overwrite", async () => {
  const pending = deferred();
  const app = createApp((target) => target.pathname === "/api/timeline" ? pending.promise : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit");
  app.get("search-account").value = ""; app.get("search-channel").value = "sample_channel";
  await app.get("search-form").dispatch("submit");
  pending.resolve(timeline([timelineVisit()])); await settle();
  assert.match(app.get("timeline-results").textContent, /Enter an exact account login/);
  assert.equal(descendants(app.get("timeline-results")).some((item) => item.tagName === "SVG"), false);
  assert.equal(app.get("timeline-results").attributes["aria-busy"], "false");
});

test("disjoint broadcast timeline period reports unknown without manufacturing an axis or receipt markers", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([], { empty: true, effective_range: { from: timelineFrom, to: timelineFrom } }) : undefined);
  await settle(); app.get("search-account").value = "sample_account";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.match(app.get("timeline-results").textContent, /No overlapping timeline period/);
  assert.equal(descendants(app.get("timeline-results")).some((item) => item.tagName === "SVG"), false);
});

const suggestionCalls = (app) => app.calls.filter((call) => call.target.pathname === "/api/suggestions");
async function typeLogin(app, id, value) {
  app.get(id).focus();
  app.get(id).value = value;
  await app.get(id).dispatch("input");
}

test("all four login comboboxes debounce normalized stored prefixes without fetching an account inventory", async () => {
  const app = createApp((target) => target.pathname === "/api/suggestions" ? {
    kind: target.searchParams.get("kind"), suggestions: [target.searchParams.get("prefix") + "_known"],
  } : undefined);
  await settle();
  for (const [id, kind, section] of [["search-account", "account", "history"], ["search-channel", "channel", "history"], ["broadcasts-account", "account", "broadcasts"], ["broadcasts-channel", "channel", "broadcasts"]]) {
    app.navigate(section);
    const before = suggestionCalls(app).length;
    await typeLogin(app, id, "  Sa  "); await app.advance(100);
    await typeLogin(app, id, "  SaM  "); await app.advance(179);
    assert.equal(suggestionCalls(app).length, before);
    await app.advance(1);
    const call = suggestionCalls(app).at(-1);
    assert.equal(suggestionCalls(app).length, before + 1);
    assert.equal(call.target.searchParams.get("kind"), kind);
    assert.equal(call.target.searchParams.get("prefix"), "sam");
    assert.equal(call.target.searchParams.get("limit"), "8");
    assert.equal(call.options.mode, "same-origin");
    assert.equal(call.options.cache, "no-store");
    assert.equal(app.get(id).attributes.role, "combobox");
    assert.equal(app.get(id).attributes["aria-autocomplete"], "list");
    assert.equal(app.get(id).attributes["aria-controls"], id + "-listbox");
    assert.equal(app.get(id).attributes["aria-expanded"], "true");
    assert.equal(app.get(id + "-listbox").children[0].textContent, "sam_known");
    assert.equal(app.document.activeElement, app.get(id));
  }
  assert.equal(app.calls.some((call) => call.target.pathname === "/api/accounts"), false);
  assert.equal(app.calls.some((call) => call.target.pathname === "/api/search" || call.target.pathname === "/api/broadcasts"), false);
});

test("empty, invalid, too-long, unfocused and hidden prefixes never request suggestions", async () => {
  const app = createApp(); await settle(); app.navigate("history");
  for (const value of ["", " ", "#sam", "@sam", "https://example", "sam-name", "sam name", "é", "a".repeat(26)]) {
    await typeLogin(app, "search-account", value); await app.advance(200);
    assert.equal(app.get("search-account-suggestions").hidden, true);
  }
  app.get("search-account").blur(); app.get("search-account").value = "sample";
  await app.get("search-account").dispatch("input"); await app.advance(200);
  app.navigate("channels"); await typeLogin(app, "search-account", "sample"); await app.advance(200);
  assert.equal(suggestionCalls(app).length, 0);
});

test("keyboard navigation retains input focus and Enter accepts only a highlighted login without searching", async () => {
  const app = createApp((target) => target.pathname === "/api/suggestions" ? { kind: "account", suggestions: ["sample_a", "sample_b"] } : undefined);
  await settle(); app.navigate("history");
  app.get("search-channel").value = "other_channel";
  await typeLogin(app, "search-account", "sample"); await app.advance(180);
  const input = app.get("search-account");
  assert.equal((await input.dispatch("keydown", { key: "Enter" })).defaultPrevented, false);
  assert.equal(input.value, "sample");
  assert.equal(input.attributes["aria-activedescendant"], undefined);
  assert.equal((await input.dispatch("keydown", { key: "ArrowUp" })).defaultPrevented, true);
  assert.equal(input.attributes["aria-activedescendant"], "search-account-suggestion-1");
  assert.equal(app.get("search-account-listbox").children[1].attributes["aria-selected"], "true");
  await input.dispatch("keydown", { key: "ArrowDown" });
  assert.equal(input.attributes["aria-activedescendant"], "search-account-suggestion-0");
  assert.equal(app.get("search-account-listbox").children[1].attributes["aria-selected"], "false");
  assert.equal(app.document.activeElement, input);
  assert.equal((await input.dispatch("keydown", { key: "Enter" })).defaultPrevented, true);
  assert.equal(input.value, "sample_a");
  assert.equal(app.get("search-channel").value, "other_channel");
  assert.equal(app.get("search-account-suggestions").hidden, true);
  assert.equal(input.attributes["aria-expanded"], "false");
  assert.equal(input.attributes["aria-activedescendant"], undefined);
  assert.equal(app.calls.some((call) => call.target.pathname === "/api/search"), false);
  await app.advance(300); assert.equal(suggestionCalls(app).length, 1);
  assert.equal((await input.dispatch("keydown", { key: "Enter" })).defaultPrevented, false);
  await app.get("search-form").dispatch("submit"); await settle();
  assert.equal(app.calls.find((call) => call.target.pathname === "/api/search").target.searchParams.get("account"), "sample_a");
});

test("mouse and touch selection fill only their field and keep both forms idle until normal submission", async () => {
  const app = createApp((target) => target.pathname === "/api/suggestions" ? { kind: target.searchParams.get("kind"), suggestions: ["stored_login"] } : undefined);
  await settle();
  for (const [id, other, section, pointerType] of [["search-channel", "search-account", "history", "mouse"], ["broadcasts-account", "broadcasts-channel", "broadcasts", "touch"]]) {
    app.navigate(section); app.get(other).value = "manual_other";
    await typeLogin(app, id, "stored"); await app.advance(180);
    const option = app.get(id + "-listbox").children[0];
    assert.equal(option.attributes.role, "option");
    assert.equal((await option.dispatch("pointerdown", { pointerType })).defaultPrevented, true);
    assert.equal(app.document.activeElement, app.get(id));
    await option.dispatch("click", { pointerType });
    assert.equal(app.get(id).value, "stored_login");
    assert.equal(app.get(other).value, "manual_other");
    assert.equal(app.get(id + "-suggestions").hidden, true);
  }
  assert.equal(app.calls.some((call) => call.target.pathname === "/api/search" || call.target.pathname === "/api/broadcasts"), false);
  await app.get("broadcasts-form").dispatch("submit"); await settle();
  const request = app.calls.find((call) => call.target.pathname === "/api/broadcasts");
  assert.equal(request.target.searchParams.get("account"), "stored_login");
  assert.equal(request.target.searchParams.get("channel"), "manual_other");
});

test("a newer prefix aborts the prior suggestion request and rejects a late out-of-order reply", async () => {
  const old = deferred();
  const app = createApp((target) => target.pathname === "/api/suggestions" ? target.searchParams.get("prefix") === "sa" ? old.promise : { kind: "account", suggestions: ["sample_new"] } : undefined);
  await settle(); app.navigate("history");
  await typeLogin(app, "search-account", "sa"); await app.advance(180);
  const original = suggestionCalls(app)[0];
  assert.match(app.get("search-account-suggestion-status").textContent, /Looking for stored accounts/);
  await typeLogin(app, "search-account", "sam");
  assert.equal(original.options.signal.aborted, true);
  assert.equal(app.get("search-account-suggestions").hidden, true);
  await app.advance(180);
  assert.equal(app.get("search-account-listbox").children[0].textContent, "sample_new");
  old.resolve({ kind: "account", suggestions: ["sa_old"] }); await settle();
  assert.equal(app.get("search-account-listbox").children[0].textContent, "sample_new");
});

test("Escape, Tab, blur, navigation, submission and a hidden page cancel pending suggestions without reopening", async () => {
  for (const reason of ["escape", "tab", "blur", "navigate", "submit", "hidden"]) {
    const pending = deferred();
    const app = createApp((target) => target.pathname === "/api/suggestions" ? pending.promise : undefined);
    await settle(); app.navigate("history");
    await typeLogin(app, "search-account", "sample"); await app.advance(180);
    const request = suggestionCalls(app)[0];
    const input = app.get("search-account");
    if (reason === "escape") assert.equal((await input.dispatch("keydown", { key: "Escape" })).defaultPrevented, true);
    if (reason === "tab") assert.equal((await input.dispatch("keydown", { key: "Tab" })).defaultPrevented, false);
    if (reason === "blur") app.get("search-channel").focus();
    if (reason === "navigate") app.navigate("broadcasts");
    if (reason === "submit") await app.get("search-form").dispatch("submit");
    if (reason === "hidden") app.visibility(true);
    assert.equal(request.options.signal.aborted, true, reason);
    pending.resolve({ kind: "account", suggestions: ["sample_old"] }); await settle(); await app.advance(300);
    assert.equal(app.get("search-account-suggestions").hidden, true, reason);
    assert.equal(input.attributes["aria-expanded"], "false", reason);
    assert.equal(app.get("search-account-listbox").children.length, 0, reason);
    assert.equal(suggestionCalls(app).length, 1, reason);
  }
});

test("Escape and Tab during the debounce cancel the scheduled lookup", async () => {
  const app = createApp(); await settle(); app.navigate("broadcasts");
  for (const key of ["Escape", "Tab"]) {
    await typeLogin(app, "broadcasts-channel", "sample"); await app.advance(90);
    await app.get("broadcasts-channel").dispatch("keydown", { key }); await app.advance(300);
    assert.equal(app.get("broadcasts-channel-suggestions").hidden, true);
  }
  assert.equal(suggestionCalls(app).length, 0);
});

test("empty and unavailable suggestions explain manual entry and never prevent normal searches", async () => {
  for (const response of [{ kind: "account", suggestions: [] }, { httpStatus: 503, error: { message: "Stored lookup unavailable" } }, new TypeError("offline")]) {
    const app = createApp((target) => target.pathname === "/api/suggestions" ? response : undefined);
    await settle(); app.navigate("history");
    await typeLogin(app, "search-account", "manual_exact"); await app.advance(180);
    assert.equal(app.get("search-account-suggestions").hidden, false);
    assert.match(app.get("search-account-suggestion-status").textContent, /still enter an exact login/);
    assert.equal(app.get("search-account-listbox").children.length, 0);
    assert.equal((await app.get("search-account").dispatch("keydown", { key: "Enter" })).defaultPrevented, false);
    await app.get("search-form").dispatch("submit"); await settle();
    assert.equal(app.calls.find((call) => call.target.pathname === "/api/search").target.searchParams.get("account"), "manual_exact");
    assert.equal(app.get("search-account-suggestions").hidden, true);
  }
});

test("late errors after dismissal stay closed and malformed suggestion payloads remain harmless", async () => {
  const pending = deferred();
  const app = createApp((target) => target.pathname === "/api/suggestions" ? pending.promise : undefined);
  await settle(); app.navigate("broadcasts");
  await typeLogin(app, "broadcasts-channel", "sample"); await app.advance(180);
  await app.get("broadcasts-channel").dispatch("keydown", { key: "Escape" });
  pending.resolve({ httpStatus: 500 }); await settle();
  assert.equal(app.get("broadcasts-channel-suggestions").hidden, true);
  assert.equal(app.get("broadcasts-channel-suggestion-status").textContent, "");
  for (const suggestions of [["<img src=x onerror=alert(1)>"], ["sample_b", "sample_a"], ["sample_a", "sample_a"], ["unrelated"], Array(9).fill("sample_a"), [42]]) {
    const invalid = createApp((target) => target.pathname === "/api/suggestions" ? { kind: "account", suggestions } : undefined);
    await settle(); invalid.navigate("history");
    await typeLogin(invalid, "search-account", "sample"); await invalid.advance(180);
    assert.match(invalid.get("search-account-suggestion-status").textContent, /Suggestions unavailable/);
    assert.equal(invalid.get("search-account-listbox").children.length, 0);
    assert.equal(descendants(invalid.get("search-account-listbox")).some((item) => item.tagName === "IMG"), false);
  }
});

test("selecting a stored login marks existing results stale without a new search", async () => {
  const app = createApp((target) => target.pathname === "/api/suggestions" ? { kind: "account", suggestions: ["sample_known"] } : undefined);
  await settle(); app.navigate("history");
  app.get("search-account").value = "sample";
  await app.get("search-form").dispatch("submit"); await settle();
  const before = app.calls.filter((call) => call.target.pathname === "/api/search").length;
  app.get("search-account").focus(); await app.advance(180);
  await app.get("search-account-listbox").children[0].dispatch("click");
  assert.equal(app.get("search-results").classList.contains("stale"), true);
  assert.match(app.get("search-message").textContent, /Filters changed/);
  assert.equal(app.calls.filter((call) => call.target.pathname === "/api/search").length, before);
});
