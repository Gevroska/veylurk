const MAX_REQUEST_BYTES = 64 * 1024;
const MAX_RESPONSE_BYTES = 1024 * 1024;
const OPERATIONS = new Set(['CommunityTab', 'ChatViewers']);
const CHANNEL_KEYS = new Set(['login', 'channelLogin', 'channelName', 'userLogin', 'channel']);
const READ_TIMEOUT_MS = 2_000;

async function withinReadDeadline(promise) {
  let timer;
  const expired = Symbol('expired');
  try {
    return await Promise.race([
      Promise.resolve(promise).catch(() => expired),
      new Promise(resolve => { timer = setTimeout(() => resolve(expired), READ_TIMEOUT_MS); }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function requestChannel(variables) {
  if (!variables || typeof variables !== 'object' || Array.isArray(variables)) return null;
  const found = [];
  const visit = (value, depth) => {
    if (!value || typeof value !== 'object' || Array.isArray(value) || depth > 2) return;
    for (const [key, child] of Object.entries(value)) {
      if (CHANNEL_KEYS.has(key) && typeof child === 'string') found.push(child.toLowerCase());
      else if (child && typeof child === 'object') visit(child, depth + 1);
    }
  };
  visit(variables, 0);
  return found.length && new Set(found).size === 1 ? found[0] : null;
}

export function matchingNativeListOperations(requestBody, pagePath, expectedChannel) {
  if (pagePath.toLowerCase() !== `/popout/${expectedChannel.toLowerCase()}/chat`) return [];
  const batch = Array.isArray(requestBody);
  const entries = batch ? requestBody : [requestBody];
  return entries.flatMap((entry, index) => OPERATIONS.has(entry?.operationName)
    && requestChannel(entry.variables) === expectedChannel.toLowerCase()
    ? [{ index, batch }] : []);
}

export function classifyNativeListResponse(status, item) {
  if (status === 401 || status === 403) return { code: 'unavailable', reason: 'native_auth_denied' };
  if (status === 429) return { code: 'unavailable', reason: 'native_rate_limited' };
  if (!item || !Array.isArray(item.errors)) return null;
  for (const error of item.errors) {
    const path = error?.path;
    if (!Array.isArray(path) || path.at(-1) !== 'chatters' || !path.includes('channel')) continue;
    const code = error?.extensions?.code;
    if (code === 'IntegrityCheckFailed') return { code: 'native_integrity_denied', reason: 'native_integrity_denied' };
    if (code === 'UNAUTHENTICATED' || code === 'FORBIDDEN') return { code: 'unavailable', reason: 'native_auth_denied' };
    if (code === 'TOO_MANY_REQUESTS' || code === 'RATE_LIMITED') return { code: 'unavailable', reason: 'native_rate_limited' };
  }
  return null;
}

export function observeNativeListResponses(page, expectedChannel) {
  const requests = new WeakMap();
  let active = true;
  let failure = null;
  const onRequest = request => {
    if (!active || request.method() !== 'POST') return;
    let url;
    try { url = new URL(request.url()); } catch { return; }
    if (url.origin !== 'https://gql.twitch.tv' || url.pathname !== '/gql') return;
    const raw = request.postData();
    if (!raw || Buffer.byteLength(raw) > MAX_REQUEST_BYTES) return;
    let body;
    try { body = JSON.parse(raw); } catch { return; }
    let pagePath;
    try { pagePath = new URL(page.url()).pathname; } catch { return; }
    const hits = matchingNativeListOperations(body, pagePath, expectedChannel);
    if (hits.length) requests.set(request, hits);
  };
  const onResponse = response => {
    if (!active || failure) return;
    const hits = requests.get(response.request());
    if (!hits) return;
    const status = response.status();
    if (status === 401 || status === 403 || status === 429) {
      failure = classifyNativeListResponse(status, null);
      return;
    }
    void (async () => {
      try {
        const contentLength = await withinReadDeadline(response.headerValue('content-length'));
        if (typeof contentLength === 'symbol') return;
        if (contentLength && /^\d+$/.test(contentLength) && Number(contentLength) > MAX_RESPONSE_BYTES) return;
        const bytes = await withinReadDeadline(response.body());
        if (!Buffer.isBuffer(bytes)) return;
        if (!active || bytes.length > MAX_RESPONSE_BYTES) return;
        const payload = JSON.parse(bytes.toString('utf8'));
        for (const hit of hits) {
          if (hit.batch !== Array.isArray(payload)) continue;
          const item = hit.batch ? payload[hit.index] : payload;
          const classified = classifyNativeListResponse(status, item);
          if (active && classified) { failure = classified; break; }
        }
      } catch {
        // An unreadable response is unknown evidence, never a guessed denial.
      }
    })();
  };
  page.on('request', onRequest);
  page.on('response', onResponse);
  return {
    get failure() { return failure; },
    stop() {
      active = false;
      page.off('request', onRequest);
      page.off('response', onResponse);
    },
  };
}
