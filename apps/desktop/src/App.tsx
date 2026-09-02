import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { QuestionOutcomeView, SessionId } from "./types.js";
import { Navigation, type DesktopView } from "./components/Navigation.js";
import { Transcript } from "./components/Transcript.js";
import { Composer } from "./components/Composer.js";
import { Steering } from "./components/Steering.js";
import { PromptQueue } from "./components/PromptQueue.js";
import { ConfirmCancel, runAtStake } from "./components/ConfirmCancel.js";
import { ConnectionBanner } from "./components/ConnectionBanner.js";
import { ViewBar } from "./components/ViewBar.js";
import { InboxView } from "./components/InboxView.js";
import { AnalyticsDashboard } from "./components/AnalyticsDashboard.js";
import { RemoteUiRenderer } from "./components/RemoteUiRenderer.js";
import { CommandPalette, type PaletteEntry } from "./components/CommandPalette.js";
import { ShortcutsCard } from "./components/ShortcutsCard.js";
import { SkillsView } from "./components/SkillsView.js";
import { MemoryView } from "./components/MemoryView.js";
import { DocsView } from "./components/DocsView.js";
import { PluginsView } from "./components/PluginsView.js";
import { ContextView } from "./components/ContextView.js";
import { EdgesView } from "./components/EdgesView.js";
import { BacktrackView } from "./components/BacktrackView.js";
import { SessionLibrary } from "./components/SessionLibrary.js";
import { WorkflowView } from "./components/WorkflowView.js";
import { KanbanView } from "./components/KanbanView.js";
import { BlackboardView } from "./components/BlackboardView.js";
import { RepoPicker } from "./components/RepoPicker.js";
import { CouncilBrowser } from "./components/CouncilBrowser.js";
import { CouncilBuilder } from "./components/CouncilBuilder.js";
import { CouncilResults } from "./components/CouncilResults.js";
import { ModelPicker } from "./components/ModelPicker.js";
import { ProviderPicker } from "./components/ProviderPicker.js";
import type { ProviderRow } from "./components/localConfig.js";
import { ApiKeys } from "./components/ApiKeys.js";
import { ModePicker } from "./components/ModePicker.js";
import {
  Onboarding,
  onboardingSkipped,
  readOnboardingStatus,
  setOnboardingSkipped,
  shouldOpenOnboarding,
} from "./components/Onboarding.js";
import { localConfigClient, shellAvailable } from "./components/localConfig";
import type { RepositorySelection } from "./localConfig.js";
import {
  missingBridge,
  unloaded,
  type DocCard,
  type KnowledgeTransport,
  type LearningCard,
  type LearningMutation,
  type Loaded,
  type MemoryCard,
  type SkillCard,
  type UiPluginLifecycleStatus,
} from "./components/knowledgeTransport.js";
import { useDaemon } from "./useDaemon.js";
import { runLifecycleAffordance } from "./daemonState.js";
import { createKnowledgeTransport, type DesktopTransport } from "./transport.js";
import type { NotificationSink } from "./osNotifications.js";
import type { InboxDeepLink, PublishTarget } from "@codypendent/protocol";
import type { UiDocument } from "@codypendent/ui";

/** The bridge commands each surface needs, named in its unavailable panel. */
const REQUIRED_COMMANDS = {
  skills: ["list_skills"],
  memories: ["list_memories", "correct_memory", "forget_memory"],
  learnings: ["list_learnings", "mutate_learning"],
  docs: [
    "list_documents",
    "create_document",
    "acquire_document_lease",
    "mutate_document",
    "release_document_lease",
    "publish_document",
  ],
  plugins: [
    "list_ui_plugins",
    "smoke_test_ui_plugin",
    "enable_ui_plugin",
    "approve_ui_plugin_update",
    "reject_ui_plugin_update",
    "revoke_ui_plugin",
  ],
} as const;

/**
 * The RemoteUiRenderer's document set while the daemon streams none. One
 * module-level instance: a per-render `new Map()` would hand the renderer a
 * fresh identity on every streamed token.
 */
const NO_REMOTE_DOCUMENTS = new Map<string, UiDocument>();

/**
 * The views that read through the knowledge transport. Without one they are
 * honest dead ends, and the sidebar and palette say so up front.
 */
const KNOWLEDGE_VIEWS: ReadonlySet<DesktopView> = new Set<DesktopView>([
  "skills",
  "memory",
  "docs",
  "plugins",
]);

interface AppProps {
  /**
   * How to reach `codypendentd`. Defaults to the Tauri shell bridge, which
   * yields `null` outside the shell — the app then shows a disconnected state
   * with the reason. Tests inject a stub to drive a connected client.
   */
  makeTransport?: () => DesktopTransport | null;
  initialView?: DesktopView;
  /**
   * Where OS notifications for blocking work go. Omitted in the app, which
   * sends them through Tauri's notification plugin; a test injects a sink to
   * assert what the user would actually have been shown.
   */
  notify?: NotificationSink;
  /**
   * The knowledge surfaces' call surface (Skills, Memory, Docs, UI plugins).
   *
   * Defaults to the shell's transport (`createKnowledgeTransport`), which is
   * `null` outside the shell — a browser tab, a test. Each surface then
   * renders an explicit unavailable panel naming the commands it is waiting
   * for, never an empty list, which would assert there is nothing to show.
   * Tests inject a stub to drive the views with data.
   */
  knowledge?: KnowledgeTransport;
  /** How to reach the shell's knowledge commands; overridden only in tests. */
  makeKnowledge?: () => KnowledgeTransport | null;
}

function describe(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}

/** Read a surface, or record exactly why it could not be read. */
export async function read<T>(
  fetcher: (() => Promise<T[]>) | undefined,
  commands: readonly string[],
  set: React.Dispatch<React.SetStateAction<Loaded<T>>>,
  /**
   * Whether this read's answer is still wanted by the time it arrives.
   *
   * Dropping a repository-scoped surface on a reconnect is not enough on its
   * own: a query started under the old connection is still in flight, and if
   * it settles AFTER the new one it writes the previous checkout's records
   * back over them. The screen then shows one repository's documents while
   * every mutation addresses another, and a block delete addresses a document
   * by id alone.
   */
  stillWanted: () => boolean = () => true,
): Promise<void> {
  if (!fetcher) {
    set({ items: [], status: "unavailable", detail: missingBridge(commands) });
    return;
  }
  set({ items: [], status: "loading", detail: null });
  try {
    const items = await fetcher();
    if (!stillWanted()) {
      return;
    }
    set({ items, status: "loaded", detail: null });
  } catch (error) {
    if (!stillWanted()) {
      return;
    }
    // A failed read is not an empty read. The surface says so.
    set({ items: [], status: "unavailable", detail: describe(error) });
  }
}

