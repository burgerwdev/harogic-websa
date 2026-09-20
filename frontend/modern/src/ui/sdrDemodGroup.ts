// The SDR demodulator buttons, generated from the DSP module's plugin registry.
//
// The list of modes lives in the Rust registry and is read over the ABI, so this module owns no
// mode list of its own: adding a mode to the DSP is what makes a button appear, and a mode whose
// kernel is not written yet is rendered *disabled* instead of silently accepting a click that
// nothing would answer.
//
// Kept out of `controls.ts` so the rendering can be unit tested without the whole control layer,
// and so the dependency direction stays one-way (controls passes its actions in).
import { analogModeIds, isModeAvailable, loadPluginManifest, pluginManifest } from '../sdr/registry';
import { dspWasmUrl } from '../sdr/wasm';

export interface DemodGroupHooks {
	/** Called with the mode id when the user picks one. */
	onSelect(id: string): void;
}

/** Button label for a mode id (the ids are already short and uppercase-able). */
export function demodLabel(id: string): string {
	return id.toUpperCase();
}

/**
 * Render `ids` into `container`. A mode the registry reports as unimplemented is disabled and
 * marked, so the panel tells the truth about what the build can do.
 */
export function renderSdrDemodGroup(
	container: HTMLElement,
	ids: string[],
	hooks: DemodGroupHooks,
): HTMLButtonElement[] {
	container.textContent = '';
	const buttons: HTMLButtonElement[] = [];
	for (const id of ids) {
		const button = document.createElement('button');
		button.className = 'btn';
		button.dataset.sdrDemod = id;
		button.textContent = demodLabel(id);
		const available = isModeAvailable(id);
		if (!available) {
			button.disabled = true;
			button.title = `${demodLabel(id)}: kernel not implemented yet`;
		}
		button.addEventListener('click', () => {
			if (button.disabled) return;
			hooks.onSelect(id);
		});
		container.appendChild(button);
		buttons.push(button);
	}
	return buttons;
}

/**
 * Load the manifest (once) and render the group. Returns the ids that were rendered, so a test
 * or a status readout can assert the panel and the registry agree.
 */
export async function initSdrDemodGroup(
	container: HTMLElement,
	hooks: DemodGroupHooks,
	url: string = dspWasmUrl(),
): Promise<string[]> {
	try {
		await loadPluginManifest(url);
	} catch {
		// No module (no wasm support, or a fetch failure): the panel stays empty rather than
		// offering modes nothing can run. The Python fallback path serves audio in that case.
		return [];
	}
	// Render from the registry, after the manifest is in place: `isModeAvailable` reads it.
	const ids = analogModeIds();
	renderSdrDemodGroup(container, ids, hooks);
	return ids;
}

/** Re-render from the already-loaded manifest (a preset or a status confirm can change it). */
export function refreshSdrDemodGroup(container: HTMLElement, hooks: DemodGroupHooks): string[] {
	if (!pluginManifest()) return [];
	const ids = analogModeIds();
	renderSdrDemodGroup(container, ids, hooks);
	return ids;
}
