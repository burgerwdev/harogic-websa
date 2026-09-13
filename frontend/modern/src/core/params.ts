/**
 * Parameter slots - one authoritative source per parameter.
 *
 * The project kept a private copy of every backend setting in module variables and wrote
 * them from several places, which produced a recurring class of bugs: a value the user had
 * just set was overwritten by a reply that was already in flight, a pending "hand-off"
 * survived a global reset, and the same parameter had several writers with no owner.
 *
 * A slot owns exactly one parameter and holds:
 *   - `confirmed` - the value the backend last reported (STATUS)
 *   - `desired`   - what the user asked for, kept until the backend confirms it
 *   - `epoch`     - bumped on every intent, so it is possible to reason about which reply
 *                   predates the newest intent
 *
 * The UI always renders `get()` = `desired ?? confirmed ?? fallback`, so:
 *   - an in-flight reply can never visually revert what the user just set;
 *   - a reply older than the newest intent cannot win;
 *   - a rejected command does not stick forever (the intent expires after `ttlMs`).
 *
 * Persistence lives here too, in one place, so a preference cannot be written by one code
 * path and forgotten by another (the SDR auto-scale flag had exactly that bug).
 */

const DEFAULT_TTL_MS = 3000;

export interface ParamOptions<T> {
	/** Used before the backend reports anything and nothing is persisted. */
	fallback: T;
	/** Group cleared by resetAll(scope), e.g. 'sdr' | 'swp' | 'rta'. */
	scope: string;
	/** localStorage key; the value survives a page reload. */
	persistKey?: string;
	/**
	 * Which value is persisted. 'confirmed' (default) stores what the backend actually
	 * accepted; 'desired' stores a client-side preference the backend does not report.
	 */
	persist?: 'confirmed' | 'desired';
	/** How long an unconfirmed intent may override the backend (a rejected command). */
	ttlMs?: number;
	/**
	 * Client-owned value: nothing confirms it, so `desired` never expires and `get()` never
	 * falls back while it is set. Preferences (audio on/off, auto-scale on/off) are of this
	 * kind - with a TTL they would silently revert, and a toggle reading a reverted value
	 * can only ever compute "on" (observed: the SDR audio switch stopped turning off).
	 */
	authoritative?: boolean;
	parse?(raw: string): T | null;
	serialize?(v: T): string;
	equals?(a: T, b: T): boolean;
}

export class Param<T> {
	readonly name: string;
	/** Group cleared by resetAll(scope). */
	readonly scope: string;
	// JS private fields: these are not reachable at runtime, so no caller can bypass
	// set()/confirm() and desync a slot.
	#opts: ParamOptions<T>;
	#ttl: number;
	#confirmed: T | null = null;
	#desired: T | null = null;
	#desiredAt = 0;
	#epoch = 0;

	constructor(name: string, opts: ParamOptions<T>) {
		this.name = name;
		this.scope = opts.scope;
		this.#opts = opts;
		this.#ttl = opts.ttlMs ?? DEFAULT_TTL_MS;

		// A persisted value is the last known backend state: treat it as confirmed so get()
		// is sensible during the first few hundred ms before the first STATUS arrives.
		const raw = this.read();
		if (raw !== null) this.#confirmed = raw;
	}

	private eq(a: T, b: T): boolean {
		return this.#opts.equals ? this.#opts.equals(a, b) : a === b;
	}

	private read(): T | null {
		if (!this.#opts.persistKey || typeof localStorage === 'undefined') return null;
		try {
			const raw = localStorage.getItem(this.#opts.persistKey);
			if (raw === null) return null;
			return this.#opts.parse ? this.#opts.parse(raw) : (raw as unknown as T);
		} catch {
			return null;
		}
	}

	private persist(v: T): void {
		if (!this.#opts.persistKey || typeof localStorage === 'undefined') return;
		try {
			localStorage.setItem(this.#opts.persistKey, this.#opts.serialize ? this.#opts.serialize(v) : String(v));
		} catch {
			/* storage disabled: persistence is best effort */
		}
	}

	/** Epoch of the newest intent (diagnostics / tests). */
	get epoch(): number {
		return this.#epoch;
	}

	confirmedValue(): T | null {
		return this.#confirmed;
	}

	desiredValue(): T | null {
		return this.#desired;
	}

	/** True while an intent is waiting to be confirmed by the backend. */
	pending(now = Date.now()): boolean {
		// An authoritative slot has nothing to wait for (nothing confirms it).
		if (this.#opts.authoritative) return false;
		return this.#desired !== null && this.pendingAge(now) <= this.#ttl;
	}

	/** Age of the pending intent in ms (Infinity when there is none). */
	pendingAge(now = Date.now()): number {
		return this.#desired === null ? Infinity : now - this.#desiredAt;
	}

	/** What the UI must render. */
	get(now = Date.now()): T {
		if (this.#opts.authoritative) {
			return this.#confirmed ?? this.#desired ?? this.#opts.fallback;
		}
		if (this.pending(now)) return this.#desired as T;
		return this.#confirmed ?? this.#opts.fallback;
	}

	/** Record user intent. A repeated identical value keeps the original timestamp. */
	set(v: T): void {
		if (this.#desired !== null && this.eq(v, this.#desired)) return;
		this.#desired = v;
		this.#desiredAt = Date.now();
		this.#epoch++;
		if (this.#opts.persist === 'desired') this.persist(v);
	}

	/**
	 * Apply a backend report. Returns true when the confirmed value changed.
	 *
	 * A contradicting report does not cancel a still-valid intent (the command may simply
	 * not have been applied yet); the intent is cleared when the backend agrees with it, or
	 * dropped once it is older than the TTL (a rejected command must not stick).
	 */
	confirm(v: T): boolean {
		if (!this.#opts.authoritative && this.#desired !== null && this.pendingAge() > this.#ttl) this.#desired = null;
		const changed = this.#confirmed === null || !this.eq(this.#confirmed, v);
		this.#confirmed = v;
		if (this.#desired !== null && this.eq(v, this.#desired)) this.#desired = null;
		if (this.#opts.persist !== 'desired') this.persist(v);
		return changed;
	}

	/** Drop any pending intent (preset / mode switch: the previous intent is meaningless). */
	reset(): void {
		this.#desired = null;
		this.#desiredAt = 0;
	}
}

const slots = new Set<Param<unknown>>();

export function createParam<T>(name: string, opts: ParamOptions<T>): Param<T> {
	const p = new Param<T>(name, opts);
	slots.add(p as unknown as Param<unknown>);
	return p;
}

/**
 * Clear pending intents, by scope or for every parameter.
 *
 * This exists because global resets (Preset) used to clear some fields by hand and missed
 * others - a stale hand-off then reapplied the pre-reset frequency. Resetting is now one
 * call that cannot miss a slot.
 */
export function resetAll(scope?: string): void {
	for (const p of slots) {
		if (scope === undefined || p.scope === scope) p.reset();
	}
}

/** Registered slots (diagnostics). */
export function allParams(): Param<unknown>[] {
	return [...slots];
}
