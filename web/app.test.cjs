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

function createApp(override, origin = "http://127.0.0.1:3131", date = Date) {
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
    if (target.pathname === "/api/community/status") return communityState();
    if (target.pathname === "/api/community/receipts") return { rows: [], next_before_member_id: null };
    if (target.pathname === "/api/community/presence") return communityModel(target, { status: "no_data", cells: [], sensitivity_scenarios: 0 });
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
    document, location, Node: Element, URL, URLSearchParams, Intl, Date: date, Set, Number, Error, TypeError, AbortController,
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
  assert.match(health.textContent, /Latest diagnostic event/);
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

test("memory-only database activity renders while aggregate status is still waiting", async () => {
  const slow = deferred();
  const app = createApp((target) => {
    if (target.pathname === "/api/status") return slow.promise;
    if (target.pathname === "/api/storage/activity") return { process_id: 42, port: 3131, activity: {
      schema_version: 1, admission_waiting: 1, waiting_to_start: 4, oldest_queued_age_ms: 5000,
      active: { operation: "append", job_class: "durable_mutation", active_age_ms: 32000, queue_wait_ms: 0 },
      totals: { succeeded: 7, read_response_timeouts: 2 },
    } };
  });
  await settle();
  const activity = app.get("storage-activity");
  assert.match(activity.textContent, /Saving data/);
  assert.match(activity.textContent, /Running for32\.0 seconds/);
  assert.match(activity.textContent, /Waiting to start4/);
  assert.match(activity.textContent, /Timed-out read responses2/);
  assert.doesNotMatch(app.get("health-results").textContent, /Stored events/);
  slow.resolve({ state: "collecting", storage_health: "read_wait_timeout", observation_count: null, storage_bytes: null });
  await settle();
  assert.match(app.get("health-results").textContent, /Local storageBusy/);
  assert.match(app.get("health-results").textContent, /Read response delayed/);
  assert.match(activity.textContent, /Saving data/);
});

test("failed activity refresh retains its last snapshot as stale and rejects unsafe fields", async () => {
  let unavailable = false;
  const app = createApp((target) => {
    if (target.pathname === "/api/storage/activity") return unavailable ? new TypeError("unreachable") : {
      process_id: 42, port: 3131, activity: { schema_version: 1, admission_waiting: 0, waiting_to_start: 3,
        oldest_queued_age_ms: null, active: { operation: "<img src=x>", job_class: "query", active_age_ms: 1000, queue_wait_ms: 0 },
        totals: { succeeded: 5, read_response_timeouts: 0 } },
    };
  });
  await settle();
  const activity = app.get("storage-activity");
  assert.match(activity.textContent, /OperationUnknown/);
  assert.match(activity.textContent, /Waiting to start3/);
  assert.doesNotMatch(activity.textContent, /img src/);
  unavailable = true;
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.equal(activity.classList.contains("stale"), true);
  assert.match(activity.textContent, /Waiting to start3/);
});

test("normal Community shutdown is a lifecycle diagnostic and never a storage fault", async () => {
  const app = createApp((target) => target.pathname === "/api/status" ? {
    state: "collecting", diagnostics: { first_failure: null,
      latest: { sequence: 1, occurred_at_utc: received, event: "community_host_termination", operation: "community_host",
        host_termination: { reason: "shutdown_requested", observed_at_unix_ms: Date.parse(received) } },
      persistence: { state: "ready", failure_count: 0, dropped_count: 0 } },
  } : undefined);
  await settle();
  const health = app.get("health-results");
  assert.match(health.textContent, /Local diagnostics/);
  assert.match(health.textContent, /Latest diagnostic event/);
  assert.match(health.textContent, /Browser stop reasonShutdown requested by Veylurk/);
  assert.doesNotMatch(health.textContent, /First storage failure|Latest recorded fault|Fault records describe program errors|SQLite primary code/);
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
  assert.match(refreshed.textContent, /No diagnostic event recorded in this application run/);
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

function communityState(extra = {}) {
  return { settings: { enabled: false, target_interval_seconds: 300, cadence_mode: "adaptive" }, state: "disabled", paused: false, last_error: null, active_channel: null, next_channel: null, next_due_at_utc: null, last_received_at_utc: null, last_received_channel: null, channels: [], selected_channels: 1, delayed_channels: 0, global_start_gap_seconds: 30, achieved_interval_seconds: null, last_returned_accounts: null, cleanup_confirmed: null, ...extra };
}
function communityChannel(channel = "sample_channel", extra = {}) {
  return { channel, effective_interval_seconds: 60, cadence_basis: "adaptive", reported_chatters: 500, viewer_category_rows: 100, achieved_interval_seconds: 67.5, failures: 0, next_eligible_at_utc: "2026-10-23T12:01:00Z", last_received_at_utc: received, ...extra };
}
function communityReceipt(memberId, extra = {}) {
  return { member_id: memberId, sample_id: 700, run_id: 3, job_id: 8, source: "community", channel: "sample_channel", account: "community_account", roles: 36, started_at_utc: "2026-10-23T11:59:59Z", received_at_utc: received, reported_chatters: 200, returned_accounts: 100, categories: { broadcasters: 1, staff: 0, moderators: 4, vips: 5, chatbots: 0, viewers: 90 }, provenance: { transport: "network_response", server_freshness: "unknown", request_after_trigger_verified: true, from_disk_cache: false, from_service_worker: false, from_prefetch_cache: null, served_from_cache_event: false }, ...extra };
}

test("Community starts disabled without browser actions and reports scheduling limits and safe error codes", async () => {
  const app = createApp((target) => target.pathname === "/api/community/status" ? communityState({ selected_channels: 50, delayed_channels: 49, achieved_interval_seconds: 1600, last_error: '<img src=x onerror="injected()">', cleanup_confirmed: false }) : undefined);
  await settle();
  assert.equal(app.get("community-enabled").value, "false");
  assert.equal(app.get("community-interval").value, "300");
  assert.equal(app.get("community-cadence").value, "adaptive");
  assert.equal(app.get("community-interval-label").textContent, "Slow-chat / unknown-count interval (seconds)");
  assert.equal(app.get("community-resume").disabled, true);
  assert.equal(app.calls.some((call) => call.options.method !== "GET"), false);
  assert.match(app.get("community-status").textContent, /Selected channels50Delayed channels49/);
  assert.ok(app.get("community-status").textContent.includes(new Intl.NumberFormat().format(1600) + " seconds"));
  assert.match(app.get("community-status").textContent, /30 seconds minimum between job starts/);
  assert.match(app.get("community-status").textContent, /Browser shutdown confirmationNot confirmed/);
  assert.match(app.get("community-status").textContent, /Current error codeunknown/);
  assert.doesNotMatch(app.get("community-status").textContent, /<img/);
  assert.match(html, /50 selected channels therefore need at least 25 minutes/);
  assert.equal(/type="password"/.test(html), false);
});

test("Community shows the source failure separately from cleanup and safely handles older status", async () => {
  let status = communityState({ paused: true, last_error: "cleanup_unconfirmed",
    primary_failure: { code: "navigation_ui_deadline", stage: "preparing" },
    cleanup_diagnostics: { confirmed: false } });
  const app = createApp((target) => target.pathname === "/api/community/status" ? status : undefined);
  await settle();
  assert.match(app.get("community-status").textContent, /Current error codecleanup_unconfirmed/);
  assert.match(app.get("community-status").textContent, /Retained source diagnostic \(current relevance unconfirmed\)navigation_ui_deadline · preparing/);
  assert.match(app.get("community-status").textContent, /Latest browser cleanup in this runNot confirmed/);
  status = communityState({ primary_failure: { code: '<img src=x>', stage: 'unsafe code' }, cleanup_diagnostics: { confirmed: true } });
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.match(app.get("community-status").textContent, /Retained source diagnostic \(current relevance unconfirmed\)unknown · unknown/);
  assert.match(app.get("community-status").textContent, /Latest browser cleanup in this runConfirmed/);
  assert.doesNotMatch(app.get("community-status").textContent, /<img|unsafe code/);
  status = communityState();
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.doesNotMatch(app.get("community-status").textContent, /source diagnostic|Latest browser cleanup in this run/);
});

test("Community explains saved cleanup and ambiguous legacy pauses without attributing a Twitch denial", async () => {
  let status = communityState({ paused: true, last_error: "cleanup_unconfirmed", persisted_pause_reason: "cleanup_unconfirmed" });
  const app = createApp((target) => target.pathname === "/api/community/status" ? status : undefined);
  await settle();
  assert.match(app.get("community-status").textContent, /Browser cleanup was not confirmed; automatic restart is paused/);
  assert.match(app.get("community-status").textContent, /Saved pause codecleanup_unconfirmed/);
  assert.match(app.get("community-status").textContent, /Browser shutdown confirmationNot reported/);
  assert.doesNotMatch(app.get("community-status").textContent, /Twitch.*denied|Twitch.*reject/i);
  status = communityState({ paused: true, last_error: "source_denied" });
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.match(app.get("community-status").textContent, /A generic source pause was retained; its original cause was not recorded/);
  assert.doesNotMatch(app.get("community-status").textContent, /Browser cleanup was not confirmed|Twitch.*denied|Twitch.*reject/i);
  status = communityState({ paused: true, last_error: "authentication_denied", persisted_pause_reason: "authentication_denied", cleanup_confirmed: false });
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.match(app.get("community-status").textContent, /Authentication checks paused collection/);
  assert.match(app.get("community-status").textContent, /Browser shutdown confirmationNot confirmed/);
});

test("Community separates recovered source diagnostics from current errors and retains safe raw codes", async () => {
  const failure = { code: "verified_player_pause_lost", stage: "waiting_for_response", observed_at_unix_ms: Date.parse(received) - 1000 };
  let status = communityState({ settings: { enabled: true, target_interval_seconds: 300 }, state: "collecting", last_error: failure.code,
    last_received_at_utc: received, primary_failure: failure });
  const app = createApp((target) => target.pathname === "/api/community/status" ? status : undefined);
  await settle();
  assert.match(app.get("community-status").textContent, /Retained source diagnostic \(later receipt succeeded\)verified_player_pause_lost · waiting_for_response/);
  assert.match(app.get("community-status").textContent, /Retained error code \(later receipt succeeded\)verified_player_pause_lost/);
  assert.equal(app.get("community-state").className, "badge good");
  assert.doesNotMatch(app.get("community-status").textContent, /Current source diagnostic/);
  status = communityState({ state: "collecting", last_error: null, last_received_at_utc: received, primary_failure: failure });
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.match(app.get("community-status").textContent, /Current error codeNone/);
  assert.match(app.get("community-status").textContent, /Retained source diagnostic \(later receipt succeeded\)/);
  status = communityState({ paused: true, last_error: failure.code, primary_failure: { ...failure, observed_at_unix_ms: Date.parse(received) + 1000 }, last_received_at_utc: received });
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.match(app.get("community-status").textContent, /Current source diagnosticverified_player_pause_lost/);
  assert.match(app.get("community-status").textContent, /Current error codeverified_player_pause_lost/);
  assert.equal(app.get("community-state").className, "badge warning");
});

test("Community distinguishes browser termination from window minimization without clearing a sign-in pause", async () => {
  let status = communityState({ paused: true, state: "sign_in", window_dismissal: { count: 2 },
    host_termination: { reason: "shutdown_requested", observed_at_unix_ms: Date.parse(received) } });
  const app = createApp((target) => target.pathname === "/api/community/status" ? status : undefined);
  await settle();
  assert.match(app.get("community-state").textContent, /Paused/);
  assert.match(app.get("community-status").textContent, /Browser stop reasonShutdown requested by Veylurk/);
  assert.match(app.get("community-status").textContent, /Helper window dismissals2 · window minimized/);
  assert.doesNotMatch(app.get("community-status").textContent, /Browser stop recordedNot reported/);
  assert.equal(app.calls.some((call) => call.options.method !== "GET"), false);
  status = communityState({ host_termination: { reason: '<img src=x>', observed_at_unix_ms: Number.MAX_SAFE_INTEGER }, window_dismissal: { count: '<img src=x>' } });
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.match(app.get("community-status").textContent, /Unrecognized stop reason/);
  assert.match(app.get("community-status").textContent, /Browser stop recordedNot reported/);
  assert.doesNotMatch(app.get("community-status").textContent, /<img|Helper window dismissals/);
  status = communityState();
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.doesNotMatch(app.get("community-status").textContent, /Browser stop reason|Helper window dismissals/);
  assert.match(html, /Closing that helper window minimizes it/);
});

test("Community settings validate full writes and preserve unsaved values across refreshes", async () => {
  let saved = { enabled: false, target_interval_seconds: 300 };
  const app = createApp((target, options) => {
    if (target.pathname === "/api/community/status") return communityState({ settings: saved });
    if (target.pathname === "/api/community/settings") { saved = JSON.parse(options.body); return saved; }
  });
  await settle();
  app.get("community-enabled").value = "true";
  app.get("community-interval").value = "29";
  await app.get("community-settings-form").dispatch("submit"); await settle();
  assert.match(app.get("community-settings-message").textContent, /30 to 86400/);
  assert.equal(app.calls.some((call) => call.options.method === "PATCH"), false);
  app.get("community-interval").value = "30";
  await app.get("community-settings-form").dispatch("submit"); await settle();
  const patch = app.calls.find((call) => call.target.pathname === "/api/community/settings");
  assert.equal(patch.options.method, "PATCH");
  assert.deepEqual(JSON.parse(patch.options.body), { enabled: true, target_interval_seconds: 30, cadence_mode: "fixed" });
  app.get("community-interval").value = "87";
  await app.get("community-interval").dispatch("input");
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.equal(app.get("community-interval").value, "87");
  assert.match(app.get("community-settings-message").textContent, /Save to apply/);
});

test("legacy Community settings select Fixed and cadence-only changes save the full settings contract", async () => {
  let saved = { enabled: false, target_interval_seconds: 300 };
  const app = createApp((target, options) => {
    if (target.pathname === "/api/community/status") return communityState({ settings: saved });
    if (target.pathname === "/api/community/settings") { saved = JSON.parse(options.body); return saved; }
  });
  await settle();
  assert.equal(app.get("community-cadence").value, "fixed");
  assert.equal(app.get("community-interval-label").textContent, "Interval per channel (seconds)");
  app.get("community-cadence").value = "adaptive";
  await app.get("community-cadence").dispatch("change");
  assert.equal(app.get("community-interval-label").textContent, "Slow-chat / unknown-count interval (seconds)");
  assert.match(app.get("community-settings-message").textContent, /Save to apply/);
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.equal(app.get("community-cadence").value, "adaptive");
  await app.get("community-settings-form").dispatch("submit"); await settle();
  const patch = app.calls.find((call) => call.target.pathname === "/api/community/settings");
  assert.deepEqual(JSON.parse(patch.options.body), { enabled: false, target_interval_seconds: 300, cadence_mode: "adaptive" });
  assert.equal(app.get("community-cadence").value, "adaptive");
  assert.match(app.get("community-settings-message").textContent, /settings saved/);
});

test("cadence edits made during a pending save remain dirty through the queued status refresh", async () => {
  const pending = deferred(), oldStatus = deferred();
  let saved = { enabled: true, target_interval_seconds: 300, cadence_mode: "fixed" }, statusRequests = 0;
  const app = createApp((target, options) => {
    if (target.pathname === "/api/community/status") { statusRequests += 1; return statusRequests === 2 ? oldStatus.promise : communityState({ settings: saved }); }
    if (target.pathname === "/api/community/settings") { saved = JSON.parse(options.body); return pending.promise; }
  });
  await settle();
  const refreshing = app.get("refresh-status").dispatch("click"); await settle();
  app.get("community-cadence").value = "adaptive"; await app.get("community-cadence").dispatch("input");
  const saving = app.get("community-settings-form").dispatch("submit"); await settle();
  app.get("community-cadence").value = "fixed"; await app.get("community-cadence").dispatch("change");
  pending.resolve({ accepted: true }); await saving; await settle();
  oldStatus.resolve(communityState({ settings: { enabled: true, target_interval_seconds: 300, cadence_mode: "fixed" } })); await refreshing; await settle();
  assert.equal(statusRequests, 3);
  assert.equal(app.get("community-cadence").value, "fixed");
  assert.equal(app.get("community-interval-label").textContent, "Interval per channel (seconds)");
  assert.match(app.get("community-settings-message").textContent, /newer changes are not saved yet/);
  assert.equal(JSON.parse(app.calls.find((call) => call.target.pathname === "/api/community/settings").options.body).cadence_mode, "adaptive");
  await app.get("community-settings-form").dispatch("submit"); await settle();
  assert.equal(JSON.parse(app.calls.filter((call) => call.target.pathname === "/api/community/settings").at(-1).options.body).cadence_mode, "fixed");
});

test("per-channel cadence distinguishes calculated targets, measured intervals, response counts and fallback reasons", async () => {
  const hostile = '<img src=x onerror="injected()">';
  const app = createApp((target) => target.pathname === "/api/community/status" ? communityState({
    achieved_interval_seconds: 67.5, last_received_channel: "sample_channel", channels: [
      communityChannel(),
      communityChannel("fixed_channel", { cadence_basis: "fixed", effective_interval_seconds: 300, achieved_interval_seconds: null }),
      communityChannel("new_channel", { cadence_basis: "no_sample", reported_chatters: null, viewer_category_rows: null, last_received_at_utc: null, next_eligible_at_utc: null }),
      communityChannel("missing_count", { cadence_basis: "missing_reported_count", reported_chatters: null }),
      communityChannel("zero_count", { cadence_basis: "zero_reported_count", reported_chatters: 0 }),
      communityChannel("no_category", { cadence_basis: "no_viewer_rows", viewer_category_rows: 0 }),
      communityChannel(hostile, { cadence_basis: "inconsistent_counts", reported_chatters: 1, viewer_category_rows: 100, failures: 2 }),
    ],
  }) : undefined);
  await settle();
  const status = app.get("community-status"), table = status.querySelector(".table-wrap");
  assert.ok(table);
  assert.match(status.textContent, /Latest achieved interval.*seconds · sample_channel/);
  assert.match(table.textContent, /Calculated targetActual interval/);
  assert.match(table.textContent, /60 seconds/);
  assert.ok(table.textContent.includes(new Intl.NumberFormat().format(67.5) + " seconds"));
  assert.match(table.textContent, /100 Viewer-category rows \/ 500 reported chat accounts/);
  for (const reason of ["Fixed interval", "Waiting for the first response", "total chat count was not reported", "reported total is zero", "no accounts in the Viewer category", "response counts do not agree"]) assert.ok(table.textContent.includes(reason));
  assert.match(table.textContent, /Not measured/);
  assert.match(table.textContent, /Not scheduled/);
  assert.match(table.textContent, /Not recorded/);
  assert.match(table.textContent, /<img src=x/);
  assert.equal(descendants(status).some((element) => element.tagName === "IMG"), false);
  assert.doesNotMatch(table.textContent, /missing_reported_count|inconsistent_counts|video viewers|99%/);
});

test("Community status refresh preserves cadence table scroll and focus and renders at most fifty channels", async () => {
  const app = createApp((target) => target.pathname === "/api/community/status" ? communityState({ channels: Array.from({ length: 52 }, (_, index) => communityChannel("channel_" + index)) }) : undefined);
  await settle();
  const first = app.get("community-status").querySelector(".table-wrap");
  first.scrollLeft = 320; first.focus();
  await app.get("refresh-status").dispatch("click"); await settle();
  const next = app.get("community-status").querySelector(".table-wrap");
  assert.equal(next.scrollLeft, 320);
  assert.equal(app.document.activeElement, next);
  assert.equal(descendants(next).filter((element) => element.tagName === "TR").length, 51);
  assert.equal(next.textContent.includes("channel_51"), false);
});

test("legacy Community status leaves missing per-channel cadence and receipt-channel attribution explicit", async () => {
  const app = createApp((target) => target.pathname === "/api/community/status" ? communityState({ settings: { enabled: false, target_interval_seconds: 300 }, channels: undefined, last_received_channel: undefined, achieved_interval_seconds: 100 }) : undefined);
  await settle();
  assert.equal(app.get("community-cadence").value, "fixed");
  assert.match(app.get("community-status").textContent, /100 seconds · channel not reported/);
  assert.match(app.get("community-status").textContent, /Per-channel cadence is not reported by this app version/);
  assert.equal(app.get("community-status").querySelector(".table-wrap"), null);
});

test("the desktop Until control spans the narrow result-size column while smaller layouts keep their columns", () => {
  assert.match(html, /class="field search-until"><label for="search-to"/);
  assert.match(stylesheet, /@media \(min-width: 901px\)\s*\{\s*#search-form \.search-until\s*\{\s*grid-column:\s*3\s*\/\s*-1;/);
  assert.match(stylesheet, /@media \(max-width: 900px\)[\s\S]*?\.search-form\s*\{\s*grid-template-columns: 1fr 1fr;/);
  assert.match(stylesheet, /@media \(max-width: 600px\)[\s\S]*?\.search-form\s*\{\s*grid-template-columns: 1fr;/);
});

test("a Community write supersedes an older status refresh and triggers a current follow-up", async () => {
  const old = deferred();
  let requests = 0, settings = { enabled: false, target_interval_seconds: 300 };
  const app = createApp((target, options) => {
    if (target.pathname === "/api/community/status") { requests += 1; return requests === 2 ? old.promise : communityState({ settings }); }
    if (target.pathname === "/api/community/settings") { settings = JSON.parse(options.body); return { accepted: true }; }
  });
  await settle();
  const refreshing = app.get("refresh-status").dispatch("click"); await settle();
  app.get("community-enabled").value = "true"; app.get("community-interval").value = "45";
  await app.get("community-settings-form").dispatch("submit"); await settle();
  old.resolve(communityState()); await refreshing; await settle();
  assert.equal(requests, 3);
  assert.equal(app.get("community-enabled").value, "true");
  assert.equal(app.get("community-interval").value, "45");
});

test("Community login and resume are separate explicit actions and duplicate clicks are ignored", async () => {
  const pending = deferred();
  let paused = false;
  const app = createApp((target) => {
    if (target.pathname === "/api/community/status") return communityState({ settings: { enabled: true, target_interval_seconds: 300 }, paused, state: paused ? "paused_for_login" : "idle" });
    if (target.pathname === "/api/community/login") { paused = true; return pending.promise; }
    if (target.pathname === "/api/community/resume") { paused = false; return { accepted: true }; }
  });
  await settle();
  const first = app.get("community-login").dispatch("click");
  await app.get("community-login").dispatch("click");
  assert.equal(app.calls.filter((call) => call.target.pathname === "/api/community/login").length, 1);
  pending.resolve({ accepted: true }); await first; await settle();
  assert.match(app.get("community-state").textContent, /Paused/);
  assert.equal(app.get("community-resume").disabled, false);
  await app.get("refresh-status").dispatch("click"); await settle();
  assert.equal(app.calls.some((call) => call.target.pathname === "/api/community/resume"), false);
  await app.get("community-resume").dispatch("click"); await settle();
  const mutations = app.calls.filter((call) => call.options.method === "POST");
  assert.deepEqual(mutations.map((call) => [call.target.pathname, JSON.parse(call.options.body)]), [["/api/community/login", {}], ["/api/community/resume", {}]]);
  assert.equal(app.get("community-state").textContent, "Idle");
});

test("Community actions require acknowledgement and invalid receipt cursors remain incomplete", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/community/login") return {};
    if (target.pathname === "/api/community/receipts") return { rows: [communityReceipt(900)], next_before_member_id: "irc:900" };
  });
  await settle();
  await app.get("community-login").dispatch("click"); await settle();
  assert.match(app.get("community-settings-message").textContent, /did not acknowledge/);
  assert.doesNotMatch(app.get("community-settings-message").textContent, /Sign-in requested/);
  app.get("search-account").value = "community_account"; app.get("search-source").value = "community";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.match(app.get("community-message").textContent, /invalid Community receipt cursor/);
  assert.match(app.get("timeline-results").textContent, /Community evidence is incomplete/);
  assert.doesNotMatch(app.get("timeline-results").textContent, /All returned pages/);
});

test("Community-only accounts render receipt ticks without IRC intervals and use the resolved timeline bounds", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([], { account: "community_account", has_more: true, next_before: 9 });
    if (target.pathname === "/api/community/receipts") return { rows: [communityReceipt(900)], next_before_member_id: null };
  });
  await settle(); app.get("search-account").value = "community_account"; app.get("search-source").value = "community";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.equal(app.get("irc-history").hidden, true);
  assert.equal(app.get("community-history").hidden, false);
  assert.equal(app.calls.some((call) => ["/api/search", "/api/relationships", "/api/history"].includes(call.target.pathname)), false);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-community")).length, 1);
  assert.equal(descendants(timelineChart(app)).some((item) => item.classList.contains("timeline-inference")), false);
  assert.equal(app.get("timeline-next").disabled, true);
  const requests = app.calls.filter((call) => call.target.pathname === "/api/community/receipts");
  assert.equal(requests.length, 2);
  assert.equal(requests[0].target.searchParams.has("from_utc"), false);
  assert.equal(requests[0].target.searchParams.has("to_utc"), false);
  assert.equal(requests[1].target.searchParams.get("from_utc"), timelineFrom);
  assert.equal(requests[1].target.searchParams.get("to_utc"), timelineTo);
  assert.equal(requests[1].target.searchParams.get("limit"), "100");
  assert.match(app.get("community-results").textContent, /Moderator, Viewer/);
  assert.match(app.get("community-results").textContent, /Sample 700 · member 900/);
  assert.match(app.get("community-results").textContent, /Returned category · Moderators4/);
  assert.match(app.get("community-results").textContent, /Server freshnessUnknown/);
});

test("Community ticks use actual receipt times in the half-open axis and never invent boundary receipts", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/community/receipts") return { rows: [communityReceipt(901, { received_at_utc: timelineFrom }), communityReceipt(900), communityReceipt(899, { received_at_utc: timelineTo }), communityReceipt(898, { received_at_utc: "2026-10-23T09:59:59Z" })], next_before_member_id: null };
  });
  await settle(); app.get("search-account").value = "community_account"; app.get("search-source").value = "community";
  await app.get("search-form").dispatch("submit"); await settle();
  const ticks = descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-community"));
  assert.equal(ticks.length, 2);
  assert.equal(Number(ticks[0].attributes.x), 178.2);
  assert.equal(Number(ticks[1].attributes.x), 573.2);
  assert.equal(descendants(timelineChart(app)).some((item) => item.classList.contains("timeline-inference")), false);
});

