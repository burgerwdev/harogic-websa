// DeepFilterNet3 dedicated worker — the tract WASM is a ~5.6 ms/hop synchronous call, which would
// starve the iqWorker's WebSocket receive on wide modes (WFM 180 kHz ≈ 240 frames/s, 4.2 ms/frame:
// measured the frame rate dropped to 150/s and the worklet underran). This worker owns the model
// and the stream state, so the iqWorker only posts PCM blocks and receives denoised blocks back,
// never blocking its own receive loop.
import { DfnProcessor, type DfnRuntime, dfnUrls, loadDfn } from './dfnWasm';

let runtime: DfnRuntime | null = null;
let model: Uint8Array | null = null;
let processor: DfnProcessor | null = null;
let atten = 6;

async function init(): Promise<void> {
	if (!runtime || !model) {
		const urls = dfnUrls();
		runtime = await loadDfn(urls);
		model = new Uint8Array(await (await fetch(urls.model)).arrayBuffer());
	}
	processor = new DfnProcessor(runtime, model, atten);
	self.postMessage({ type: 'ready' });
}

self.onmessage = (event: MessageEvent) => {
	const msg = (event.data || {}) as Record<string, any>;
	if (msg.type === 'init') {
		if (typeof msg.atten === 'number') atten = msg.atten;
		init().catch((error) =>
			self.postMessage({ type: 'error', message: String((error as Error)?.message || error) }),
		);
	} else if (msg.type === 'process') {
		// Synchronous on purpose: this worker's own loop may block; the iqWorker must not.
		const out = processor ? processor.process(msg.pcm as Float32Array) : new Float32Array(0);
		(self as any).postMessage({ type: 'pcm', seq: msg.seq as number, out }, [out.buffer]);
	} else if (msg.type === 'atten') {
		atten = Number(msg.value) || 0;
		processor?.setAttenLim(atten);
	} else if (msg.type === 'reset') {
		processor?.reset();
	}
};
