import assert from 'node:assert/strict';
import { chromium } from 'playwright';
import { collectPanel, sanitizeFailure } from './helper.mjs';

const safe = sanitizeFailure({ name: 'SecurityError', message: 'secret net::ERR_BLOCKED_BY_CLIENT private text' }, 'navigation');
assert.equal(safe.error_class, 'SecurityError');
assert.equal(safe.network_code, 'net::ERR_BLOCKED_BY_CLIENT');
assert.equal(JSON.stringify(safe).includes('private text'), false);

const fixture = new URL('./fixtures/chat.html', import.meta.url);
const browser = await chromium.launch({ channel: 'chromium', headless: true, chromiumSandbox: true });
try {
  const context = await browser.newContext({ locale: 'en-US' });
  try {
    const page = await context.newPage();
    await page.goto(fixture.href);
    const first = await collectPanel(page);
    assert.deepEqual(first.usernames, ['Alice_1']);
    assert.equal(first.role_lists, 1);
    assert.equal(await page.locator('#cookie-banner').count(), 0);
    const second = await collectPanel(page);
    assert.deepEqual(second.usernames, ['Bob_2']);
    assert.equal(second.role_lists, 1);
    const delayedPage = await context.newPage();
    await delayedPage.goto(`${fixture.href}?delayed-consent`);
    const recovered = await collectPanel(delayedPage);
    assert.deepEqual(recovered.usernames, ['Bob_2']);
    assert.equal(await delayedPage.locator('#cookie-banner').count(), 0);
    await page.evaluate(() => { document.title = 'Verify you are human'; });
    await assert.rejects(() => collectPanel(page), error => error.code === 'challenge');
  } finally {
    await context.close();
  }
} finally {
  await browser.close();
}
process.stdout.write('offline helper fixture passed\n');