test("Community and IRC page cursors stay independent and IRC source performs no receipt requests", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/search") return { observations: [event(8)], next_before: 8 };
    if (target.pathname === "/api/community/receipts") return target.searchParams.has("from_utc") ? { rows: [], next_before_member_id: null } : { rows: [communityReceipt(900)], next_before_member_id: target.searchParams.has("before_member_id") ? null : 900 };
  });
  await settle(); app.get("search-account").value = "sample_account"; app.get("search-limit").value = "250";
  await app.get("search-form").dispatch("submit"); await settle();
  await app.get("community-next").dispatch("click"); await settle();
  const page = app.calls.find((call) => call.target.searchParams.has("before_member_id"));
  assert.equal(page.target.searchParams.get("before_member_id"), "900");
  assert.equal(page.target.searchParams.has("before"), false);
  assert.equal(page.target.searchParams.get("limit"), "100");
  assert.equal(app.get("search-page").textContent, "Page 1");
  await app.get("search-next").dispatch("click"); await settle();
  assert.equal(app.calls.filter((call) => call.target.pathname === "/api/search").at(-1).target.searchParams.get("before"), "8");
  const receipts = app.calls.filter((call) => call.target.pathname === "/api/community/receipts").length;
  app.get("search-source").value = "irc";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.equal(app.calls.filter((call) => call.target.pathname === "/api/community/receipts").length, receipts);
  assert.equal(app.get("community-history").hidden, true);
  assert.equal(app.get("irc-history").hidden, false);
});

