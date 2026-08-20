import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { SessionId } from "./types.js";
import { Navigation, type DesktopView } from "./components/Navigation.js";
import { Transcript } from "./components/Transcript.js";
import { Composer } from "./components/Composer.js";
import { Steering } from "./components/Steering.js";
import { PromptQueue } from "./components/PromptQueue.js";
import { ConfirmCancel, runAtStake } from "./components/ConfirmCancel.js";
import { InboxView } from "./components/InboxView.js";
import { AnalyticsDashboard } from "./components/AnalyticsDashboard.js";
import { RemoteUiRenderer } from "./components/RemoteUiRenderer.js";
import { CommandPalette, type PaletteEntry } from "./components/CommandPalette.js";
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
import { ApiKeys } from "./components/ApiKeys.js";
import { ModePicker } from "./components/ModePicker.js";
import {
  Onboarding,
  onboardingSkipped,
  readOnboardingStatus,
  setOnboardingSkipped,
  shouldOpenOnboarding,
} from "./components/Onboarding.js";
import { shellAvailable } from "./components/localConfig";
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
import type { DesktopTransport } from "./transport.js";
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
   * Omitted in the app today, because NONE of those bridge commands are
   * registered in `src-tauri/src/bridge.rs` yet. Each surface then renders an
   * explicit unavailable panel naming the command it is waiting for — never an
   * empty list, which would assert there is nothing to show.
   */
  knowledge?: KnowledgeTransport;
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
async function read<T>(
  fetcher: (() => Promise<T[]>) | undefined,
  commands: readonly string[],
  set: React.Dispatch<React.SetStateAction<Loaded<T>>>,
): Promise<void> {
  if (!fetcher) {
    set({ items: [], status: "unavailable", detail: missingBridge(commands) });
    return;
  }
  set({ items: [], status: "loading", detail: null });
  try {
    const items = await fetcher();
    set({ items, status: "loaded", detail: null });
  } catch (error) {
    // A failed read is not an empty read. The surface says so.
    set({ items: [], status: "unavailable", detail: describe(error) });
  }
}

