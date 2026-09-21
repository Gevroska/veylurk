import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { test } from 'node:test';
import { classifyNativeListResponse, matchingNativeListOperations, observeNativeListResponses } from './native-response.mjs';

const channel = 'iliesomg';
const path = `/popout/${channel}/chat`;
const requestBody = { operationName: 'CommunityTab', variables: { login: channel } };
const denied = { errors: [{ message: 'untrusted body text', path: ['user', 'channel', 'chatters'], extensions: { code: 'IntegrityCheckFailed' } }], data: { user: { channel: { chatters: null } } } };

test('matches only the native list operation for the page channel and batch index', () => {
  assert.deepEqual(matchingNativeListOperations([{ operationName: 'DropChannelCampaignsProgress', variables: { login: channel } }, requestBody], path, channel), [{ index: 1, batch: true }]);
  assert.deepEqual(matchingNativeListOperations(requestBody, '/popout/other/chat', channel), []);
  assert.deepEqual(matchingNativeListOperations({ ...requestBody, variables: { login: 'other' } }, path, channel), []);
  assert.deepEqual(matchingNativeListOperations({ ...requestBody, variables: { login: channel, channelLogin: 'other' } }, path, channel), []);
  assert.deepEqual(matchingNativeListOperations({ ...requestBody, variables: {} }, path, channel), []);
  assert.deepEqual(matchingNativeListOperations({ operationName: 'DropChannelCampaignsProgress', variables: { login: channel } }, path, channel), []);
});

test('classifies only chatters denial, auth, and rate limit using fixed codes', () => {
  const result = classifyNativeListResponse(200, denied);
  assert.deepEqual(result, { code: 'native_integrity_denied', reason: 'native_integrity_denied' });
  assert.equal(JSON.stringify(result).includes('untrusted'), false);
  assert.equal(classifyNativeListResponse(200, { errors: [{ path: ['channel', 'other'], extensions: { code: 'IntegrityCheckFailed' } }] }), null);
  assert.deepEqual(classifyNativeListResponse(403, null), { code: 'unavailable', reason: 'native_auth_denied' });
  assert.deepEqual(classifyNativeListResponse(429, null), { code: 'unavailable', reason: 'native_rate_limited' });
});

test('observer correlates the native batch response and detaches cleanly', async () => {
  const page = new EventEmitter();
  page.url = () => `https://www.twitch.tv${path}`;
  const observer = observeNativeListResponses(page, channel);
  const request = {
    method: () => 'POST',
    url: () => 'https://gql.twitch.tv/gql',
    postData: () => JSON.stringify([{ operationName: 'DropChannelCampaignsProgress', variables: { login: channel } }, requestBody]),
  };
  page.emit('request', request);
  page.emit('response', {
    request: () => request,
    status: () => 200,
    headerValue: async () => null,
    body: async () => Buffer.from(JSON.stringify([{ errors: [{ path: ['channel', 'chatters'], extensions: { code: 'IntegrityCheckFailed' } }] }, denied])),
  });
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(observer.failure, { code: 'native_integrity_denied', reason: 'native_integrity_denied' });
  observer.stop();
  assert.equal(page.listenerCount('request'), 0);
  assert.equal(page.listenerCount('response'), 0);
});

test('oversized or unreadable matching responses remain unknown evidence', async () => {
  const page = new EventEmitter();
  page.url = () => `https://www.twitch.tv${path}`;
  const observer = observeNativeListResponses(page, channel);
  const request = { method: () => 'POST', url: () => 'https://gql.twitch.tv/gql', postData: () => JSON.stringify(requestBody) };
  page.emit('request', request);
  page.emit('response', { request: () => request, status: () => 200, headerValue: async () => '2000000', body: async () => { throw new Error('body should not be read'); } });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(observer.failure, null);
  observer.stop();
});

test('an unrelated operation denial in the same batch does not stop collection', async () => {
  const page = new EventEmitter();
  page.url = () => `https://www.twitch.tv${path}`;
  const observer = observeNativeListResponses(page, channel);
  const request = {
    method: () => 'POST', url: () => 'https://gql.twitch.tv/gql',
    postData: () => JSON.stringify([{ operationName: 'DropChannelCampaignsProgress', variables: { login: channel } }, requestBody]),
  };
  page.emit('request', request);
  page.emit('response', {
    request: () => request, status: () => 200, headerValue: async () => null,
    body: async () => Buffer.from(JSON.stringify([denied, { data: { user: { channel: { chatters: { viewers: [] } } } } }])),
  });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(observer.failure, null);
  observer.stop();
});
