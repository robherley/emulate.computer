import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ConnectionHistory } from '../src/network/connections.ts';

test('tracks TCP lifecycle and retains closed connections', () => {
  const history = new ConnectionHistory();
  history.start(1, 'RESOLVE example.com');
  assert.deepEqual(history.snapshot(), []);
  history.start(2, 'CONNECT 93.184.216.34:443');
  assert.equal(history.snapshot()[0].state, 'Connecting');
  history.reply(2, 'OK');
  assert.equal(history.snapshot()[0].state, 'Connected');
  history.start(2, 'FIN');
  history.close(2);
  assert.deepEqual(history.snapshot(), [{ id: 2, destination: '93.184.216.34:443', state: 'Closed' }]);
});

test('preserves relay errors and reports interrupted attempts as failed', () => {
  const history = new ConnectionHistory();
  history.start(1, 'CONNECT 10.0.2.2:80');
  history.reply(1, 'ERR connection refused');
  history.close(1);
  history.reply(1, 'OK');
  assert.equal(history.snapshot()[0].state, 'Failed');
  assert.equal(history.snapshot()[0].error, 'connection refused');
  history.start(2, 'CONNECT 10.0.2.2:81');
  history.close(2);
  assert.equal(history.snapshot()[0].state, 'Failed');
  history.start(3, 'CONNECT 10.0.2.2:82');
  history.reply(3, 'OK');
  history.close(3, true);
  assert.equal(history.snapshot()[0].state, 'Failed');
});

test('keeps only the latest 20 and returns independent snapshots', () => {
  const history = new ConnectionHistory();
  for (let id = 1; id <= 25; id++) history.start(id, `CONNECT 10.0.2.2:${id}`);
  const snapshot = history.snapshot();
  assert.equal(snapshot.length, 20);
  assert.equal(snapshot[0].id, 25);
  assert.equal(snapshot[19].id, 6);
  history.close(1);
  snapshot[0].state = 'Closed';
  assert.equal(history.snapshot()[0].state, 'Connecting');
});
