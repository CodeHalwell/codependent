import type { UiJsonValue } from "./protocol.js";

export type HotReloadMessage =
  | { type: "prepare"; generation: number; changedModules: string[] }
  | { type: "state"; generation: number; states: Readonly<Record<string, UiJsonValue>> }
  | { type: "apply"; generation: number }
  | { type: "rollback"; generation: number; reason: string };

export class HotReloadStateStore {
  #generation = 0;
  readonly #states = new Map<string, UiJsonValue>();
  readonly #listeners = new Map<string, Set<() => void>>();

  constructor(initial: Readonly<Record<string, UiJsonValue>> = {}, generation = 0) {
    this.#generation = generation;
    Object.entries(initial).forEach(([key, value]) => this.#states.set(key, structuredClone(value)));
  }

  get generation(): number { return this.#generation; }
  get<T extends UiJsonValue>(key: string, initial: T): T { return (this.#states.get(key) ?? initial) as T; }

  set<T extends UiJsonValue>(key: string, value: T | ((current: T) => T), initial: T): void {
    const current = this.get(key, initial);
    const next = typeof value === "function" ? (value as (current: T) => T)(current) : value;
    this.#states.set(key, next);
    this.#listeners.get(key)?.forEach((listener) => listener());
  }

  subscribe(key: string, listener: () => void): () => void {
    const listeners = this.#listeners.get(key) ?? new Set<() => void>();
    listeners.add(listener);
    this.#listeners.set(key, listeners);
    return () => {
      listeners.delete(listener);
      if (listeners.size === 0) this.#listeners.delete(key);
    };
  }

  prepare(changedModules: string[]): HotReloadMessage {
    return { type: "prepare", generation: this.#generation + 1, changedModules };
  }

  snapshot(): HotReloadMessage {
    return { type: "state", generation: this.#generation, states: Object.fromEntries(this.#states) };
  }

  /** JSON-safe state copied into a replacement worker/workbench generation. */
  exportStates(): Readonly<Record<string, UiJsonValue>> {
    return structuredClone(Object.fromEntries(this.#states));
  }

  apply(message: Extract<HotReloadMessage, { type: "state" | "apply" }>): void {
    if (message.type === "state") {
      this.#states.clear();
      Object.entries(message.states).forEach(([key, value]) => this.#states.set(key, value));
    }
    this.#generation = message.generation;
    this.#listeners.forEach((listeners) => listeners.forEach((listener) => listener()));
  }
}

function workbenchInitialState(): { states: Readonly<Record<string, UiJsonValue>>; generation: number } {
  const processLike = (globalThis as typeof globalThis & {
    process?: { env?: Readonly<Record<string, string | undefined>> };
  }).process;
  const encoded = processLike?.env?.CODYPENDENT_UI_HMR_STATE;
  const rawGeneration = processLike?.env?.CODYPENDENT_UI_HMR_GENERATION;
  if (encoded === undefined || encoded.length > 256 * 1024) return { states: {}, generation: 0 };
  try {
    const parsed = JSON.parse(encoded) as unknown;
    const states = parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)
      ? parsed as Readonly<Record<string, UiJsonValue>>
      : {};
    const generation = Number.parseInt(rawGeneration ?? "0", 10);
    return { states, generation: Number.isSafeInteger(generation) && generation >= 0 ? generation : 0 };
  } catch {
    return { states: {}, generation: 0 };
  }
}

const initialWorkbenchState = workbenchInitialState();

/** Opt-in state store used by generated projects for transactional dev reloads. */
export const workbenchHotReloadState = new HotReloadStateStore(
  initialWorkbenchState.states,
  initialWorkbenchState.generation,
);

export interface HotReloadCandidate<T> {
  value: T;
  states?: Readonly<Record<string, UiJsonValue>>;
}

export type HotReloadTransactionResult<T> =
  | { committed: true; generation: number; value: T }
  | { committed: false; generation: number; value: T; reason: string };

/**
 * Host-side two-phase reload coordinator. A candidate receives the exact
 * JSON-safe state from the last committed generation and cannot replace the
 * active value until its asynchronous preflight succeeds. Failed candidates
 * are discarded and the last-valid generation remains active.
 */
export class TransactionalHotReload<T> {
  #generation = 0;
  #active: T;
  #states: Readonly<Record<string, UiJsonValue>>;

  constructor(initial: HotReloadCandidate<T>, generation = 0) {
    this.#active = initial.value;
    this.#states = structuredClone(initial.states ?? {});
    this.#generation = generation;
  }

  get generation(): number { return this.#generation; }
  get active(): T { return this.#active; }
  get states(): Readonly<Record<string, UiJsonValue>> { return structuredClone(this.#states); }

  async reload(
    prepare: (input: { generation: number; states: Readonly<Record<string, UiJsonValue>> }) => Promise<HotReloadCandidate<T>>,
  ): Promise<HotReloadTransactionResult<T>> {
    const generation = this.#generation + 1;
    try {
      const candidate = await prepare({ generation, states: this.states });
      this.#active = candidate.value;
      this.#states = structuredClone(candidate.states ?? this.#states);
      this.#generation = generation;
      return { committed: true, generation, value: this.#active };
    } catch (cause) {
      return {
        committed: false,
        generation,
        value: this.#active,
        reason: cause instanceof Error ? cause.message : String(cause),
      };
    }
  }
}
