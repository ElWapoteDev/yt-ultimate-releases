import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  PlaybackRequests, isCanceledRequest, retryStreamPath, streamMedia, waitForRecovery,
} from '../../src/lib/stream.js';

test('new selections abort previous requests and ignore late completions', () => {
  const requests = new PlaybackRequests();
  const first = requests.begin();
  const second = requests.begin();
  assert.equal(first.signal.aborted, true);
  assert.equal(first.isCurrent(), false);
  first.finish();
  assert.equal(second.isCurrent(), true);
  assert.equal(requests.pending, true);
  second.finish();
  assert.equal(requests.pending, false);
});

test('local playback cancels pending remote resolution', () => {
  const requests = new PlaybackRequests();
  const stream = requests.begin();
  requests.cancel();
  assert.equal(stream.isCurrent(), false);
  assert.equal(stream.signal.aborted, true);
});

test('recovery backoff aborts without waiting or making another request', async () => {
  const controller = new AbortController();
  const pending = waitForRecovery(60_000, controller.signal);
  controller.abort();
  await assert.rejects(pending, (error) => isCanceledRequest(error));
  await assert.rejects(waitForRecovery(60_000, controller.signal), { name: 'AbortError' });
});

test('queue stores stable proxy references and never upstream signed URLs', () => {
  const item = { id: 'example', url: 'https://www.youtube.com/watch?v=example', title: 'Track' };
  const stream = streamMedia(item, { url: 'https://cdn.example/audio?expire=1' });
  assert.equal(stream.id, 'stream:example');
  assert.equal(stream.name, 'Track');
  assert.equal(stream._streamUrl, `/stream/proxy?url=${encodeURIComponent(item.url)}`);
  assert.equal(streamMedia(stream, { title: 'Updated' }).id, stream.id);
  assert.equal(retryStreamPath(stream._streamUrl, 2), `${stream._streamUrl}&playback_retry=2`);
});
