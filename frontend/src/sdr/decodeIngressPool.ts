import { sourceWorkerPool } from '../core/sourceWorkerPool';
export type { SourceWorkerClient as DecodeIngressClient } from '../core/sourceWorkerPool';
export const decodeIngressClient = sourceWorkerPool(() =>
  new Worker(new URL('./decodeWorker.ts', import.meta.url), { type: 'module' }));
