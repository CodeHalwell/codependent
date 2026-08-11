import type { UiCapabilities, UiContributionPoint, UiJsonValue, UiNode } from "./protocol.js";

export type ContributionPoint = UiContributionPoint;

export type ContributionTarget = "shared" | "terminal" | "web";

export interface ContributionContext<TData extends UiJsonValue = UiJsonValue> {
  data: TData;
  capabilities: UiCapabilities;
  contributionId: string;
  point: ContributionPoint;
}

export type ContributionRenderer<TData extends UiJsonValue = UiJsonValue> = (context: ContributionContext<TData>) => UiNode;

interface ContributionBase<TData extends UiJsonValue> {
  id: string;
  point: ContributionPoint;
  /** Renderer discriminator, e.g. media type, schema id, tool name, or named host slot. */
  renderer: string;
  render: ContributionRenderer<TData>;
  priority?: number;
  requiredCapabilities?: readonly string[];
}

export type ContributionDefinition<TData extends UiJsonValue = UiJsonValue> =
  | (ContributionBase<TData> & { target: "shared" | "terminal"; terminalFallback?: never })
  | (ContributionBase<TData> & { target: "web"; terminalFallback: ContributionRenderer<TData> | { rendererId: string } });

export interface ContributionRegistration { dispose(): void; }

function registryKey(point: ContributionPoint, renderer: string): string {
  return `${point}\u0000${renderer}`;
}

/** Indexed, deterministic contribution registry. Later registrations do not silently replace an id. */
export class ContributionRegistry {
  readonly #byKey = new Map<string, Map<string, ContributionDefinition>>();
  readonly #byId = new Map<string, ContributionDefinition>();

  register<TData extends UiJsonValue>(definition: ContributionDefinition<TData>): ContributionRegistration {
    if (this.#byId.has(definition.id)) throw new Error(`Contribution id already registered: ${definition.id}`);
    const erased = definition as ContributionDefinition;
    const key = registryKey(definition.point, definition.renderer);
    const group = this.#byKey.get(key) ?? new Map<string, ContributionDefinition>();
    group.set(definition.id, erased);
    this.#byKey.set(key, group);
    this.#byId.set(definition.id, erased);
    return { dispose: () => this.unregister(definition.id) };
  }

  unregister(id: string): boolean {
    const definition = this.#byId.get(id);
    if (definition === undefined) return false;
    this.#byId.delete(id);
    const key = registryKey(definition.point, definition.renderer);
    const group = this.#byKey.get(key);
    group?.delete(id);
    if (group?.size === 0) this.#byKey.delete(key);
    return true;
  }

  get(id: string): ContributionDefinition | undefined { return this.#byId.get(id); }

  list(point?: ContributionPoint): ContributionDefinition[] {
    return [...this.#byId.values()]
      .filter((definition) => point === undefined || definition.point === point)
      .sort((left, right) => (right.priority ?? 0) - (left.priority ?? 0) || left.id.localeCompare(right.id));
  }

  resolve(point: ContributionPoint, renderer: string, capabilities: UiCapabilities): ContributionDefinition[] {
    return [...(this.#byKey.get(registryKey(point, renderer))?.values() ?? [])]
      .filter((definition) => definition.target === "shared" || definition.target === capabilities.client || (definition.target === "web" && capabilities.client !== "terminal"))
      .sort((left, right) => (right.priority ?? 0) - (left.priority ?? 0) || left.id.localeCompare(right.id));
  }

  render<TData extends UiJsonValue>(definition: ContributionDefinition<TData>, data: TData, capabilities: UiCapabilities): UiNode {
    if (definition.target === "web" && capabilities.client === "terminal") {
      if (typeof definition.terminalFallback === "function") {
        return definition.terminalFallback({ data, capabilities, contributionId: definition.id, point: definition.point });
      }
      const fallback = this.get(definition.terminalFallback.rendererId);
      if (fallback === undefined) throw new Error(`Missing terminal fallback ${definition.terminalFallback.rendererId}`);
      return fallback.render({ data, capabilities, contributionId: fallback.id, point: fallback.point });
    }
    return definition.render({ data, capabilities, contributionId: definition.id, point: definition.point });
  }
}

export const contributions = new ContributionRegistry();

export function registerContribution<TData extends UiJsonValue>(definition: ContributionDefinition<TData>): ContributionRegistration {
  return contributions.register(definition);
}
