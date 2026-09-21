import { chromium } from 'playwright';
import { createInterface } from 'node:readline';
import { writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const VERSION = 1;
const MAX_REQUEST_BYTES = 64 * 1024;
const MAX_REPLY_BYTES = 1024 * 1024;
const MAX_SCREENSHOT_BYTES = 8 * 1024 * 1024;
const ROW_SELECTOR = 'button[data-test-selector="chat-viewers-list__button"][data-username]';
const ROLE_SELECTOR = '[aria-labelledby^="chat-viewers-list-header-"]';
const INPUT_SELECTOR = 'input[aria-label="Search Chat Viewers"]';
const TOGGLE_SELECTOR = 'button[data-test-selector="chat-viewer-list"]';
const LOGIN_REGEX = /^[a-zA-Z0-9_]{1,25}$/;

class PanelFailure extends Error {
  constructor(code, reason) {
    super(code);
    this.code = code;
    this.reason = reason;
  }
}

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const ERROR_CLASSES = new Set(['TypeError', 'ReferenceError', 'SyntaxError', 'RangeError', 'SecurityError', 'NetworkError', 'TimeoutError']);

export function sanitizeFailure(error, phase, pageErrors = { count: 0, lastClass: null }) {
  const code = allowedCodes.has(error?.code) ? error.code : 'ui_changed';
  const reason = allowedReasons.has(error?.reason) ? error.reason : 'browser_operation_failed';
  const errorClass = ERROR_CLASSES.has(error?.name) ? error.name : 'OtherError';
  const networkCode = String(error?.message || '').match(/net::ERR_[A-Z_]{1,48}/)?.[0] || null;
  return {
    code,
    reason,
    phase,
    error_class: errorClass,
    network_code: networkCode,
    page_error_class: pageErrors.lastClass,
    page_error_count: Math.min(pageErrors.count, 1000),
  };
}

async function snapshot(page) {
  return page.evaluate(({ row, role, input, toggle }) => ({
    ready_state: document.readyState,
    document_lang: String(document.documentElement?.lang || '').slice(0, 24),
    known_error_title_present: /access denied|verify you are human|unusual traffic/i.test(document.title),
    challenge_element_present: Boolean(document.querySelector('iframe[src*="captcha" i], iframe[src*="challenge" i], [data-a-target*="captcha" i], form[action*="challenge" i]')),
    viewer_toggle_present: Boolean(document.querySelector(toggle)),
    viewer_input_present: Boolean(document.querySelector(input)),
    role_lists: document.querySelectorAll(role).length,
    rendered_row_count: document.querySelectorAll(row).length,
    login_prompt_present: Boolean(document.querySelector('button[data-a-target="login-button"], a[data-a-target="login-button"]')),
  }), { row: ROW_SELECTOR, role: ROLE_SELECTOR, input: INPUT_SELECTOR, toggle: TOGGLE_SELECTOR });
}

async function stopOnChallenge(page) {
  const state = await snapshot(page);
  if (state.known_error_title_present || state.challenge_element_present) {
    throw new PanelFailure('challenge', 'challenge_indicator');
  }
  return state;
}

async function rejectKnownConsent(page) {
  const title = page.getByText('Cookies and Advertising Choices', { exact: true }).first();
  if (await title.count() === 0) return false;
  let scope = title;
  for (let depth = 0; depth < 6; depth += 1) {
    scope = scope.locator('xpath=..');
    const accept = scope.getByRole('button', { name: 'Accept', exact: true });
    const customize = scope.getByRole('button', { name: 'Customize', exact: true });
    const reject = scope.getByRole('button', { name: 'Reject', exact: true });
    if (await accept.count() && await customize.count() && await reject.count()) {
      await reject.first().click({ timeout: 3_000 });
      return true;
    }
  }
  return false;
}

async function closePanel(page) {
  if (await page.locator(INPUT_SELECTOR).count() === 0) return true;
  const close = page.locator('button[aria-label="Close"][data-a-target="chat-viewer-list"]').first();
  const back = page.getByRole('button', { name: 'Go back to chat', exact: true }).first();
  if (await close.count()) await close.click({ timeout: 2_000 });
  else if (await back.count()) await back.click({ timeout: 2_000 });
  else await page.locator(TOGGLE_SELECTOR).click({ timeout: 2_000 });
  try {
    await page.locator(INPUT_SELECTOR).waitFor({ state: 'detached', timeout: 2_000 });
    return true;
  } catch {
    return false;
  }
}

export async function collectPanel(page) {
  const deadline = Date.now() + 20_000;
  let consentRejected = false;
  await stopOnChallenge(page);
  if (!await closePanel(page)) throw new PanelFailure('ui_changed', 'stale_panel_close_failed');

  let opened = false;
  while (Date.now() < deadline) {
    await stopOnChallenge(page);
    if (!consentRejected && await rejectKnownConsent(page)) {
      consentRejected = true;
      await sleep(100);
      continue;
    }
    if (await page.locator(TOGGLE_SELECTOR).count()) {
      await page.locator(TOGGLE_SELECTOR).click({ timeout: 2_000 });
      opened = true;
      break;
    }
    await sleep(150);
  }
  if (!opened) throw new PanelFailure('unavailable', 'viewer_toggle_missing');

  let inputObserved = false;
  let reopened = false;
  let reopenedInputObserved = false;
  while (Date.now() < deadline) {
    const state = await stopOnChallenge(page);
    if (!consentRejected && await rejectKnownConsent(page)) {
      consentRejected = true;
      await sleep(100);
      continue;
    }
    if (state.rendered_row_count > 0) break;
    if (state.viewer_input_present) {
      inputObserved = true;
      if (reopened) reopenedInputObserved = true;
    } else if (inputObserved && !reopened) {
      if (!state.viewer_toggle_present) throw new PanelFailure('unavailable', 'viewer_toggle_missing_after_panel_disappeared');
      await page.locator(TOGGLE_SELECTOR).click({ timeout: 2_000 });
      reopened = true;
    } else if (reopenedInputObserved) {
      throw new PanelFailure('unavailable', 'panel_disappeared_after_reopen');
    }
    await sleep(150);
  }
  const firstRows = await stopOnChallenge(page);
  if (firstRows.rendered_row_count === 0) {
    throw new PanelFailure('unavailable', inputObserved ? 'viewer_rows_timeout' : 'panel_input_timeout');
  }

  const found = new Set();
  let scrollRounds = 0;
  let unchanged = 0;
  let reachedEnd = false;
  while (scrollRounds < 40 && unchanged < 3 && Date.now() < deadline) {
    await stopOnChallenge(page);
    const before = found.size;
    for (const login of await page.locator(ROW_SELECTOR).evaluateAll(rows => rows.map(row => row.getAttribute('data-username')))) {
      if (!LOGIN_REGEX.test(login || '')) throw new PanelFailure('ui_changed', 'invalid_username');
      found.add(login);
    }
    reachedEnd = await page.locator(ROLE_SELECTOR).evaluateAll(lists => {
      const scrollables = [...new Set(lists.map(list => {
        let node = list;
        while (node && node !== document.body) {
          if (node.scrollHeight > node.clientHeight + 2) return node;
          node = node.parentElement;
        }
        return list;
      }))];
      for (const scroller of scrollables) {
        scroller.scrollTop = Math.min(scroller.scrollTop + Math.max(scroller.clientHeight * 0.8, 300), scroller.scrollHeight);
      }
      return scrollables.length > 0 && scrollables.every(scroller => scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2);
    });
    unchanged = found.size === before ? unchanged + 1 : 0;
    scrollRounds += 1;
    await sleep(100);
  }
  for (const login of await page.locator(ROW_SELECTOR).evaluateAll(rows => rows.map(row => row.getAttribute('data-username')))) {
    if (!LOGIN_REGEX.test(login || '')) throw new PanelFailure('ui_changed', 'invalid_username');
    found.add(login);
  }
  const state = await stopOnChallenge(page);
  if (found.size === 0) throw new PanelFailure('unavailable', 'viewer_rows_timeout');
  if (!await closePanel(page)) throw new PanelFailure('ui_changed', 'sample_panel_close_failed');
  return {
    status: 'ok',
    reason: 'sample_complete',
    usernames: [...found],
    role_lists: state.role_lists,
    scroll_rounds: scrollRounds,
    reached_end: reachedEnd,
    ready_state: state.ready_state,
    document_lang: state.document_lang,
    known_error_title_present: state.known_error_title_present,
    viewer_toggle_present: state.viewer_toggle_present,
    viewer_input_present: state.viewer_input_present,
    rendered_row_count: state.rendered_row_count,
    login_prompt_present: state.login_prompt_present,
  };
}

const allowedReasons = new Set([
  'challenge_indicator', 'viewer_toggle_missing', 'viewer_toggle_missing_after_panel_disappeared',
  'stale_panel_close_failed', 'panel_disappeared_after_reopen', 'viewer_rows_timeout',
  'panel_input_timeout', 'invalid_username', 'sample_panel_close_failed',
  'unexpected_origin', 'unexpected_channel', 'browser_operation_failed',
]);
const allowedCodes = new Set(['challenge', 'unavailable', 'ui_changed', 'protocol', 'browser_launch']);

async function captureFailure(page, filePath) {
  if (!filePath || typeof filePath !== 'string') return;
  try {
    const image = await page.screenshot({ type: 'png', timeout: 3_000 });
    if (image.length <= MAX_SCREENSHOT_BYTES) await writeFile(filePath, image);
  } catch {
    // Screenshot failure cannot replace the collection failure.
  }
}

function reply(value) {
  const serialized = JSON.stringify(value);
  if (Buffer.byteLength(serialized) > MAX_REPLY_BYTES) {
    process.stdout.write(`${JSON.stringify({ v: VERSION, id: value.id, ok: false, error: { ...sanitizeFailure(null, 'panel'), code: 'protocol' } })}\n`);
  } else {
    process.stdout.write(`${serialized}\n`);
  }
}

async function serve() {
  let browser;
  let context;
  const pages = new Map();
  const pageErrors = new Map();
  let lastId = 0;
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
  try {
    for await (const line of input) {
      let request;
      try {
        if (Buffer.byteLength(line) > MAX_REQUEST_BYTES) throw new Error('request too large');
        request = JSON.parse(line);
        if (!request || typeof request !== 'object' || Array.isArray(request)) throw new Error('invalid request');
      } catch {
        reply({ v: VERSION, id: 0, ok: false, error: { ...sanitizeFailure(null, 'panel'), code: 'protocol' } });
        break;
      }
      const id = Number.isSafeInteger(request.id) && request.id > 0 ? request.id : 0;
      if (request.v !== VERSION || id !== lastId + 1 || !['init', 'collect', 'shutdown'].includes(request.op)) {
        reply({ v: VERSION, id, ok: false, error: { ...sanitizeFailure(null, 'panel'), code: 'protocol' } });
        break;
      }
      lastId = id;
      const expectedFields = request.op === 'init' ? ['v', 'id', 'op', 'channels']
        : request.op === 'collect' ? ['v', 'id', 'op', 'channel', 'failure_screenshot'] : ['v', 'id', 'op'];
      if (Object.keys(request).some(key => !expectedFields.includes(key))
          || request.op === 'collect' && (typeof request.channel !== 'string' ||
            !(request.failure_screenshot === null || typeof request.failure_screenshot === 'string' && request.failure_screenshot.length <= 4096))) {
        reply({ v: VERSION, id, ok: false, error: { ...sanitizeFailure(null, 'panel'), code: 'protocol' } });
        break;
      }
      if (request.op === 'shutdown') {
        reply({ v: VERSION, id, ok: true, ready: true });
        break;
      }
      if (request.op === 'init') {
        if (browser || !Array.isArray(request.channels) || request.channels.length < 1 || request.channels.length > 3
            || request.channels.some(channel => typeof channel !== 'string' || !LOGIN_REGEX.test(channel))
            || new Set(request.channels).size !== request.channels.length) {
          reply({ v: VERSION, id, ok: false, error: { ...sanitizeFailure(null, 'panel'), code: 'protocol' } });
          break;
        }
        try {
          browser = await chromium.launch({ channel: 'chromium', headless: true, chromiumSandbox: true, timeout: 15_000 });
          context = await browser.newContext({ locale: 'en-US', viewport: { width: 1280, height: 720 } });
          context.setDefaultTimeout(3_000);
          await Promise.all(request.channels.map(async channel => {
            const page = await context.newPage();
            pages.set(channel, page);
            const errors = { count: 0, lastClass: null };
            pageErrors.set(channel, errors);
            page.on('pageerror', error => {
              errors.count = Math.min(errors.count + 1, 1000);
              errors.lastClass = ERROR_CLASSES.has(error?.name) ? error.name : 'OtherError';
            });
            await page.goto(`https://www.twitch.tv/popout/${channel}/chat?popout=`, { waitUntil: 'domcontentloaded', timeout: 15_000 });
          }));
          reply({ v: VERSION, id, ok: true, ready: true });
        } catch (error) {
          const phase = browser ? 'navigation' : 'launch';
          reply({ v: VERSION, id, ok: false, error: { ...sanitizeFailure(error, phase), code: 'browser_launch' } });
          break;
        }
        continue;
      }
      const page = typeof request.channel === 'string' ? pages.get(request.channel) : null;
      if (!page) {
        reply({ v: VERSION, id, ok: false, error: { ...sanitizeFailure(null, 'panel'), code: 'protocol' } });
        break;
      }
      try {
        const url = new URL(page.url());
        if (url.origin !== 'https://www.twitch.tv') throw new PanelFailure('ui_changed', 'unexpected_origin');
        if (url.pathname.toLowerCase() !== `/popout/${request.channel.toLowerCase()}/chat`) throw new PanelFailure('ui_changed', 'unexpected_channel');
        const sample = await collectPanel(page);
        const finalUrl = new URL(page.url());
        if (finalUrl.origin !== 'https://www.twitch.tv') throw new PanelFailure('ui_changed', 'unexpected_origin');
        if (finalUrl.pathname.toLowerCase() !== `/popout/${request.channel.toLowerCase()}/chat`) throw new PanelFailure('ui_changed', 'unexpected_channel');
        reply({ v: VERSION, id, ok: true, sample: { ...sample, channel: request.channel, origin: finalUrl.origin } });
      } catch (error) {
        await captureFailure(page, request.failure_screenshot);
        const phase = ['viewer_rows_timeout', 'invalid_username', 'panel_disappeared_after_reopen'].includes(error?.reason) ? 'rows' : 'panel';
        reply({ v: VERSION, id, ok: false, error: sanitizeFailure(error, phase, pageErrors.get(request.channel)) });
      }
    }
  } finally {
    try { await context?.close({ timeout: 3_000 }); } catch {}
    try { await browser?.close({ timeout: 3_000 }); } catch {}
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await serve();
}