test("Community receipt pages reject late searches and keep edited filters visibly stale", async () => {
  const old = deferred(), current = deferred();
  const app = createApp((target) => {
    if (target.pathname === "/api/community/receipts" && !target.searchParams.has("from_utc")) return target.searchParams.get("account") === "old_account" ? old.promise : current.promise;
  });
  await settle(); app.get("search-source").value = "community";
  app.get("search-account").value = "old_account"; await app.get("search-form").dispatch("submit");
  app.get("search-account").value = "new_account"; await app.get("search-form").dispatch("submit");
  current.resolve({ rows: [communityReceipt(900, { account: "new_account" })], next_before_member_id: 900 }); await settle();
  old.resolve({ rows: [communityReceipt(800, { account: "old_account" })], next_before_member_id: null }); await settle();
  assert.match(app.get("community-results").textContent, /new_account/);
  assert.doesNotMatch(app.get("community-results").textContent, /old_account/);
  app.get("search-channel").value = "other_channel"; await app.get("search-channel").dispatch("input");
  assert.equal(app.get("community-next").disabled, true);
  assert.equal(app.get("community-results").classList.contains("stale"), true);
  assert.match(app.get("community-message").textContent, /Filters changed/);
});

test("Community timeline load-more keeps the fixed axis, adds channels, and preserves zoom", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([], { account: "community_account" });
    if (target.pathname === "/api/community/receipts") {
      if (!target.searchParams.has("from_utc")) return { rows: [], next_before_member_id: null };
      return target.searchParams.has("before_member_id") ? { rows: [communityReceipt(800, { channel: "second_channel", received_at_utc: "2026-10-23T12:01:00Z" })], next_before_member_id: null } : { rows: [communityReceipt(900)], next_before_member_id: 900 };
    }
  });
  await settle(); app.get("search-account").value = "community_account"; app.get("search-source").value = "community";
  await app.get("search-form").dispatch("submit"); await settle();
  assert.match(app.get("timeline-results").textContent, /chart is incomplete/);
  await timelineButton(app, "Zoom in").dispatch("click");
  const viewport = descendants(app.get("timeline-results")).find((element) => element.id === "timeline-viewport").textContent;
  await timelineButton(app, "Load more Community ticks").dispatch("click"); await settle();
  const page = app.calls.find((call) => call.target.searchParams.has("before_member_id"));
  assert.equal(page.target.searchParams.get("from_utc"), timelineFrom);
  assert.equal(page.target.searchParams.get("to_utc"), timelineTo);
  assert.equal(page.target.searchParams.get("before_member_id"), "900");
  assert.equal(descendants(app.get("timeline-results")).find((element) => element.id === "timeline-viewport").textContent, viewport);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-community")).length, 2);
  assert.match(app.get("timeline-results").textContent, /second_channel/);
  assert.match(app.get("timeline-results").textContent, /All returned pages for this query are loaded/);
});

