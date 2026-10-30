// Catalog presentation; receiver edits remain owned by controls.ts.
import { onLangChange, t } from '../core/i18n';
import { isModeAvailable, loadPluginManifest, modeDescriptor, pluginManifest, sdrModeIds } from '../sdr/registry';

export interface DemodGroupHooks {
	onSelect(id: string): void;
	selected?(): string;
	canSelect?(): boolean;
}
interface Picker {
	hooks: DemodGroupHooks;
	common: HTMLElement;
	search: HTMLInputElement;
	quick: HTMLElement;
	more: HTMLButtonElement;
	dialog: HTMLDialogElement;
	drawerSearch: HTMLInputElement;
	categories: HTMLSelectElement;
	results: HTMLElement;
	status: HTMLElement;
	selected: string;
}
const pickers = new WeakMap<HTMLElement, Picker>();

export function demodLabel(id: string): string {
	if (id === 'drmplus') return 'DRM+';
	if (id === 'radiofax') return 'FAX';
	return id.toUpperCase();
}

function categoryLabel(category: string): string {
	const key = `sdr_modes_category_${category}`;
	const translated = t(key);
	return translated === key ? category : translated;
}

/** Shared button action for common modes, quick matches and drawer results. */
export function renderSdrDemodGroup(container: HTMLElement, ids: string[], hooks: DemodGroupHooks): HTMLButtonElement[] {
	container.replaceChildren();
	return ids.map((id) => {
		const descriptor = modeDescriptor(id);
		const button = container.ownerDocument.createElement('button');
		button.type = 'button';
		button.className = 'btn';
		button.dataset.sdrDemod = id;
		button.textContent = demodLabel(id);
		button.disabled = !isModeAvailable(id) || hooks.canSelect?.() === false;
		button.title = !isModeAvailable(id) ? descriptor?.unavailableReason || t('sdr_modes_unavailable')
			: hooks.canSelect?.() === false ? t('sdr_modes_read_only') : descriptor?.label ?? demodLabel(id);
		button.setAttribute('aria-pressed', String(id === hooks.selected?.()));
		button.classList.toggle('active', id === hooks.selected?.());
		button.addEventListener('click', () => {
			if (!isModeAvailable(id) || hooks.canSelect?.() === false) return;
			hooks.onSelect(id);
		});
		container.append(button);
		return button;
	});
}

function matchingModes(query: string, category = ''): string[] {
	const words = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
	return sdrModeIds().filter((id) => {
		const mode = modeDescriptor(id)!;
		const haystack = [id, mode.label, mode.description, mode.provider, categoryLabel(mode.category), ...mode.aliases].join(' ').toLocaleLowerCase();
		return (!category || mode.category === category) && words.every((word) => haystack.includes(word));
	});
}

function renderResults(container: HTMLElement, ids: string[], hooks: DemodGroupHooks): void {
	const buttons = renderSdrDemodGroup(container, ids, hooks);
	for (const button of buttons) {
		const mode = modeDescriptor(button.dataset.sdrDemod!)!;
		button.classList.add('sdr-mode-result');
		const name = container.ownerDocument.createElement('strong');
		name.textContent = `${demodLabel(mode.id)} · ${mode.label}`;
		const detail = container.ownerDocument.createElement('span');
		detail.textContent = `${categoryLabel(mode.category)} · ${mode.provider}${mode.description ? ` · ${mode.description}` : ''}`;
		button.replaceChildren(name, detail);
		if (!isModeAvailable(mode.id)) {
			const reason = container.ownerDocument.createElement('span');
			reason.className = 'sdr-mode-unavailable';
			reason.textContent = mode.unavailableReason || t('sdr_modes_unavailable');
			button.append(reason);
		}
	}
	if (!ids.length) {
		const empty = container.ownerDocument.createElement('p');
		empty.textContent = t('sdr_modes_no_matches');
		container.append(empty);
	}
}

function closePicker(picker: Picker, restoreFocus = true): void {
	if (picker.dialog.open) {
		if (typeof picker.dialog.close === 'function') picker.dialog.close();
		else picker.dialog.removeAttribute('open');
	}
	picker.more.setAttribute('aria-expanded', 'false');
	if (restoreFocus) picker.more.focus();
}

