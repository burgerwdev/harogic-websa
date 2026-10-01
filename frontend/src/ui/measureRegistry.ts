/**
 * Measurement tab registry (report finding E-5).
 *
 * `ui/measure.ts` used to hard-code the four tabs (amp/harm/pnm/chan) in two if/elif chains:
 * one to apply the measurement and one to toggle the tab DOM. The tabs register themselves
 * here instead, so a new measurement is one registration plus its own module.
 *
 * Leaf module: the measurement modules must be able to register without importing the
 * measurement state machine back (which would create a cycle).
 */

export interface MeasurementTab {
	/** Stable id, also the value stored in store.measTabSel. */
	id: string;
	/** Tab button element id in index.html. */
	domId: string;
	/** Run the measurement (the tab's Measure/Set action). */
	apply: () => void;
	/** Refresh the result table after a measurement (optional). */
	updateTable?: () => void;
}

const tabs = new Map<string, MeasurementTab>();

export function registerMeasurementTab(tab: MeasurementTab): void {
	tabs.set(tab.id, tab);
}

export function getMeasurementTab(id: string): MeasurementTab | undefined {
	return tabs.get(id);
}

/** Registered tabs in registration order (UI iteration + diagnostics). */
export function measurementTabs(): MeasurementTab[] {
	return [...tabs.values()];
}
