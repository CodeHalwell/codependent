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

  apply(message: Extract<HotReloadMessage, { type: "state" | "apply" }>): void {
    if (message.type === "state") {
      this.#states.clear();
      Object.entries(message.states).forEach(([key, value]) => this.#states.set(key, value));
    }
    this.#generation = message.generation;
    this.#listeners.forEach((listeners) => listeners.forEach((listener) => listener()));
  }
}
