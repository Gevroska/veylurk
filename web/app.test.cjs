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
  const elements = new Map([...html.matchAll(/<([a-z][a-z0-9]*)\b[^>]*\bid="([^"]+)"/g)].map((match) => [match[2], new Element(match[1])]));
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
    createElementNS(_namespace, tag) { return this.createElement(tag); },
    querySelectorAll(selector) { assert.equal(selector, "[data-section]"); return navigation; },
    addEventListener() {},
  };
  for (const element of elements.values()) element.ownerDocument = document;
  const defaultResponse = (target) => {
    if (target.pathname === "/api/status") return { state: "collecting", started_at: received, storage_path: "local.sqlite", enabled_channels: 1, connected_channels: 1, observation_count: 2, session_count: 1, storage_bytes: 1024 };
    if (target.pathname === "/api/channels") return { channels: [{ login: "sample_channel", tracked: true, enabled: true, active_session_id: 1, last_observed_at: received }] };
    if (target.pathname === "/api/metadata/status") return { configured: false, state: "unavailable", channels: [] };
    if (target.pathname === "/api/settings") return { collection_mode: "all", unknown_policy: "collect" };
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
