/**
 * Measurement tabs register their actions and DOM identifiers here.
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
