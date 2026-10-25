(() => {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const clock = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "medium" });
  const numbers = new Intl.NumberFormat();
  const pendingChannels = new Set();
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
    } finally { clearTimeout(deadline); }
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
    const requested = location.hash.slice(1);
    const section = ["channels", "history", "broadcasts", "health"].includes(requested) ? requested : "channels";
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
      detail(fault, "Category", label(record.category));
      detail(fault, "Elapsed milliseconds", count(record.elapsed_ms));
      detail(fault, "SQLite primary code", record.sqlite_primary_code);
      detail(fault, "SQLite extended code", record.sqlite_extended_code);
      detail(fault, "Deadline interruption requested", record.progress_interrupt_requested === true ? "Yes" : record.progress_interrupt_requested === false ? "No" : "Unknown");
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
    const state = { query: null, positions: [offsetMode ? 0 : null], index: 0, next: null, generation: 0, loading: false, loaded: false, dirty: false };
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
    previous.addEventListener("click", () => { if (!state.loading && state.index > 0) load(state.index - 1, state.positions[state.index - 1]); });
    next.addEventListener("click", () => { if (!state.loading && state.next !== null) load(state.index + 1, state.next); });
    return {
      reset(query) {
        state.query = { ...query };
        state.positions = [offsetMode ? 0 : null];
        state.index = 0; state.next = null; state.loaded = false; state.dirty = false;
        return load(0, state.positions[0]);
      },
      clear() {
        state.generation += 1; state.query = null; state.loaded = false; state.loading = false;
        state.next = null; state.index = 0; state.dirty = false;
        result.replaceChildren(); result.setAttribute("aria-busy", "false"); result.classList.remove("stale");
        message(prefix + "-message", ""); controls();
      },
      markStale() { if (state.query) { state.dirty = true; result.classList.add("stale"); message(prefix + "-message", "Filters changed. Submit the search to update these results.", "warning"); controls(); } },
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

  function evidenceTimeline(item, start, end) {
    const graphic = svgElement("svg", { viewBox: "0 0 260 30", width: "260", height: "30", "aria-hidden": "true", class: "evidence-timeline" });
    const position = (value) => 10 + 240 * Math.max(0, Math.min(1, (Date.parse(value) - start) / Math.max(1, end - start)));
    const first = position(item.first_observed_at);
    const last = position(item.last_observed_at);
    graphic.append(svgElement("line", { x1: first, y1: 15, x2: last, y2: 15, class: "evidence-span" }), svgElement("circle", { cx: first, cy: 15, r: 4, class: "positive-point" }), svgElement("circle", { cx: last, cy: 15, r: 4, class: "positive-point" }));
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
    try {
      broadcastQuery = { channel: login($("broadcasts-channel").value, true), account: login($("broadcasts-account").value, true), limit: 50, ...timeRange("broadcasts") };
      $("broadcasts-channel").value = broadcastQuery.channel;
      $("broadcasts-account").value = broadcastQuery.account;
      $("broadcast-evidence").hidden = true;
      broadcastEventsView.clear();
      broadcastAccountsView.clear();
      broadcastVisitsView.clear();
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
  for (const id of ["search-account", "search-channel", "search-limit", "search-from", "search-to"]) $(id).addEventListener("input", () => {
    eventsView.markStale(); relationshipsView.markStale(); sessionsView.markStale(); visitsView.markStale(); metadataHistoryView.markStale(); presenceView.markStale();
  });
  for (const id of ["broadcasts-account", "broadcasts-channel", "broadcasts-from", "broadcasts-to"]) $(id).addEventListener("input", () => {
    broadcastsView.markStale(); broadcastEventsView.markStale(); broadcastAccountsView.markStale(); broadcastVisitsView.markStale();
  });
  $("open-channel-broadcasts").addEventListener("click", () => {
    if (!searchQuery || !searchQuery.channel) return;
    $("broadcasts-channel").value = searchQuery.channel;
    $("broadcasts-account").value = searchQuery.account;
    $("broadcasts-from").value = $("search-from").value;
    $("broadcasts-to").value = $("search-to").value;
    navigate("broadcasts"); startBroadcastSearch();
  });
  window.addEventListener("hashchange", showSection);
  document.addEventListener("visibilitychange", () => { if (!document.hidden) refreshStatus(); });
  showSection();
  refreshStatus();
  setInterval(() => { if (!document.hidden) refreshStatus(); }, 15000);
})();