test("failed Community timeline loading keeps IRC evidence and offers an explicit bounded retry", async () => {
  let failed = true;
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([timelineVisit()]);
    if (target.pathname === "/api/community/receipts" && target.searchParams.has("from_utc")) return failed ? { httpStatus: 500, error: { message: "Community storage unavailable" } } : { rows: [communityReceipt(900)], next_before_member_id: null };
  });
  await searchTimeline(app);
  assert.match(app.get("timeline-results").textContent, /Community timeline receipts unavailable.*Community evidence is incomplete/);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.tagName === "CIRCLE").length, 2);
  failed = false;
  await timelineButton(app, "Retry Community ticks").dispatch("click"); await settle();
  const retry = app.calls.filter((call) => call.target.pathname === "/api/community/receipts").at(-1);
  assert.equal(retry.target.searchParams.get("from_utc"), timelineFrom);
  assert.equal(retry.target.searchParams.get("to_utc"), timelineTo);
  assert.equal(retry.target.searchParams.get("limit"), "100");
  assert.equal(descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-community")).length, 1);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.tagName === "CIRCLE").length, 2);
});

test("Community tick metadata tooltip keeps exact receipt and sample provenance alongside historical titles", async () => {
  const hostile = '<img src=x onerror="injected()">';
  const app = createApp((target) => {
    if (target.pathname === "/api/community/receipts") return { rows: [communityReceipt(900, { account: hostile })], next_before_member_id: null };
    if (target.pathname.endsWith("/metadata/at")) return metadataAt();
  });
  await searchTimeline(app); app.navigate("history");
  const tick = timelineElement(app, "timeline-community");
  await tick.dispatch("focus"); await app.advance(180);
  const hover = timelineElement(app, "timeline-hover");
  assert.match(hover.textContent, /Community source/);
  assert.match(hover.textContent, /sample 700 · member receipt 900 · job 8/);
  assert.match(hover.textContent, /2026-10-23T12:00:00.000Z/);
  assert.match(hover.textContent, /Server freshness unknown/);
  assert.match(hover.textContent, /Last reported title: Stored broadcast title/);
  assert.match(hover.textContent, /<img src=x/);
  assert.equal(descendants(app.get("timeline-results")).some((item) => item.tagName === "IMG"), false);
});

test("timeline zoom preserves IRC and Community evidence disclosure state independently", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([timelineVisit()]);
    if (target.pathname === "/api/community/receipts") return { rows: [communityReceipt(900)], next_before_member_id: null };
  });
  await searchTimeline(app);
  let disclosures = descendants(app.get("timeline-results")).filter((element) => element.tagName === "DETAILS" && element.classList.contains("timeline-details"));
  disclosures[0].open = true; disclosures[1].open = false;
  await timelineButton(app, "Zoom in").dispatch("click");
  disclosures = descendants(app.get("timeline-results")).filter((element) => element.tagName === "DETAILS" && element.classList.contains("timeline-details"));
  assert.equal(disclosures[0].open, true); assert.equal(disclosures[1].open, false);
  disclosures[0].open = false; disclosures[1].open = true;
  await timelineButton(app, "Reset zoom").dispatch("click");
  disclosures = descendants(app.get("timeline-results")).filter((element) => element.tagName === "DETAILS" && element.classList.contains("timeline-details"));
  assert.equal(disclosures[0].open, false); assert.equal(disclosures[1].open, true);
});

