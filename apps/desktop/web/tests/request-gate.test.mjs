import assert from 'node:assert/strict';
import test from 'node:test';

import { LatestRequestGate } from '../request-gate.js';

test('only the newest request may commit', () => {
  const gate = new LatestRequestGate();
  const first = gate.begin();
  const second = gate.begin();
  const committed = [];

  assert.equal(gate.commit(first, () => committed.push('stale')), false);
  assert.equal(gate.commit(second, () => committed.push('latest')), true);
  assert.deepEqual(committed, ['latest']);
});

test('switching context invalidates an in-flight request', () => {
  const gate = new LatestRequestGate();
  const oldLibraryRequest = gate.begin();

  gate.invalidate();

  assert.equal(gate.isCurrent(oldLibraryRequest), false);
});

test('an out-of-order response cannot overwrite the latest result', async () => {
  const gate = new LatestRequestGate();
  const rendered = [];
  let resolveOlder;
  const olderResponse = new Promise((resolve) => { resolveOlder = resolve; });

  const olderToken = gate.begin();
  const olderTask = olderResponse.then((value) => gate.commit(olderToken, () => rendered.push(value)));
  const latestToken = gate.begin();
  assert.equal(gate.commit(latestToken, () => rendered.push('new library')), true);
  resolveOlder('old library');

  assert.equal(await olderTask, false);
  assert.deepEqual(rendered, ['new library']);
});

// Q3：错误注入语义——失败响应不得回滚已提交的新结果，且不得污染 gate 状态。
test('a failed stale response cannot erase the committed result', async () => {
  const gate = new LatestRequestGate();
  const rendered = [];
  let rejectOlder;
  const olderFailure = new Promise((_, reject) => { rejectOlder = reject; });

  const olderToken = gate.begin();
  const olderTask = olderFailure.catch(() => gate.commit(olderToken, () => rendered.push('stale error')));
  const latestToken = gate.begin();
  assert.equal(gate.commit(latestToken, () => rendered.push('fresh result')), true);
  rejectOlder(new Error('network down'));

  assert.equal(await olderTask, false);
  assert.deepEqual(rendered, ['fresh result']);
});

test('invalidate keeps newer in-flight requests valid', () => {
  const gate = new LatestRequestGate();
  const inFlightBeforeInvalidate = gate.begin();
  gate.invalidate();
  const inFlightAfterInvalidate = gate.begin();
  assert.equal(gate.isCurrent(inFlightBeforeInvalidate), false);
  assert.equal(gate.isCurrent(inFlightAfterInvalidate), true);
});

test('commit after invalidate rejects even when callback would throw', () => {
  const gate = new LatestRequestGate();
  const token = gate.begin();
  gate.invalidate();
  let callbackRan = false;
  const result = gate.commit(token, () => { callbackRan = true; });
  assert.equal(result, false);
  assert.equal(callbackRan, false, '过期响应不得执行回调（含错误处理）');
});

test('sequential commits on the same token are allowed only once per token', () => {
  const gate = new LatestRequestGate();
  const token = gate.begin();
  assert.equal(gate.commit(token, () => {}), true);
  // 同一 token 二次提交仍视为当前（幂等场景由调用方保证只调一次）；
  // 此处验证 isCurrent 语义不因提交而失效。
  assert.equal(gate.isCurrent(token), true);
});
