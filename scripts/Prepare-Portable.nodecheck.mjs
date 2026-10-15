import test from 'node:test';
import assert from 'node:assert/strict';
import { portablePayload } from './Prepare-Portable.mjs';

function fixture() {
  const source = Buffer.alloc(512);
  source.write('MZ');
  source.writeUInt32LE(0x80, 0x3c);
  source.write('PE\0\0', 0x80);
  source.writeUInt16LE(240, 0x94);
  source.writeUInt16LE(0x20b, 0x98);
  source.writeUInt32LE(16, 0x98 + 108);
  source.write('__TAURI_BUNDLE_TYPE_VAR_UNK', 0x1c0);
  source.write('__TAURI_BUNDLE_TYPE_VAR_NSS', 0x1e0);
  return source;
}

test('matches the NSIS payload marker without changing source bytes or other data', () => {
  const source = fixture();
  const original = Buffer.from(source);
  const expected = fixture();
  expected.write('__TAURI_BUNDLE_TYPE_VAR_NSS', 0x1c0);
  const actual = portablePayload(source);
  assert.deepEqual(actual, expected);
  assert.deepEqual(source, original);
  assert.equal(actual.length, source.length);
});

test('rejects a non-PE input', () => {
  assert.throws(() => portablePayload(Buffer.from('__TAURI_BUNDLE_TYPE_VAR_UNK')), /Windows PE/);
  assert.throws(() => portablePayload(fixture().subarray(0, 0xa0)), /Windows PE/);
});

test('rejects missing and ambiguous runtime markers', () => {
  const missing = fixture();
  missing.write('__TAURI_BUNDLE_TYPE_VAR_NSS', 0x1c0);
  assert.throws(() => portablePayload(missing), /exactly one/);
  const duplicate = fixture();
  duplicate.write('__TAURI_BUNDLE_TYPE_VAR_UNK', 0x1e0);
  assert.throws(() => portablePayload(duplicate), /exactly one/);
});

test('refuses to invalidate an Authenticode signature', () => {
  const signed = fixture();
  signed.writeUInt32LE(512, 0x98 + 112 + 32);
  signed.writeUInt32LE(64, 0x98 + 112 + 36);
  assert.throws(() => portablePayload(signed), /before Authenticode signing/);
});
