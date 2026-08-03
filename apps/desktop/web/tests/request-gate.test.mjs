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