test("channel-only Community timelines resolve an axis without creating account intervals", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([], { account: "", channel: "sample_channel" });
    if (target.pathname === "/api/community/receipts") return { rows: [communityReceipt(900)], next_before_member_id: null };
  });
  await settle(); app.get("search-channel").value = "sample_channel"; app.get("search-source").value = "community";
  await app.get("search-form").dispatch("submit"); await settle();
  const request = app.calls.find((call) => call.target.pathname === "/api/timeline");
  assert.equal(request.target.searchParams.has("account"), false);
  assert.equal(request.target.searchParams.get("channel"), "sample_channel");
  assert.equal(descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-community")).length, 1);
  assert.equal(descendants(timelineChart(app)).some((item) => item.classList.contains("timeline-inference")), false);
  assert.match(app.get("timeline-results").textContent, /community_account/);
});

test("Community-only broadcast lookup and timeline receipts use the selected source and clipped range", async () => {
  const clippedFrom = "2026-10-23T11:30:00.000Z", clippedTo = "2026-10-23T12:30:00.001Z";
  const app = createApp((target) => {
    if (target.pathname === "/api/broadcasts") return { broadcasts: [{ id: 9, channel: "sample_channel", title: "Stored broadcast", started_at: clippedFrom, last_live_at: "2026-10-23T12:30:00Z" }], next_before: null };
    if (target.pathname === "/api/timeline") return timeline([], { account: "community_account", effective_range: { from: clippedFrom, to: clippedTo }, broadcast: { id: 9, channel: "sample_channel", started_at: clippedFrom, last_live_at: "2026-10-23T12:30:00Z" } });
    if (target.pathname === "/api/community/receipts") return { rows: [communityReceipt(900)], next_before_member_id: null };
  });
  await settle(); app.get("broadcasts-account").value = "community_account"; app.get("broadcasts-source").value = "community";
  await app.get("broadcasts-form").dispatch("submit"); await settle();
  assert.equal(app.calls.find((call) => call.target.pathname === "/api/broadcasts").target.searchParams.get("source"), "community");
  const open = descendants(app.get("broadcasts-results")).find((item) => item.tagName === "BUTTON" && item.textContent === "View summary / visits / events");
  await open.dispatch("click"); await settle();
  const requests = app.calls.filter((call) => call.target.pathname === "/api/community/receipts");
  assert.equal(requests.length, 2);
  assert.equal(requests[1].target.searchParams.get("broadcast_id"), "9");
  assert.equal(requests[1].target.searchParams.get("from_utc"), clippedFrom);
  assert.equal(requests[1].target.searchParams.get("to_utc"), clippedTo);
  assert.equal(app.get("broadcast-community").hidden, false);
  assert.equal(app.get("broadcast-irc-evidence").hidden, true);
  assert.equal(descendants(timelineChart(app, "broadcast-timeline")).filter((item) => item.classList.contains("timeline-community")).length, 1);
});

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

test("IRC timeline empty evidence remains unknown and IRC channel-only searches require an account", async () => {
  const app = createApp(); await settle();
  app.get("search-source").value = "irc";
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

test("IRC broadcast timeline requires an exact account and requests only the selected broadcast and time bounds", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/broadcasts") return { broadcasts: [{ id: 9, channel: "sample_channel", title: "A title", started_at: timelineFrom, last_live_at: timelineTo }], next_before: null };
    if (target.pathname === "/api/timeline") return timeline([timelineVisit()], { broadcast: { id: 9, channel: "sample_channel", started_at: timelineFrom, last_live_at: timelineTo } });
  });
  await settle();
  app.get("broadcasts-source").value = "irc";
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
  const details = elements.find((item) => item.tagName === "DETAILS" && item.classList.contains("timeline-details"));
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
  app.get("search-source").value = "irc";
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


const modelDefaults = { connected_minutes: 60, disconnected_minutes: 720, detection_scale: 0.5, false_report_ratio: 0.01, lag_seconds: 30, max_gap_seconds: 3600 };
function communityModel(target, extra = {}) {
  const from = target.searchParams.get("from") || timelineFrom, to = target.searchParams.get("to") || timelineTo;
  const start = Date.parse(from), end = Date.parse(to);
  const states = [["estimated_present", 0.999, 0.995, 0.9999], ["estimated_absent", 0.001, 0.0001, 0.005], ["uncertain", 0.5, 0.1, 0.9], ["unknown", null, null, null]];
  const cells = states.map(([classification, presence_probability, sensitivity_min, sensitivity_max], index) => {
    const left = Math.round(start + (end - start) * index / states.length), right = Math.round(start + (end - start) * (index + 1) / states.length);
    return { from: new Date(left).toISOString(), to: new Date(right).toISOString(), evaluated_at: new Date(Math.floor(left + (right - left) / 2)).toISOString(), presence_probability, sensitivity_min, sensitivity_max, classification, unknown_reason: classification === "unknown" ? "outside_support_or_segment_boundary" : null };
  });
  return {
    model_version: "community_presence_v1", calibration_status: "uncalibrated", probability_kind: "model_based_pointwise", inference: "historical_smoother", threshold: 0.99, left_censored: true,
    account: target.searchParams.get("account") || "sample_account", channel: target.searchParams.get("channel") || "sample_channel", broadcast_id: target.searchParams.has("broadcast_id") ? Number(target.searchParams.get("broadcast_id")) : null,
    effective_range: { from, to }, context_range: { from: new Date(start - 6 * 3600000).toISOString(), to }, as_of: to, generated_at: to,
    parameters: Object.fromEntries(Object.entries(modelDefaults).map(([key, value]) => [key, target.searchParams.has(key) ? Number(target.searchParams.get(key)) : value])),
    sensitivity_scenarios: 65, complete: true, status: "ok", sample_counts: { successful_in_context: 20, attempts_in_id_span: 22, eligible_observations: 20, target_positive_receipts: 4, segments: 1, modeled_segments: 1 }, excluded_reason_counts: { not_successful: 2 }, cells, cell_width_seconds: (end - start) / 4000, ...extra,
  };
}
const modelCalls = (app) => app.calls.filter((call) => call.target.pathname === "/api/community/presence");
const modelShapes = (app, prefix = "timeline") => descendants(app.get(prefix + "-results")).filter((item) => item.classList.contains("timeline-model-cell"));
// Decode the painted rectangle subpaths so existing cell-count and clipping checks
// still assert every original display cell, rather than only the grouped DOM count.
const modelCells = (app, prefix = "timeline") => modelShapes(app, prefix).flatMap((shape) => {
  const commands = [...shape.attributes.d.matchAll(/M([^,]+),([^H]+)H([^V]+)V([^H]+)H([^Z]+)Z/g)];
  assert.equal(commands.map((command) => command[0]).join(""), shape.attributes.d);
  return commands.map(([, left, top, right, bottom, close]) => {
    const [x, y, end, lower, closedX] = [left, top, right, bottom, close].map(Number);
    assert.ok([x, y, end, lower, closedX].every(Number.isFinite)); assert.equal(closedX, x);
    return { classList: shape.classList, listeners: shape.listeners, attributes: { ...shape.attributes, x, y, width: end - x, height: lower - y } };
  });
});
const modelControl = (app, key, prefix = "timeline") => descendants(app.get(prefix + "-results")).find((item) => item.id === prefix + "-model-" + key);
async function searchModel(app, channel = "", source = "all") {
  await settle(); app.navigate("history"); app.get("search-account").value = "sample_account"; app.get("search-channel").value = channel; app.get("search-source").value = source;
  await app.get("search-form").dispatch("submit"); await settle();
}

test("Community model uses complete per-channel analysis and distinct midpoint-only lanes alongside raw IRC and ticks", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([timelineVisit()]);
    if (target.pathname === "/api/community/receipts") return { rows: [communityReceipt(900)], next_before_member_id: 900 };
    if (target.pathname === "/api/community/presence") return communityModel(target);
  });
  await searchModel(app, "sample_channel");
  assert.equal(modelCalls(app).length, 1);
  const target = modelCalls(app)[0].target;
  assert.equal(target.searchParams.get("from"), timelineFrom); assert.equal(target.searchParams.get("to"), timelineTo);
  assert.equal(target.searchParams.has("before_member_id"), false); assert.equal(target.searchParams.has("limit"), false);
  assert.equal(modelCells(app).length, 4);
  for (const classification of ["estimated_present", "estimated_absent", "uncertain", "unknown"]) assert.equal(modelCells(app).filter((item) => item.classList.contains(classification)).length, 1);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-inference")).length, 1);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-community")).length, 1);
  const text = app.get("timeline-results").textContent;
  assert.match(text, /Model estimates — uncalibrated/); assert.match(text, /Model-based, not empirically calibrated/);
  assert.match(text, /99% threshold is pointwise/); assert.match(text, /not certainty for a whole display cell, interval, path or broadcast/);
  assert.match(text, /Estimated absent under the model/); assert.match(text, /not a known arrival/); assert.match(text, /not a confidence interval/);
  assert.match(text, /More receipts are retained/); assert.match(text, /processing complete/);
  assert.doesNotMatch(text, /99% confident interval|observed absence is proven|whole.broadcast certainty/i);
});

