-- Milestone 3 Task 3.1: the durable, owner-scoped notification and approval
-- inbox. Rows are PRODUCED beside durable approval / question / run-terminal /
-- budget / workflow-block / plugin-permission / runner-failure records; they are
-- never authored by a client. Ownership is derived from the SOURCE row, not from
-- the connection that happens to be attached — a run started by one principal
-- can raise an approval while another client is connected.
--
-- There is deliberately no client-writable column here: `InboxListQuery` and
-- `InboxMutation` carry no owner, no source, and no dedup key
-- (`crates/protocol/src/inbox.rs:1-5`, `:127-129`).

CREATE TABLE inbox_entries (
    -- `InboxEntryId` (UUID).
    id TEXT PRIMARY KEY,
    -- Derived from the source row: approval -> run -> session.owner_uid, via
    -- `crate::ledger::session_owner_uid` (`crates/daemon/src/ledger.rs:135`).
    -- NOT NULL: there are no pre-migration rows to adopt, so NULL is a bug.
    owner_uid INTEGER NOT NULL,
    -- `InboxEntry.repository_id`. Required by the contract, so a producer with
    -- no repository context must resolve one before writing rather than
    -- inventing a placeholder.
    repository_id TEXT NOT NULL,
    -- Serde tag of `InboxEntryKind`. `Unknown` is absent by design: this daemon
    -- is the only writer, so an unrecognised kind is a code defect, not data.
    kind TEXT NOT NULL CHECK (kind IN (
        'ApprovalRequest', 'AgentQuestion', 'RunCompleted', 'RunFailed',
        'BudgetWarning', 'WorkflowBlocked', 'PluginPermissionChanged',
        'RunnerFailed'
    )),
    -- Serde tag of `InboxEntryState`.
    state TEXT NOT NULL DEFAULT 'Unread' CHECK (state IN (
        'Unread', 'Acknowledged', 'Dismissed', 'Resolved'
    )),
    title TEXT NOT NULL,
    -- `InboxEntry.summary` is `skip_serializing_if = "String::is_empty"`, so ''
    -- and absent are the same wire value; store '' rather than NULL to keep the
    -- round trip exact.
    summary TEXT NOT NULL DEFAULT '',
    -- Serialized `InboxSourceIdentity` (externally tagged). Stored whole rather
    -- than as a (type, id) pair because the variants carry differently-typed
    -- ids (`ApprovalId`, `QuestionId`, `RunId`, plain `String` for budget and
    -- runner) and flattening them would lose that.
    source_identity_json TEXT NOT NULL,
    -- `InboxSource.dedup_key`. Daemon-derived from the source identity; the
    -- contract says "replaying the same source must reuse this key"
    -- (`inbox.rs:84`), which is what makes producer retries idempotent.
    dedup_key TEXT NOT NULL,
    -- Serialized `InboxDeepLink`. A typed navigation target, never a URL:
    -- clients must not have to parse anything (`inbox.rs:94`).
    deep_link_json TEXT NOT NULL,
    -- `InboxSource`'s optional correlation ids, denormalized out of the JSON so
    -- the resolution sweep below can find every entry for a terminating run
    -- with an index instead of a JSON scan.
    session_id TEXT REFERENCES sessions(id),
    run_id TEXT REFERENCES runs(id),
    -- Logical only, no FK: `workflow_runs.workflow_id` is not a primary key
    -- (0010), so a reference here would not resolve.
    workflow_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    acknowledged_at TEXT,
    dismissed_at TEXT,
    -- Set ONLY when the authoritative source operation resolves. Acknowledging
    -- or dismissing an entry never decides an approval or a question
    -- (`inbox.rs:148-151`) — these CHECKs make that a schema invariant rather
    -- than a convention a handler can forget.
    resolved_at TEXT,
    CHECK (state != 'Acknowledged' OR acknowledged_at IS NOT NULL),
    CHECK (state != 'Dismissed'    OR dismissed_at    IS NOT NULL),
    CHECK (state != 'Resolved'     OR resolved_at     IS NOT NULL)
);

-- The deduplication contract from plan §291. Producers use
-- `INSERT ... ON CONFLICT(owner_uid, dedup_key) DO UPDATE SET ...` so a
-- re-delivered or replayed source updates one row instead of minting a second.
-- Scoped to the owner, not global: two principals may legitimately hold
-- entries with the same source-derived key.
CREATE UNIQUE INDEX idx_inbox_entries_dedup
    ON inbox_entries (owner_uid, dedup_key);

-- The list page. Leading `owner_uid` is what makes the owner predicate an index
-- seek rather than a filter applied after counting — the property that keeps
-- page length from leaking another user's volume.
CREATE INDEX idx_inbox_entries_owner_page
    ON inbox_entries (owner_uid, state, created_at DESC, id);
CREATE INDEX idx_inbox_entries_owner_repository
    ON inbox_entries (owner_uid, repository_id, created_at DESC, id);

-- The resolution sweep: when a run reaches a terminal state, every entry whose
-- source is that run flips to 'Resolved'.
CREATE INDEX idx_inbox_entries_run
    ON inbox_entries (run_id) WHERE run_id IS NOT NULL;
CREATE INDEX idx_inbox_entries_session
    ON inbox_entries (session_id) WHERE session_id IS NOT NULL;

-- Adapter delivery is kept OUT of `inbox_entries` (plan §291: "separate
-- adapter-delivery attempts"). Entry state is what the human did; delivery is
-- what each client was told, and one entry has many adapters.
CREATE TABLE inbox_delivery_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id TEXT NOT NULL REFERENCES inbox_entries(id),
    -- 'desktop-native' | 'vscode-native' | 'tui' | policy adapters such as
    -- 'email' / 'chat', which ship disabled (plan §302).
    adapter TEXT NOT NULL,
    -- Correlation only, never authority: `client_id` confers no identity
    -- (`crates/daemon/src/principal.rs:16-18`).
    client_id TEXT,
    state TEXT NOT NULL CHECK (state IN ('delivered', 'suppressed', 'failed')),
    attempted_at TEXT NOT NULL,
    -- Bounded failure reason for diagnostics. Never surfaced on the wire.
    detail TEXT
);

-- The durable "notify once, across reconnect" claim (plan §300). PARTIAL so a
-- failed attempt can be retried while a second SUCCESSFUL delivery is
-- impossible. This is a database constraint precisely because client-side
-- memory does not survive the reconnect the requirement is about.
CREATE UNIQUE INDEX idx_inbox_delivery_once
    ON inbox_delivery_attempts (entry_id, adapter) WHERE state = 'delivered';
CREATE INDEX idx_inbox_delivery_entry
    ON inbox_delivery_attempts (entry_id, attempted_at);
