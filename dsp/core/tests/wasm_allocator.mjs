// Run in a separate Node process after rebuilding dsp.wasm:
// node dsp/core/tests/wasm_allocator.mjs [path/to/dsp.wasm]
// Native unit tests cover checked arithmetic without allocating. This exercises
// the actual 32-bit exports; every large request must fail before heap growth.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const path = process.argv[2] ?? new URL('../../../frontend/public/dsp.wasm', import.meta.url);
const module = await WebAssembly.compile(await readFile(path));
const imports = {};
for (const entry of WebAssembly.Module.imports(module)) {
  assert.equal(entry.kind, 'function', `unexpected import ${entry.module}.${entry.name}`);
  (imports[entry.module] ??= {})[entry.name] = () => {
    throw new Error(`allocator reached ${entry.module}.${entry.name}`);
  };
}
const instance = await WebAssembly.instantiate(module, imports);
const { malloc, calloc, realloc, free, memory } = instance.exports;
for (const fn of [malloc, calloc, realloc, free]) assert.equal(typeof fn, 'function');
assert.ok(memory instanceof WebAssembly.Memory);

function recovery() {
  const ptr = malloc(31) >>> 0;
  assert.notEqual(ptr, 0);
  assert.equal(ptr % 16, 0);
  const expected = Uint8Array.from({ length: 31 }, (_, i) => i * 7 + 1);
  new Uint8Array(memory.buffer, ptr, expected.length).set(expected);
  const sizeBeforeFailure = memory.buffer.byteLength;
  assert.equal(realloc(ptr, 0xffff_ffff), 0);
  assert.equal(memory.buffer.byteLength, sizeBeforeFailure);
  assert.deepEqual(new Uint8Array(memory.buffer, ptr, expected.length), expected,
    'failed realloc must retain the original allocation');
  const grown = realloc(ptr, 97) >>> 0;
  assert.notEqual(grown, 0);
  assert.deepEqual(new Uint8Array(memory.buffer, grown, expected.length), expected);
  free(grown);

  const zeros = calloc(9, 7) >>> 0;
  assert.notEqual(zeros, 0);
  assert.deepEqual(new Uint8Array(memory.buffer, zeros, 63), new Uint8Array(63));
  free(zeros);
  free(0);
}

recovery();
for (const size of [0xffff_ffff, 0xffff_fff0, 0x8000_0000, 0x7fff_fff0]) {
  const before = memory.buffer.byteLength;
  assert.equal(malloc(size), 0, `malloc(${size})`);
  assert.equal(memory.buffer.byteLength, before);
  recovery();
}
for (const [count, size] of [[0x8000_0000, 2], [0xffff_ffff, 2], [0x1_0000, 0x1_0000]]) {
  const before = memory.buffer.byteLength;
  assert.equal(calloc(count, size), 0, `calloc(${count}, ${size})`);
  assert.equal(memory.buffer.byteLength, before);
  recovery();
}
console.log('wasm allocator: overflow rejected without heap growth; allocation/reallocation recovered');
