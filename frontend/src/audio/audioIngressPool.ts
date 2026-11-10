import { sourceWorkerPool } from '../core/sourceWorkerPool';
export type { SourceWorkerClient as AudioIngressClient } from '../core/sourceWorkerPool';
export const audioIngressClient = sourceWorkerPool(() =>
  new Worker(new URL('./sdrAudioWorker.ts', import.meta.url), { type: 'module' }));
