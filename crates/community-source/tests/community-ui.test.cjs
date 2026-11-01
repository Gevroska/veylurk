"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");

// Exercise the actual browser script; do not duplicate its gate in the fixture.
const native = fs.readFileSync(path.join(__dirname, "../src/native.rs"), "utf8");
const matches = [...native.matchAll(/const COMMUNITY_UI: &str = r#"([\s\S]*?)"#;/g)];
assert.equal(matches.length, 1, "The native Community script must be uniquely identified.");
const source = matches[0][1];

function fixture({
  videos = [{ paused: true, readyState: 0, ended: false }],
  playerLabel = null,
  signedIn = true,
  chatReady = true,
  communityReady = true,
  origin = "https://www.twitch.tv",
  pathname = "/strano",
  challenge = false,
  challengeFrame = false,
  popout = false,
  identity = signedIn,
  identityParent = true,
  identityDisabled = false,
  editableChat = true,
  loginButton = false,
  panelOpen = false,
  panelVisible = true,
  panelHeader = true,
  closeControl = true,
  closeDisabled = false,
  restoreWorks = true,
} = {}) {
  const clicks = [];
  let domReads = 0;
  function button(name, attributes) {
    return {
      disabled: false,
      textContent: "",
      getBoundingClientRect: () => ({ width: panelOpen && name === "identity" || !panelOpen && name === "close" ? 0 : 30, height: 20 }),
      getAttribute: key => attributes[key] ?? null,
      querySelector: () => null,
      closest: selector => selector === '[data-a-target="chat-badge-carousel"]' && identityParent ? {} : null,
      click() {
        clicks.push(name);
        if (popout && name === "community") panelOpen = true;
        if (name === "close" && restoreWorks) panelOpen = false;
      },
    };
  }
  const buttons = [];
  if (signedIn) buttons.push(button("avatar", { "aria-label": "User Avatar" }));
  if (identity) {
    const control = button("identity", { "aria-label": "ChatBadgeCarousel", "data-a-target": "chat-badge-carousel-badge-icon" });
    control.disabled = identityDisabled;
    buttons.push(control);
  }
  const loginControl = button("login", { "data-a-target": "login-button" });
  if (loginButton) buttons.push(loginControl);
  if (closeControl) {
    const close = button("close", { "aria-label": "Close", "data-test-selector": "close-viewer-list" });
    close.disabled = closeDisabled;
    buttons.push(close);
  }
  buttons.push(button("bits", { "data-a-target": "bits-button" }));
  if (communityReady) buttons.push(button("community", { "aria-label": "Community" }));
  if (playerLabel !== null) {
    buttons.push(button("player", {
      "data-a-target": "player-play-pause-button",
      "aria-label": playerLabel,
    }));
  }
  const document = {
    title: challenge ? "Verify you are human" : "Channel",
    querySelectorAll(selector) {
      domReads++;
      if (selector === 'button,[role="tab"]') return buttons;
      if (selector === "section.chat-viewers__container") return panelOpen && panelVisible ? [{
        getBoundingClientRect: () => ({ width: 200, height: 300 }),
        querySelector: selector => {
          assert.equal(selector, '[data-test-selector="chat-viewers__header"]');
          return panelHeader ? { getBoundingClientRect: () => ({ width: 200, height: 30 }) } : null;
        },
      }] : [];
      assert.equal(selector, "video");
      return videos;
    },
    querySelector(selector) {
      domReads++;
      if (selector === '[data-a-target="chat-input"]') {
        return chatReady ? {
          getBoundingClientRect: () => ({ width: panelOpen ? 0 : 100, height: 30 }),
          isContentEditable: editableChat,
          getAttribute: key => key === "aria-label" ? "Send a message" : null,
        } : null;
      }
      if (selector === '[data-a-target="chat-input"],.chat-scrollable-area__message-container') {
        return chatReady ? {} : null;
      }
      assert.equal(selector, 'iframe[src*="captcha" i],iframe[src*="challenge" i],form[action*="challenge" i]');
      return challengeFrame ? {} : null;
    },
  };
  function run({ click = true, allowPause = false, deadline = 2_000, panelExpected = false, panelOpening = false, restore = false, authRequired = panelExpected || restore } = {}) {
    const script = source
      .replaceAll("__POPOUT__", String(popout))
      .replaceAll("__CHANNEL__", "strano")
      .replaceAll("__AUTOMATIC__", "true")
      .replaceAll("__AUTH_REQUIRED__", String(authRequired))
      .replaceAll("__PANEL_EXPECTED__", String(panelExpected))
      .replaceAll("__PANEL_OPENING__", String(panelOpening))
      .replaceAll("__RESTORE_COMMUNITY__", String(restore))
      .replaceAll("__ALLOW_PAUSE__", String(allowPause))
      .replaceAll("__CLICK__", String(click))
      .replaceAll("__TRIGGER_DEADLINE_MS__", String(deadline));
    assert.doesNotMatch(script, /__[A-Z_]+__/);
    return vm.runInNewContext(script, {
      document,
      location: { origin, pathname },
      performance: { timeOrigin: 1_000, now: () => 10 },
    }, { timeout: 1_000 });
  }
  return {
    run, clicks, domReads: () => domReads, panelOpen: () => panelOpen,
    setPanelVisible(visible) { panelVisible = visible; },
    setLoginVisible(visible) {
      const index = buttons.indexOf(loginControl);
      if (visible && index < 0) buttons.push(loginControl);
      if (!visible && index >= 0) buttons.splice(index, 1);
    },
  };
}

function popoutFixture(options = {}) {
  return fixture({ popout: true, pathname: "/popout/strano/chat", videos: [], ...options });
}

test("chat-only popout uses positive self-identity and editable chat without a video or avatar", () => {
  const page = popoutFixture({ signedIn: false, identity: true });
  const result = page.run();
  assert.equal(result.code, "native_community_clicked");
  assert.equal(result.signed_in, true);
  assert.equal(result.video_present, false);
  assert.equal(result.video_paused, null);
  assert.equal(result.pause_clicked, false);
  assert.deepEqual(page.clicks, ["community"]);
});

test("anonymous editable chat and generic bits controls never establish popout authentication", () => {
  for (const options of [
    { signedIn: false, identity: false },
    { identityParent: false },
    { identityDisabled: true },
    { loginButton: true },
  ]) {
    const page = popoutFixture(options);
    const result = page.run();
    assert.equal(result.signed_in, false);
    assert.equal(result.code, "pause_or_readiness_lost");
    assert.deepEqual(page.clicks, []);
  }
});

test("popout requires its editable chat and ordinary Community control before clicking", () => {
  for (const options of [{ editableChat: false }, { chatReady: false }, { communityReady: false }]) {
    const page = popoutFixture(options);
    assert.equal(page.run().code, "pause_or_readiness_lost");
    assert.deepEqual(page.clicks, []);
  }
});

test("any popout video immediately fails before observation, pause or Community action", () => {
  for (const paused of [true, false, undefined]) {
    for (const click of [true, false]) {
      const page = popoutFixture({ videos: [{ paused }], playerLabel: "Pause" });
      assert.equal(page.run({ click, allowPause: true }).code, "unexpected_popout_video");
      assert.deepEqual(page.clicks, []);
    }
  }
});

test("chat-only challenge and anonymous guard observations never click Community", () => {
  for (const options of [{ challenge: true }, { challengeFrame: true }]) {
    const page = popoutFixture(options);
    assert.equal(page.run().code, "challenge_denied");
    assert.deepEqual(page.clicks, []);
  }
  const anonymous = popoutFixture({ signedIn: false, identity: false });
  const result = anonymous.run({ click: false });
  assert.equal(result.code, "observed");
  assert.equal(result.signed_in, false);
  assert.deepEqual(anonymous.clicks, []);
});

test("actual Community panel hides authentication until one ordinary close restores fresh visible chat", () => {
  const page = popoutFixture({ signedIn: false, identity: true });
  assert.equal(page.run().code, "native_community_clicked");
  assert.equal(page.panelOpen(), true);
  const hidden = page.run({ click: false, panelExpected: true });
  assert.equal(hidden.code, "community_panel_open");
  assert.equal(hidden.signed_in, false, "Hidden identity is unobservable, never authenticated.");
  assert.equal(hidden.chat_ready, false);
  assert.equal(page.run({ click: false, restore: true }).code, "community_restore_clicked");
  assert.equal(page.panelOpen(), false);
  const restored = page.run({ click: false });
  assert.equal(restored.code, "observed");
  assert.equal(restored.signed_in, true);
  assert.equal(restored.chat_ready, true);
  assert.deepEqual(page.clicks, ["community", "close"]);
});

test("an acknowledged opener may hide the exact composer before the panel mounts without claiming auth", () => {
  const page = popoutFixture({ signedIn: false, identity: true, panelVisible: false });
  assert.equal(page.run().code, "native_community_clicked");
  const opening = page.run({ click: false, panelExpected: true, panelOpening: true });
  assert.equal(opening.code, "community_panel_transition");
  assert.equal(opening.signed_in, false);
  assert.equal(opening.chat_ready, false);
  assert.deepEqual(page.clicks, ["community"]);
  page.setPanelVisible(true);
  assert.equal(page.run({ click: false, panelExpected: true }).code, "community_panel_open");
  assert.equal(page.run({ click: false, restore: true }).code, "community_restore_clicked");
  assert.equal(page.run({ click: false, authRequired: true }).signed_in, true);
  assert.deepEqual(page.clicks, ["community", "close"]);
});

test("opening transition is rejected outside AwaitingPanel or without exact hidden continuity", () => {
  const opening = { panelOpen: true, panelVisible: false };
  assert.equal(popoutFixture(opening).run({ click: false, panelExpected: true }).code, "community_panel_unverified");
  for (const missing of [{ identity: false }, { identityParent: false }, { identityDisabled: true }, { chatReady: false }, { editableChat: false }, { communityReady: false }]) {
    const page = popoutFixture({ ...opening, ...missing });
    assert.equal(page.run({ click: false, panelExpected: true, panelOpening: true }).code, "community_panel_unverified");
    assert.deepEqual(page.clicks, []);
  }
  const malformed = popoutFixture({ panelOpen: true, panelHeader: false });
  assert.equal(malformed.run({ click: false, panelExpected: true, panelOpening: true }).code, "community_panel_unverified");
});

test("explicit denial, wrong source or any video still fails during an opening gap", () => {
  for (const [options, code] of [
    [{ loginButton: true }, "authentication_denied"],
    [{ challenge: true }, "challenge_denied"],
    [{ challengeFrame: true }, "challenge_denied"],
    [{ videos: [{ paused: true }] }, "unexpected_popout_video"],
    [{ pathname: "/other" }, "wrong_page"],
  ]) {
    const page = popoutFixture({ panelOpen: true, panelVisible: false, ...options });
    assert.equal(page.run({ click: false, panelExpected: true, panelOpening: true }).code, code);
    assert.deepEqual(page.clicks, []);
  }
});

test("an explicit Login immediately after ordinary close remains an authentication denial", () => {
  const page = popoutFixture({ signedIn: false, identity: true });
  assert.equal(page.run().code, "native_community_clicked");
  assert.equal(page.run({ click: false, panelExpected: true }).code, "community_panel_open");
  assert.equal(page.run({ click: false, restore: true }).code, "community_restore_clicked");
  page.setLoginVisible(true);
  const denied = page.run({ click: false, authRequired: true });
  assert.equal(denied.code, "authentication_denied");
  assert.equal(denied.signed_in, false);
  page.setLoginVisible(false);
  // The native denial handler cancels this generation and discards the frozen
  // receipt; later positive UI must not revive that accepted round.
  assert.equal(page.run({ click: false, authRequired: true }).signed_in, true);
  assert.deepEqual(page.clicks, ["community", "close"]);
});

test("an unexpected, malformed or changed Community panel never establishes readiness", () => {
  const unexpected = popoutFixture({ panelOpen: true });
  assert.equal(unexpected.run({ click: false }).code, "unexpected_community_panel");
  const malformed = popoutFixture({ panelOpen: true, panelHeader: false });
  assert.equal(malformed.run({ click: false, panelExpected: true }).code, "community_panel_unverified");
  const disappeared = popoutFixture({ signedIn: false, identity: false });
  assert.equal(disappeared.run({ click: false, panelExpected: true }).code, "community_panel_unverified");
  for (const page of [unexpected, malformed, disappeared]) assert.deepEqual(page.clicks, []);
});

test("panel denial, video, absent close control and failed restore cannot authorize fresh chat", () => {
  for (const [options, code] of [
    [{ loginButton: true }, "authentication_denied"],
    [{ challenge: true }, "challenge_denied"],
    [{ videos: [{ paused: true }] }, "unexpected_popout_video"],
    [{ closeControl: false }, "community_restore_control_unavailable"],
    [{ closeDisabled: true }, "community_restore_control_unavailable"],
  ]) {
    const page = popoutFixture({ panelOpen: true, ...options });
    assert.equal(page.run({ click: false, restore: true }).code, code);
    assert.deepEqual(page.clicks, []);
  }
  const lostAuth = popoutFixture({ panelOpen: true, signedIn: false, identity: false });
  assert.equal(lostAuth.run({ click: false, restore: true }).code, "community_restore_clicked");
  assert.equal(lostAuth.run({ click: false }).signed_in, false);
  const stuck = popoutFixture({ panelOpen: true, restoreWorks: false });
  assert.equal(stuck.run({ click: false, restore: true }).code, "community_restore_clicked");
  assert.equal(stuck.run({ click: false }).code, "unexpected_community_panel");
  assert.deepEqual(stuck.clicks, ["close"]);
  for (const deadline of [1_010, 1_009, NaN, Infinity]) {
    const expired = popoutFixture({ panelOpen: true });
    const result = expired.run({ click: false, restore: true, deadline });
    assert.equal(result.code, "community_restore_deadline");
    assert.equal(result.signed_in, false, "An expired restore reports the hidden composer without asserting auth loss.");
    assert.equal(result.chat_ready, false);
    assert.deepEqual(expired.clicks, []);
  }
});

test("expected popout rejects full channel, wrong channel and other origin without reading DOM", () => {
  for (const location of [{ pathname: "/strano" }, { pathname: "/popout/other/chat" }, { origin: "http://www.twitch.tv" }]) {
    const page = popoutFixture(location);
    assert.equal(page.run().code, "wrong_page");
    assert.equal(page.domReads(), 0);
    assert.deepEqual(page.clicks, []);
  }
  const full = fixture({ pathname: "/popout/strano/chat" });
  assert.equal(full.run().code, "wrong_page");
  assert.equal(full.domReads(), 0);
});

test("paused video without Play or decoded frames gets one ordinary Community click", () => {
  const page = fixture();
  const result = page.run();
  assert.equal(result.code, "native_community_clicked");
  assert.equal(result.video_present, true);
  assert.equal(result.video_paused, true);
  assert.equal(result.video_ready, false);
  assert.equal(result.play_control, false);
  assert.equal(result.pause_clicked, false);
  assert.equal(result.trigger_unix_ms, 1_010);
  assert.deepEqual(page.clicks, ["community"]);
});

test("all videos must actually be paused before Community can be clicked", () => {
  for (const videos of [
    [],
    [{ paused: false, readyState: 4 }],
    [{ readyState: 4 }],
    [{ paused: true }, { paused: false }],
  ]) {
    const page = fixture({ videos, playerLabel: "Play (space/k)" });
    assert.equal(page.run().code, "pause_or_readiness_lost");
    assert.deepEqual(page.clicks, []);
  }
});

test("signed-in chat and Community remain required immediately before the click", () => {
  for (const missing of ["signedIn", "chatReady", "communityReady"]) {
    const page = fixture({ [missing]: false });
    assert.equal(page.run().code, "pause_or_readiness_lost");
    assert.deepEqual(page.clicks, []);
  }
});

test("wrong source location prevents clicks and even DOM reads", () => {
  for (const location of [
    { origin: "https://example.com" },
    { pathname: "/other_channel" },
  ]) {
    const page = fixture(location);
    assert.equal(page.run().code, "wrong_page");
    assert.deepEqual(page.clicks, []);
    assert.equal(page.domReads(), 0);
  }
});

test("challenge title or structural challenge prevents any ordinary click", () => {
  for (const challenge of [{ challenge: true }, { challengeFrame: true }]) {
    const page = fixture({ ...challenge, playerLabel: "Pause", videos: [{ paused: false }] });
    assert.equal(page.run({ allowPause: true }).code, "challenge_denied");
    assert.deepEqual(page.clicks, []);
  }
});

test("the ordinary Pause click requires actual playback and host authorization", () => {
  const playing = fixture({ playerLabel: "Pause (space/k)", videos: [{ paused: false }] });
  const paused = playing.run({ allowPause: true });
  assert.equal(paused.code, "observed");
  assert.equal(paused.pause_clicked, true);
  assert.deepEqual(playing.clicks, ["player"]);
  // The host's PlayerGate consumes the one-action authorization.
  assert.equal(playing.run({ allowPause: false }).code, "pause_or_readiness_lost");
  assert.deepEqual(playing.clicks, ["player"]);
  for (const videos of [[], [{ paused: true }]]) {
    const page = fixture({ videos, playerLabel: "Pause" });
    page.run({ click: false, allowPause: true });
    assert.deepEqual(page.clicks, []);
  }
});

test("observation and expired trigger deadlines never click Community", () => {
  const observed = fixture();
  assert.equal(observed.run({ click: false }).code, "observed");
  assert.deepEqual(observed.clicks, []);
  for (const deadline of [1_010, 1_009, NaN, Infinity]) {
    const expired = fixture();
    assert.equal(expired.run({ deadline }).code, "community_trigger_deadline");
    assert.deepEqual(expired.clicks, []);
  }
});
