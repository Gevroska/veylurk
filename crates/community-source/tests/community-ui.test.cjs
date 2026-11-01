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
} = {}) {
  const clicks = [];
  let domReads = 0;
  function button(name, attributes) {
    return {
      disabled: false,
      textContent: "",
      getBoundingClientRect: () => ({ width: 30, height: 20 }),
      getAttribute: key => attributes[key] ?? null,
      querySelector: () => null,
      click: () => clicks.push(name),
    };
  }
  const buttons = [];
  if (signedIn) buttons.push(button("avatar", { "aria-label": "User Avatar" }));
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
      assert.equal(selector, "video");
      return videos;
    },
    querySelector(selector) {
      domReads++;
      if (selector === '[data-a-target="chat-input"],.chat-scrollable-area__message-container') {
        return chatReady ? {} : null;
      }
      assert.equal(selector, 'iframe[src*="captcha" i],iframe[src*="challenge" i],form[action*="challenge" i]');
      return challengeFrame ? {} : null;
    },
  };
  function run({ click = true, allowPause = false, deadline = 2_000 } = {}) {
    const script = source
      .replaceAll("__POPOUT__", "false")
      .replaceAll("__CHANNEL__", "strano")
      .replaceAll("__AUTOMATIC__", "true")
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
  return { run, clicks, domReads: () => domReads };
}

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