test("Community model inspects original evaluated points with one row Tab stop and preserves title lookup through clipping and keyboard movement", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit()]) : target.pathname === "/api/community/presence" ? communityModel(target) : target.pathname.endsWith("/metadata/at") ? metadataAt() : undefined);
  await searchModel(app, "sample_channel");
  const inspector = timelineElement(app, "timeline-model-inspector");
  assert.equal(inspector.attributes.tabindex, "0"); inspector.focus(); await app.advance(180);
  let request = app.calls.filter((call) => call.target.pathname.endsWith("/metadata/at")).at(-1);
  assert.equal(request.target.searchParams.get("at"), "2026-10-23T10:30:00.000Z");
  await inspector.dispatch("keydown", { key: "ArrowRight" }); await app.advance(180);
  request = app.calls.filter((call) => call.target.pathname.endsWith("/metadata/at")).at(-1);
  assert.equal(request.target.searchParams.get("at"), "2026-10-23T11:30:00.000Z");
  assert.match(timelineElement(app, "timeline-hover").textContent, /Estimated absent under the model/);
  await timelineButton(app, "Zoom in").dispatch("click");
  const next = timelineElement(app, "timeline-model-inspector"); await next.dispatch("pointerenter", mouseAt(330, 114)); await app.advance(180);
  request = app.calls.filter((call) => call.target.pathname.endsWith("/metadata/at")).at(-1);
  assert.equal(request.target.searchParams.get("at"), "2026-10-23T11:30:00.000Z");
  assert.equal(modelCalls(app).length, 1);
  for (const cell of modelCells(app)) { assert.ok(Number(cell.attributes.x) >= 180); assert.ok(Number(cell.attributes.x) + Number(cell.attributes.width) <= 970.000001); assert.equal(cell.attributes.tabindex, undefined); assert.equal(cell.listeners.size, 0); }
});

test("Community model requests explicit channels without positive receipts inside the displayed range and never models channel-only or IRC-only queries", async () => {
  const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([], { empty: true }) : target.pathname === "/api/community/presence" ? communityModel(target) : undefined);
  await searchModel(app, "sample_channel", "community");
  assert.equal(modelCalls(app).length, 1); assert.equal(modelCells(app).length, 4);
  assert.match(app.get("timeline-results").textContent, /No matching raw evidence/);
  app.get("search-account").value = ""; await app.get("search-form").dispatch("submit"); await settle();
  assert.equal(modelCalls(app).length, 1); assert.match(app.get("timeline-results").textContent, /Enter an exact account login/);
  await searchModel(app, "sample_channel", "irc"); assert.equal(modelCalls(app).length, 1); assert.equal(modelCells(app).length, 0);
});

test("Community model keeps no-data, unsupported, limits, busy, timeout and SQL errors unknown without hiding raw evidence", async () => {
  for (const status of ["no_data", "insufficient_data", "role_unsupported", "input_limit", "analysis_busy", "analysis_timeout", "sql_error"]) {
    const app = createApp((target) => target.pathname === "/api/timeline" ? timeline([timelineVisit()]) : target.pathname === "/api/community/presence" ? status === "sql_error" ? { httpStatus: 503, error: { message: '<img src=x onerror="bad()"> unavailable' } } : communityModel(target, { status, complete: ["no_data", "insufficient_data", "role_unsupported"].includes(status), cells: [], sensitivity_scenarios: 0 }) : undefined);
    await searchModel(app, "sample_channel");
    assert.equal(modelCells(app).length, 0, status); assert.ok(timelineChart(app), status);
    assert.equal(descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-inference")).length, 1);
    assert.match(app.get("timeline-results").textContent, status === "sql_error" ? /Model unavailable: <img src=x/ : /no model probabilities drawn/);
    assert.equal(descendants(app.get("timeline-results")).some((item) => item.tagName === "IMG"), false);
  }
});

test("Community model rejects partial, mismatched, nonfinite, out-of-bounds and incorrectly strong results before drawing any cells", async () => {
  const mutations = [
    (value) => { value.complete = false; }, (value) => { value.channel = "other"; }, (value) => { value.effective_range.to = "2026-10-24T00:00:00Z"; },
    (value) => { value.cells[0].presence_probability = NaN; }, (value) => { value.cells[0].sensitivity_max = 1.01; },
    (value) => { value.cells[0].sensitivity_min = 0.98; }, (value) => { value.cells[1].sensitivity_max = 0.02; },
    (value) => { value.cells[3].presence_probability = 0.5; }, (value) => { value.cells[1].from = value.cells[0].from; },
    (value) => { value.cells[0].evaluated_at = value.cells[0].from; }, (value) => { value.sensitivity_scenarios = 66; },
    (value) => { value.cells = Array(2049).fill(value.cells[0]); }, (value) => { value.parameters.lag_seconds = 31; },
  ];
  for (const mutate of mutations) {
    const app = createApp((target) => { if (target.pathname === "/api/community/presence") { const result = communityModel(target); mutate(result); return result; } });
    await searchModel(app, "sample_channel"); assert.equal(modelCells(app).length, 0); assert.match(app.get("timeline-results").textContent, /invalid or mismatched Community model/);
  }
});

test("Community model overview requests at most eight visible channels sequentially and offers an honest omitted-channel notice", async () => {
  const first = deferred(); let running = 0, maximum = 0;
  const app = createApp(async (target) => {
    if (target.pathname === "/api/timeline") return timeline(Array.from({ length: 10 }, (_, index) => timelineVisit({ id: index + 1, channel: "channel_" + index })));
    if (target.pathname === "/api/community/presence") { running++; maximum = Math.max(maximum, running); if (target.searchParams.get("channel") === "channel_0") await first.promise; running--; return communityModel(target); }
  });
  await searchModel(app);
  assert.equal(modelCalls(app).length, 1); first.resolve(); await settle();
  assert.equal(modelCalls(app).length, 8); assert.equal(maximum, 1);
  assert.match(app.get("timeline-results").textContent, /2 visible channel\(s\) omitted.*eight-channel overview/);
  assert.match(app.get("timeline-results").textContent, /Filter one channel/);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-model-inspector")).length, 8);
});

test("Community model cache pins axis and channel identity across receipt loading and IRC pagination without recomputing completed channels", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([timelineVisit()], { has_more: !target.searchParams.has("before"), next_before: target.searchParams.has("before") ? null : 8 });
    if (target.pathname === "/api/community/receipts") return { rows: [communityReceipt(target.searchParams.has("before_member_id") ? 899 : 900)], next_before_member_id: target.searchParams.has("before_member_id") ? null : 900 };
    if (target.pathname === "/api/community/presence") return communityModel(target);
  });
  await searchModel(app, "sample_channel"); await timelineButton(app, "Zoom in").dispatch("click");
  const viewport = descendants(app.get("timeline-results")).find((item) => item.id === "timeline-viewport").textContent;
  await timelineButton(app, "Load more Community ticks").dispatch("click"); await settle();
  await app.get("timeline-next").dispatch("click"); await settle();
  assert.equal(modelCalls(app).length, 1);
  assert.equal(descendants(app.get("timeline-results")).find((item) => item.id === "timeline-viewport").textContent, viewport);
  assert.equal(modelCells(app).length, 2);
  await searchModel(app, "another_channel"); assert.equal(modelCalls(app).length, 2);
  assert.equal(modelCalls(app).at(-1).target.searchParams.get("channel"), "another_channel");
});

test("Community model assumption edits are stale until applied, accept finite fractions and bounds, and reject late old-parameter replies", async () => {
  const old = deferred(); let oldTarget;
  const app = createApp((target) => {
    if (target.pathname === "/api/community/presence") { if (target.searchParams.get("detection_scale") === "0.5") { oldTarget = target; return old.promise; } return communityModel(target); }
  });
  await searchModel(app, "sample_channel");
  const oldSignal = modelCalls(app)[0].options.signal;
  const scale = modelControl(app, "detection_scale"); scale.value = "0.75"; await scale.dispatch("input");
  assert.equal(oldSignal.aborted, true); assert.equal(modelCells(app).length, 0); assert.match(app.get("timeline-results").textContent, /Assumptions changed/);
  const form = descendants(app.get("timeline-results")).find((item) => item.classList.contains("model-assumptions-form"));
  modelControl(app, "connected_minutes").value = "5.5"; modelControl(app, "lag_seconds").value = "0.25";
  await form.dispatch("submit"); await settle(); assert.equal(modelCalls(app).length, 2); assert.equal(modelCells(app).length, 4);
  assert.equal(modelCalls(app)[1].target.searchParams.get("connected_minutes"), "5.5"); assert.equal(modelCalls(app)[1].target.searchParams.get("lag_seconds"), "0.25");
  old.resolve(communityModel(oldTarget, { generated_at: "2026-10-23T10:00:00Z" })); await settle(); assert.equal(modelCells(app).length, 4);
  for (const [key, invalid] of [["connected_minutes", ""], ["connected_minutes", "4.99"], ["disconnected_minutes", "10081"], ["detection_scale", "Infinity"], ["false_report_ratio", "0"], ["lag_seconds", "301"], ["max_gap_seconds", "59.9"]]) {
    modelControl(app, key).value = invalid; await form.dispatch("submit"); await settle(); assert.equal(modelCalls(app).length, 2); modelControl(app, key).value = String(modelDefaults[key]);
  }
});

