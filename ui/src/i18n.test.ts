import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { test } from 'node:test';

test('both locales cover the same messages and placeholders', () => {
  const en = JSON.parse(readFileSync(new URL('../../shared/i18n/en-US.json', import.meta.url), 'utf8'));
  const zh = JSON.parse(readFileSync(new URL('../../shared/i18n/zh-CN.json', import.meta.url), 'utf8'));
  assert.deepEqual(Object.keys(en).sort(), Object.keys(zh).sort());
  for (const key of Object.keys(en)) {
    assert.deepEqual(en[key].match(/\{\w+\}/g) ?? [], zh[key].match(/\{\w+\}/g) ?? [], key);
  }
  for (const file of readdirSync(new URL('.', import.meta.url)).filter(file => /\.tsx?$/.test(file) && !file.endsWith('.test.ts'))) {
    const source = readFileSync(new URL(file, import.meta.url), 'utf8');
    for (const [, key] of source.matchAll(/\bt\('([^']+)'/g)) assert.ok(Object.hasOwn(en, key), `${file}: ${key}`);
  }
});