export const App: React.FC<AppProps> = ({
  makeTransport,
  initialView = "sessions",
  notify,
  knowledge,
}) => {
  const [currentView, setCurrentView] = useState<DesktopView>(initialView);
  const [paletteOpen, setPaletteOpen] = useState(false);
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
  const canControlRun = connected && state.activeRunId !== null;
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
    connected && state.activeSessionId !== null && Boolean(transport?.queuePrompt);
  /** Shown when the queue has something to say even if nobody opened it. */
  const queueVisible =
    queueOpen || state.pendingPrompts.length > 0 || state.promptQueueError !== null;

  // A run that ended is no longer cancellable or steerable, so neither
  // affordance may outlive it holding a stale run id.
  useEffect(() => {
    if (state.activeRunId === null) {
      setCancelPending(false);
      setSteeringOpen(false);
    }
  }, [state.activeRunId]);
  // Remote UI documents arrive with adoption 14 milestone 5; until the daemon
  // streams them there are none, and the panel stays closed. Module-level so
  // the renderer is not handed a fresh Map identity on every render.
  const documents = NO_REMOTE_DOCUMENTS;

  // Stable so the memoized `Navigation` — 6 groups and 22 destinations — is
  // skipped entirely while a reply streams, instead of reconciling per token.
  const openPalette = useCallback(() => setPaletteOpen(true), []);
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
  const reject = useCallback(
    (approvalId: string) => void resolveApproval(approvalId, "reject"),
    [resolveApproval],
  );

  const loadSkills = useCallback(
    () =>
      read(knowledge && (() => knowledge.listSkills()), REQUIRED_COMMANDS.skills, setSkills),
    [knowledge],
  );
  const loadMemories = useCallback(
    () =>
      read(
        knowledge && (() => knowledge.listMemories()),
        REQUIRED_COMMANDS.memories,
        setMemories,
      ),
    [knowledge],
  );
  const loadLearnings = useCallback(
    () =>
      read(
        knowledge && (() => knowledge.listLearnings()),
        REQUIRED_COMMANDS.learnings,
        setLearnings,
      ),
    [knowledge],
  );
  const loadDocs = useCallback(
    () => read(knowledge && (() => knowledge.listDocuments()), REQUIRED_COMMANDS.docs, setDocs),
    [knowledge],
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
        setPaletteOpen(true);
        return;
      }
      if (event.key === "Escape") {
        // The palette is the topmost layer, so it closes first. With nothing
        // over the view, Escape walks back out of it instead of doing nothing.
        // `paletteOpen` is read here, not inside the setter's updater —
        // StrictMode double-invokes updaters, and `goBack` pops history.
        if (paletteOpen) {
          setPaletteOpen(false);
        } else {
          goBack();
        }
        return;
      }
      const target = event.target as HTMLElement | null;
      const typing =
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement ||
        target?.isContentEditable === true;
      if (event.key === "/" && !typing) {
        event.preventDefault();
        setPaletteOpen(true);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [paletteOpen, goBack]);

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
    ...(connected && state.activeRunId !== null
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
      description: "edit, review, and publish documents that already exist",
      key: "—",
      group: "Workspace",
    },
    {
      id: "view:skills",
      title: "/skills  Skill Studio · read only",
      description: "inspect registered skills and their permissions",
      key: "—",
      group: "Workspace",
    },
    {
      id: "view:memory",
      title: "/memory  Memory",
      description: "browse curated memories and their provenance",
      key: "—",
      group: "Workspace",
    },
    {
      id: "view:plugins",
      title: "/plugins  Remote UI plugins",
      description: "inspect, smoke-test, scope, approve, reject, or revoke verified UI plugins",
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
    <div style={{ display: "flex", width: "100vw", height: "100vh", overflow: "hidden", background: "#121417" }}>
      <Navigation
        sessions={state.sessions}
        activeSessionId={state.activeSessionId}
        onSelectSession={selectSessionFromNav}
        connectionStatus={state.status}
        statusDetail={state.detail}
        currentView={currentView}
        onSelectView={selectView}
        unreadInboxCount={state.unreadInboxCount}
        onOpenPalette={openPalette}
      />

      <main style={{ flex: 1, display: "flex", flexDirection: "column", height: "100vh", overflow: "hidden" }}>
        {/*
          The connection state moved out of the sidebar footer and up here.
          The green dot beside the title is enough while everything is fine,
          but "not connected" must not be a detail in a corner — it changes
          what every view below can be trusted to mean, so it interrupts the
          content, spans every view, and carries the daemon's own reason.
        */}
        {!connected && (
          <div
            data-testid="connection-banner"
            role={state.status === "disconnected" ? "alert" : "status"}
            style={{
              padding: "8px 24px",
              background: state.status === "disconnected" ? "#2d1214" : "#2b2109",
              borderBottom: `1px solid ${state.status === "disconnected" ? "#da3633" : "#9e6a03"}`,
              color: state.status === "disconnected" ? "#ffa198" : "#e3b341",
              fontSize: 12,
              lineHeight: 1.5,
            }}
          >
            <strong>
              {state.status === "disconnected"
                ? // The reconnect loop only runs when there is a transport to
                  // retry with; outside the shell nothing is coming back.
                  transport
                  ? "Not connected to codypendentd. Reconnecting…"
                  : "Not connected to codypendentd."
                : "Connecting to codypendentd…"}
            </strong>{" "}
            {state.detail}
          </div>
        )}

        {currentView === "sessions" && (
          <>
            <Transcript
              items={state.transcript}
              connectionStatus={state.status}
              statusDetail={state.detail}
              onApprove={connected ? approve : undefined}
              onReject={connected ? reject : undefined}
            />
            {state.error && (
              <div
                role="alert"
                style={{
                  padding: "8px 24px",
                  background: "#2d1214",
                  borderTop: "1px solid #da3633",
                  color: "#ffa198",
                  fontSize: 12,
                }}
              >
                {state.error}
              </div>
            )}
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
                    ? state.activeSessionId === null
                      ? "No session is attached, so there is no queue to change — open or start a session first."
                      : "This build's bridge does not offer the prompt-queue commands."
                    : state.detail
                }
                error={state.promptQueueError}
                onPromote={(promptId) => void promoteQueuedPrompt(promptId)}
                onEdit={(promptId, text) => updateQueuedPrompt(promptId, text)}
                onDelete={(promptId) => void deleteQueuedPrompt(promptId)}
                onClose={() => setQueueOpen(false)}
              />
            )}
            <Composer
              onSend={(text) => void submit(text)}
              onQueue={(text) => void queuePrompt(text)}
              canQueue={canQueuePrompts}
              queuedCount={state.pendingPrompts.length}
              queueOpen={queueVisible}
              onToggleQueue={connected ? () => setQueueOpen((open) => !open) : undefined}
              onRequestCancel={() => setCancelPending(true)}
              isRunning={state.isRunning}
              disabled={!connected}
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
            unavailable={transport ? null : state.detail}
            onOpenBlackboard={(workflowRunId) => {
              setBlackboardRunId(workflowRunId);
              setCurrentView("blackboard");
            }}
          />
        )}

        {currentView === "board" && (
          <KanbanView transport={transport} unavailable={transport ? null : state.detail} />
        )}

        {currentView === "blackboard" && (
          <BlackboardView
            transport={transport}
            workflowRunId={blackboardRunId}
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
            skipped={skipOnboarding}
            onSkip={(skipped) => {
              setOnboardingSkipped(skipped);
              setSkipOnboarding(skipped);
            }}
          />
        )}

        {/* Local-config surfaces. Each fetches its own data and renders its own
            unavailable state, so they take no props — see localConfig.ts. */}
        {currentView === "models" && <ModelPicker />}
        {currentView === "providers" && <ProviderPicker />}
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
                    await knowledge.publishDocument(documentId, target);
                    return "publish requested — the daemon rates and approves it";
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
            repository={repository?.path ?? null}
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