test("Community model late responses are rejected after filters, newer searches, navigation or a hidden page", async () => {
  for (const action of ["filters", "new_search", "navigate", "hidden"]) {
    const pending = deferred(); let target;
    const app = createApp((request) => { if (request.pathname === "/api/community/presence") { target = request; return pending.promise; } });
    await searchModel(app, "sample_channel"); const signal = modelCalls(app)[0].options.signal;
    if (action === "filters") { app.get("search-channel").value = "other_channel"; await app.get("search-channel").dispatch("input"); }
    if (action === "new_search") { app.get("search-account").value = "new_account"; await app.get("search-form").dispatch("submit"); await settle(); }
    if (action === "navigate") app.navigate("health");
    if (action === "hidden") app.visibility(true);
    assert.equal(signal.aborted, true, action); pending.resolve(communityModel(target)); await settle();
    if (action !== "new_search") assert.equal(modelCells(app).length, 0, action);
  }
});

test("Community model selected-broadcast request uses the resolved half-open bounds and exact evaluated-point title scope", async () => {
  const from = "2026-10-23T11:30:00.000Z", to = "2026-10-23T12:30:00.001Z";
  const app = createApp((target) => {
    if (target.pathname === "/api/broadcasts") return { broadcasts: [{ id: 9, channel: "sample_channel", title: "Stored broadcast", started_at: from, last_live_at: "2026-10-23T12:30:00Z" }], next_before: null };
    if (target.pathname === "/api/timeline") return timeline([], { effective_range: { from, to }, broadcast: { id: 9, channel: "sample_channel", started_at: from, last_live_at: "2026-10-23T12:30:00Z" } });
    if (target.pathname === "/api/community/presence") return communityModel(target);
    if (target.pathname.endsWith("/metadata/at")) return metadataAt();
  });
  await settle(); app.navigate("broadcasts"); app.get("broadcasts-account").value = "sample_account";
  await app.get("broadcasts-form").dispatch("submit"); await settle();
  const open = descendants(app.get("broadcasts-results")).find((item) => item.tagName === "BUTTON" && item.textContent === "View summary / visits / events"); await open.dispatch("click"); await settle();
  assert.equal(modelCalls(app).length, 1); const target = modelCalls(app)[0].target;
  assert.equal(target.searchParams.get("broadcast_id"), "9"); assert.equal(target.searchParams.has("channel"), false); assert.equal(target.searchParams.get("from"), from); assert.equal(target.searchParams.get("to"), to);
  timelineElement(app, "timeline-model-inspector", "broadcast-timeline").focus(); await app.advance(180);
  const title = app.calls.filter((call) => call.target.pathname.endsWith("/metadata/at")).at(-1);
  assert.equal(title.target.searchParams.get("broadcast_id"), "9"); assert.equal(title.target.searchParams.get("at"), communityModel(target).cells[0].evaluated_at);
});