export const App: React.FC<AppProps> = ({
  makeTransport,
  initialView = "sessions",
  notify,
  knowledge: injectedKnowledge,
  makeKnowledge = createKnowledgeTransport,
}) => {
  // Resolved once: the shell is either there or not for the life of the app,
  // and a transport re-created per render would re-read every surface.
  const [knowledge] = useState<KnowledgeTransport | undefined>(
    () => injectedKnowledge ?? makeKnowledge() ?? undefined,
  );
  const [currentView, setCurrentView] = useState<DesktopView>(initialView);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  /**
   * Whether the operator has picked a view themselves yet.
   *
   * The first-run gate below runs once, asynchronously, and must never yank
   * somebody off a view they deliberately opened while it was reading.
   */
  const chosenView = useRef(false);
  /**
   * Where Escape goes back to.
   *
   * The TUI has one working surface and summons everything else over it, so
   * there is always a way out. The desktop had 22 sidebar destinations and no
   * way back at all — every secondary view was a dead end you had to leave by
   * aiming at the sidebar again. This is that way out.
   */
  const viewHistory = useRef<DesktopView[]>([]);
  /** A provider chosen on the Providers page, handed to the add-model flow. */
  const [pendingProvider, setPendingProvider] = useState<ProviderRow | null>(null);
  const selectView = useCallback(
    (view: DesktopView) => {
      chosenView.current = true;
      // The history push happens here, not inside the `setCurrentView`
      // updater: StrictMode double-invokes updaters, which would push twice.
      if (currentView !== view) {
        viewHistory.current.push(currentView);
        // A wandering session should not grow an unbounded stack.
        if (viewHistory.current.length > 32) {
          viewHistory.current.shift();
        }
      }
      // A provider chosen on the Providers page opens the add-model form on
      // the NEXT visit to Models. Leaving for anywhere else abandons that
      // choice, or the form re-opened on a provider the operator had moved on
      // from every time they came back.
      if (view !== "models") {
        setPendingProvider(null);
      }
      setCurrentView(view);
    },
    [currentView],
  );
  /** Escape: pop back, or fall back to the session — never a dead end. */
  const goBack = useCallback(() => {
    chosenView.current = true;
    // The history pop likewise stays out of the updater (double-invoke would
    // pop twice).
    const target = viewHistory.current.pop();
    if (target !== undefined && target !== currentView) {
      setCurrentView(target);
      return;
    }
    setCurrentView(currentView === "sessions" ? currentView : "sessions");
  }, [currentView]);
  /** The "stop opening setup automatically" preference (see `Onboarding.tsx`). */
  const [skipOnboarding, setSkipOnboarding] = useState(onboardingSkipped);
  const {
    state,
    reconnect,
    startDaemon,
    dismissError,
    submit,
    cancel,
    steer,
    pauseRun,
    resumeRun,
    queuePrompt,
    updateQueuedPrompt,
    promoteQueuedPrompt,
    deleteQueuedPrompt,
    selectSession,
    resolveApproval,
    resolveQuestion,
    loadInbox,
    acknowledgeInbox,
    dismissInbox,
    queryAnalytics,
    exportAnalytics,
    transport,
  } = useDaemon(makeTransport, notify);
  /**
   * The run whose blackboard the Blackboard panel opens on. Set by the Workflow
   * panel so "show this run's board" is one click rather than a copied id; the
   * panel still reads the board itself, so nothing is displayed that a real
   * `ReadBlackboard` did not return.
   */
  const [blackboardRunId, setBlackboardRunId] = useState<string | undefined>(undefined);
  /**
   * Whether the steering panel is open under the composer, and whether a
   * cancellation is awaiting confirmation. Both belong to the run the operator
   * is already looking at, so both live here rather than in a nav entry.
   */
  const [steeringOpen, setSteeringOpen] = useState(false);
  const [cancelPending, setCancelPending] = useState(false);
  /**
   * Whether the pending-prompt queue panel is open.
   *
   * The panel also renders unasked when there is something to say — a queue
   * with entries in it, or a mutation the daemon refused — so a failure is
   * never hidden behind a toggle the operator has not clicked.
   */
  const [queueOpen, setQueueOpen] = useState(false);
  /**
   * Set by the panel's Close button. With prompts queued, `queueVisible` used
   * to force the panel open regardless, which made Close a button that
   * flipped a flag and changed nothing. Reset whenever the queue GROWS, so a
   * new prompt still surfaces the panel.
   */
  const [queueDismissed, setQueueDismissed] = useState(false);
  const previousQueueLength = useRef(0);
  useEffect(() => {
    if (state.pendingPrompts.length > previousQueueLength.current) {
      setQueueDismissed(false);
    }
    previousQueueLength.current = state.pendingPrompts.length;
  }, [state.pendingPrompts.length]);

  /**
   * The selected repository, and the configured council names.
   *
   * Both are LOCAL configuration read through the shell, not daemon state, so
   * they are held here rather than in the session store. `repository` starts as
   * `undefined` meaning "not asked yet" — distinct from `null`, which is the
   * real answer "no repository is selected"; the Council run panel says which.
   */
  const [repository, setRepository] = useState<RepositorySelection | null | undefined>(undefined);
  const [councilNames, setCouncilNames] = useState<string[] | undefined>(undefined);
  /** True while the council builder is open in place of the browser. */
  const [buildingCouncil, setBuildingCouncil] = useState(false);
  /**
   * Configured model ids for the council builder's suggestion datalists. Read
   * lazily when the builder opens — the builder declared the prop from day
   * one and was never passed it, so the operator typed model ids from memory
   * against a store that hard-refuses unknown ones.
   */
  const [configuredModelIds, setConfiguredModelIds] = useState<string[] | undefined>(undefined);
  /**
   * The staged run defaults, for the composer's status strip. The strip used
   * to hardcode "Build Mode" whatever the Mode/Model pickers had staged;
   * re-read whenever the operator lands back on the session view, which is
   * where the label shows (and the pickers are other views, so a change
   * there is always followed by a return here).
   */
  const [runDefaultsLabel, setRunDefaultsLabel] = useState<string | null>(null);
  /** The composer draft, lifted so a view round-trip cannot lose it. */
  const [composerDraft, setComposerDraft] = useState("");
  useEffect(() => {
    if (currentView !== "sessions" || !shellAvailable()) {
      return;
    }
    let cancelled = false;
    void localConfigClient
      .runDefaults()
      .then((defaults) => {
        if (!cancelled) {
          const mode = defaults.mode.type === "Unknown" ? "Build" : defaults.mode.type;
          setRunDefaultsLabel(`${mode} mode · ${defaults.model ?? "model: daemon chooses"}`);
        }
      })
      .catch(() => {
        // The fallback constant stays; a failed read must not invent a label.
      });
    return () => {
      cancelled = true;
    };
  }, [currentView]);
  useEffect(() => {
    if (!buildingCouncil || configuredModelIds !== undefined || !shellAvailable()) {
      return;
    }
    let cancelled = false;
    void localConfigClient
      .listModels()
      .then((view) => {
        if (!cancelled) {
          setConfiguredModelIds(view.models.map((model) => model.id));
        }
      })
      .catch(() => {
        // Unknown stays unknown: the field remains free text and the crate's
        // refusal stays the authority.
      });
    return () => {
      cancelled = true;
    };
  }, [buildingCouncil, configuredModelIds]);
  /** The council the results panel opens on, when the browser named one. */
  const [councilToRun, setCouncilToRun] = useState<string | null>(null);

  const [skills, setSkills] = useState<Loaded<SkillCard>>(unloaded);
  const [memories, setMemories] = useState<Loaded<MemoryCard>>(unloaded);
  const [learnings, setLearnings] = useState<Loaded<LearningCard>>(unloaded);
  const [docs, setDocs] = useState<Loaded<DocCard>>(unloaded);
  const [plugins, setPlugins] = useState<Loaded<UiPluginLifecycleStatus>>(unloaded);
  const [memoryNotice, setMemoryNotice] = useState<string | null>(null);
  const [docsNotice, setDocsNotice] = useState<string | null>(null);
  const [pluginNotice, setPluginNotice] = useState<string | null>(null);

  const connected = state.status === "connected";
  /**
   * Whether a run-scoped command (`CancelRun`, `QueueSteering`) can actually
   * be sent: connected, and holding the run id the daemon named. Without both
   * the buttons are not offered — an affordance that cannot reach a run is
   * worse than no affordance.
   */
  const canControlRun =
    connected &&
    state.attachingSessionId === null &&
    state.sessionAttachmentConfirmed &&
    state.activeRunId !== null;
  /**
   * What the operator would be throwing away, read from the run's own
   * `RunStarted` event. Unknown when this client attached mid-run; the
   * confirmation says so rather than filling in a plausible objective.
   * Memoized: `runAtStake` scans the stream backwards, which is O(n) in the
   * worst case (no matching `RunStarted`) and must not run per render — i.e.
   * per streamed token.
   */
  const atStake = useMemo(
    () => runAtStake(state.durableEvents, state.activeRunId),
    [state.durableEvents, state.activeRunId],
  );
  /**
   * Which of pause/resume the daemon would accept for this run right now, or
   * `null` for neither. Read from the run state the store folded off
   * `RunStateChanged`, never from `isRunning` — see `runLifecycleAffordance`.
   */
  const lifecycle = runLifecycleAffordance(state);
  /**
   * Whether a real `QueuePrompt` can be sent: connected, with a session the
   * shell is attached to, and a bridge that offers the command. A run id is
   * NOT required — the queue is session-scoped, and queueing work for a session
   * whose run has just ended is exactly the point.
   */
  const canQueuePrompts =
    connected &&
    state.attachingSessionId === null &&
    state.sessionAttachmentConfirmed &&
    state.activeSessionId !== null &&
    Boolean(transport?.queuePrompt);
  /** Shown when the queue has something to say even if nobody opened it. */
  const queueVisible =
    !queueDismissed &&
    (queueOpen || state.pendingPrompts.length > 0 || state.promptQueueError !== null);

  // A run that ended or whose session is not confirmed on this connection is
  // no longer controllable, so neither affordance may outlive the boundary
  // holding a stale run id.
  useEffect(() => {
    if (!canControlRun) {
      setCancelPending(false);
      setSteeringOpen(false);
    }
  }, [canControlRun]);
  // Remote UI documents arrive with adoption 14 milestone 5; until the daemon
  // streams them there are none, and the panel stays closed. Module-level so
  // the renderer is not handed a fresh Map identity on every render.
  const documents = NO_REMOTE_DOCUMENTS;

  // Stable so the memoized `Navigation` — 6 groups and 22 destinations — is
  // skipped entirely while a reply streams, instead of reconciling per token.
  const openPalette = useCallback(() => {
    // The palette always opens on top: closing shortcuts here keeps
    // `shortcutsOpen` from going stale-true underneath it, which was letting
    // one Escape dismiss both overlays instead of just the topmost one.
    setShortcutsOpen(false);
    setPaletteOpen(true);
  }, []);
  const selectSessionFromNav = useCallback(
    (id: SessionId) => {
      setCurrentView("sessions");
      void selectSession(id);
    },
    [selectSession],
  );

  // Referentially stable across renders. Inline arrows here would hand
  // `TranscriptRow` a new `onApprove`/`onReject` on every token and defeat its
  // memo entirely — the whole transcript would reconcile per token again.
  const approve = useCallback(
    (approvalId: string) => void resolveApproval(approvalId, "approve"),
    [resolveApproval],
  );
  const answerQuestion = useCallback(
    (questionId: string, outcome: QuestionOutcomeView) =>
      void resolveQuestion(questionId, outcome),
    [resolveQuestion],
  );
  /** A failure card's Retry: the same objective, a new run. */
  const retryObjective = useCallback(
    (objective: string) => void submit(objective),
    [submit],
  );
  const reject = useCallback(
    (approvalId: string) => void resolveApproval(approvalId, "reject"),
    [resolveApproval],
  );

  /**
   * The connection this render belongs to, readable from an async callback.
   * `state.connectionEpoch` captured in a closure is the value at call time,
   * which is precisely the value a staleness check must not use.
   */
  const liveEpoch = useRef(state.connectionEpoch);
  liveEpoch.current = state.connectionEpoch;
  /**
   * A read of a REPOSITORY-SCOPED surface: its answer is discarded if the
   * connection changed while it was in flight, because a reconnect is what
   * rebinds the repository.
   */
  const scopedRead = useCallback(
    <T,>(
      fetcher: (() => Promise<T[]>) | undefined,
      commands: readonly string[],
      set: React.Dispatch<React.SetStateAction<Loaded<T>>>,
    ) => {
      const startedUnder = liveEpoch.current;
      return read(fetcher, commands, set, () => liveEpoch.current === startedUnder);
    },
    [],
  );

  const loadSkills = useCallback(
    () =>
      read(knowledge && (() => knowledge.listSkills()), REQUIRED_COMMANDS.skills, setSkills),
    [knowledge],
  );
  // These three are scoped to the repository the connection carries, so their
  // answers are discarded if a reconnect rebound it mid-flight.
  const loadMemories = useCallback(
    () =>
      scopedRead(
        knowledge && (() => knowledge.listMemories()),
        REQUIRED_COMMANDS.memories,
        setMemories,
      ),
    [knowledge, scopedRead],
  );
  const loadLearnings = useCallback(
    () =>
      scopedRead(
        knowledge && (() => knowledge.listLearnings()),
        REQUIRED_COMMANDS.learnings,
        setLearnings,
      ),
    [knowledge, scopedRead],
  );
  const loadDocs = useCallback(
    () =>
      scopedRead(knowledge && (() => knowledge.listDocuments()), REQUIRED_COMMANDS.docs, setDocs),
    [knowledge, scopedRead],
  );
  const loadPlugins = useCallback(
    () =>
      read(knowledge && (() => knowledge.listUiPlugins()), REQUIRED_COMMANDS.plugins, setPlugins),
    [knowledge],
  );

  /**
   * The post-boot first-run gate — the desktop's
   * `apply_post_boot_onboard_gate` (`crates/cli/src/tui.rs`).
   *
   * It runs ONCE, reads the three setup conditions from the shell, and opens
   * the setup surface only when one of them is PROVEN to block a run. Three
   * things it deliberately does not do:
   *
   * - It does not run outside the Tauri shell. There is no `models.toml` to
   *   read from a browser tab, and opening a setup wizard there would assert
   *   something about files this build never opened.
   * - It does not open on a failed read. `shouldOpenOnboarding` sees only the
   *   answers; a rejection leaves the app exactly where it was.
   * - It does not override the operator. `skipOnboarding` is their explicit
   *   "stop doing this", and `chosenView` covers the case where they navigated
   *   while the read was in flight.
   */
  useEffect(() => {
    if (skipOnboarding || !shellAvailable()) {
      return;
    }
    let cancelled = false;
    void readOnboardingStatus()
      .then((status) => {
        if (!cancelled && !chosenView.current && shouldOpenOnboarding(status)) {
          setCurrentView("onboarding");
        }
      })
      // We could not find out whether setup is needed, which is not the same
      // as finding out that it is. Say nothing rather than nag.
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
    // Mount-only: this is a boot decision, not a subscription. `skipOnboarding`
    // is read at mount; toggling it later must not re-open the surface.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /**
   * The repository selection and the council roster, read from the shell.
   *
   * Both are local configuration, so they are readable whether or not a daemon
   * is up. A read that FAILS leaves `repository` at `null` only when the shell
   * itself answered "none selected"; a thrown error leaves it `undefined`, and
   * the Repository panel — which does its own read and holds its own three-way
   * status — is where that distinction is shown to the operator.
   */
  useEffect(() => {
    let cancelled = false;
    if (!transport?.currentRepository) {
      return;
    }
    void transport
      .currentRepository()
      .then((selection) => {
        if (!cancelled) {
          setRepository(selection);
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [transport]);

  useEffect(() => {
    let cancelled = false;
    if (!transport?.listCouncils) {
      return;
    }
    void transport
      .listCouncils()
      .then((councils) => {
        if (!cancelled) {
          setCouncilNames(councils.map((council) => council.name));
        }
      })
      // A failure leaves the name list `undefined` — "we do not know" — so the
      // run panel offers no suggestions rather than an empty roster that would
      // read as "you have configured none".
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [transport]);

  /**
   * Drop the repository-scoped surfaces when the connection changes.
   *
   * A reconnect is what rebinds the repository, so Memory, Learnings and Docs
   * are now about a DIFFERENT checkout. Leaving them `loaded` meant the
   * read-once effect below never refetched: reopening Docs showed the previous
   * repository's records while every mutation addressed the new one — and
   * `deleteDocumentBlock` addresses a document by id alone, so acting on a
   * stale card could modify a checkout the operator had already left.
   *
   * Skills and Plugins are daemon-wide and survive the change.
   */
  const knownEpoch = useRef(state.connectionEpoch);
  useEffect(() => {
    if (knownEpoch.current === state.connectionEpoch) {
      return;
    }
    knownEpoch.current = state.connectionEpoch;
    setMemories(unloaded);
    setLearnings(unloaded);
    setDocs(unloaded);
  }, [state.connectionEpoch]);

  // Each surface reads once, the first time it is opened. An unavailable
  // surface is not retried automatically — its Refresh button is the retry,
  // and only exists when there is a transport that could answer.
  useEffect(() => {
    if (currentView === "skills" && skills.status === "unloaded") {
      void loadSkills();
    } else if (currentView === "memory") {
      if (memories.status === "unloaded") {
        void loadMemories();
      }
      if (learnings.status === "unloaded") {
        void loadLearnings();
      }
    } else if (currentView === "docs" && docs.status === "unloaded") {
      void loadDocs();
    } else if (currentView === "plugins" && plugins.status === "unloaded") {
      void loadPlugins();
    }
  }, [
    currentView,
    skills.status,
    memories.status,
    learnings.status,
    docs.status,
    plugins.status,
    loadSkills,
    loadMemories,
    loadLearnings,
    loadDocs,
    loadPlugins,
  ]);

  // ⌘K / Ctrl-K anywhere; `/` only when the focus is not a text field, so it
  // still types a slash in the composer.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        openPalette();
        return;
      }
      if (event.key === "Escape") {
        // Escape closes the TOPMOST thing, and only that. It used to fall
        // straight through to `goBack`, so dismissing the cancel dialog also
        // navigated the view away underneath it, and pressing Escape over a
        // modal with no handler of its own left a destructive dialog floating
        // on a screen the operator had just left.
        //
        // `paletteOpen` and the rest are read here, not inside a setter's
        // updater — StrictMode double-invokes updaters and `goBack` pops
        // history, which is how Escape once walked back two views at a time.
        if (shortcutsOpen) {
          setShortcutsOpen(false);
          return;
        }
        if (paletteOpen) {
          setPaletteOpen(false);
          return;
        }
        if (cancelPending) {
          // `ConfirmCancel` has its own Escape handler and no
          // `stopPropagation`, so both fired: it dismissed AND the view moved.
          setCancelPending(false);
          return;
        }
        if (buildingCouncil) {
          setBuildingCouncil(false);
          return;
        }
        // Editing text is not a navigation gesture. Escape in a textarea used
        // to leave the view mid-edit and lose what was typed.
        const target = event.target as HTMLElement | null;
        if (
          target instanceof HTMLInputElement ||
          target instanceof HTMLTextAreaElement ||
          target?.isContentEditable === true
        ) {
          return;
        }
        goBack();
        return;
      }
      const target = event.target as HTMLElement | null;
      const typing =
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement ||
        target?.isContentEditable === true;
      if (event.key === "/" && !typing) {
        event.preventDefault();
        openPalette();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [paletteOpen, goBack, cancelPending, buildingCouncil, shortcutsOpen, openPalette]);

  const handleNavigateInbox = (deepLink: InboxDeepLink) => {
    if (deepLink.type === "Session") {
      void selectSession(deepLink.session_id);
      setCurrentView("sessions");
    } else if (deepLink.type === "Run") {
      void selectSession(deepLink.session_id);
      setCurrentView("sessions");
    } else if (deepLink.type === "Approval") {
      setCurrentView("sessions");
    } else if (deepLink.type === "Question") {
      setCurrentView("sessions");
    } else if (deepLink.type === "Workflow") {
      // The inbox row says "Open Workflow wf-…"; these three used to be
      // silent no-ops — a labeled button that did nothing at all.
      setCurrentView("workflow");
    } else if (deepLink.type === "Plugin") {
      setCurrentView("plugins");
    } else if (deepLink.type === "Repository") {
      setCurrentView("repository");
    }
  };

  /** Run a mutation, surface its outcome sentence, then re-read the list. */
  const applyMutation = async (
    run: () => Promise<string | void>,
    setNotice: (notice: string | null) => void,
    reload: () => Promise<void>,
  ) => {
    try {
      const outcome = await run();
      setNotice(typeof outcome === "string" && outcome.length > 0 ? outcome : null);
    } catch (error) {
      // A refused mutation is an outcome, not an absence: say what happened
      // and leave the list showing what the daemon last actually reported.
      setNotice(describe(error));
    }
    await reload();
  };

  const paletteEntries: PaletteEntry[] = [
    {
      id: "view:onboarding",
      title: "Get Started  First-run setup",
      description:
        "re-read what setup still needs: a configured model, a credential that resolves, a repository",
      key: "—",
      group: "Setup",
    },
    {
      id: "view:repository",
      title: "Repository",
      description: "choose the git checkout every repository-scoped command anchors to",
      key: "—",
      group: "Setup",
    },
    {
      id: "view:sessions",
      title: "Sessions",
      description: "the attached session's transcript and composer",
      key: "—",
      group: "Run",
    },
    {
      id: "view:inbox",
      title: "Inbox",
      description: "notifications and human work from the durable inbox",
      key: "—",
      group: "Run",
    },
    {
      id: "view:analytics",
      title: "Analytics",
      description: "measured execution observations and aggregates",
      key: "—",
      group: "Run",
    },
    {
      id: "view:context",
      title: "/context  Context usage breakdown",
      description: "view detailed token usage breakdown for the active run",
      key: "—",
      group: "Run",
    },
    {
      id: "view:library",
      title: "/library  Session library",
      description: "search every session and rename, pin, archive, or export one",
      key: "—",
      group: "Run",
    },
    ...(canControlRun
      ? [
          {
            id: "action:steer",
            title: "Steer run",
            description: "redirect the live run without killing it",
            key: "—",
            group: "Run",
          },
        ]
      : []),
    ...(canControlRun
      ? [
          {
            id: "action:cancel",
            title: "Cancel run",
            description: "confirm, then cancel the selected run",
            key: "—",
            group: "Run",
          },
        ]
      : []),
    {
      id: "action:shortcuts",
      title: "Keyboard shortcuts",
      description: "every key the desktop app answers to, in one card",
      key: "—",
      group: "Session",
    },
    {
      id: "view:workflow",
      title: "/workflow  Executable workflow graph",
      description: "open and control persisted workflow runs with a live DAG",
      key: "—",
      group: "Workflow",
    },
    {
      id: "view:board",
      title: "/board  Kanban task board",
      description: "create, assign, and move repository backlog tasks in Kanban columns",
      key: "—",
      group: "Workflow",
    },
    {
      id: "view:blackboard",
      title: "/blackboard  Blackboard evidence stream",
      description: "inspect attributed workflow evidence, or post an open question",
      key: "—",
      group: "Workflow",
    },
    {
      id: "view:docs",
      title: "/docs  Docs Studio · existing docs",
      description: `edit, review, and publish documents that already exist${knowledge ? "" : " (not in this build)"}`,
      key: "—",
      group: "Workspace",
    },
    {
      id: "view:skills",
      title: "/skills  Skill Studio · read only",
      description: `inspect registered skills and their permissions${knowledge ? "" : " (not in this build)"}`,
      key: "—",
      group: "Workspace",
    },
    {
      id: "view:memory",
      title: "/memory  Memory",
      description: `browse curated memories and their provenance${knowledge ? "" : " (not in this build)"}`,
      key: "—",
      group: "Workspace",
    },
    {
      id: "view:plugins",
      title: "/plugins  Remote UI plugins",
      description: `inspect, smoke-test, scope, approve, reject, or revoke verified UI plugins${knowledge ? "" : " (not in this build)"}`,
      key: "—",
      group: "Workspace",
    },
    {
      id: "view:council",
      title: "/council  Agent council",
      description: "list, run, and manage persisted multi-model councils",
      key: "—",
      group: "Models",
    },
    {
      id: "view:councilResults",
      title: "/council result  Council results",
      description: "open durable council outcomes",
      key: "—",
      group: "Models",
    },
  ];

  const runPaletteCommand = (id: string) => {
    setPaletteOpen(false);
    if (id === "action:steer") {
      setCurrentView("sessions");
      setSteeringOpen(true);
      return;
    }
    if (id === "action:shortcuts") {
      setShortcutsOpen(true);
      return;
    }
    if (id === "action:cancel") {
      // The palette asks the same question the composer button does. There is
      // no second path that cancels a run without confirmation.
      setCurrentView("sessions");
      setCancelPending(true);
      return;
    }
    // The palette is a front door to the existing views, never a second code
    // path: it only ever selects a view this build actually mounts.
    const view = id.startsWith("view:") ? (id.slice("view:".length) as DesktopView) : null;
    if (view) {
      selectView(view);
    }
  };

  return (
    <div style={{ display: "flex", width: "100vw", height: "100vh", overflow: "hidden", background: "var(--cody-bg)" }}>
      <Navigation
        sessions={state.sessions}
        activeSessionId={state.activeSessionId}
        onSelectSession={selectSessionFromNav}
        connectionStatus={state.status}
        statusDetail={state.detail}
        connectionInfo={state.info}
        currentView={currentView}
        onSelectView={selectView}
        unreadInboxCount={state.unreadInboxCount}
        onOpenPalette={openPalette}
        unavailableViews={knowledge ? undefined : KNOWLEDGE_VIEWS}
      />

      <main style={{ flex: 1, display: "flex", flexDirection: "column", height: "100vh", overflow: "hidden" }}>
        {/*
          The connection state lives here, across every view, not in a sidebar
          corner: "not connected" changes what everything below can be trusted
          to mean. The banner also offers to START the daemon, which the shell
          never could before — every first launch used to be a raw socket error
          and a reconnect loop that could not succeed.
        */}
        <ConnectionBanner
          status={state.status}
          detail={state.detail}
          hasTransport={transport !== null}
          canStart={Boolean(transport?.startDaemon)}
          onStart={startDaemon}
          // The rejection is already on screen: a failed attempt dispatches
          // `connect-failed`, which is what this banner renders. Catching it
          // keeps the promise from surfacing as an unhandled rejection — which
          // would reach global error reporting and fail UI tests — while
          // telling the operator nothing they are not already being told.
          onRetry={() => {
            void reconnect().catch(() => undefined);
          }}
          launchStatus={transport?.daemonLaunchStatus?.bind(transport)}
        />

        {state.error && (
          // Hoisted above the per-view blocks: `command-failed` fires from a
          // dozen call sites reachable from EVERY view (inbox acks, analytics
          // queries, approvals), and the banner used to render only inside
          // the Sessions view — an error earned elsewhere was invisible.
          <div
            role="alert"
            style={{
              padding: "8px 24px",
              background: "var(--cody-danger-bg)",
              borderBottom: "1px solid var(--cody-danger)",
              color: "var(--cody-danger-text)",
              fontSize: 12,
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
              gap: 12,
            }}
          >
            <span>{state.error}</span>
            <button
              aria-label="Dismiss error"
              onClick={dismissError}
              style={{
                background: "transparent",
                border: "none",
                color: "var(--cody-danger-text)",
                cursor: "pointer",
                fontSize: 14,
                lineHeight: 1,
              }}
            >
              ×
            </button>
          </div>
        )}

        {/*
          Where you are and the way back, on every view but the working
          surface. Escape has always walked the view history; this is the
          first time the screen says so.
        */}
        {currentView !== "sessions" && <ViewBar view={currentView} onBack={goBack} />}

        {currentView === "sessions" && (
          <>
            <Transcript
              items={state.transcript}
              connectionStatus={state.status}
              statusDetail={state.detail}
              onApprove={connected ? approve : undefined}
              onReject={connected ? reject : undefined}
              activity={state.activity}
              onRetry={connected && !state.isRunning ? retryObjective : undefined}
              onOpenView={selectView}
              onResolveQuestion={connected ? answerQuestion : undefined}
            />
            {steeringOpen && (
              <Steering
                runId={state.activeRunId}
                events={state.durableEvents}
                onSteer={steer}
                canSteer={canControlRun}
                unavailableDetail={
                  connected
                    ? "The daemon has not named a run id for this session, so there is nothing to steer."
                    : state.detail
                }
                onClose={() => setSteeringOpen(false)}
              />
            )}
            {queueVisible && (
              <PromptQueue
                prompts={state.pendingPrompts}
                canMutate={canQueuePrompts}
                unavailableDetail={
                  connected
                    ? state.attachingSessionId !== null
                      ? `Session ${state.attachingSessionId} is still attaching, so queue changes are temporarily unavailable.`
                      : !state.sessionAttachmentConfirmed
                        ? "The current connection has not confirmed a session attachment, so queue changes are unavailable. Reopen the session or start a new one."
                      : state.activeSessionId === null
                        ? "No session is attached, so there is no queue to change — open or start a session first."
                        : "This build's bridge does not offer the prompt-queue commands."
                    : state.detail
                }
                error={state.promptQueueError}
                onPromote={(promptId) => void promoteQueuedPrompt(promptId)}
                onEdit={(promptId, text) => updateQueuedPrompt(promptId, text)}
                onDelete={(promptId) => void deleteQueuedPrompt(promptId)}
                onClose={() => {
                  setQueueOpen(false);
                  setQueueDismissed(true);
                }}
              />
            )}
            <Composer
              statusLabel={runDefaultsLabel}
              draft={composerDraft}
              onDraftChange={setComposerDraft}
              onSend={submit}
              activity={state.activity}
              usage={state.usage}
              onQueue={(text) => void queuePrompt(text)}
              canQueue={canQueuePrompts}
              queuedCount={state.pendingPrompts.length}
              queueOpen={queueVisible}
              onToggleQueue={
                connected
                  ? () => {
                      setQueueDismissed(false);
                      setQueueOpen(!queueVisible);
                    }
                  : undefined
              }
              onRequestCancel={() => setCancelPending(true)}
              isRunning={state.isRunning}
              disabled={!connected || state.attachingSessionId !== null}
              canCancel={canControlRun}
              canSteer={canControlRun}
              steeringOpen={steeringOpen}
              onToggleSteering={() => setSteeringOpen((open) => !open)}
              lifecycle={lifecycle}
              onPause={() => void pauseRun()}
              onResume={() => void resumeRun()}
            />
          </>
        )}

        {cancelPending && state.activeRunId !== null && (
          <ConfirmCancel
            runId={state.activeRunId}
            objective={atStake.objective}
            startedAt={atStake.startedAt}
            onConfirm={() => {
              setCancelPending(false);
              void cancel();
            }}
            onDismiss={() => setCancelPending(false)}
          />
        )}

        {currentView === "inbox" && (
          <InboxView
            entries={state.inbox}
            onAcknowledge={(id) => void acknowledgeInbox(id)}
            onDismiss={(id) => void dismissInbox(id)}
            onNavigate={handleNavigateInbox}
            onApprove={connected ? approve : undefined}
            onReject={connected ? reject : undefined}
            onRefresh={() => void loadInbox()}
            unavailable={state.inboxStatus === "unavailable" ? state.inboxDetail : null}
          />
        )}

        {currentView === "analytics" && (
          <AnalyticsDashboard
            onQueryAnalytics={queryAnalytics}
            onExportAnalytics={exportAnalytics}
          />
        )}

        {currentView === "library" && (
          <SessionLibrary
            transport={transport}
            unavailable={transport ? null : state.detail}
            onOpenSession={(sessionId) => {
              setCurrentView("sessions");
              void selectSession(sessionId);
            }}
          />
        )}

        {currentView === "workflow" && (
          <WorkflowView
            transport={transport}
            connectionEpoch={state.connectionEpoch}
            unavailable={transport ? null : state.detail}
            onOpenBlackboard={(workflowRunId) => {
              setBlackboardRunId(workflowRunId);
              setCurrentView("blackboard");
            }}
          />
        )}

        {currentView === "board" && (
          <KanbanView
            transport={transport}
            unavailable={transport ? null : state.detail}
            connectionEpoch={state.connectionEpoch}
          />
        )}

        {currentView === "blackboard" && (
          <BlackboardView
            transport={transport}
            workflowRunId={blackboardRunId}
            connectionEpoch={state.connectionEpoch}
            unavailable={transport ? null : state.detail}
          />
        )}

        {currentView === "context" && (
          <ContextView events={state.durableEvents} activeRunId={state.activeRunId} />
        )}

        {/* The code graph. Daemon-backed and PAGED: the panel sends its own
            limit on every read and renders the daemon's pre-limit totals, so a
            cut page never reads as the whole graph. */}
        {currentView === "edges" && (
          <EdgesView transport={transport} unavailable={transport ? null : state.detail} />
        )}

        {/* Backtrack reads the attached session's own ledger — the same durable
            events the transcript is built from — so its checkpoint list is
            daemon state, not a client-side history of what this window did. */}
        {currentView === "backtrack" && (
          <BacktrackView
            events={state.durableEvents}
            activeSessionId={state.activeSessionId}
            transport={transport}
            unavailable={transport ? null : state.detail}
            onOpenSession={(sessionId) => {
              setCurrentView("sessions");
              void selectSession(sessionId);
            }}
          />
        )}

        {/* First-run setup. Also local config — it reads models.toml,
            auth.json and the repository preference, never the daemon — and it
            routes to the four surfaces below rather than duplicating them. */}
        {currentView === "onboarding" && (
          <Onboarding
            onOpen={selectView}
            connected={connected}
            skipped={skipOnboarding}
            onSkip={(skipped) => {
              setOnboardingSkipped(skipped);
              setSkipOnboarding(skipped);
            }}
          />
        )}

        {/* Local-config surfaces. Each fetches its own data and renders its own
            unavailable state, so they take no props — see localConfig.ts. */}
        {currentView === "models" && (
          <ModelPicker
            initialProvider={pendingProvider}
            onInitialProviderUsed={() => setPendingProvider(null)}
          />
        )}
        {currentView === "providers" && (
          <ProviderPicker
            // The page had no `onSelect`, so every click optional-chained into
            // nothing and the list looked inert. A chosen provider now opens the
            // add-model flow already on it, which is where its credential and
            // model details are entered.
            onSelect={(provider) => {
              setPendingProvider(provider);
              selectView("models");
            }}
          />
        )}
        {currentView === "keys" && <ApiKeys />}
        {currentView === "mode" && <ModePicker />}

        {currentView === "skills" && (
          <SkillsView skills={skills} onRefresh={knowledge && (() => void loadSkills())} />
        )}

        {currentView === "memory" && (
          <MemoryView
            memories={memories}
            learnings={learnings}
            notice={memoryNotice}
            onRefresh={
              knowledge &&
              (() => {
                setMemoryNotice(null);
                void loadMemories();
                void loadLearnings();
              })
            }
            onCorrectMemory={
              knowledge &&
              ((memoryId, statement) =>
                void applyMutation(
                  () => knowledge.correctMemory(memoryId, statement),
                  setMemoryNotice,
                  loadMemories,
                ))
            }
            onForgetMemory={
              knowledge &&
              ((memoryId) =>
                void applyMutation(
                  () => knowledge.forgetMemory(memoryId),
                  setMemoryNotice,
                  loadMemories,
                ))
            }
            onMutateLearning={
              knowledge &&
              ((id: string, revision: number, mutation: LearningMutation) =>
                void applyMutation(
                  () => knowledge.mutateLearning(id, revision, mutation),
                  setMemoryNotice,
                  loadLearnings,
                ))
            }
          />
        )}

        {currentView === "docs" && (
          <DocsView
            docs={docs}
            notice={docsNotice}
            onRefresh={
              knowledge &&
              (() => {
                setDocsNotice(null);
                void loadDocs();
              })
            }
            onCreateDocument={
              knowledge &&
              ((title) =>
                void applyMutation(
                  async () => {
                    await knowledge.createDocument(title);
                    return "document created";
                  },
                  setDocsNotice,
                  loadDocs,
                ))
            }
            onReplaceBlock={
              knowledge &&
              ((documentId, blockId, original, replacement) =>
                void applyMutation(
                  async () => {
                    await knowledge.replaceDocumentBlock(
                      documentId,
                      blockId,
                      original,
                      replacement,
                    );
                    return "block replaced";
                  },
                  setDocsNotice,
                  loadDocs,
                ))
            }
            onDeleteBlock={
              knowledge &&
              ((documentId, blockId) =>
                void applyMutation(
                  async () => {
                    await knowledge.deleteDocumentBlock(documentId, blockId);
                    return "block deleted";
                  },
                  setDocsNotice,
                  loadDocs,
                ))
            }
            onPublish={
              knowledge &&
              ((documentId: string, target: PublishTarget) =>
                void applyMutation(
                  async () => {
                    const plan = await knowledge.publishDocument(documentId, target);
                    const files = plan.changed_files.length;
                    return (
                      `publish parked for approval: ${plan.git_action} to ${plan.target} ` +
                      `(${files} file${files === 1 ? "" : "s"}) — approve it from the Inbox or the session`
                    );
                  },
                  setDocsNotice,
                  loadDocs,
                ))
            }
          />
        )}

        {currentView === "plugins" && (
          <PluginsView
            plugins={plugins}
            notice={pluginNotice}
            onRefresh={
              knowledge &&
              (() => {
                setPluginNotice(null);
                void loadPlugins();
              })
            }
            onSmokeTest={
              knowledge &&
              ((pluginId) =>
                void applyMutation(
                  async () => {
                    await knowledge.smokeTestUiPlugin(pluginId);
                    return "smoke test requested";
                  },
                  setPluginNotice,
                  loadPlugins,
                ))
            }
            onEnable={
              knowledge &&
              ((pluginId, scope) =>
                void applyMutation(
                  async () => {
                    await knowledge.enableUiPlugin(pluginId, scope);
                    return `enable requested at ${scope} scope`;
                  },
                  setPluginNotice,
                  loadPlugins,
                ))
            }
            onApprove={
              knowledge &&
              ((pluginId, receipt) =>
                void applyMutation(
                  async () => {
                    await knowledge.approveUiPluginUpdate(pluginId, receipt);
                    return "update approved";
                  },
                  setPluginNotice,
                  loadPlugins,
                ))
            }
            onReject={
              knowledge &&
              ((pluginId, receipt) =>
                void applyMutation(
                  async () => {
                    await knowledge.rejectUiPluginUpdate(pluginId, receipt);
                    return "update rejected";
                  },
                  setPluginNotice,
                  loadPlugins,
                ))
            }
            onRevoke={
              knowledge &&
              ((pluginId) =>
                void applyMutation(
                  async () => {
                    await knowledge.revokeUiPlugin(pluginId);
                    return "plugin revoked";
                  },
                  setPluginNotice,
                  loadPlugins,
                ))
            }
          />
        )}

        {currentView === "repository" && (
          <RepoPicker
            connected={connected}
            // The prop and its button already existed; nothing ever passed it,
            // so "Reconnect now" could not render and rebinding the repository
            // on demand was unreachable.
            onReconnect={reconnect}
            onLoad={async () => {
              if (!transport?.currentRepository) {
                throw new Error(
                  "the shell does not expose `current_repository`; run the desktop app rather than a browser tab",
                );
              }
              const selection = await transport.currentRepository();
              setRepository(selection);
              return selection;
            }}
            onPick={async () => {
              if (!transport?.pickRepository) {
                throw new Error(
                  "the shell does not expose `pick_repository`; run the desktop app rather than a browser tab",
                );
              }
              const selection = await transport.pickRepository();
              if (selection !== null) {
                setRepository(selection);
              }
              return selection;
            }}
            onSetPath={
              transport?.setRepository &&
              (async (path) => {
                const selection = await transport.setRepository!(path);
                setRepository(selection);
                return selection;
              })
            }
            onClear={
              transport?.clearRepository &&
              (async () => {
                await transport.clearRepository!();
                setRepository(null);
              })
            }
          />
        )}

        {currentView === "council" &&
          (buildingCouncil ? (
            <CouncilBuilder
              configuredModels={configuredModelIds}
              onCreate={async (draft) => {
                if (!transport?.createCouncil) {
                  throw new Error(
                    "the shell does not expose `create_council`; run the desktop app rather than a browser tab",
                  );
                }
                return transport.createCouncil(draft);
              }}
              onCancel={() => setBuildingCouncil(false)}
              onCreated={(council) => {
                setCouncilNames((current) =>
                  current === undefined ? [council.name] : [...current, council.name],
                );
                setBuildingCouncil(false);
              }}
            />
          ) : (
            <CouncilBrowser
              onLoad={async () => {
                if (!transport?.listCouncils) {
                  throw new Error(
                    "the shell does not expose `list_councils`; run the desktop app rather than a browser tab",
                  );
                }
                const councils = await transport.listCouncils();
                setCouncilNames(councils.map((council) => council.name));
                return councils;
              }}
              onDelete={
                transport?.deleteCouncil &&
                (async (name) => {
                  await transport.deleteCouncil!(name);
                  setCouncilNames((current) =>
                    current?.filter((candidate) => candidate !== name),
                  );
                })
              }
              onCreate={() => setBuildingCouncil(true)}
              onRun={(name) => {
                setCouncilToRun(name);
                setCurrentView("councilResults");
              }}
            />
          ))}

        {currentView === "councilResults" && (
          <CouncilResults
            initialCouncil={councilToRun}
            councilNames={councilNames}
            repository={repository === undefined ? undefined : (repository?.path ?? null)}
            onLoad={async () => {
              if (!transport?.listCouncilResults) {
                throw new Error(
                  "the shell does not expose `list_council_results`; run the desktop app rather than a browser tab",
                );
              }
              return transport.listCouncilResults();
            }}
            onRun={
              transport?.runCouncil &&
              ((name, objective, onProgress) =>
                transport.runCouncil!(
                  name,
                  objective,
                  // The repository is the operator's selection, never a guess;
                  // passing `null` lets the shell refuse when none is chosen.
                  { repository: repository?.path ?? null, sessionId: state.activeSessionId },
                  onProgress,
                ))
            }
          />
        )}
      </main>

      {shortcutsOpen && <ShortcutsCard onClose={() => setShortcutsOpen(false)} />}
      <CommandPalette
        open={paletteOpen}
        entries={paletteEntries}
        onRun={runPaletteCommand}
        onClose={() => setPaletteOpen(false)}
      />

      <RemoteUiRenderer documents={documents} />
    </div>
  );
};
