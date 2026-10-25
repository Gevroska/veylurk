(() => {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const clock = new Intl.DateTimeFormat("en", { dateStyle: "medium", timeStyle: "medium" });
  const timelineClock = new Intl.DateTimeFormat("en", { dateStyle: "medium", timeStyle: "long" });
  const timelineDay = new Intl.DateTimeFormat("en", { month: "short", day: "numeric" });
  const timelineTime = new Intl.DateTimeFormat("en", { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
  const numbers = new Intl.NumberFormat();
  const pendingChannels = new Set();
  const loginSuggestions = [];
  let channels = [];
  let metadata = null;
  let refreshInProgress = false;
  let channelsGeneration = 0;
  let channelsLoaded = false;
  let healthLoaded = false;
  let healthDiagnostics = null;
  let renderedChannelsSignature = null;
  let searchQuery = null;
  let broadcastQuery = null;
  let settings = null;
  let settingsDirty = false;
  let settingsSaving = false;
  let settingsGeneration = 0;
  let stopping = false;
  let stopped = false;
  let collectorStatus = null;
  let activeTimelineCancel = null;

  function node(tag, text, className) {
    const result = document.createElement(tag);
    if (text !== undefined && text !== null) result.textContent = String(text);
    if (className) result.className = className;
    return result;
  }

  function message(id, text, tone = "") {
    const target = $(id);
    target.textContent = text;
    target.className = "inline-message" + (tone ? " " + tone : "");
  }

  function failure(error) {
    return error instanceof Error ? error.message : "The local app could not complete this request.";
  }

  function timestamp(value) {
    if (!value) return node("span", "Not recorded", "quiet");
    const date = new Date(value);
    if (!Number.isFinite(date.getTime())) return node("span", "Unknown time", "quiet");
    const result = node("time", clock.format(date));
    result.dateTime = date.toISOString();
    result.title = date.toISOString();
    return result;
  }

  function count(value) {
    return Number.isFinite(value) && value >= 0 ? numbers.format(value) : "Unknown";
  }

  function bytes(value) {
    if (!Number.isFinite(value) || value < 0) return "Unknown";
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let size = value;
    let index = 0;
    while (size >= 1024 && index < units.length - 1) { size /= 1024; index += 1; }
    return `${numbers.format(Math.round(size * 10) / 10)} ${units[index]}`;
  }

  function label(value) {
    const text = typeof value === "string" ? value.replaceAll("_", " ") : "unknown";
    return text.charAt(0).toUpperCase() + text.slice(1);
  }

  function badge(text, tone = "neutral") { return node("span", text, "badge " + tone); }

  function button(text, handler, className = "secondary small") {
    const result = node("button", text, className);
    result.type = "button";
    result.addEventListener("click", handler);
    return result;
  }

  function login(raw, optional = false) {
    const result = raw.trim().replace(/^[#@]/, "").toLowerCase();
    if (optional && !result) return "";
    if (!/^[a-z0-9_]{1,25}$/.test(result)) {
      throw new Error("Use an exact login with 1–25 letters, digits, or underscores. Display names and URLs are not accepted.");
    }
    return result;
  }

  function localTime(id, required = false) {
    const raw = $(id).value.trim();
    if (!raw && !required) return "";
    if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?$/.test(raw)) throw new Error("Enter a valid local date and time.");
    const date = new Date(raw);
    if (!Number.isFinite(date.getTime())) throw new Error("Enter a valid local date and time.");
    const [year, month, day, hour, minute, second = 0] = raw.split(/[-T:]/).map(Number);
    if (date.getFullYear() !== year || date.getMonth() !== month - 1 || date.getDate() !== day || date.getHours() !== hour || date.getMinutes() !== minute || date.getSeconds() !== second) throw new Error("This local date and time does not exist. Check the date or daylight-saving transition.");
    return date.toISOString();
  }

  function timeRange(prefix) {
    const from = localTime(prefix + "-from");
    const to = localTime(prefix + "-to");
    if (from && to && Date.parse(to) <= Date.parse(from)) throw new Error("The end time must be later than the start time.");
    return { from, to };
  }

  function url(path, parameters = {}) {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(parameters)) {
      if (value !== null && value !== undefined && value !== "") query.set(key, String(value));
    }
    return path + (query.size ? "?" + query.toString() : "");
  }

  async function api(path, options = {}) {
    const target = new URL(path, location.origin);
    if (target.origin !== location.origin || !target.pathname.startsWith("/api/") ||
        location.protocol !== "http:" || !["localhost", "127.0.0.1", "[::1]"].includes(location.hostname)) {
      throw new Error("Open this interface from the Veylurk local address.");
    }
    const controller = new AbortController();
    const cancel = () => controller.abort();
    if (options.signal) {
      if (options.signal.aborted) controller.abort();
      else options.signal.addEventListener("abort", cancel, { once: true });
    }
    const deadline = setTimeout(() => controller.abort(), 15000);
    try {
      const response = await fetch(target.href, {
        method: options.method || "GET",
        headers: { Accept: "application/json", ...(options.method && options.method !== "GET" ? { "Content-Type": "application/json" } : {}) },
        body: options.body ? JSON.stringify(options.body) : undefined,
        mode: "same-origin", credentials: "same-origin", redirect: "error", cache: "no-store",
        referrerPolicy: "same-origin", signal: controller.signal,
      });
      const data = await response.json().catch(() => null);
      if (!response.ok) {
        const detail = data && data.error && typeof data.error.message === "string" ? data.error.message.slice(0, 1000) : `Request failed (${response.status}).`;
        throw new Error(detail);
      }
      if (!data || typeof data !== "object") throw new Error("The local app returned an unreadable response.");
      return data;
    } catch (error) {
      if (error && error.name === "AbortError") throw new Error("The local app took too long to respond. Try again.");
      if (error instanceof TypeError) throw new Error("Cannot reach the local app. Check that Veylurk is still running, then retry.");
      throw error;
    } finally {
      clearTimeout(deadline);
      if (options.signal) options.signal.removeEventListener("abort", cancel);
    }
  }

  function attachLoginSuggestions(id, kind, section, onSelect) {
    const input = $(id);
    const popup = $(id + "-suggestions");
    const list = $(id + "-listbox");
    const status = $(id + "-suggestion-status");
    let timer = null;
    let controller = null;
    let generation = 0;
    let dismissed = true;
    let requestedPrefix = "";
    let suggestions = [];
    let highlighted = -1;
    const prefix = () => input.value.trim().toLowerCase();
    const current = (request, query) => request === generation && !dismissed &&
      document.activeElement === input && !document.hidden && !$("section-" + section).hidden &&
      !input.disabled && prefix() === query;

    function dismiss() {
      generation += 1;
      dismissed = true;
      requestedPrefix = "";
      clearTimeout(timer); timer = null;
      if (controller) controller.abort();
      controller = null;
      suggestions = [];
      highlighted = -1;
      popup.hidden = true;
      list.replaceChildren();
      status.textContent = "";
      input.setAttribute("aria-expanded", "false");
      input.removeAttribute("aria-activedescendant");
    }

    function select(index) {
      if (!current(generation, requestedPrefix) || popup.hidden || !suggestions[index]) return;
      input.value = suggestions[index];
      dismiss();
      onSelect();
    }

    function render(text, values = []) {
      suggestions = values;
      highlighted = -1;
      input.removeAttribute("aria-activedescendant");
      list.replaceChildren(...values.map((value, index) => {
        const option = node("li", value, "login-suggestion");
        option.id = id + "-suggestion-" + index;
        option.setAttribute("role", "option");
        option.setAttribute("aria-selected", "false");
        // Keep input focus for keyboard and assistive-technology navigation.
        option.addEventListener("pointerdown", (event) => event.preventDefault());
        option.addEventListener("click", () => select(index));
        return option;
      }));
      status.textContent = text;
      popup.hidden = false;
      input.setAttribute("aria-expanded", "true");
    }

    function schedule() {
      dismiss();
      const query = prefix();
      if (!/^[a-z0-9_]{1,25}$/.test(query) || document.activeElement !== input ||
          document.hidden || $("section-" + section).hidden || input.disabled) return;
      dismissed = false;
      requestedPrefix = query;
      const request = generation;
      timer = setTimeout(async () => {
        timer = null;
        if (!current(request, query)) return;
        const pending = new AbortController();
        controller = pending;
        render("Looking for stored " + (kind === "account" ? "accounts" : "channels") + "…");
        try {
          const data = await api(url("/api/suggestions", { kind, prefix: query, limit: 8 }), { signal: pending.signal });
          if (!current(request, query)) return;
          const values = rows(data, "suggestions");
          if (data.kind !== kind || values.length > 8 || values.some((value, index) =>
            typeof value !== "string" || !/^[a-z0-9_]{1,25}$/.test(value) ||
            !value.startsWith(query) || (index > 0 && values[index - 1] >= value))) {
            throw new Error("Unexpected login suggestions.");
          }
          render(values.length ? `${values.length} stored ${kind}${values.length === 1 ? "" : "s"}. Use the arrow keys to choose.` : `No stored ${kind}s match. You can still enter an exact login.`, values);
        } catch (_) {
          if (current(request, query)) render("Suggestions unavailable. You can still enter an exact login.");
        } finally { if (controller === pending) controller = null; }
      }, 180);
    }

    input.addEventListener("input", schedule);
    input.addEventListener("focus", schedule);
    input.addEventListener("blur", dismiss);
    input.addEventListener("keydown", (event) => {
      if (event.isComposing) return;
      if (event.key === "Escape") {
        if (!dismissed) event.preventDefault();
        dismiss();
      } else if (event.key === "Tab") dismiss();
      else if ((event.key === "ArrowDown" || event.key === "ArrowUp") && !popup.hidden && suggestions.length) {
        event.preventDefault();
        const direction = event.key === "ArrowDown" ? 1 : -1;
        highlighted = highlighted < 0 ? (direction > 0 ? 0 : suggestions.length - 1) :
          (highlighted + direction + suggestions.length) % suggestions.length;
        for (let index = 0; index < list.children.length; index += 1) list.children[index].setAttribute("aria-selected", String(index === highlighted));
        const option = list.children[highlighted];
        input.setAttribute("aria-activedescendant", option.id);
        option.scrollIntoView({ block: "nearest" });
      } else if (event.key === "Enter" && highlighted >= 0 && !popup.hidden) {
        event.preventDefault();
        select(highlighted);
      }
    });
    loginSuggestions.push({ section, dismiss });
  }

  function dismissLoginSuggestions(section) {
    for (const suggestions of loginSuggestions) if (!section || suggestions.section === section) suggestions.dismiss();
  }

  function rows(data, key) {
    if (!Array.isArray(data[key])) throw new Error("The local app returned an unexpected result format.");
    return data[key];
  }

  function table(headers, data, makeRow, caption) {
    const wrapper = node("div", null, "table-wrap");
    wrapper.tabIndex = 0;
    wrapper.setAttribute("role", "region");
    wrapper.setAttribute("aria-label", caption || "Scrollable results table");
    const result = node("table");
    if (caption) result.append(node("caption", caption));
    const head = node("thead");
    const headings = node("tr");
    for (const heading of headers) {
      const cell = node("th", heading);
      cell.scope = "col";
      headings.append(cell);
    }
    head.append(headings);
    const body = node("tbody");
    for (const item of data) body.append(makeRow(item));
    result.append(head, body);
    wrapper.append(result);
    return wrapper;
  }

  function cell(content, className) {
    const result = node("td", null, className);
    if (content instanceof Node) result.append(content);
    else if (content !== undefined && content !== null) result.textContent = String(content);
    return result;
  }

  function empty(text) { return node("p", text, "empty"); }

  function navigate(section) {
    if (location.hash !== "#" + section) location.hash = section;
    showSection();
  }

  function showSection() {
    if (activeTimelineCancel) activeTimelineCancel();
    const requested = location.hash.slice(1);
    const section = ["channels", "history", "broadcasts", "health"].includes(requested) ? requested : "channels";
    for (const suggestions of loginSuggestions) if (suggestions.section !== section) suggestions.dismiss();
    for (const name of ["channels", "history", "broadcasts", "health"]) $("section-" + name).hidden = name !== section;
    for (const link of document.querySelectorAll("[data-section]")) {
      if (link.dataset.section === section) link.setAttribute("aria-current", "page");
      else link.removeAttribute("aria-current");
    }
  }

  function openHistory(account, channel) {
    $("search-account").value = account || "";
    $("search-channel").value = channel || "";
    navigate("history");
    startSearch();
    $("history-title").focus({ preventScroll: true });
    $("section-history").scrollIntoView({ block: "start" });
  }

  function historyLink(text, account, channel) {
    return button(text, () => openHistory(account, channel), "link-button");
  }

  function renderChannels() {
    const rooms = collectorStatus && Array.isArray(collectorStatus.rooms) ? collectorStatus.rooms : [];
    const receiptFor = (channel) => { const room = rooms.find((item) => item.channel === channel.login); return room ? room.last_positive_at : channel.last_observed_at; };
    const signature = JSON.stringify([channels, rooms, channels.map((channel) => receiptFor(channel) && Date.now() - Date.parse(receiptFor(channel)) >= 300000), metadata && metadata.channels, [...pendingChannels].sort(), settings, stopping]);
    if (signature === renderedChannelsSignature) return;
    renderedChannelsSignature = signature;
    const container = $("channels-results");
    const focused = document.activeElement;
    const focusKey = focused && container.contains(focused) ? focused.dataset.channelAction : null;
    const scrollLeft = container.querySelector(".table-wrap")?.scrollLeft || 0;
    const tracked = channels.filter((channel) => channel.tracked !== false);
    $("channel-count").textContent = `${count(tracked.length)} tracked · ${count(channels.length - tracked.length)} history only`;
    if (!channels.length) {
      $("channels-results").replaceChildren(empty("No channels yet. Add channel logins above to start collecting chat evidence."));
      return;
    }
    const ordered = [...channels].sort((a, b) => Number(b.tracked !== false) - Number(a.tracked !== false) || String(a.login).localeCompare(String(b.login)));
    const content = table(["Channel", "Broadcast", "Tracking / evidence quality", "Latest receipt", "Actions"], ordered, (channel) => {
      const row = node("tr");
      const historical = channel.tracked === false;
      const room = rooms.find((item) => item.channel === channel.login);
      const collectionState = room ? room.state : channel.collection_state;
      const collection = new Map([
        ["joining", ["Joining chat", "warning"]],
        ["collecting", ["Collecting chat", "good"]],
        ["unavailable", ["Membership unavailable", "bad"]],
        ["policy_paused", ["Paused while offline", "neutral"]],
        ["gap", ["Coverage gap", "warning"]],
        ["stopped", ["Stopped", "neutral"]],
      ]).get(collectionState);
      const state = node("div");
      state.append(historical ? badge("History only") : !channel.enabled ? badge("Paused", "warning") : collection ? badge(...collection) : channel.active_session_id != null ? badge("Session open", "good") : badge("Waiting for session", "warning"));
      if (!historical && channel.enabled && (room && room.last_error || channel.collection_error)) state.append(node("span", room && room.last_error || channel.collection_error, "secondary-line"));
      const report = metadata && Array.isArray(metadata.channels) ? metadata.channels.find((entry) => entry.channel === channel.login) : null;
      const live = report && !report.stale && ["live", "offline"].includes(report.state) ? report.state : "unknown";
      const policyPaused = !historical && channel.enabled && settings && settings.collection_mode === "live_only" && live === "offline";
      if (policyPaused) state.replaceChildren(badge("Paused while offline"));
      const liveState = node("div");
      liveState.append(badge(label(live), live === "live" ? "good" : "neutral"));
      if (report && report.checked_at) liveState.append(node("span", `${report.stale ? "Stale report" : "Checked"}: ${timestamp(report.checked_at).textContent}`, "secondary-line"));
      if (live === "unknown" && settings && settings.collection_mode === "live_only" && channel.enabled) liveState.append(node("span", "Unknown: chat observation continues", "secondary-line"));
      const evidenceReceipt = receiptFor(channel);
      const receiptAge = evidenceReceipt ? Date.now() - Date.parse(evidenceReceipt) : NaN;
      const quality = policyPaused ? "Current offline metadata pauses chat observation; metadata checks continue." : historical || !channel.enabled ? "Collection inactive; retained evidence only." : collectionState === "unavailable" ? "Membership evidence unavailable; completeness unknown." : collectionState === "gap" ? "Recorded collection gap; continuity is unknown." : !Number.isFinite(receiptAge) ? "No recent positive receipts; coverage unknown. Silence is inconclusive." : receiptAge >= 300000 ? "No recent positive receipts; silence is inconclusive. Completeness remains unknown." : "Recent receipt; membership completeness remains unknown.";
      state.append(node("span", quality, "secondary-line"));
      if (room && room.last_positive_at) state.append(node("span", `Latest positive receipt: ${timestamp(room.last_positive_at).textContent}`, "secondary-line"));
      const control = (text, action, handler, className = "secondary small") => {
        const result = button(text, handler, className);
        result.dataset.channelAction = JSON.stringify([channel.login, action]);
        if ((pendingChannels.has(channel.login) || stopping) && !["history", "login"].includes(action)) result.setAttribute("aria-disabled", "true");
        return result;
      };
      const actions = node("div", null, "actions-inner");
      actions.append(control("History", "history", () => openHistory("", channel.login)));
      if (historical) {
        actions.append(control("Track again", "tracking", () => mutateChannel(channel.login, "track")));
      } else {
        actions.append(control(channel.enabled ? "Pause" : "Resume", "toggle", () => mutateChannel(channel.login, channel.enabled ? "pause" : "resume")));
        const remove = control("Remove tracking", "tracking", () => mutateChannel(channel.login, "remove"), "secondary small danger");
        remove.title = "Stop tracking this channel and retain its stored history";
        actions.append(remove);
      }
      row.append(cell(control(channel.login, "login", () => openHistory("", channel.login), "link-button")), cell(liveState), cell(state), cell(timestamp(channel.last_observed_at), "time"), cell(actions, "actions"));
      return row;
    }, "Channels configured for collection and channels with retained history");
    content.dataset.channelAction = "table";
    container.replaceChildren(content);
    content.scrollLeft = scrollLeft;
    if (focusKey) {
      const replacement = [...container.querySelectorAll("[data-channel-action]")].find((element) => element.dataset.channelAction === focusKey);
      (replacement || content).focus({ preventScroll: true });
    }
  }

  async function updateChannels() {
    const generation = ++channelsGeneration;
    const data = await api("/api/channels");
    if (generation !== channelsGeneration) return;
    channels = rows(data, "channels");
    channelsLoaded = true;
    renderChannels();
    $("channels-results").classList.remove("stale");
  }

  async function mutateChannel(name, action) {
    if (pendingChannels.has(name) || stopping) return;
    pendingChannels.add(name);
    renderChannels();
    message("channel-message", `Updating ${name}…`);
    try {
      if (action === "track") await api("/api/channels", { method: "POST", body: { login: name, enabled: true } });
      else await api("/api/channels/" + encodeURIComponent(name), { method: action === "remove" ? "DELETE" : "PATCH", body: action === "remove" ? undefined : { enabled: action === "resume" } });
      const result = { track: "Tracking requested", pause: "Collection paused", resume: "Collection resumed", remove: "Tracking removed; stored history retained" }[action];
      message("channel-message", `${name}: ${result}.`, "success");
      try { await updateChannels(); } catch (error) { message("channel-message", `${name}: ${result}. Refresh failed: ${failure(error)}`, "error"); }
      refreshStatus();
    } catch (error) { message("channel-message", failure(error), "error"); }
    finally { pendingChannels.delete(name); renderChannels(); }
  }

  async function addChannels(event) {
    event.preventDefault();
    if ($("add-channels").disabled || stopping) return;
    const original = $("channel-logins").value;
    let names;
    try {
      const tokens = original.trim().split(/[\s,;]+/).filter(Boolean);
      if (!tokens.length) throw new Error("Enter at least one channel login.");
      names = [...new Set(tokens.map((value) => login(value)))];
    } catch (error) { message("channel-message", failure(error), "error"); return; }
    $("add-channels").disabled = true;
    message("channel-message", `Adding ${count(names.length)} channel${names.length === 1 ? "" : "s"}…`);
    try {
      await api("/api/channels", { method: "POST", body: { logins: names, enabled: true } });
      if ($("channel-logins").value === original) $("channel-logins").value = "";
      message("channel-message", `${count(names.length)} unique channel${names.length === 1 ? "" : "s"} submitted for tracking.`, "success");
      try { await updateChannels(); } catch (error) { message("channel-message", "Channels saved. Refresh failed: " + failure(error), "error"); }
      refreshStatus();
    } catch (error) { message("channel-message", failure(error), "error"); }
    finally { $("add-channels").disabled = false; }
  }

  function metric(title, value, detail) {
    const result = node("div", null, "card metric");
    result.append(node("h3", title), node("div", value, "metric-value"));
    if (detail) result.append(node("p", detail));
    return result;
  }

  function detail(list, title, value) {
    list.append(node("dt", title));
    const description = node("dd");
    if (value instanceof Node) description.append(value);
    else description.textContent = value == null || value === "" ? "Not recorded" : String(value);
    list.append(description);
  }

  function diagnosticDetails(diagnostics) {
    if (!diagnostics || typeof diagnostics !== "object") return null;
    const panel = node("details", null, "card");
    panel.append(node("summary", "Local fault diagnostics"));
    panel.append(node("p", "Fault records describe program errors. They do not establish that missing observations were recovered.", "quiet"));
    const persistence = diagnostics.persistence || {};
    const fields = node("dl", null, "detail-list");
    detail(fields, "Diagnostic file status", label(persistence.state || "unknown"));
    detail(fields, "Diagnostic write failures", count(persistence.failure_count));
    detail(fields, "Diagnostic records not queued", count(persistence.dropped_count));
    panel.append(fields);
    const first = diagnostics.first_failure || diagnostics.latest;
    if (!first) panel.append(node("p", "No fault recorded in this application run."));
    const records = first ? [[diagnostics.first_failure ? "First storage failure" : "Latest recorded fault", first]] : [];
    if (diagnostics.latest && diagnostics.latest.sequence !== first?.sequence) records.push(["Latest recorded fault", diagnostics.latest]);
    for (const [heading, record] of records) {
      const fault = node("dl", null, "detail-list");
      panel.append(node("h3", heading));
      detail(fault, "Recorded", timestamp(record.occurred_at_utc));
      detail(fault, "Operation", label(record.operation));
      detail(fault, "Execution policy", record.job_class == null ? "Not recorded" : label(record.job_class));
      detail(fault, "Category", label(record.category));
      detail(fault, "Elapsed milliseconds", count(record.elapsed_ms));
      detail(fault, "SQLite primary code", record.sqlite_primary_code);
      detail(fault, "SQLite extended code", record.sqlite_extended_code);
      detail(fault, "Query budget interruption requested", record.progress_interrupt_requested === true ? "Yes" : record.progress_interrupt_requested === false ? "No" : "Unknown");
      detail(fault, "SQLite transaction active after operation", record.autocommit_after === true ? "No" : record.autocommit_after === false ? "Yes" : "Unknown");
      panel.append(fault);
    }
    return panel;
  }

  function renderHealth(status) {
    const diagnosticsOpen = healthDiagnostics?.open === true;
    const diagnosticsFocused = healthDiagnostics && healthDiagnostics.contains(document.activeElement);
    collectorStatus = status;
    const tone = ["collecting", "connected", "running"].includes(status.state) ? "good" : ["error", "failed"].includes(status.state) ? "bad" : "warning";
    $("connection-status").textContent = label(status.state);
    $("connection-status").className = "badge " + tone;
    if (stopping) { $("connection-status").textContent = "Stopping"; $("connection-status").className = "badge warning"; }
    const grid = node("div", null, "health-grid");
    const connected = Array.isArray(status.connected_channels) ? status.connected_channels.length : status.connected_channels;
    const enabled = Array.isArray(status.enabled_channels) ? status.enabled_channels.length : status.enabled_channels;
    const readiness = Number.isFinite(connected) && Number.isFinite(enabled) && connected < enabled ? `${count(enabled)} enabled; some channels are joining or unavailable. Check Channels for details.` : `${count(enabled)} enabled for tracking`;
    grid.append(metric("Collector", label(status.state), "Chat collection status"), metric("Connected channels", count(connected), readiness), metric("Local storage", status.storage_health === "unavailable" ? "Unavailable" : bytes(status.storage_bytes), "SQLite database and reported storage"), metric("Stored events", count(status.observation_count), "Received protocol evidence"), metric("Collection sessions", count(status.session_count), "Sessions do not establish continuous presence"), metric("Retention", "Indefinite", "History survives pause and removal"));
    const details = node("dl", null, "card detail-list");
    detail(details, "App started", timestamp(status.started_at));
    detail(details, "Active session", status.active_session_id == null ? "None" : status.active_session_id);
    detail(details, "Database location", status.storage_path || "Unknown");
    detail(details, "Storage health", badge(label(status.storage_health), status.storage_health === "unavailable" ? "bad" : status.storage_health === "available" ? "good" : "neutral"));
    detail(details, "Latest collector error", status.last_error || "None reported");
    detail(details, "Evidence scope", status.evidence_scope || "Chat membership observations only; completeness unknown.");
    detail(details, "Available disk space", bytes(status.disk_available_bytes));
    $("storage-warning").hidden = !status.storage_warning;
    $("storage-warning").textContent = status.storage_warning || "";
    $("health-results").replaceChildren(grid, details);
    const diagnostics = diagnosticDetails(status.diagnostics);
    healthDiagnostics = diagnostics;
    if (diagnostics) {
      diagnostics.open = diagnosticsOpen;
      $("health-results").append(diagnostics);
      if (diagnosticsFocused) diagnostics.children[0].focus({ preventScroll: true });
    }
    healthLoaded = true;
    $("health-results").classList.remove("stale");
    $("status-updated").textContent = "Updated " + clock.format(new Date());
  }

  function renderMetadata() {
    const available = metadata && metadata.configured;
    $("metadata-state").textContent = available ? label(metadata.state) : "Unknown";
    $("metadata-state").className = "badge " + (available && metadata.state === "ready" ? "good" : "neutral");
    $("metadata-message").textContent = !metadata ? "Unknown — metadata status could not be loaded. Chat evidence remains available." : !available ? "Unknown — broadcast metadata is not configured." : metadata.state === "error" ? "Metadata check failed. " + (metadata.last_error || "Current broadcast state is unknown.") : metadata.last_check_at ? "Latest metadata check: " + timestamp(metadata.last_check_at).textContent + "." : "Waiting for the first metadata check. Current broadcast state is unknown.";
    const item = metadata && Array.isArray(metadata.channels) && searchQuery ? metadata.channels.find((entry) => entry.channel === searchQuery.channel) : null;
    const current = item && !item.stale && ["live", "offline"].includes(item.state) ? item.state : "unknown";
    const reportedState = (value) => value === "offline" ? "Not reported live" : label(value);
    $("channel-broadcast-state").textContent = reportedState(current);
    $("channel-broadcast-state").className = "badge " + (current === "live" ? "good" : "neutral");
    if (item && current !== "unknown") $("broadcast-message").textContent = `${reportedState(current)} according to Twitch metadata checked at ${timestamp(item.checked_at).textContent}. This status comes from metadata, not the chat connection.`;
    else if (item && item.stale && item.last_reported_state) $("broadcast-message").textContent = `Unknown now. The last report was ${reportedState(item.last_reported_state).toLowerCase()} at ${timestamp(item.checked_at).textContent}; that report is stale.`;
    else $("broadcast-message").textContent = "Unknown — no current broadcast metadata is available for this channel. Chat connections cannot establish whether a channel is live.";
  }

  async function refreshStatus(force = false) {
    if (stopped && !force) return;
    if (refreshInProgress) return;
    refreshInProgress = true;
    $("refresh-status").setAttribute("aria-disabled", "true");
    $("refresh-status").setAttribute("aria-busy", "true");
    const policyGeneration = settingsGeneration;
    const results = await Promise.allSettled([api("/api/status"), updateChannels(), api("/api/metadata/status"), api("/api/settings")]);
    const errors = [];
    if (results[0].status === "fulfilled") {
      if (stopped) {
        stopped = false; stopping = false;
        $("stopped-message").hidden = true;
        $("stop-app").disabled = false; $("confirm-stop").disabled = false; $("cancel-stop").disabled = false;
        $("add-channels").disabled = false; $("save-settings").disabled = settingsSaving;
        message("stop-message", "The local app is reachable again. Collection status is shown above.");
      }
      renderHealth(results[0].value);
    }
    else {
      collectorStatus = null;
      if (stopping) {
        stopped = true;
        $("connection-status").textContent = "App unreachable after stop request";
        $("connection-status").className = "badge neutral";
        $("stopped-message").textContent = "Veylurk is no longer reachable after the stop request. Restart the app to resume collection. Stored history is retained.";
      } else {
      $("connection-status").textContent = "Status unavailable";
      $("connection-status").className = "badge bad";
      }
      if (!healthLoaded) $("health-results").replaceChildren(empty("Collection status is unavailable. Use Refresh status to retry."));
      $("health-results").classList.add("stale");
      errors.push("Collection status: " + failure(results[0].reason));
    }
    if (results[1].status === "rejected") {
      if (!channelsLoaded) $("channels-results").replaceChildren(empty("Channel list is unavailable. Use Refresh status to retry."));
      $("channels-results").classList.add("stale");
      errors.push("Channel list: " + failure(results[1].reason));
    }
    metadata = results[2].status === "fulfilled" ? results[2].value : null;
    renderMetadata();
    if (results[3].status === "fulfilled") {
      if (policyGeneration === settingsGeneration) {
        settings = results[3].value;
        if (!settingsDirty && !settingsSaving) $("collection-mode").value = settings.collection_mode === "live_only" ? "live_only" : "all";
      }
    } else if (!settings) message("settings-message", "Collection policy is unavailable. " + failure(results[3].reason), "error");
    renderChannels();
    $("app-message").hidden = !errors.length || stopping;
    $("app-message").textContent = errors.length ? errors.join(" ") + " Previously loaded data may be stale. Use Refresh status to retry." : "";
    $("app-message").className = "notice error";
    $("channels-results").setAttribute("aria-busy", "false");
    $("health-results").setAttribute("aria-busy", "false");
    $("refresh-status").removeAttribute("aria-disabled");
    $("refresh-status").setAttribute("aria-busy", "false");
    refreshInProgress = false;
  }

  // Each view owns its cursor and generation so late replies cannot replace a newer search.
  function pagedView(prefix, fetchPage, renderPage, offsetMode = false) {
    const state = { query: null, positions: [offsetMode ? 0 : null], index: 0, next: null, generation: 0, loading: false, loaded: false, dirty: false, cancelPending: null };
    const result = $(prefix + "-results");
    const pagination = $(prefix + "-pagination");
    const previous = $(prefix + "-previous");
    const next = $(prefix + "-next");
    function controls() {
      pagination.hidden = !state.loaded;
      previous.disabled = state.loading || state.dirty || state.index === 0;
      next.disabled = state.loading || state.dirty || state.next === null;
      $(prefix + "-page").textContent = `Page ${state.index + 1}`;
    }
    async function load(index, position) {
      if (state.cancelPending) state.cancelPending();
      const generation = ++state.generation;
      const query = state.query;
      state.loading = true;
      result.setAttribute("aria-busy", "true");
      result.classList.toggle("stale", state.loaded);
      if (!state.loaded) result.replaceChildren(empty("Loading…"));
      message(prefix + "-message", "Loading…");
      controls();
      try {
        const data = await fetchPage(query, position);
        if (generation !== state.generation) return;
        const content = renderPage(data, query);
        state.cancelPending = typeof content.cancelPending === "function" ? content.cancelPending : null;
        const candidate = offsetMode ? (data.has_more ? data.next_offset : null) : data.next_before;
        state.next = Number.isSafeInteger(candidate) && candidate >= 0 && (position === null || candidate !== position) ? candidate : null;
        state.index = index;
        state.positions[index] = position;
        state.loaded = true;
        result.replaceChildren(content);
        result.classList.toggle("stale", state.dirty);
        message(prefix + "-message", state.dirty ? "Filters changed. Submit the search to update these results." : "Loaded " + clock.format(new Date()) + ". Results do not update automatically.", state.dirty ? "warning" : "");
      } catch (error) {
        if (generation !== state.generation) return;
        if (!state.loaded) result.replaceChildren(empty("Results unavailable. Retry the search."));
        message(prefix + "-message", failure(error) + (state.loaded ? " The previous page remains below; it may be stale." : ""), "error");
      } finally {
        if (generation === state.generation) {
          state.loading = false;
          result.setAttribute("aria-busy", "false");
          controls();
        }
      }
    }
    previous.addEventListener("click", () => { if (!state.loading && !state.dirty && state.index > 0) load(state.index - 1, state.positions[state.index - 1]); });
    next.addEventListener("click", () => { if (!state.loading && !state.dirty && state.next !== null) load(state.index + 1, state.next); });
    return {
      reset(query) {
        state.query = { ...query };
        state.positions = [offsetMode ? 0 : null];
        state.index = 0; state.next = null; state.loaded = false; state.dirty = false;
        return load(0, state.positions[0]);
      },
      clear() {
        if (state.cancelPending) state.cancelPending();
        state.cancelPending = null;
        state.generation += 1; state.query = null; state.loaded = false; state.loading = false;
        state.next = null; state.index = 0; state.dirty = false;
        result.replaceChildren(); result.setAttribute("aria-busy", "false"); result.classList.remove("stale");
        message(prefix + "-message", ""); controls();
      },
      markStale() { if (state.query) { if (state.cancelPending) state.cancelPending(); state.dirty = true; result.classList.add("stale"); message(prefix + "-message", "Filters changed. Submit the search to update these results.", "warning"); controls(); } },
    };
  }

  const eventMeaning = {
    names: "Account listed in a received membership response; the list may be partial or delayed.",
    join: "Chat membership JOIN received; not a verified arrival time.",
    part: "Chat membership PART received; not a verified departure time.",
    names_end: "Membership response terminator; does not prove a complete list or an empty channel.",
  };

  function eventTable(data) {
    const events = rows(data, "observations");
    if (!events.length) return empty("No matching events are stored. This does not prove the account was absent or the channel was empty.");
    return table(["Received locally", "Channel", "Account", "Event", "Evidence"], events, (event) => {
      const row = node("tr");
      const explanation = node("details", null, "evidence-details");
      explanation.append(node("summary", "Details"));
      const list = node("dl");
      detail(list, "Meaning", eventMeaning[event.kind] || "Unrecognized stored protocol event.");
      detail(list, "Receipt time (UTC)", event.received_at);
      detail(list, "Source", event.source === "twitch_irc" ? "Twitch IRC membership" : event.source || "Twitch IRC membership");
      detail(list, "Collection session", event.session_id);
      detail(list, "Stored event ID", event.id);
      explanation.append(list);
      const eventLabel = event.kind === "names_end" ? "NAMES end" : String(event.kind || "Unknown").toUpperCase();
      row.append(cell(timestamp(event.received_at), "time"), cell(historyLink(event.channel, "", event.channel)), cell(event.account ? historyLink(event.account, event.account, "") : node("span", "Protocol only", "quiet")), cell(badge(eventLabel)), cell(explanation));
      return row;
    }, `${count(events.length)} stored events on this page · newest receipts first`);
  }

  const eventsView = pagedView("search", (query, before) => api(url("/api/search", { ...query, before })), eventTable);
  const relationshipsView = pagedView("relationships", (query, offset) => api(url("/api/relationships", { ...query, offset })), (data) => {
    const relationships = rows(data, "relationships");
    if (!relationships.length) return empty("No matching account/channel relationships are stored.");
    return table(["Channel", "Account", "First receipt", "Latest receipt", "Events", "Inspect"], relationships, (item) => {
      const row = node("tr");
      row.append(cell(historyLink(item.channel, "", item.channel)), cell(historyLink(item.account, item.account, "")), cell(timestamp(item.first_observed_at), "time"), cell(timestamp(item.last_observed_at), "time"), cell(count(item.observation_count), "number"), cell(button("This pair", () => openHistory(item.account, item.channel))));
      return row;
    }, `${count(relationships.length)} account/channel pairs on this page · includes only retained evidence`);
  }, true);

  const sessionsView = pagedView("sessions", (query, before) => api(url("/api/channels/" + encodeURIComponent(query.channel) + "/history", { limit: query.limit, from: query.from, to: query.to, before })), (data) => {
    const sessions = rows(data, "sessions");
    if (!sessions.length) return empty("No collection sessions are stored for this channel.");
    return table(["Session", "Collection started", "Collection ended", "End reason", "Stored events"], sessions, (session) => {
      const row = node("tr");
      const ended = session.end_uncertain ? badge("Unknown — interrupted", "warning") : session.ended_at ? timestamp(session.ended_at) : badge("No end recorded");
      const reason = node("div", session.end_reason ? label(session.end_reason) : "Not recorded");
      if (session.end_uncertain && session.closed_recorded_at) {
        const recovery = node("span", "Recovery recorded: ", "secondary-line");
        recovery.append(timestamp(session.closed_recorded_at));
        reason.append(recovery);
      }
      row.append(cell(session.id, "session-id"), cell(timestamp(session.started_at), "time"), cell(ended, "time"), cell(reason), cell(count(session.observation_count), "number"));
      return row;
    }, "Separate collection sessions · interrupted end times are unknown · recovery times are labeled separately");
  });

  function svgElement(tag, attributes = {}, text) {
    const element = document.createElementNS("http://www.w3.org/2000/svg", tag);
    for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, String(value));
    if (text !== undefined) element.textContent = text;
    return element;
  }

  function exactTime(value) {
    const result = node("span");
    result.append(timestamp(value));
    const date = new Date(value);
    if (value && Number.isFinite(date.getTime())) result.append(node("span", date.toISOString(), "timeline-exact-time"));
    return result;
  }

  function timelineText(value) {
    const date = new Date(value);
    return value && Number.isFinite(date.getTime()) ? `${timelineClock.format(date)} (${date.toISOString()})` : "Unknown time";
  }

  async function fetchTimeline(query, before) {
    if (query.from && query.to && Date.parse(query.to) - Date.parse(query.from) > 31 * 86400000) throw new Error("The account timeline supports a maximum of 31 days. Narrow the From / Until range and search again.");
    const data = await api(url("/api/timeline", { account: query.account, channel: query.channel, broadcast_id: query.broadcast_id, from: query.from, to: query.to, limit: 25, before }));
    const range = data.effective_range;
    if (!range || !Number.isFinite(Date.parse(range.from)) || !Number.isFinite(Date.parse(range.to)) || Date.parse(range.to) < Date.parse(range.from)) throw new Error("The local app returned an invalid timeline period.");
    if (query.timelineRangeFixed && (Date.parse(query.from) !== Date.parse(range.from) || Date.parse(query.to) !== Date.parse(range.to))) throw new Error("The timeline period changed unexpectedly. Search again to refresh its axis.");
    // Keep the same axis on every page, including when the first query used default bounds.
    query.from = range.from; query.to = range.to;
    query.timelineRangeFixed = true;
    query.timelineBefore = before;
    return data;
  }

  function timelinePage(data, query, prefix) {
    const intervals = rows(data, "intervals");
    if (intervals.length > 100) throw new Error("The local app returned too much timeline evidence for one page.");
    const loadedStart = Date.parse(data.effective_range.from), loadedEnd = Date.parse(data.effective_range.to);
    const savedViewport = query.timelineViewport;
    const hasSavedViewport = savedViewport && savedViewport.from >= loadedStart && savedViewport.to <= loadedEnd && savedViewport.to > savedViewport.from;
    let start = hasSavedViewport ? savedViewport.from : loadedStart, end = hasSavedViewport ? savedViewport.to : loadedEnd;
    const result = node("div");
    const period = node("p", null, "timeline-period");
    period.id = prefix + "-summary";
    period.append(node("strong", `${data.account || query.account} · query period (local): `), node("span", `${timelineText(data.effective_range.from)} inclusive — ${timelineText(data.effective_range.to)} exclusive`));
    result.append(period);
    if (data.broadcast) result.append(node("p", `Broadcast metadata window: ${timelineText(data.broadcast.started_at)} — ${timelineText(data.broadcast.last_live_at)}. This identifies the reported broadcast window, not account attendance.`, "table-note"));
    result.append(node("p", "Dots show actual first / last positive receipt endpoints for each segment, not every received event. Patterned spans are uncertain inferences. Unknown background establishes neither presence nor absence. No confidence percentage is calibrated.", "table-note"));
    result.append(node("p", "The query period is not a frozen snapshot: stored evidence can change between page requests.", "table-note"));
    if (data.has_more || query.timelineBefore != null) result.append(node("p", "Partial evidence page: additional evidence is on other pages and is not drawn here. The query period stays fixed. Zoom uses only this loaded page. Unmarked areas remain unknown; they are not evidence of absence.", "timeline-warning"));
    if (data.empty || end <= start) {
      result.append(empty("No overlapping timeline period is available for these bounds and broadcast metadata. Account presence remains unknown."));
      return result;
    }
    const legend = node("ul", null, "timeline-legend");
    for (const [key, text] of [["positive", "Actual positive endpoint receipt"], ["inferred", "Uncertain inference"], ["departure", "PART receipt · departure unverified"], ["unknown", "Unknown / unloaded evidence"]]) {
      const item = node("li");
      const swatch = node("span", null, "timeline-key " + key); swatch.setAttribute("aria-hidden", "true");
      item.append(swatch, node("span", text)); legend.append(item);
    }
    if (data.broadcast) {
      const item = node("li"); const swatch = node("span", null, "timeline-key window"); swatch.setAttribute("aria-hidden", "true");
      item.append(swatch, node("span", "Broadcast metadata window (clipped)")); legend.append(item);
    }
    result.append(legend);
    const channelNames = [...new Set(intervals.map((item) => String(item.channel || "Unknown channel")))].sort((a, b) => a.localeCompare(b));
    if (!channelNames.length) channelNames.push(data.channel || query.channel || "No recorded channel");
    const height = 95 + channelNames.length * 58;
    const minimumSpan = Math.min(1000, loadedEnd - loadedStart);
    const controls = node("div", null, "timeline-zoom-controls");
    controls.setAttribute("role", "group"); controls.setAttribute("aria-label", "Timeline zoom");
    const zoomIn = button("Zoom in", () => resizeViewport((end - start) / 2));
    const zoomOut = button("Zoom out", () => resizeViewport((end - start) * 2));
    const reset = button("Reset zoom", () => setViewport(loadedStart, loadedEnd));
    controls.append(zoomIn, zoomOut, reset);
    const viewport = node("p", null, "timeline-period timeline-viewport");
    viewport.id = prefix + "-viewport"; viewport.setAttribute("role", "status"); viewport.setAttribute("aria-live", "polite");
    result.append(controls, viewport, node("p", "Drag horizontally between two positions inside the plot with a mouse to zoom. Zoom controls also work with a keyboard. Zoom uses only the current evidence page and keeps the viewport when paging. A new search resets zoom. Scroll horizontally to reach the rest of the chart on a narrow screen.", "table-note"));
    const hover = node("p", null, "timeline-hover");
    hover.id = prefix + "-hover"; hover.tabIndex = 0;
    hover.setAttribute("role", "status"); hover.setAttribute("aria-live", "polite");
    hover.setAttribute("aria-label", "Stored broadcast metadata; scroll for full details");
    result.append(hover);
    const hoverPrompt = "Hover or focus a positive receipt or uncertain patterned span to inspect stored broadcast title metadata. Metadata does not establish account viewing.";
    let hoverTimer = null, hoverController = null, hoverGeneration = 0, hoverKey = null, cancelDrag = null, hoverComplete = false;
    function cancelHover(keepResult = false) {
      hoverGeneration += 1; hoverKey = null;
      if (hoverTimer !== null) clearTimeout(hoverTimer);
      if (hoverController) hoverController.abort();
      hoverTimer = null; hoverController = null;
      if (!keepResult || !hoverComplete) { hover.textContent = hoverPrompt; hover.scrollTop = 0; hoverComplete = false; }
    }
    function cancelPending() {
      cancelHover(); if (cancelDrag) cancelDrag();
      if (activeTimelineCancel === cancelPending) activeTimelineCancel = null;
    }
    result.cancelPending = cancelPending;
    function currentHover(generation) {
      return generation === hoverGeneration && !document.hidden && !$("section-" + (prefix === "timeline" ? "history" : "broadcasts")).hidden && $(prefix + "-results").contains(result) && !$(prefix + "-results").classList.contains("stale");
    }
    function scheduleHover(channel, at, evidence) {
      if (!Number.isFinite(at)) return;
      const selectedAt = new Date(Math.max(start, Math.min(end - 1, Math.floor(at)))).toISOString();
      const key = `${channel}:${selectedAt}:${evidence}`;
      if (key === hoverKey) return;
      if (activeTimelineCancel && activeTimelineCancel !== cancelPending) activeTimelineCancel();
      cancelHover(); activeTimelineCancel = cancelPending; hoverKey = key;
      const generation = hoverGeneration;
      hoverTimer = setTimeout(async () => {
        hoverTimer = null;
        if (!currentHover(generation)) { cancelHover(); return; }
        const selectedText = `${channel} · selected time: ${timelineText(selectedAt)}. ${evidence}. `;
        hover.textContent = selectedText + "Loading stored broadcast metadata…";
        const controller = new AbortController(); hoverController = controller;
        try {
          const context = await api(url("/api/channels/" + encodeURIComponent(channel) + "/metadata/at", { at: selectedAt, broadcast_id: data.broadcast ? data.broadcast.id : query.broadcast_id }), { signal: controller.signal });
          if (!currentHover(generation)) return;
          const checkAt = context.check && context.check.checked_at;
          const title = context.title;
          if (["exact_check", "last_reported"].includes(context.basis) && title && typeof title.title === "string" && Number.isFinite(Date.parse(checkAt)) && Number.isFinite(Date.parse(title.seen_at))) {
            const reported = title.title === "" ? "Empty reported title" : title.title;
            hover.textContent = selectedText + `${context.basis === "exact_check" ? "Reported title" : "Last reported title"}: ${reported}. Metadata checked: ${timelineText(checkAt)}. Title first recorded: ${timelineText(title.seen_at)}. This metadata sample does not establish continuity or video viewing.`;
          } else {
            const reasons = { no_history: "No stored metadata history", metadata_unknown: "The stored metadata state is unknown", offline_report: "The stored check reported the channel offline", polling_gap: "The metadata polling gap leaves this time unknown", outside_broadcast_window: "The selected time is outside the recorded broadcast window", broadcast_mismatch: "The stored report does not match the selected broadcast", transition_ambiguous: "The broadcast transition is ambiguous", missing_title_history: "No historical title was stored for this time", inconsistent_history: "Stored metadata history is inconsistent" };
            hover.textContent = selectedText + `Unknown broadcast title — ${reasons[context.reason] || "No usable stored title metadata"}.` + (Number.isFinite(Date.parse(checkAt)) ? ` Metadata checked: ${timelineText(checkAt)}.` : "") + " Missing metadata does not establish absence or video viewing.";
          }
          hoverComplete = true;
        } catch (_) {
          if (currentHover(generation)) { hover.textContent = selectedText + "Unknown broadcast title — stored metadata could not be loaded. Chat evidence is unchanged."; hoverComplete = true; }
        } finally {
          if (generation === hoverGeneration) hoverController = null;
        }
      }, 180);
    }
    const display = node("div"); result.append(display);
    let previousScroll = 0, detailsOpen = false;
    function setViewport(from, to) {
      start = Math.max(loadedStart, Math.round(from)); end = Math.min(loadedEnd, Math.round(to));
      query.timelineViewport = { from: start, to: end };
      renderViewport();
    }
    function resizeViewport(span) {
      const width = Math.max(minimumSpan, Math.min(loadedEnd - loadedStart, span));
      const from = Math.max(loadedStart, Math.min(loadedEnd - width, (start + end - width) / 2));
      setViewport(from, from + width);
    }
    function renderViewport() {
      cancelHover();
      if (cancelDrag) cancelDrag();
      viewport.textContent = `Visible viewport (local): ${timelineText(new Date(start).toISOString())} inclusive — ${timelineText(new Date(end).toISOString())} exclusive. Current loaded evidence page only.`;
      zoomIn.disabled = end - start <= minimumSpan;
      zoomOut.disabled = reset.disabled = start === loadedStart && end === loadedEnd;
      const priorScroll = display.children[0], priorDetails = display.children[display.children.length - 1];
      if (priorScroll) previousScroll = priorScroll.scrollLeft;
      if (priorDetails && priorDetails.tagName === "DETAILS") detailsOpen = priorDetails.open;
      display.replaceChildren();
      const graphic = svgElement("svg", { id: prefix + "-chart", viewBox: `0 0 1000 ${height}`, width: 1000, height, role: "img", "aria-labelledby": prefix + "-chart-title " + prefix + "-chart-description" });
      graphic.append(svgElement("title", { id: prefix + "-chart-title" }, `IRC evidence for ${data.account || query.account}, grouped by channel on a shared time axis`), svgElement("desc", { id: prefix + "-chart-description" }, `Visible viewport ${timelineText(new Date(start).toISOString())} inclusive to ${timelineText(new Date(end).toISOString())} exclusive. Actual receipt endpoint dots and PART diamonds are separate from patterned uncertain inference. Unmarked and unloaded evidence is unknown. Exact times and evidence basis follow in a table.`));
      const defs = svgElement("defs");
      const pattern = svgElement("pattern", { id: prefix + "-hatch", width: 7, height: 7, patternUnits: "userSpaceOnUse" });
      pattern.append(svgElement("path", { d: "M-2 2L2 -2M0 7L7 0M5 9L9 5", class: "timeline-hatch" })); defs.append(pattern); graphic.append(defs);
      const x = (at) => 180 + 790 * (at - start) / (end - start);
      for (let tick = 0; tick <= 4; tick += 1) {
        const at = start + (end - start) * tick / 4;
        const position = x(at);
        const anchor = tick === 0 ? "start" : tick === 4 ? "end" : "middle";
        graphic.append(svgElement("line", { x1: position, y1: 47, x2: position, y2: height - 20, class: "chart-axis" }), svgElement("text", { x: position, y: 18, "text-anchor": anchor, class: "timeline-axis-label" }, timelineDay.format(new Date(at))), svgElement("text", { x: position, y: 35, "text-anchor": anchor, class: "timeline-axis-label" }, timelineTime.format(new Date(at))));
      }
      for (const [index, channel] of channelNames.entries()) {
        const y = 62 + index * 58;
        const channelLabel = channel.length > 16 ? channel.slice(0, 15) + "…" : channel;
        const channelAttributes = { x: 12, y: y + 17, class: "timeline-channel" };
        // Leave a 12px gutter before the plot, even for wide valid login glyphs.
        if (channel.length > 12) { channelAttributes.textLength = 156; channelAttributes.lengthAdjust = "spacingAndGlyphs"; }
        const channelText = svgElement("text", channelAttributes, channelLabel);
        channelText.append(svgElement("title", {}, channel));
        graphic.append(channelText, svgElement("rect", { x: 180, y, width: 790, height: 32, class: "timeline-unknown" }));
        if (data.broadcast) graphic.append(svgElement("rect", { x: 180, y: y - 3, width: 790, height: 38, class: "timeline-window" }));
      }
      for (const item of intervals) {
        const y = 78 + channelNames.indexOf(String(item.channel || "Unknown channel")) * 58;
        const inferenceFrom = Date.parse(item.inference_from), inferenceTo = Date.parse(item.inference_to);
        if (item.kind === "visit" && item.supported_visit === true && !item.out_of_order && Number.isFinite(inferenceFrom) && Number.isFinite(inferenceTo)) {
          const left = Math.max(start, inferenceFrom), right = Math.min(end, inferenceTo);
          if (right > left) {
            const attributes = { x: x(left), y: y - 9, width: x(right) - x(left), height: 18, class: "timeline-inference" };
            const span = svgElement("rect", attributes);
            span.append(svgElement("title", {}, `${item.channel}: uncertain inference ${timelineText(new Date(left).toISOString())} — ${timelineText(new Date(right).toISOString())}; ${label(item.inference_end_basis)}. Continuous presence is unknown.`));
            bindHover(span, item.channel, null, "Uncertain chat inference; continuous presence is unknown", left + Math.floor((right - left) / 2));
            graphic.append(span, svgElement("rect", { x: x(left), y: y - 9, width: x(right) - x(left), height: 18, fill: `url(#${prefix}-hatch)`, "aria-hidden": "true", "pointer-events": "none" }));
          }
        }
        const seen = new Set();
        for (const marker of Array.isArray(item.markers) ? item.markers.slice(0, 3) : []) {
          const at = Date.parse(marker.at);
          if (!Number.isFinite(at) || at < start || at >= end || !["first_positive", "last_positive", "part"].includes(marker.kind) || seen.has(`${marker.kind}:${at}`)) continue;
          seen.add(`${marker.kind}:${at}`);
          const position = x(at);
          const shape = marker.kind === "part" ? svgElement("polygon", { points: `${position},${y - 6} ${position + 6},${y} ${position},${y + 6} ${position - 6},${y}`, class: "timeline-departure" }) : svgElement("circle", { cx: position, cy: y, r: 4.5, class: "positive-point" });
          shape.append(svgElement("title", {}, `${item.channel}: ${label(marker.kind)} received ${timelineText(marker.at)}; ${label(marker.basis)}. Stored event ${marker.observation_id}.`));
          if (marker.kind !== "part") bindHover(shape, item.channel, at, "Actual positive chat receipt", at);
          graphic.append(shape);
        }
      }
      const scroll = node("div", null, "timeline-scroll");
      scroll.tabIndex = 0; scroll.setAttribute("role", "region"); scroll.setAttribute("aria-label", "Scrollable account evidence timeline; exact values are in the following evidence table"); scroll.append(graphic); display.append(scroll); scroll.scrollLeft = previousScroll;
      if (!intervals.length) display.append(node("p", "No matching evidence is loaded in this period. The entire visible period remains unknown; this does not establish absence.", "table-note"));
      const details = node("details", null, "timeline-details");
      details.append(node("summary", `Exact times and evidence basis (${count(intervals.length)} segments on this page)`));
      details.append(table(["Channel / session", "Evidence quality", "Actual positive endpoint receipts", "Uncertain inference shown", "Actual PART receipts / boundary"], intervals, (item) => {
        const row = node("tr");
        const identity = node("div", item.channel); identity.append(node("span", `Session ${item.session_id} · ${item.kind === "departure" ? "PART only" : "Segment " + item.id}`, "secondary-line"));
        const quality = node("div", item.kind === "departure" ? "Departure receipt only" : item.supported_visit && !item.out_of_order ? "Supported segment · continuity uncertain" : "Ambiguous observation only · no inference");
        quality.append(node("span", `Start: ${label(item.start_basis)}. End: ${label(item.end_basis)}${item.end_reason ? " (" + label(item.end_reason) + ")" : ""}.`, "secondary-line"));
        if (item.out_of_order) quality.append(node("span", "Out-of-order evidence; no inference drawn.", "secondary-line"));
        if (item.session_end_uncertain) quality.append(node("span", "Collection session end is uncertain.", "secondary-line"));
        const endpoints = node("div");
        if (item.first_observed_at) { endpoints.append(node("span", "First endpoint: "), exactTime(item.first_observed_at)); }
        if (item.last_observed_at && item.last_observed_at !== item.first_observed_at) endpoints.append(node("span", "Last endpoint: ", "secondary-line"), exactTime(item.last_observed_at));
        if (!item.first_observed_at) endpoints.append(node("span", "No positive receipt endpoint"));
        endpoints.append(node("span", "Only actual markers inside the visible period are drawn; endpoints can be outside it.", "secondary-line"));
        const inference = node("div");
        const left = Math.max(start, Date.parse(item.inference_from)), right = Math.min(end, Date.parse(item.inference_to));
        if (item.kind === "visit" && item.supported_visit === true && !item.out_of_order && Number.isFinite(left) && Number.isFinite(right) && right > left) inference.append(exactTime(new Date(left).toISOString()), node("span", "Until (exclusive): ", "secondary-line"), exactTime(new Date(right).toISOString()), node("span", `Basis: earlier positive receipt; end: ${label(item.inference_end_basis)}. Patterned interval is uncertain.`, "secondary-line"));
        else inference.append(node("span", "None · unknown between receipts"));
        const boundary = node("div");
        const parts = (Array.isArray(item.markers) ? item.markers : []).filter((marker) => marker.kind === "part");
        for (const part of parts) boundary.append(node("span", "PART received: "), exactTime(part.at), node("span", `Basis: ${label(part.basis)}; event ${part.observation_id}. Departure time is unverified.`, "secondary-line"));
        if (item.boundary_at) boundary.append(node("span", `Boundary (${label(item.boundary_basis)}): `, "secondary-line"), exactTime(item.boundary_at));
        if (!parts.length && !item.boundary_at) boundary.append(node("span", "No recorded boundary"));
        row.append(cell(identity), cell(quality), cell(endpoints), cell(inference), cell(boundary)); return row;
      }, "Equivalent timeline evidence · exact local receipt times and UTC · IRC observations only · unknown is not absence"));
      details.open = detailsOpen; display.append(details);
      const plotTop = 62, plotBottom = 62 + (channelNames.length - 1) * 58 + 32;
      const selection = svgElement("rect", { x: 180, y: plotTop, width: 0, height: plotBottom - plotTop, class: "timeline-selection", visibility: "hidden", "aria-hidden": "true" });
      graphic.append(selection);
      let drag = null;
      function plotPosition(event) {
        if (!Number.isFinite(event.clientX) || !Number.isFinite(event.clientY)) return null;
        try {
          const matrix = graphic.getScreenCTM();
          if (!matrix) return null;
          const point = graphic.createSVGPoint(); point.x = event.clientX; point.y = event.clientY;
          const local = point.matrixTransform(matrix.inverse());
          return Number.isFinite(local.x) && Number.isFinite(local.y) ? local : null;
        } catch (_) { return null; }
      }
      function bindHover(shape, channel, exactAt, evidence, focusAt) {
        shape.setAttribute("tabindex", "0"); shape.setAttribute("aria-describedby", hover.id);
        shape.setAttribute("aria-label", `${channel}: ${evidence}. Focus inspects metadata at ${timelineText(new Date(focusAt).toISOString())}.`);
        function inspect(event) {
          if (drag || event.pointerType === "touch") return;
          const position = exactAt === null ? plotPosition(event) : null;
          if (exactAt !== null || position) scheduleHover(channel, exactAt === null ? start + (end - start) * (position.x - 180) / 790 : exactAt, evidence);
        }
        shape.addEventListener("pointerenter", inspect); shape.addEventListener("pointermove", inspect);
        shape.addEventListener("pointerleave", () => cancelHover(true));
        shape.addEventListener("focus", () => { if (!drag) scheduleHover(channel, focusAt, evidence); });
        shape.addEventListener("blur", () => cancelHover(true));
        shape.addEventListener("keydown", (event) => { if (event.key === "Escape") cancelHover(); });
      }
      function finishDrag() {
        const pointerId = drag && drag.pointerId; drag = null;
        selection.setAttribute("visibility", "hidden");
        if (pointerId !== null && pointerId !== undefined && graphic.hasPointerCapture(pointerId)) graphic.releasePointerCapture(pointerId);
      }
      cancelDrag = finishDrag;
      graphic.addEventListener("pointerdown", (event) => {
        if (drag || event.pointerType !== "mouse" || event.button !== 0 || event.isPrimary === false) return;
        const position = plotPosition(event);
        if (!position || position.x < 180 || position.x > 970 || position.y < plotTop || position.y > plotBottom) return;
        if (activeTimelineCancel && activeTimelineCancel !== cancelPending) activeTimelineCancel();
        cancelHover();
        try { graphic.setPointerCapture(event.pointerId); } catch (_) { return; }
        drag = { pointerId: event.pointerId, x: position.x, clientX: event.clientX };
        activeTimelineCancel = cancelPending;
        selection.setAttribute("x", position.x); selection.setAttribute("width", 0); selection.setAttribute("visibility", "visible");
        event.preventDefault();
      });
      graphic.addEventListener("pointermove", (event) => {
        if (!drag || event.pointerId !== drag.pointerId) return;
        const position = plotPosition(event); if (!position) { finishDrag(); return; }
        const last = Math.max(180, Math.min(970, position.x));
        selection.setAttribute("x", Math.min(drag.x, last)); selection.setAttribute("width", Math.abs(last - drag.x));
      });
      graphic.addEventListener("pointerup", (event) => {
        if (!drag || event.pointerId !== drag.pointerId) return;
        const position = plotPosition(event), first = drag.x, distance = Math.abs(event.clientX - drag.clientX);
        finishDrag();
        if (!position || distance < 6) return;
        const last = Math.max(180, Math.min(970, position.x));
        const from = start + (end - start) * (Math.min(first, last) - 180) / 790;
        const to = start + (end - start) * (Math.max(first, last) - 180) / 790;
        if (to - from >= minimumSpan) setViewport(from, to);
      });
      graphic.addEventListener("pointercancel", (event) => { cancelHover(); if (drag && event.pointerId === drag.pointerId) finishDrag(); });
      graphic.addEventListener("lostpointercapture", (event) => { if (drag && event.pointerId === drag.pointerId) finishDrag(); });
      graphic.addEventListener("pointerleave", () => { cancelHover(true); if (drag && !graphic.hasPointerCapture(drag.pointerId)) finishDrag(); });
    }
    renderViewport();
    return result;
  }

  const timelineView = pagedView("timeline", fetchTimeline, (data, query) => timelinePage(data, query, "timeline"));
  const broadcastTimelineView = pagedView("broadcast-timeline", fetchTimeline, (data, query) => timelinePage(data, query, "broadcast-timeline"));

  function timelinePrompt(view, prefix, text) {
    view.clear();
    $(prefix + "-results").replaceChildren(empty(text));
  }

  function evidenceTimeline(item, start, end) {
    const graphic = svgElement("svg", { viewBox: "0 0 260 30", width: "260", height: "30", "aria-hidden": "true", class: "evidence-timeline" });
    const position = (value) => 10 + 240 * Math.max(0, Math.min(1, (Date.parse(value) - start) / Math.max(1, end - start)));
    const first = position(item.first_observed_at);
    const last = position(item.last_observed_at);
    if (item.supported_visit !== false && !item.out_of_order) graphic.append(svgElement("line", { x1: first, y1: 15, x2: last, y2: 15, class: "evidence-span" }));
    graphic.append(svgElement("circle", { cx: first, cy: 15, r: 4, class: "positive-point" }), svgElement("circle", { cx: last, cy: 15, r: 4, class: "positive-point" }));
    return graphic;
  }

  function visitTable(data) {
    const visits = rows(data, "visits");
    const wrapper = node("div");
    wrapper.append(node("p", data.explanation || "Positive receipts support these separate chat visits. Their boundaries are uncertain; collection gaps end inference. No viewing duration is established.", "table-note"), node("p", "Visits overlap the selected time bounds or broadcast window. Full first/last positive receipt bounds are shown and can extend beyond that window.", "table-note"));
    if (!visits.length) { wrapper.append(empty("No supported visits match this range. Missing evidence does not prove absence.")); return wrapper; }
    const times = visits.flatMap((item) => [Date.parse(item.first_observed_at), Date.parse(item.last_observed_at)]).filter(Number.isFinite);
    const start = Math.min(...times), end = Math.max(...times);
    const supportedCount = visits.filter((item) => item.supported_visit !== false).length;
    wrapper.append(table(["Account / channel", "First positive receipt", "Last positive receipt", "Evidence span", "Boundary / quality"], visits, (item) => {
      const row = node("tr");
      const identity = node("div");
      identity.append(historyLink(item.account, item.account, item.channel), node("span", item.channel, "secondary-line"), node("span", `Session ${item.session_id}`, "secondary-line"));
      identity.append(badge(item.supported_visit === false ? "Observation only — ambiguous" : "Supported visit — uncertain", item.supported_visit === false ? "warning" : "neutral"));
      const span = node("div");
      if (Number.isFinite(start) && Number.isFinite(end)) span.append(evidenceTimeline(item, start, end));
      span.append(node("span", `${count(item.positive_observation_count)} positive receipts · uncertain continuity`, "secondary-line"));
      const boundaries = node("div");
      boundaries.append(badge("Uncertain boundaries", "warning"), node("span", `Start evidence: ${label(item.start_basis)}. End: ${label(item.end_basis)}${item.end_reason ? " (" + label(item.end_reason) + ")" : ""}.`, "secondary-line"));
      if (item.supported_visit === false) boundaries.append(node("span", "Ambiguous receipt segment; no supported visit or continuity inference is established.", "secondary-line"));
      if (item.end_observed_at) { const part = node("span", "PART received: ", "secondary-line"); part.append(timestamp(item.end_observed_at)); boundaries.append(part); }
      if (item.duplicate_or_repeated) boundaries.append(node("span", "Repeated positive observations were combined.", "secondary-line"));
      if (item.out_of_order) boundaries.append(node("span", "Out-of-order evidence limits inference.", "secondary-line"));
      row.append(cell(identity), cell(timestamp(item.first_observed_at), "time"), cell(timestamp(item.last_observed_at), "time"), cell(span), cell(boundaries));
      return row;
    }, `${count(supportedCount)} separate supported visits · ${count(visits.length - supportedCount)} ambiguous observation segments · dots are first/last positive receipts on a shared page time scale · dotted spans are not measured attendance`));
    return wrapper;
  }

  const visitsView = pagedView("visits", (query, before) => api(url("/api/history", { ...query, before })), visitTable);
  const broadcastVisitsView = pagedView("broadcast-visits", (query, before) => api(url("/api/history", { ...query, before })), visitTable);
  const presenceView = pagedView("presence", (query, offset) => api(url("/api/presence", { ...query, offset })), (data) => {
    const accounts = rows(data, "accounts");
    const result = node("div");
    result.append(node("p", data.explanation || "Observed: a positive receipt at the selected instant. Inferred: earlier positive evidence in an unclosed supported visit. Unknown coverage cannot establish absence.", "table-note"));
    if (!accounts.length) { result.append(empty("Unknown — no account evidence supports presence at this instant. This does not mean the channel was empty.")); return result; }
    result.append(table(["Account", "Evidence at selected instant", "Supporting receipt", "Interpretation"], accounts, (item) => {
      const row = node("tr");
      const state = ["observed", "inferred"].includes(item.state) ? item.state : "unknown";
      row.append(cell(historyLink(item.account, item.account, searchQuery.channel)), cell(badge(label(state), state === "observed" ? "good" : state === "inferred" ? "warning" : "neutral")), cell(timestamp(item.observed_at), "time"), cell(item.explanation || "Chat evidence only; viewing and continuous membership remain unknown."));
      return row;
    }, "Point-in-time chat evidence · presence is uncertain · unknown is not absence"));
    return result;
  }, true);

  function metadataChart(observations) {
    const samples = observations.filter((item) => item.state === "live" && Number.isFinite(item.viewer_count) && item.viewer_count >= 0 && Number.isFinite(Date.parse(item.checked_at)));
    if (!samples.length) return null;
    const start = Math.min(...samples.map((item) => Date.parse(item.checked_at)));
    const end = Math.max(...samples.map((item) => Date.parse(item.checked_at)));
    const maximum = Math.max(1, ...samples.map((item) => item.viewer_count));
    const figure = node("figure", null, "metadata-chart");
    const graphic = svgElement("svg", { viewBox: "0 0 720 180", role: "img", "aria-label": `Reported live viewer-count samples, from zero to ${count(maximum)}. Exact samples are in the following table. Unconnected points do not imply continuity.` });
    graphic.append(svgElement("line", { x1: 45, y1: 140, x2: 700, y2: 140, class: "chart-axis" }), svgElement("text", { x: 8, y: 22, class: "chart-label" }, count(maximum)), svgElement("text", { x: 22, y: 145, class: "chart-label" }, "0"));
    for (const sample of samples) {
      const dot = svgElement("circle", { cx: 50 + 640 * (Date.parse(sample.checked_at) - start) / Math.max(1, end - start), cy: 140 - 120 * sample.viewer_count / maximum, r: 4, class: "positive-point" });
      dot.append(svgElement("title", {}, `${timestamp(sample.checked_at).textContent}: ${count(sample.viewer_count)} reported viewers`));
      graphic.append(dot);
    }
    graphic.append(svgElement("text", { x: 45, y: 170, class: "chart-label" }, clock.format(new Date(start))));
    if (end !== start) graphic.append(svgElement("text", { x: 700, y: 170, "text-anchor": "end", class: "chart-label" }, clock.format(new Date(end))));
    figure.append(graphic, node("figcaption", "Reported live viewer-count samples on this page. Points are not connected across gaps; exact times and values follow.", "table-note"));
    return figure;
  }

  const metadataHistoryView = pagedView("metadata-history", (query, before) => api(url("/api/channels/" + encodeURIComponent(query.channel) + "/metadata/history", { from: query.from, to: query.to, limit: query.limit, before })), (data) => {
    const observations = rows(data, "observations");
    if (!observations.length) return empty("No metadata reports match this range. Titles, categories, and viewer counts remain unknown.");
    const result = node("div");
    const chart = metadataChart(observations);
    if (chart) result.append(chart);
    result.append(table(["Checked locally", "Reported state", "Title", "Category", "Reported viewers"], observations, (item) => {
      const row = node("tr");
      row.append(cell(timestamp(item.checked_at), "time"), cell(badge(label(item.state))), cell(item.title || "Not reported"), cell(item.game_name || "Not reported"), cell(item.state === "live" ? count(item.viewer_count) : "Not reported", "number"));
      return row;
    }, "Stored metadata samples · no interpolation across checks · newest checks first"));
    return result;
  });

  function startSearch(event) {
    if (event) event.preventDefault();
    dismissLoginSuggestions("history");
    try {
      const account = login($("search-account").value, true);
      const channel = login($("search-channel").value, true);
      if (!account && !channel) throw new Error("Enter an account or channel login to search history.");
      searchQuery = { account, channel, limit: Number($("search-limit").value), ...timeRange("search") };
      $("search-account").value = account;
      $("search-channel").value = channel;
      $("relationships-section").hidden = false;
      eventsView.reset(searchQuery);
      relationshipsView.reset(searchQuery);
      visitsView.reset(searchQuery);
      if (account) timelineView.reset(searchQuery);
      else timelinePrompt(timelineView, "timeline", "Enter an exact account login to show its timeline. A channel filter is optional.");
      presenceView.clear();
      $("channel-context").hidden = !channel;
      if (channel) {
        $("sessions-channel").textContent = `${channel} · sessions cover the channel, regardless of the account filter.`;
        sessionsView.reset(searchQuery);
        metadataHistoryView.reset(searchQuery);
      } else { sessionsView.clear(); metadataHistoryView.clear(); }
      renderMetadata();
    } catch (error) { message("search-message", failure(error), "error"); }
  }

  const broadcastEventsView = pagedView("broadcast-events", (query, before) => api(url("/api/broadcasts/" + encodeURIComponent(query.id) + "/observations", { account: query.account, from: query.from, to: query.to, limit: 50, before })), (data) => {
    $("broadcast-evidence-scope").textContent = data.association_scope || "Chat events received in this channel during a confirmed broadcast window; not evidence of video watching.";
    return eventTable(data);
  });

  const broadcastAccountsView = pagedView("broadcast-accounts", (query, offset) => api(url("/api/broadcasts/" + encodeURIComponent(query.id) + "/accounts", { account: query.account, from: query.from, to: query.to, limit: 50, offset })), (data) => {
    const accounts = rows(data, "accounts");
    const wrapper = node("div");
    wrapper.append(node("h3", "Observed and inferred chat account associations"), node("p", data.explanation || "Positive receipts in the confirmed broadcast window are observed evidence. Earlier receipts can support separately labeled uncertain inferences. Neither establishes video watching or complete membership.", "table-note"));
    if (data.summary) {
      const summary = node("dl", null, "card detail-list");
      if (data.summary.account_count !== undefined) detail(summary, "Associated accounts", count(data.summary.account_count));
      if (data.summary.observed_account_count !== undefined) detail(summary, "Accounts observed in window", count(data.summary.observed_account_count));
      if (data.summary.inferred_only_account_count !== undefined) detail(summary, "Inferred-only accounts", count(data.summary.inferred_only_account_count));
      if (data.summary.visit_count !== undefined) detail(summary, "Supported visits", count(data.summary.visit_count));
      if (data.summary.positive_observation_count !== undefined) detail(summary, "Positive receipts", count(data.summary.positive_observation_count));
      wrapper.append(summary);
    }
    if (!accounts.length) { wrapper.append(empty("No matching chat association is supported for this broadcast. This does not prove nobody was present.")); return wrapper; }
    wrapper.append(table(["Account", "Association evidence", "First positive receipt in window", "Last positive receipt in window", "Supported visits", "Positive receipts in window"], accounts, (item) => {
      const row = node("tr");
      const observed = item.state === "observed" || item.association_basis === "positive_receipts" || item.positive_observation_count > 0;
      const basis = node("div");
      basis.append(badge(observed ? "Observed" : "Inferred — uncertain", observed ? "good" : "warning"));
      if (item.prior_observed_at) { const prior = node("span", "Supporting earlier receipt: ", "secondary-line"); prior.append(timestamp(item.prior_observed_at)); basis.append(prior); }
      basis.append(node("span", item.explanation || (observed ? "Positive receipt within the confirmed window." : "Earlier positive receipt without a recorded departure or gap; continuous presence remains unknown."), "secondary-line"));
      row.append(cell(historyLink(item.account, item.account, item.channel || data.broadcast && data.broadcast.channel || "")), cell(basis), cell(item.first_observed_at ? timestamp(item.first_observed_at) : "No positive receipt in window", "time"), cell(item.last_observed_at ? timestamp(item.last_observed_at) : "No positive receipt in window", "time"), cell(count(item.visit_count), "number"), cell(count(item.positive_observation_count), "number"));
      return row;
    }, "Account summaries within the selected broadcast and time bounds"));
    return wrapper;
  }, true);

  function openBroadcast(item, query) {
    if (!Number.isSafeInteger(item.id) || item.id < 1) return;
    $("broadcast-evidence").hidden = false;
    $("broadcast-evidence-title").textContent = `${item.channel} · ${item.title || "Untitled broadcast"}`;
    $("broadcast-evidence-scope").textContent = "Loading chat evidence for this reported broadcast…";
    broadcastEventsView.reset({ id: item.id, account: query.account, from: query.from, to: query.to });
    broadcastAccountsView.reset({ id: item.id, account: query.account, from: query.from, to: query.to });
    broadcastVisitsView.reset({ broadcast_id: item.id, account: query.account, from: query.from, to: query.to, limit: 50 });
    if (query.account) broadcastTimelineView.reset({ broadcast_id: item.id, account: query.account, from: query.from, to: query.to });
    else timelinePrompt(broadcastTimelineView, "broadcast-timeline", "Search broadcasts with an exact account login, then open a summary to show its timeline.");
    $("broadcast-evidence").scrollIntoView({ behavior: "auto", block: "start" });
  }

  const broadcastsView = pagedView("broadcasts", (query, before) => api(url("/api/broadcasts", { ...query, before })), (data, query) => {
    const broadcasts = rows(data, "broadcasts");
    if (!broadcasts.length) return empty("No matching broadcasts are stored. Metadata may be unavailable; chat collection and its history remain independent.");
    const result = node("div");
    result.append(node("p", data.association_scope || "Account matches can use positive receipts in the confirmed window or separately labeled uncertain prior-connected inferences. Open the summary to inspect their basis.", "table-note"));
    result.append(table(["Channel / title", "Reported start", "Last confirmed live", "First not-live report", "Chat evidence"], broadcasts, (item) => {
      const row = node("tr");
      const identity = node("div");
      identity.append(historyLink(item.channel, "", item.channel), node("p", item.title || "Untitled broadcast"), node("span", [item.game_name, item.language].filter(Boolean).join(" · ") || "Category not recorded", "secondary-line"));
      const evidence = button("View summary / visits / events", () => openBroadcast(item, query));
      row.append(cell(identity), cell(timestamp(item.started_at), "time"), cell(timestamp(item.last_live_at), "time"), cell(timestamp(item.offline_observed_at), "time"), cell(evidence));
      return row;
    }, `${count(broadcasts.length)} reported broadcasts on this page · missing not-live reports do not prove a broadcast is still live`));
    return result;
  });

  function startBroadcastSearch(event) {
    if (event) event.preventDefault();
    dismissLoginSuggestions("broadcasts");
    try {
      broadcastQuery = { channel: login($("broadcasts-channel").value, true), account: login($("broadcasts-account").value, true), limit: 50, ...timeRange("broadcasts") };
      $("broadcasts-channel").value = broadcastQuery.channel;
      $("broadcasts-account").value = broadcastQuery.account;
      $("broadcast-evidence").hidden = true;
      broadcastEventsView.clear();
      broadcastAccountsView.clear();
      broadcastVisitsView.clear();
      broadcastTimelineView.clear();
      broadcastsView.reset(broadcastQuery);
    } catch (error) { message("broadcasts-message", failure(error), "error"); }
  }

  $("add-channels-form").addEventListener("submit", addChannels);
  $("collection-mode").addEventListener("change", () => { settingsDirty = true; message("settings-message", "Policy changed. Save to apply it.", "warning"); });
  $("settings-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    if (settingsSaving || stopping) return;
    settingsSaving = true;
    settingsGeneration += 1;
    $("save-settings").disabled = true;
    const collection_mode = $("collection-mode").value;
    message("settings-message", "Saving collection policy…");
    try {
      settings = await api("/api/settings", { method: "PATCH", body: { collection_mode } });
      settingsDirty = $("collection-mode").value !== collection_mode;
      message("settings-message", settingsDirty ? "Policy saved. Your newer selection is not saved yet." : "Collection policy saved. Unknown metadata keeps chat observation running.", settingsDirty ? "warning" : "success");
      renderChannels(); refreshStatus();
    } catch (error) { message("settings-message", failure(error), "error"); }
    finally { settingsSaving = false; $("save-settings").disabled = false; }
  });
  $("presence-form").addEventListener("submit", (event) => {
    event.preventDefault();
    try {
      if (!searchQuery || !searchQuery.channel) throw new Error("Search a channel before inspecting a time.");
      presenceView.reset({ channel: searchQuery.channel, account: searchQuery.account, at: localTime("presence-at", true), limit: searchQuery.limit });
    } catch (error) { message("presence-message", failure(error), "error"); }
  });
  $("presence-at").addEventListener("input", () => presenceView.markStale());
  $("stop-app").addEventListener("click", () => {
    if (stopping) return;
    $("stop-confirmation").hidden = false;
    $("confirm-stop").focus({ preventScroll: true });
  });
  $("cancel-stop").addEventListener("click", () => { $("stop-confirmation").hidden = true; $("stop-app").focus({ preventScroll: true }); });
  $("confirm-stop").addEventListener("click", async () => {
    if (stopping) return;
    stopping = true;
    $("confirm-stop").disabled = true;
    $("cancel-stop").disabled = true;
    message("stop-message", "Requesting graceful shutdown…");
    try {
      const response = await api("/api/shutdown", { method: "POST", body: {} });
      if (response.state !== "stopping") throw new Error("The app did not acknowledge a shutdown request.");
      $("stop-confirmation").hidden = true;
      $("stop-app").disabled = true;
      $("add-channels").disabled = true;
      $("save-settings").disabled = true;
      $("stopped-message").hidden = false;
      $("stopped-message").textContent = "Veylurk accepted the stop request and is shutting down. Collection will stop; stored history is retained.";
      $("connection-status").textContent = "Stopping";
      $("connection-status").className = "badge warning";
      message("stop-message", "Shutdown requested. Restart Veylurk to collect again.");
      renderChannels();
      $("stop-message").focus({ preventScroll: true });
    } catch (error) {
      stopping = false;
      $("confirm-stop").disabled = false;
      $("cancel-stop").disabled = false;
      message("stop-message", failure(error) + " Shutdown was not confirmed.", "error");
    }
  });
  $("search-form").addEventListener("submit", startSearch);
  $("broadcasts-form").addEventListener("submit", startBroadcastSearch);
  $("refresh-status").addEventListener("click", () => refreshStatus(true));
  function markHistoryFiltersStale() {
    eventsView.markStale(); relationshipsView.markStale(); sessionsView.markStale(); visitsView.markStale(); metadataHistoryView.markStale(); presenceView.markStale(); timelineView.markStale();
  }
  function markBroadcastFiltersStale() {
    broadcastsView.markStale(); broadcastEventsView.markStale(); broadcastAccountsView.markStale(); broadcastVisitsView.markStale(); broadcastTimelineView.markStale();
  }
  for (const id of ["search-account", "search-channel", "search-limit", "search-from", "search-to"]) $(id).addEventListener("input", markHistoryFiltersStale);
  for (const id of ["broadcasts-account", "broadcasts-channel", "broadcasts-from", "broadcasts-to"]) $(id).addEventListener("input", markBroadcastFiltersStale);
  for (const [id, kind] of [["search-account", "account"], ["search-channel", "channel"]]) attachLoginSuggestions(id, kind, "history", markHistoryFiltersStale);
  for (const [id, kind] of [["broadcasts-account", "account"], ["broadcasts-channel", "channel"]]) attachLoginSuggestions(id, kind, "broadcasts", markBroadcastFiltersStale);
  $("open-channel-broadcasts").addEventListener("click", () => {
    if (!searchQuery || !searchQuery.channel) return;
    $("broadcasts-channel").value = searchQuery.channel;
    $("broadcasts-account").value = searchQuery.account;
    $("broadcasts-from").value = $("search-from").value;
    $("broadcasts-to").value = $("search-to").value;
    navigate("broadcasts"); startBroadcastSearch();
  });
  window.addEventListener("hashchange", showSection);
  document.addEventListener("visibilitychange", () => { if (!document.hidden) refreshStatus(); else { dismissLoginSuggestions(); if (activeTimelineCancel) activeTimelineCancel(); } });
  showSection();
  refreshStatus();
  setInterval(() => { if (!document.hidden) refreshStatus(); }, 15000);
})();
