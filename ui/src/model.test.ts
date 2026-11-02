import assert from 'node:assert/strict';
import { test } from 'node:test';
import { containerRows, runParams, visibleResources } from './model.ts';
import type { Container } from './types.ts';

test('groups Compose resources without changing stable container identities', () => {
  const items = [
    { id: 'a', name: 'web-2', image: 'nginx', state: 'running', project: 'site', configFiles: '/work/site/compose.yaml,/work/site/override.yaml' },
    { id: 'b', name: 'web-10', image: 'nginx', state: 'exited', project: 'site', configFiles: '/work/site/compose.yaml' },
    { id: 'c', name: 'standalone', image: 'redis', state: 'running', project: null },
  ] as Container[];
  assert.deepEqual(visibleResources(items, 'nginx', true).map(item => 'id' in item && item.id), ['a']);
  const rows = containerRows(items, new Set(['site']));
  assert.equal(rows.filter(row => row.type === 'item').length, 1);
  assert.equal(rows[0].type === 'group' && rows[0].count, 2);
  assert.deepEqual(rows[0].type === 'group' && rows[0].paths, ['/work/site/compose.yaml', '/work/site/override.yaml']);
});

test('run form produces argument arrays and loopback port bindings', () => {
  const form = new FormData();
  form.set('image', 'nginx:alpine'); form.set('ports', '8080:80\n5353:53/udp');
  form.set('environment', 'MODE=dev\nVALUE=a=b'); form.set('volumes', 'cache:/cache:ro');
  form.set('command', '["echo", "$(does-not-run)"]');
  const result = runParams(form);
  assert.deepEqual(result.command, ['echo', '$(does-not-run)']);
  assert.deepEqual(result.ports, [
    { host: '8080', container: '80', protocol: 'tcp', address: '127.0.0.1' },
    { host: '5353', container: '53', protocol: 'udp', address: '127.0.0.1' },
  ]);
  form.set('ports', '70000:80'); assert.throws(() => runParams(form), /invalidPorts/);
  form.set('ports', ''); form.set('command', 'echo hello'); assert.throws(() => runParams(form), /invalidCommand/);
});
