import {
  createContext,
  createElement,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  use,
  type ReactNode,
  type RefObject,
} from "react";
import type { UiCapabilities, UiJsonValue, UiViewport } from "../protocol.js";
import type {
  ArtifactView,
  ArtifactProjectionOptions,
  CommandDescriptor,
  ExternalProjection,
  IdeContextView,
  RunView,
  SessionSummary,
  ThemeTokens,
  WorkflowView,
  UiCommandActions,
  UiProjectionStore,
  UiProviderMeta,
} from "../session.js";
import type { HotReloadStateStore } from "../hot-reload.js";

const ProjectionContext = createContext<UiProjectionStore | null>(null);
const ActionsContext = createContext<UiCommandActions | null>(null);
const MetaContext = createContext<UiProviderMeta | null>(null);

export interface UiProviderProps {
  state: UiProjectionStore;
  actions: UiCommandActions;
  meta: UiProviderMeta;
  children: ReactNode;
}

/** The provider is the sole owner of the host's projection-store implementation. */
export function UiProvider({ state, actions, meta, children }: UiProviderProps): ReactNode {
  return createElement(
    ProjectionContext.Provider,
    { value: state },
    createElement(ActionsContext.Provider, { value: actions }, createElement(MetaContext.Provider, { value: meta }, children)),
  );
}

function requiredContext<T>(value: T | null, name: string): T {
  if (value === null) throw new Error(`${name} must be used inside UiProvider`);
  return value;
}

function useProjection<T>(projection: ExternalProjection<T>): T {
  return useSyncExternalStore(projection.subscribe, projection.getSnapshot, projection.getSnapshot);
}

export function useUiState(): UiProjectionStore { return requiredContext(use(ProjectionContext), "useUiState"); }
export function useUiActions(): UiCommandActions { return requiredContext(use(ActionsContext), "useUiActions"); }
export function useUiMeta(): UiProviderMeta { return requiredContext(use(MetaContext), "useUiMeta"); }

export function useSession(id: string): SessionSummary | undefined {
  const store = useUiState();
  return useProjection(useMemo(() => store.session(id), [store, id]));
}

export function useRun(id: string): RunView | undefined {
  const store = useUiState();
  return useProjection(useMemo(() => store.run(id), [store, id]));
}

export function useContext(sessionId: string): IdeContextView | undefined {
  const store = useUiState();
  return useProjection(useMemo(() => store.context(sessionId), [store, sessionId]));
}

export function useWorkflow(id: string): WorkflowView | undefined {
  const store = useUiState();
  return useProjection(useMemo(() => store.workflow(id), [store, id]));
}

export function useArtifact<T extends UiJsonValue = UiJsonValue>(id: string, options?: ArtifactProjectionOptions): ArtifactView<T> | undefined {
  const store = useUiState();
  const includeContent = options?.includeContent;
  const maxBytes = options?.maxBytes;
  const offset = options?.offset;
  const length = options?.length;
  const page = options?.page;
  const pageSize = options?.pageSize;
  return useProjection(useMemo(
    () => store.artifact<T>(id, {
      ...(includeContent === undefined ? {} : { includeContent }),
      ...(maxBytes === undefined ? {} : { maxBytes }),
      ...(offset === undefined ? {} : { offset }),
      ...(length === undefined ? {} : { length }),
      ...(page === undefined ? {} : { page }),
      ...(pageSize === undefined ? {} : { pageSize }),
    }),
    [store, id, includeContent, maxBytes, offset, length, page, pageSize],
  ));
}

export function useTheme(): ThemeTokens {
  const store = useUiState();
  return useProjection(useMemo(() => store.theme(), [store]));
}

export function useViewport(): UiViewport {
  const store = useUiState();
  return useProjection(useMemo(() => store.viewport(), [store]));
}

export function useCapabilities(): UiCapabilities {
  const store = useUiState();
  return useProjection(useMemo(() => store.capabilities(), [store]));
}

/** High-frequency consumers can read current viewport dimensions without re-rendering. */
export function useTransientViewport(): RefObject<UiViewport> {
  const store = useUiState();
  const projection = useMemo(() => store.viewport(), [store]);
  const current = useRef(projection.getSnapshot());
  useEffect(() => projection.subscribe(() => { current.current = projection.getSnapshot(); }), [projection]);
  return current;
}

export interface CommandHook<TInput extends UiJsonValue, TOutput extends UiJsonValue> {
  descriptor: CommandDescriptor<TInput, TOutput> | undefined;
  pending: boolean;
  error: unknown;
  execute(input: TInput, options?: { signal?: AbortSignal }): Promise<TOutput>;
}

export function useCommand<TInput extends UiJsonValue = UiJsonValue, TOutput extends UiJsonValue = UiJsonValue>(id: string): CommandHook<TInput, TOutput> {
  const store = useUiState();
  const actions = useUiActions();
  const descriptor = useProjection(useMemo(() => store.command<TInput, TOutput>(id), [store, id]));
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(undefined);
  const execute = useCallback(async (input: TInput, options?: { signal?: AbortSignal }): Promise<TOutput> => {
    setPending(() => true);
    setError(() => undefined);
    try {
      return await actions.invoke<TInput, TOutput>(id, input, options);
    } catch (cause) {
      setError(() => cause);
      throw cause;
    } finally {
      setPending(() => false);
    }
  }, [actions, id]);
  return { descriptor, pending, error, execute };
}

export function useHotReloadState<T extends UiJsonValue>(store: HotReloadStateStore, key: string, initial: T): readonly [T, (next: T | ((current: T) => T)) => void] {
  const subscribe = useCallback((listener: () => void) => store.subscribe(key, listener), [store, key]);
  const getSnapshot = useCallback(() => store.get(key, initial), [store, key, initial]);
  const value = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  const setValue = useCallback((next: T | ((current: T) => T)) => store.set(key, next, initial), [store, key, initial]);
  return [value, setValue] as const;
}