test("Community model paints 2048 exact cells in four paths with one inspector, using theme tokens and distinct patterns", async () => {
  let returned;
  const app = createApp((target) => {
    if (target.pathname !== "/api/community/presence") return;
    const result = communityModel(target), from = Date.parse(result.effective_range.from), to = Date.parse(result.effective_range.to);
    result.cells = Array.from({ length: 2048 }, (_, index) => { const left = Math.round(from + (to - from) * index / 2048), right = Math.round(from + (to - from) * (index + 1) / 2048); return { ...result.cells[index % 4], from: new Date(left).toISOString(), to: new Date(right).toISOString(), evaluated_at: new Date(Math.floor(left + (right - left) / 2)).toISOString() }; });
    result.cell_width_seconds = (to - from) / 2048000; returned = result; return result;
  });
  await searchModel(app, "sample_channel"); assert.equal(modelCells(app).length, 2048);
  assert.equal(modelShapes(app).length, 4);
  assert.equal(descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-model-inspector")).length, 1);
  assert.ok(modelCells(app).every((item) => item.listeners.size === 0 && item.attributes.tabindex === undefined));
  const painted = modelCells(app).sort((left, right) => left.attributes.x - right.attributes.x);
  for (const [index, cell] of returned.cells.entries()) {
    assert.ok(painted[index].classList.contains(cell.classification));
    assert.ok(Math.abs(painted[index].attributes.x - (180 + 790 * (Date.parse(cell.from) - Date.parse(timelineFrom)) / (Date.parse(timelineTo) - Date.parse(timelineFrom)))) < 1e-9);
  }
  const inspector = timelineElement(app, "timeline-model-inspector");
  for (const key of ["End", "ArrowLeft", "Home"]) {
    await inspector.dispatch("keydown", { key }); await app.advance(180);
    const index = key === "End" ? 2047 : key === "ArrowLeft" ? 2046 : 0;
    assert.equal(app.calls.filter((call) => call.target.pathname.endsWith("/metadata/at")).at(-1).target.searchParams.get("at"), returned.cells[index].evaluated_at);
  }
  await inspector.dispatch("pointermove", mouseAt(180 + 790 * 1024.25 / 2048, 114)); await app.advance(180);
  assert.equal(app.calls.filter((call) => call.target.pathname.endsWith("/metadata/at")).at(-1).target.searchParams.get("at"), returned.cells[1024].evaluated_at);
  const patterns = descendants(timelineChart(app)).filter((item) => item.tagName === "PATTERN"); assert.equal(patterns.length, 5);
  assert.match(stylesheet, /\.model-pattern-background\.estimated_present\s*\{\s*fill: var\(--good\)/);
  assert.match(stylesheet, /\.model-pattern-background\.estimated_absent\s*\{\s*fill: var\(--bad\)/);
  assert.match(stylesheet, /@media \(max-width: 600px\)[\s\S]*?\.model-assumptions-form\s*\{\s*grid-template-columns: minmax\(0, 1fr\)/);
});

test("Community grouped paths preserve clipped microsecond geometry and unknown gaps without reparsing cached cell times", async () => {
  const parsed = new Map();
  class CountingDate extends Date {
    static parse(value) { parsed.set(value, (parsed.get(value) || 0) + 1); return Date.parse(value); }
  }
  const base = Date.parse(timelineFrom), duration = Date.parse(timelineTo) - base;
  const micros = (offset) => new Date(base + Math.floor(offset / 1000)).toISOString().slice(0, -1) + String(offset % 1000).padStart(3, "0") + "Z";
  let original, numeric;
  const app = createApp((target) => {
    if (target.pathname !== "/api/community/presence") return;
    const result = communityModel(target), order = [0, 3, 0, 1, 3, 2, 0];
    numeric = order.map((_, index) => {
      const from = Math.floor(duration * 1000 * index / order.length), to = Math.floor(duration * 1000 * (index + 1) / order.length);
      return { from, to, at: Math.floor(from + (to - from) / 2) };
    });
    result.cells = order.map((state, index) => ({ ...result.cells[state], from: micros(numeric[index].from), to: micros(numeric[index].to), evaluated_at: micros(numeric[index].at) }));
    result.cell_width_seconds = duration / order.length / 1000; original = result.cells; return result;
  }, undefined, CountingDate);
  await searchModel(app, "sample_channel");
  const times = new Set(original.flatMap((point) => [point.from, point.to, point.evaluated_at]));
  const reads = [...times].map((time) => [time, parsed.get(time)]);
  for (const point of original) assert.equal(parsed.get(point.evaluated_at), 1);
  function geometry(from, to) {
    const painted = modelCells(app).sort((left, right) => left.attributes.x - right.attributes.x);
    const expected = numeric.map((point, index) => ({ point, index, left: Math.max(from, base + point.from / 1000), right: Math.min(to, base + point.to / 1000) })).filter((point) => point.right > point.left);
    assert.equal(painted.length, expected.length); assert.ok(modelShapes(app).length <= 4);
    for (const [index, value] of expected.entries()) {
      const cell = painted[index], left = 180 + 790 * (value.left - from) / (to - from), right = 180 + 790 * (value.right - from) / (to - from);
      assert.ok(cell.classList.contains(original[value.index].classification));
      assert.ok(Math.abs(cell.attributes.x - left) < 1e-7); assert.ok(Math.abs(cell.attributes.width - (right - left)) < 1e-7);
      assert.equal(cell.attributes.height, 24); assert.ok(cell.attributes.x >= 180 && cell.attributes.x + cell.attributes.width <= 970.000001);
    }
    return expected;
  }
  geometry(base, base + duration);
  assert.equal(modelCells(app).filter((cell) => cell.classList.contains("estimated_present")).length, 3);
  assert.equal(modelCells(app).filter((cell) => cell.classList.contains("unknown")).length, 2);
  await timelineButton(app, "Zoom in").dispatch("click");
  const from = base + duration / 4, to = base + duration * 3 / 4, visible = geometry(from, to);
  assert.equal(visible.length, 5); // Both viewport edges cut through original cells.
  let inspector = timelineElement(app, "timeline-model-inspector");
  for (const value of visible) {
    const at = value.left + (value.right - value.left) / 2;
    await inspector.dispatch("pointermove", mouseAt(180 + 790 * (at - from) / (to - from), 114)); await app.advance(180);
    const point = original[value.index];
    assert.equal(app.calls.filter((call) => call.target.pathname.endsWith("/metadata/at")).at(-1).target.searchParams.get("at"), point.evaluated_at);
    assert.match(inspector.attributes["aria-label"], new RegExp(point.classification === "unknown" ? "No supported probability" : `P\\(present\\)=${point.presence_probability}`));
    const cursor = timelineElement(app, "timeline-model-midpoint"), evaluated = base + value.point.at / 1000;
    assert.ok(Math.abs(Number(cursor.attributes.x1) - (180 + 790 * (evaluated - from) / (to - from))) < 1e-7);
    assert.equal(cursor.attributes.visibility, evaluated >= from && evaluated < to ? "visible" : "hidden");
  }
  await inspector.dispatch("keydown", { key: "Home" }); await app.advance(180);
  assert.equal(app.calls.filter((call) => call.target.pathname.endsWith("/metadata/at")).at(-1).target.searchParams.get("at"), original[visible[0].index].evaluated_at);
  await timelineButton(app, "Reset zoom").dispatch("click"); geometry(base, base + duration);
  inspector = timelineElement(app, "timeline-model-inspector"); await inspector.dispatch("keydown", { key: "End" }); await app.advance(180);
  assert.equal(app.calls.filter((call) => call.target.pathname.endsWith("/metadata/at")).at(-1).target.searchParams.get("at"), original.at(-1).evaluated_at);
  assert.deepEqual([...times].map((time) => [time, parsed.get(time)]), reads);
  assert.equal(modelCalls(app).length, 1);
});


test("Community model exact probability thresholds and inclusive fractional assumptions remain conditional rather than rounded confidence", async () => {
  const app = createApp((target) => {
    if (target.pathname !== "/api/community/presence") return;
    const model = communityModel(target);
    Object.assign(model.cells[0], { presence_probability: 1, sensitivity_min: 0.99, sensitivity_max: 1 });
    Object.assign(model.cells[1], { presence_probability: 0, sensitivity_min: 0, sensitivity_max: 0.01 });
    Object.assign(model.cells[2], { presence_probability: 0.9899999999, sensitivity_min: 0.98, sensitivity_max: 0.999 });
    return model;
  });
  await searchModel(app, "sample_channel");
  assert.equal(modelCells(app).filter((cell) => cell.classList.contains("estimated_present")).length, 1);
  assert.equal(modelCells(app).filter((cell) => cell.classList.contains("estimated_absent")).length, 1);
  assert.equal(modelCells(app).filter((cell) => cell.classList.contains("uncertain")).length, 1);
  const values = { connected_minutes: 5, disconnected_minutes: 10080, detection_scale: 0.05, false_report_ratio: 0.1, lag_seconds: 0, max_gap_seconds: 21600 };
  for (const [key, value] of Object.entries(values)) modelControl(app, key).value = String(value);
  await descendants(app.get("timeline-results")).find((item) => item.classList.contains("model-assumptions-form")).dispatch("submit"); await settle();
  assert.equal(modelCalls(app).length, 2);
  for (const [key, value] of Object.entries(values)) assert.equal(Number(modelCalls(app).at(-1).target.searchParams.get(key)), value);
  const inspector = timelineElement(app, "timeline-model-inspector"); inspector.focus(); await inspector.dispatch("keydown", { key: "ArrowRight" }); await inspector.dispatch("keydown", { key: "ArrowRight" });
  assert.match(inspector.attributes["aria-label"], /Model uncertain.*P\(present\)=0.9899999999/);
});

test("Community model preserves microsecond evaluated_at in historical-title requests even for a one-millisecond display cell", async () => {
  const from = "2026-10-23T12:00:00.000Z", to = "2026-10-23T12:00:00.001Z", evaluated = "2026-10-23T12:00:00.000500Z";
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([], { effective_range: { from, to } });
    if (target.pathname === "/api/community/presence") return communityModel(target, { cells: [{ from, to, evaluated_at: evaluated, classification: "unknown", presence_probability: null, sensitivity_min: null, sensitivity_max: null, unknown_reason: "outside_support_or_segment_boundary" }], cell_width_seconds: 0.001 });
    if (target.pathname.endsWith("/metadata/at")) return metadataAt();
  });
  await searchModel(app, "sample_channel"); assert.equal(modelCells(app).length, 1);
  timelineElement(app, "timeline-model-inspector").focus(); await app.advance(180);
  assert.equal(app.calls.filter((call) => call.target.pathname.endsWith("/metadata/at")).at(-1).target.searchParams.get("at"), evaluated);
  assert.match(timelineElement(app, "timeline-hover").textContent, /exact evaluation UTC 2026-10-23T12:00:00.000500Z/);
});

test("Community model asynchronous completion preserves raw hover/focus and completed-channel processing details", async () => {
  const second = deferred();
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline([timelineVisit(), timelineVisit({ id: 9, channel: "second_channel" })]);
    if (target.pathname === "/api/community/presence") return target.searchParams.get("channel") === "second_channel" ? second.promise.then(() => communityModel(target)) : communityModel(target);
    if (target.pathname.endsWith("/metadata/at")) return metadataAt();
  });
  await searchModel(app);
  const raw = descendants(timelineChart(app)).find((item) => item.classList.contains("positive-point")); raw.focus(); await app.advance(180);
  const oldGraphic = timelineChart(app), hover = timelineElement(app, "timeline-hover"), text = hover.textContent;
  const details = descendants(app.get("timeline-results")).find((item) => item.modelDetails)?.modelDetails; details.open = true;
  second.resolve(); await settle();
  assert.equal(timelineChart(app), oldGraphic); assert.equal(app.document.activeElement, raw); assert.equal(hover.textContent, text); assert.equal(details.open, true);
  assert.ok(descendants(app.get("timeline-results")).includes(details));
});


test("Community model assumption changes reset the bounded overview cache and remove old-scope processing rows", async () => {
  const app = createApp((target) => {
    if (target.pathname === "/api/timeline") return timeline(Array.from({ length: 8 }, (_, index) => timelineVisit({ id: index + 1, channel: "m_channel_" + index })));
    if (target.pathname === "/api/community/receipts") return { rows: target.searchParams.has("before_member_id") ? Array.from({ length: 8 }, (_, index) => communityReceipt(800 - index, { channel: "a_channel_" + index })) : [communityReceipt(900, { channel: "z_channel" })], next_before_member_id: target.searchParams.has("before_member_id") ? null : 900 };
    if (target.pathname === "/api/community/presence") return communityModel(target);
  });
  await searchModel(app); assert.equal(modelCalls(app).length, 8);
  await timelineButton(app, "Load more Community ticks").dispatch("click"); await settle(); assert.equal(modelCalls(app).length, 8);
  modelControl(app, "detection_scale").value = "0.6"; await modelControl(app, "detection_scale").dispatch("input");
  await descendants(app.get("timeline-results")).find((item) => item.classList.contains("model-assumptions-form")).dispatch("submit"); await settle();
  assert.equal(modelCalls(app).length, 16);
  const rows = descendants(app.get("timeline-results")).filter((item) => item.classList.contains("model-channel-status"));
  assert.equal(rows.length, 8); assert.ok(rows.every((row) => row.textContent.startsWith("a_channel_")));
  assert.equal(descendants(timelineChart(app)).filter((item) => item.classList.contains("timeline-model-inspector")).length, 8);
});