/** Project desired/confirmed selection without submitting another receiver edit. */
export function syncSdrDemodGroup(container: HTMLElement, selected: string): void {
	const picker = pickers.get(container);
	if (!picker) return;
	const ids = sdrModeIds().filter((id) => modeDescriptor(id)?.common || id === selected);
	if (picker.selected !== selected || picker.common.childElementCount !== ids.length) {
		const focused = picker.common.contains(container.ownerDocument.activeElement)
			? (container.ownerDocument.activeElement as HTMLElement).dataset.sdrDemod : undefined;
		picker.selected = selected;
		renderSdrDemodGroup(picker.common, ids, picker.hooks);
		if (focused) [...picker.common.querySelectorAll<HTMLButtonElement>('button')]
			.find((button) => button.dataset.sdrDemod === focused)?.focus();
	}
	for (const button of container.querySelectorAll<HTMLButtonElement>('[data-sdr-demod]')) {
		const id = button.dataset.sdrDemod!;
		button.disabled = !isModeAvailable(id) || picker.hooks.canSelect?.() === false;
		button.title = !isModeAvailable(id) ? modeDescriptor(id)?.unavailableReason || t('sdr_modes_unavailable')
			: picker.hooks.canSelect?.() === false ? t('sdr_modes_read_only') : modeDescriptor(id)?.label ?? demodLabel(id);
		button.classList.toggle('active', id === selected);
		button.setAttribute('aria-pressed', String(id === selected));
	}
	picker.status.textContent = t('sdr_modes_selected', { mode: modeDescriptor(selected)?.label ?? demodLabel(selected) });
}

