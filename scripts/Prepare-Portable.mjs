import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const unknownBundle = Buffer.from('__TAURI_BUNDLE_TYPE_VAR_UNK');
const nsisBundle = Buffer.from('__TAURI_BUNDLE_TYPE_VAR_NSS');

export function portablePayload(source) {
  const invalidPe = () => { throw new Error('Portable input must be a valid unsigned Windows PE executable.'); };
  if (source.length < 0x40 || source.subarray(0, 2).toString('ascii') !== 'MZ') {
    invalidPe();
  }
  const pe = source.readUInt32LE(0x3c);
  if (pe > source.length - 24 || source.subarray(pe, pe + 4).toString('ascii') !== 'PE\0\0') {
    invalidPe();
  }
  const optional = pe + 24;
  const optionalSize = source.readUInt16LE(pe + 20);
  if (optionalSize < 2 || optional + optionalSize > source.length) invalidPe();
  const magic = source.readUInt16LE(optional);
  const directories = magic === 0x10b ? 96 : magic === 0x20b ? 112 : 0;
  if (!directories || optionalSize < directories + 40 || source.readUInt32LE(optional + directories - 4) < 5) invalidPe();
  const security = optional + directories + 32;
  if (source.readUInt32LE(security) !== 0 || source.readUInt32LE(security + 4) !== 0) {
    throw new Error('Prepare the bundle marker before Authenticode signing; signed PE files must not be modified.');
  }
  const offset = source.indexOf(unknownBundle);
  if (offset < 0 || source.indexOf(unknownBundle, offset + 1) >= 0) {
    throw new Error('Expected exactly one unpatched Tauri bundle marker.');
  }
  // Tauri 2.11.4 applies this same marker to the NSIS payload, then restores
  // its input. Both delivery modes must use identical bytes: older EnvNexus
  // updaters verify an installed executable against the portable asset hash.
  const result = Buffer.from(source);
  nsisBundle.copy(result, offset);
  return result;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [source, destination] = process.argv.slice(2);
  if (!source || !destination) throw new Error('Usage: node Prepare-Portable.mjs <built-exe> <portable-exe>');
  if (path.resolve(source).toLowerCase() === path.resolve(destination).toLowerCase()) {
    throw new Error('The portable destination must differ from the compiler output.');
  }
  const payload = portablePayload(fs.readFileSync(source));
  fs.writeFileSync(destination, payload);
  process.stdout.write(JSON.stringify({
    portable: path.resolve(destination),
    sha256: crypto.createHash('sha256').update(payload).digest('hex'),
    bytes: payload.length,
  }) + '\n');
}
