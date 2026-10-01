// Types for the generated ggmorse module (tools/build_ggmorse_wasm.sh writes ggmorse.js next to
// this file; it is an emscripten EXPORT_ES6 module with the wasm embedded, so TS needs the shape
// declared here). Only what the engine uses.
export interface GgMorseModule {
  cwrap(name: string, returnType: string | null, argTypes: string[]): (...args: number[]) => number;
  _malloc(bytes: number): number;
  _free(pointer: number): void;
  HEAP16: Int16Array;
  HEAPU8: Uint8Array;
}

declare const createGgMorse: (options?: Record<string, unknown>) => Promise<GgMorseModule>;
export default createGgMorse;