function buildPicker(container: HTMLElement, hooks: DemodGroupHooks): void {
	const previous = pickers.get(container);
	const wasOpen = previous?.dialog.open ?? false;
	const hadFocus = container.contains(container.ownerDocument.activeElement);
	const query = previous?.search.value ?? '';
	const drawerQuery = previous?.drawerSearch.value ?? '';
	const category = previous?.categories.value ?? '';
	if (previous) closePicker(previous, false);
	container.replaceChildren();
	container.classList.add('sdr-mode-picker');
	const doc = container.ownerDocument;
	const common = doc.createElement('div');
	common.className = 'btn-group sdr-mode-common';
	const status = doc.createElement('div');
	status.className = 'sdr-mode-selected';
	status.setAttribute('role', 'status');
	const tools = doc.createElement('div');
	tools.className = 'sdr-mode-tools';
	const search = doc.createElement('input');
	search.type = 'search';
	search.placeholder = t('sdr_modes_search');
	search.setAttribute('aria-label', t('sdr_modes_search'));
	search.setAttribute('aria-controls', 'sdr-mode-quick-results');
	const more = doc.createElement('button');
	more.type = 'button';
	more.className = 'btn';
	more.textContent = t('sdr_modes_more');
	more.setAttribute('aria-haspopup', 'dialog');
	more.setAttribute('aria-expanded', 'false');
	more.setAttribute('aria-controls', 'sdr-mode-drawer');
	const quick = doc.createElement('div');
	quick.id = 'sdr-mode-quick-results';
	quick.className = 'sdr-mode-quick';
	quick.hidden = true;
	const dialog = doc.createElement('dialog');
	dialog.id = 'sdr-mode-drawer';
	dialog.className = 'sdr-mode-drawer';
	dialog.setAttribute('role', 'dialog');
	dialog.setAttribute('aria-modal', 'true');
	dialog.setAttribute('aria-labelledby', 'sdr-mode-drawer-title');
	const header = doc.createElement('div');
	header.className = 'sdr-mode-drawer-header';
	const title = doc.createElement('h2');
	title.id = 'sdr-mode-drawer-title';
	title.textContent = t('sdr_modes_title');
	const close = doc.createElement('button');
	close.type = 'button';
	close.className = 'btn';
	close.textContent = t('sdr_modes_close');
	const drawerSearch = doc.createElement('input');
	drawerSearch.type = 'search';
	drawerSearch.placeholder = t('sdr_modes_search');
	drawerSearch.setAttribute('aria-label', t('sdr_modes_search'));
	const categories = doc.createElement('select');
	categories.setAttribute('aria-label', t('sdr_modes_category'));
	for (const category of ['', ...new Set(sdrModeIds().map((id) => modeDescriptor(id)!.category))]) {
		const option = doc.createElement('option');
		option.value = category;
		option.textContent = category ? categoryLabel(category) : t('sdr_modes_all');
		categories.append(option);
	}
	const results = doc.createElement('div');
	results.className = 'sdr-mode-results';
	const picker: Picker = { hooks, common, status, search, quick, more, dialog, drawerSearch, categories, results, selected: '' };
	pickers.set(container, picker);
	const choose: DemodGroupHooks = { ...hooks, onSelect: (id) => {
		hooks.onSelect(id);
		search.value = '';
		quick.hidden = true;
		if (dialog.open) closePicker(picker);
		else search.focus();
	} };
	const updateDrawer = () => renderResults(results, matchingModes(drawerSearch.value, categories.value), choose);
	const updateQuick = () => {
		quick.hidden = !search.value.trim();
		if (!quick.hidden) renderResults(quick, matchingModes(search.value), choose);
	};
	search.addEventListener('input', updateQuick);
	drawerSearch.addEventListener('input', updateDrawer);
	categories.addEventListener('change', updateDrawer);
	more.addEventListener('click', () => {
		drawerSearch.value = search.value;
		updateDrawer();
		if (typeof dialog.showModal === 'function') dialog.showModal();
		else dialog.setAttribute('open', '');
		more.setAttribute('aria-expanded', 'true');
		drawerSearch.focus();
	});
	close.addEventListener('click', () => closePicker(picker));
	dialog.addEventListener('cancel', (event) => { event.preventDefault(); closePicker(picker); });
	dialog.addEventListener('click', (event) => {
		if (event.target !== dialog) return;
		const bounds = dialog.getBoundingClientRect();
		if (event.clientX < bounds.left || event.clientX > bounds.right
			|| event.clientY < bounds.top || event.clientY > bounds.bottom) closePicker(picker);
	});
	dialog.addEventListener('keydown', (event) => {
		if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); closePicker(picker); }
		if (event.key !== 'Tab') return;
		const focusable = [...dialog.querySelectorAll<HTMLElement>('button:not(:disabled),input,select')];
		const edge = event.shiftKey ? focusable[0] : focusable.at(-1);
		if (doc.activeElement === edge) {
			event.preventDefault();
			(event.shiftKey ? focusable.at(-1) : focusable[0])?.focus();
		}
	});
	const navigateResults = (event: KeyboardEvent, list: HTMLElement, input: HTMLInputElement) => {
		if (event.key === 'Escape' && !dialog.open) {
			event.preventDefault(); search.value = ''; quick.hidden = true; search.focus(); return;
		}
		const buttons = [...list.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')];
		if (event.key === 'Enter' && doc.activeElement === input) { event.preventDefault(); buttons[0]?.click(); }
		if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
		event.preventDefault(); event.stopPropagation();
		const index = buttons.indexOf(doc.activeElement as HTMLButtonElement);
		const next = index < 0 ? (event.key === 'ArrowDown' ? 0 : buttons.length - 1)
			: (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length;
		buttons[next]?.focus();
	};
	search.addEventListener('keydown', (event) => navigateResults(event, quick, search));
	quick.addEventListener('keydown', (event) => navigateResults(event, quick, search));
	drawerSearch.addEventListener('keydown', (event) => navigateResults(event, results, drawerSearch));
	results.addEventListener('keydown', (event) => navigateResults(event, results, drawerSearch));
	header.append(title, close);
	dialog.append(header, drawerSearch, categories, results);
	tools.append(search, more);
	container.append(common, status, tools, quick, dialog);
	syncSdrDemodGroup(container, hooks.selected?.() ?? '');
	search.value = query;
	updateQuick();
	if (wasOpen) {
		more.click();
		drawerSearch.value = drawerQuery;
		categories.value = category;
		updateDrawer();
	} else if (hadFocus) search.focus();
}

export async function initSdrDemodGroup(container: HTMLElement, hooks: DemodGroupHooks, url = '/api/sdr/plugins'): Promise<string[]> {
	try { await loadPluginManifest(url); }
	catch { container.textContent = t('sdr_modes_load_failed'); return []; }
	buildPicker(container, hooks);
	onLangChange(() => { if (container.isConnected) buildPicker(container, hooks); });
	return sdrModeIds();
}

export function refreshSdrDemodGroup(container: HTMLElement, hooks: DemodGroupHooks): string[] {
	if (!pluginManifest()) return [];
	buildPicker(container, hooks);
	return sdrModeIds();
}
