# M6 — Cross-repository architecture intelligence (`crates/federation`)

**Audience:** the implementer of Milestone 6 of
[`plans/2026-08-16-hybrid-platform-program.md`](../plans/2026-08-16-hybrid-platform-program.md) (Tasks 6.1–6.4).
**Read first:** [`00-conventions-and-traps.md`](./00-conventions-and-traps.md) — repository-wide traps
are there and are not repeated here.
**Design authority:** [`specs/2026-08-16-hybrid-platform-program-design.md`](../specs/2026-08-16-hybrid-platform-program-design.md)
§4.1, §6.3, §6.4, §8.7.

The plan gives you files, TDD steps and commit messages. This document gives you the schema, the
classification algebra, the authority rules per table, and the things in the shipped tree that will
bite you.

---

## 1. Status — verified against the tree on 2026-08-17

| Path | State |
|---|---|
| `crates/federation/` | **absent** |
| `crates/control-plane/` | absent (M7) |
| `crates/control-plane-protocol/` | absent (M7) |
| `sdk/control-plane/` | absent (M7) |
| `sdk/control-plane-react/` | absent (M7) |
| `apps/web/` | absent (M7) |
| `crates/protocol/src/federated_graph.rs` | absent |
| `migrations/0047_graph_publication.sql`, `0048_multi_repo_campaigns.sql` | absent |

M6 is a **greenfield crate over shipped local storage**. Nothing about federation exists yet; every
input it federates does:

| Input | Where it is today |
|---|---|
| Code graph nodes/edges | `migrations/0003_phase2.sql:81` (`code_nodes`), `:98` (`code_edges`); `migrations/0004_codegraph_source_path.sql:17` adds `source_path` |
| Graph writes | `crates/knowledge/src/codegraph.rs:672` `upsert_file_graph` — per-file transactional upsert keyed `(repository, symbol_key)` |
| Local repository identity | `crates/knowledge/src/codegraph.rs:174` `stable_repository_id` |
| Local (private) graph reads | `crates/daemon/src/codegraph.rs:1-30` — the repository-bound seam; keep it as-is |
| Derived-index outbox | `crates/knowledge/src/outbox.rs:55` `enqueue` — same-transaction append pattern to copy |
| Owner-scoped store + cursor | `crates/daemon/src/session_library.rs:111` |
| Idempotent owned workflow runs | `crates/workflow/src/store.rs:354` `create_run_idempotent_owned` |
| Approval action digests | `crates/daemon/src/approvals.rs:970` `action_digest` |

### Migration numbering caveat — check before you write the file

The highest migration on disk is `0040_session_library.sql`. `0041`–`0046` are reserved by the plan
for M2–M5 and **do not exist yet**. `0047`/`0048` are correct only if those milestones land first.
If M6 is being implemented out of order, take the next two free numbers and update the plan's
dependency map in the same commit — do not leave a gap and do not squat on a number another
in-flight milestone claimed (three specs once all claimed `0034`).

---

## 2. Data model

Both files are root SQLite migrations: append-only, immutable once merged, and gated by
`migrations/checksums.json` via `.github/scripts/check_migration_immutability.py`. Run
`python3 .github/scripts/check_migration_immutability.py --update` and commit the regenerated
checksums **in the same commit** as the SQL.

### 2.1 `migrations/0047_graph_publication.sql`

```sql
-- M6 Task 6.1 — outbound, policy-gated publication of code-graph facts.
--
-- Nothing in this file is authoritative. `code_nodes`/`code_edges` remain the
-- only truth about this checkout; every table here is an outbound projection
-- plus the provenance record of what left the machine and under what policy.

-- ---------------------------------------------------------------------------
-- Federated repository identity
-- ---------------------------------------------------------------------------
-- `stable_repository_id` (crates/knowledge/src/codegraph.rs:174) is the first
-- 16 bytes of SHA-256 over the CANONICAL LOCAL PATH. It is a correct local
-- partition key and an unusable federated one: two clones of the same
-- repository at different paths get different ids, and the same path on two
-- machines collides. Federation therefore mints its own identity and keeps the
-- local id only as a join key.
CREATE TABLE federated_repository_identity (
    -- The local RepositoryId (path-derived). Join key to code_nodes.repository.
    repository_id TEXT PRIMARY KEY,
    -- SHA-256 hex of `root_commit || '\n' || normalized_remote`. Deterministic
    -- across machines and checkout paths, which is the whole point.
    federated_id TEXT NOT NULL UNIQUE CHECK (length(federated_id) = 64),
    -- The repository's first root commit. Survives renames, host migrations and
    -- remote changes; a fork shares it, which is correct — a fork IS the same
    -- code lineage and its policy is still evaluated separately per row.
    root_commit TEXT NOT NULL CHECK (length(root_commit) BETWEEN 7 AND 64),
    -- Scheme/credentials/`.git`/trailing-slash stripped, host and path
    -- lowercased. NULL for a repository with no remote: such a repository can
    -- still be published (root_commit alone identifies it) but two unrelated
    -- local-only repositories that share a root commit would collide, so the
    -- publisher refuses to publish a NULL-remote repository above
    -- 'metadata-shared'.
    normalized_remote TEXT,
    -- Operator-visible label. Never used for identity or authorization.
    display_name TEXT NOT NULL,
    established_at TEXT NOT NULL,
    -- Kernel-derived uid of the principal that established the identity, per
    -- migrations/0031_multi_user.sql:17. Not a wire value.
    established_by_uid INTEGER NOT NULL
);

-- ---------------------------------------------------------------------------
-- Publication policy (design §6.4)
-- ---------------------------------------------------------------------------
-- One row per repository. Absent row == 'private-local' == publish nothing.
-- This table can only ever NARROW: it is intersected with, never substituted
-- for, `crates/daemon/src/policy/` decisions.
CREATE TABLE graph_publication_policy (
    repository_id TEXT PRIMARY KEY
        REFERENCES federated_repository_identity(repository_id),
    -- The most permissive class any fact from this repository may reach.
    -- Default is the strictest value; the metadata-only product default is an
    -- explicit operator action that writes 'metadata-shared', never a default.
    max_class TEXT NOT NULL DEFAULT 'private-local' CHECK (max_class IN (
        'private-local', 'metadata-shared', 'content-shared',
        'organization-knowledge', 'public-marketplace'
    )),
    -- The DataClassification ceiling (crates/protocol/src/artifact.rs:41).
    -- Orthogonal to max_class: class decides AUDIENCE, classification decides
    -- SENSITIVITY. Publication requires both gates to pass — see §3.2.
    max_classification TEXT NOT NULL DEFAULT 'internal' CHECK (max_classification IN (
        'public', 'internal', 'confidential', 'secret'
    )),
    -- Field-level opt-ins. Every one defaults to off, because each leaks
    -- progressively more of the private tree: a symbol name leaks API surface,
    -- a source path leaks directory layout, a signature hash allows
    -- confirmation of a guessed signature.
    publish_symbol_names INTEGER NOT NULL DEFAULT 0 CHECK (publish_symbol_names IN (0, 1)),
    publish_source_paths INTEGER NOT NULL DEFAULT 0 CHECK (publish_source_paths IN (0, 1)),
    publish_signature_hashes INTEGER NOT NULL DEFAULT 0 CHECK (publish_signature_hashes IN (0, 1)),
    publish_evidence_artifacts INTEGER NOT NULL DEFAULT 0 CHECK (publish_evidence_artifacts IN (0, 1)),
    -- Monotonically incremented on every change. Stamped onto every publication
    -- record so a later audit can answer "under which policy did this leave?",
    -- and so a policy tightening can find every row published under a looser
    -- version and tombstone it.
    policy_version INTEGER NOT NULL DEFAULT 1 CHECK (policy_version >= 1),
    updated_at TEXT NOT NULL,
    updated_by_uid INTEGER NOT NULL
);

-- ---------------------------------------------------------------------------
-- The outbound projection
-- ---------------------------------------------------------------------------
-- Published node facts. Deliberately NOT a view over code_nodes: a view would
-- silently re-widen when policy changes, and would expose `code_nodes.id`,
-- which is a local row identity and must never leave the machine.
CREATE TABLE shared_graph_node (
    -- Stable, content-derived, cross-machine identity:
    -- SHA-256(federated_id || '\0' || symbol_key). Two machines with the same
    -- checkout independently derive the same value, which is what makes
    -- publication idempotent without a coordinator.
    shared_node_id TEXT PRIMARY KEY CHECK (length(shared_node_id) = 64),
    repository_id TEXT NOT NULL
        REFERENCES federated_repository_identity(repository_id),
    -- The local row this projects. Nullable so a tombstone survives the local
    -- row's deletion; ON DELETE is not used because SQLite would then erase the
    -- retraction evidence.
    code_node_id TEXT,
    -- Redacted-by-policy payload. Fields the policy withholds are absent, NOT
    -- empty strings: an empty string is a measurable difference from a missing
    -- field and therefore an oracle.
    kind TEXT NOT NULL,
    language TEXT NOT NULL,
    package TEXT,
    qualified_name TEXT,       -- NULL unless publish_symbol_names = 1
    source_path TEXT,          -- NULL unless publish_source_paths = 1
    signature_hash TEXT,       -- NULL unless publish_signature_hashes = 1
    -- The class this node was computed at. Denormalized because every edge and
    -- index that inherits from it reads it on the hot path.
    class TEXT NOT NULL CHECK (class IN (
        'private-local', 'metadata-shared', 'content-shared',
        'organization-knowledge', 'public-marketplace'
    )),
    classification TEXT NOT NULL CHECK (classification IN (
        'public', 'internal', 'confidential', 'secret'
    )),
    -- Git revision the fact was observed at, and the SHA-256 of the canonical
    -- serialization of the published payload. The hash is the idempotency key
    -- for republication: identical hash == no delta to send.
    revision TEXT NOT NULL,
    content_hash TEXT NOT NULL CHECK (length(content_hash) = 64),
    computed_at TEXT NOT NULL,
    UNIQUE (repository_id, code_node_id)
);

CREATE INDEX idx_shared_graph_node_repo_class
    ON shared_graph_node (repository_id, class, shared_node_id);
CREATE INDEX idx_shared_graph_node_code_node
    ON shared_graph_node (code_node_id) WHERE code_node_id IS NOT NULL;

-- Published edge facts, including cross-repository ones.
CREATE TABLE shared_graph_edge (
    shared_edge_id TEXT PRIMARY KEY CHECK (length(shared_edge_id) = 64),
    from_shared_node_id TEXT NOT NULL REFERENCES shared_graph_node(shared_node_id),
    to_shared_node_id TEXT NOT NULL REFERENCES shared_graph_node(shared_node_id),
    -- Denormalized endpoint repositories. A cross-repository edge's class is
    -- bounded by BOTH repositories' policies, and joining twice through
    -- shared_graph_node on every traversal step is the difference between an
    -- index seek and a nested loop.
    from_repository_id TEXT NOT NULL
        REFERENCES federated_repository_identity(repository_id),
    to_repository_id TEXT NOT NULL
        REFERENCES federated_repository_identity(repository_id),
    relation TEXT NOT NULL,
    confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    evidence_kind TEXT NOT NULL,
    -- The evidence artifact (file + byte range) is a source-location leak, so
    -- it is NULL unless publish_evidence_artifacts = 1.
    evidence_artifact TEXT,
    -- The INHERITED class: the strictest of the two endpoints, the two
    -- repository policies, and the evidence. Stored, not computed at read time,
    -- so a traversal cannot be tricked into recomputing it from stale inputs.
    class TEXT NOT NULL CHECK (class IN (
        'private-local', 'metadata-shared', 'content-shared',
        'organization-knowledge', 'public-marketplace'
    )),
    classification TEXT NOT NULL CHECK (classification IN (
        'public', 'internal', 'confidential', 'secret'
    )),
    -- SHA-256 over the exact tuple of class inputs that produced `class`. A
    -- reclassification pass recomputes the digest and, on mismatch, tombstones
    -- and republishes. Without it, a node narrowed from content-shared to
    -- private-local leaves its edges published at the old, wider class.
    class_inputs_digest TEXT NOT NULL CHECK (length(class_inputs_digest) = 64),
    revision TEXT NOT NULL,
    content_hash TEXT NOT NULL CHECK (length(content_hash) = 64),
    computed_at TEXT NOT NULL,
    UNIQUE (from_shared_node_id, to_shared_node_id, relation)
);

CREATE INDEX idx_shared_graph_edge_from
    ON shared_graph_edge (from_shared_node_id, class);
CREATE INDEX idx_shared_graph_edge_to
    ON shared_graph_edge (to_shared_node_id, class);
-- Reclassification sweeps scan by repository; both endpoint columns are
-- indexed because a narrowing in repository B must find edges that only
-- *point into* B.
CREATE INDEX idx_shared_graph_edge_from_repo ON shared_graph_edge (from_repository_id);
CREATE INDEX idx_shared_graph_edge_to_repo ON shared_graph_edge (to_repository_id);

-- ---------------------------------------------------------------------------
-- Publication provenance and idempotency (design §6.4)
-- ---------------------------------------------------------------------------
-- A batch is the unit of at-least-once delivery. `idempotency_key` is
-- principal-scoped for the same reason workflow runs are
-- (crates/workflow/src/store.rs:392): two users may legitimately generate the
-- same client-local key.
CREATE TABLE graph_publication_batch (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL
        REFERENCES federated_repository_identity(repository_id),
    owner_uid INTEGER NOT NULL,
    idempotency_key TEXT NOT NULL,
    policy_version INTEGER NOT NULL,
    -- 'building' rows are invisible to a consumer; only 'sealed' is a
    -- publishable unit, and 'acknowledged' records the remote receipt. There is
    -- no 'failed': a batch that cannot be delivered stays 'sealed' and is
    -- retried, because the alternative is losing the record of what was already
    -- sent.
    state TEXT NOT NULL CHECK (state IN ('building', 'sealed', 'acknowledged')),
    fact_count INTEGER NOT NULL DEFAULT 0 CHECK (fact_count >= 0),
    -- Merkle root over the batch's ordered content hashes. The consumer
    -- verifies it before applying anything, so a truncated transfer is a
    -- rejected batch rather than a partially applied one.
    batch_hash TEXT CHECK (batch_hash IS NULL OR length(batch_hash) = 64),
    sealed_at TEXT,
    -- Opaque receipt returned by the consumer. In M6 the only consumer is a
    -- local test double; M7 supplies the real one. Never parsed for authority.
    remote_receipt TEXT,
    acknowledged_at TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (owner_uid, idempotency_key),
    CHECK (state <> 'sealed' OR (batch_hash IS NOT NULL AND sealed_at IS NOT NULL)),
    CHECK (state <> 'acknowledged' OR (remote_receipt IS NOT NULL AND acknowledged_at IS NOT NULL))
);

CREATE INDEX idx_graph_publication_batch_pending
    ON graph_publication_batch (repository_id, created_at)
    WHERE state = 'sealed';

-- One row per fact that left the machine. This is the audit answer to "what did
-- we publish, when, under which policy, by whom, at which hash".
CREATE TABLE graph_publication (
    id TEXT PRIMARY KEY,
    batch_id TEXT NOT NULL REFERENCES graph_publication_batch(id),
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('node', 'edge')),
    -- shared_node_id or shared_edge_id. Not a foreign key: the record must
    -- outlive a retraction that deletes the projection row.
    subject_id TEXT NOT NULL CHECK (length(subject_id) = 64),
    repository_id TEXT NOT NULL
        REFERENCES federated_repository_identity(repository_id),
    class TEXT NOT NULL,
    classification TEXT NOT NULL,
    -- The decision, not just the outcome: 'published', 'withheld-class',
    -- 'withheld-classification', 'withheld-field', 'retracted'. A withheld row
    -- is recorded so an operator can see WHY a fact never left, without which
    -- "nothing published" is indistinguishable from "publisher broken".
    decision TEXT NOT NULL CHECK (decision IN (
        'published', 'withheld-class', 'withheld-classification',
        'withheld-field', 'retracted'
    )),
    policy_version INTEGER NOT NULL,
    content_hash TEXT NOT NULL CHECK (length(content_hash) = 64),
    -- Envelope-encryption state for the published bytes. 'none' is legal only
    -- at 'metadata-shared' and below.
    encryption TEXT NOT NULL DEFAULT 'none' CHECK (encryption IN ('none', 'envelope')),
    -- Retention class the consumer must honour; recorded locally so a deletion
    -- request can be proven to have been issued.
    retention_class TEXT NOT NULL DEFAULT 'default',
    actor_uid INTEGER NOT NULL,
    published_at TEXT NOT NULL,
    UNIQUE (batch_id, subject_kind, subject_id)
);

CREATE INDEX idx_graph_publication_subject
    ON graph_publication (subject_kind, subject_id, published_at DESC);
CREATE INDEX idx_graph_publication_repo_policy
    ON graph_publication (repository_id, policy_version);

-- ---------------------------------------------------------------------------
-- Tombstones (design §6.4: consumed BEFORE new deltas)
-- ---------------------------------------------------------------------------
CREATE TABLE graph_tombstone (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL
        REFERENCES federated_repository_identity(repository_id),
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('node', 'edge', 'repository')),
    subject_id TEXT NOT NULL,
    -- 'deleted'   — the local fact no longer exists
    -- 'narrowed'  — policy tightened; the fact still exists but not at the
    --               class it was published at
    -- 'revoked'   — an operator explicitly retracted it
    reason TEXT NOT NULL CHECK (reason IN ('deleted', 'narrowed', 'revoked')),
    -- The class the fact was published at, so a consumer knows which of its
    -- copies to drop when a fact was published at several classes over time.
    published_class TEXT NOT NULL,
    created_at TEXT NOT NULL,
    created_by_uid INTEGER NOT NULL,
    -- NULL until the consumer confirms. The reconnect ordering rule keys off
    -- exactly this: unacknowledged tombstones are drained before any new batch
    -- is sealed.
    acknowledged_at TEXT,
    remote_receipt TEXT,
    UNIQUE (repository_id, subject_kind, subject_id, created_at)
);

CREATE INDEX idx_graph_tombstone_unacknowledged
    ON graph_tombstone (repository_id, created_at)
    WHERE acknowledged_at IS NULL;
```

### 2.2 `migrations/0048_multi_repo_campaigns.sql`

```sql
-- M6 Task 6.3 — coordinated multi-repository campaigns.
--
-- A campaign is a COORDINATOR, never an authority. It aggregates the outcomes
-- of ordinary per-repository workflow runs created through
-- WorkflowStore::create_run_idempotent_owned (crates/workflow/src/store.rs:354).
-- It grants nothing: no shared worktree, no shared budget, no blanket approval,
-- no shared secret lease.

CREATE TABLE campaigns (
    id TEXT PRIMARY KEY,
    -- Kernel-derived; the coordinator's authority never exceeds this uid's.
    owner_uid INTEGER NOT NULL,
    title TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN (
        'api-migration', 'schema-migration', 'dependency-upgrade',
        'ownership-review', 'custom'
    )),
    -- The workflow every child run instantiates. One workflow, N runs — there
    -- is no campaign-specific runtime.
    workflow_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'planning', 'running', 'partially-failed', 'completed', 'cancelled'
    )),
    -- Denormalized rollup, maintained in the same transaction as the child
    -- transitions. Never authoritative — recomputable from campaign_runs.
    repository_count INTEGER NOT NULL DEFAULT 0 CHECK (repository_count >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    terminal_at TEXT,
    UNIQUE (owner_uid, idempotency_key),
    CHECK (terminal_at IS NULL OR state IN ('completed', 'cancelled', 'partially-failed'))
);

CREATE INDEX idx_campaigns_owner_state ON campaigns (owner_uid, state, updated_at DESC);

CREATE TABLE campaign_repositories (
    campaign_id TEXT NOT NULL REFERENCES campaigns(id),
    repository_id TEXT NOT NULL,
    -- Snapshot of the federated identity at enrolment, so a later identity
    -- change cannot silently retarget an in-flight campaign.
    federated_id TEXT NOT NULL,
    -- Per-repository worktree. NOT shared: a shared worktree would let a
    -- denial in repository A be bypassed by an approved write in repository B.
    worktree_path TEXT,
    -- Per-repository ceiling in the smallest currency unit. NULL means "no
    -- campaign-specific ceiling" — the repository's own budget still applies.
    -- It is never a grant: it can only lower the effective ceiling.
    budget_minor_units INTEGER CHECK (budget_minor_units IS NULL OR budget_minor_units >= 0),
    -- 'per-effect' requires a separate human decision for every proposed
    -- action in this repository. There is deliberately no 'campaign-wide'
    -- value: blanket approval across repositories is prohibited by design §8.7.
    approval_mode TEXT NOT NULL DEFAULT 'per-effect'
        CHECK (approval_mode IN ('per-effect', 'per-run')),
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'running', 'succeeded', 'failed', 'denied', 'skipped'
    )),
    enrolled_at TEXT NOT NULL,
    terminal_at TEXT,
    PRIMARY KEY (campaign_id, repository_id)
);

CREATE INDEX idx_campaign_repositories_state
    ON campaign_repositories (campaign_id, state);

CREATE TABLE campaign_runs (
    campaign_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    -- The ordinary workflow run. UNIQUE because one run belongs to at most one
    -- campaign slot; a retry mints a new run with a new attempt number rather
    -- than rebinding this one.
    run_id TEXT NOT NULL UNIQUE,
    attempt INTEGER NOT NULL CHECK (attempt >= 1),
    -- The exact key handed to create_run_idempotent_owned. Persisting it makes
    -- a crashed coordinator's retry adopt the existing run instead of forking
    -- a second one.
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    terminal_at TEXT,
    PRIMARY KEY (campaign_id, repository_id, attempt),
    FOREIGN KEY (campaign_id, repository_id)
        REFERENCES campaign_repositories(campaign_id, repository_id)
);

CREATE TABLE campaign_approvals (
    campaign_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    approval_id TEXT NOT NULL,
    -- crates/daemon/src/approvals.rs:970. Bound per repository: the SAME digest
    -- approved in repository A confers nothing in repository B, which is the
    -- concrete meaning of "no blanket approval".
    action_digest TEXT NOT NULL CHECK (length(action_digest) = 64),
    decision TEXT NOT NULL CHECK (decision IN ('pending', 'approved', 'rejected', 'expired')),
    decided_at TEXT,
    decided_by_uid INTEGER,
    PRIMARY KEY (campaign_id, repository_id, approval_id),
    FOREIGN KEY (campaign_id, repository_id)
        REFERENCES campaign_repositories(campaign_id, repository_id)
);

CREATE UNIQUE INDEX idx_campaign_approvals_digest
    ON campaign_approvals (campaign_id, repository_id, action_digest);

-- Effect ledger: what actually happened per repository. `effect_digest` is
-- UNIQUE per (campaign, repository), which is what makes an idempotent retry
-- safe — a re-driven attempt that recomputes the same effect cannot apply it
-- twice.
CREATE TABLE campaign_effects (
    id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    effect_kind TEXT NOT NULL,
    effect_digest TEXT NOT NULL CHECK (length(effect_digest) = 64),
    applied_at TEXT NOT NULL,
    UNIQUE (campaign_id, repository_id, effect_digest),
    FOREIGN KEY (campaign_id, repository_id)
        REFERENCES campaign_repositories(campaign_id, repository_id)
);
```

---

## 3. Publication classes and the classification algebra

### 3.1 The ordering is the inverse of `DataClassification` — do not copy the comparison

`DataClassification::rank` (`crates/protocol/src/artifact.rs:58`) ranks *sensitivity*: higher is
more restrictive, `Unknown` = 4, above `Secret`. `PublicationClass` ranks *audience*: higher is more
shared. The comparison operators are therefore opposite, and `Unknown` must be the **lowest**:

```rust
/// Audience breadth, narrowest (0) to widest (5).
///
/// `Unknown` is 0, NOT 5: an unrecognized class from a newer peer must be
/// treated as the narrowest audience this build knows, exactly as
/// `DataClassification::Unknown` is treated as the most sensitive.
pub fn breadth(self) -> u8 {
    match self {
        PublicationClass::Unknown => 0,
        PublicationClass::PrivateLocal => 1,
        PublicationClass::MetadataShared => 2,
        PublicationClass::ContentShared => 3,
        PublicationClass::OrganizationKnowledge => 4,
        PublicationClass::PublicMarketplace => 5,
    }
}

/// The strictest (narrowest) of two classes. Inheritance is always a MIN.
pub fn strictest(self, other: Self) -> Self { if self.breadth() <= other.breadth() { self } else { other } }
```

Follow the shipped protocol enum convention (`crates/protocol/src/inbox.rs:15-31`):
`#[serde(tag = "type")]`, `#[non_exhaustive]`, `#[serde(other)] Unknown`.

### 3.2 Two independent gates, both must pass

A fact is publishable at class `C` only if **both** hold:

1. `C.breadth() <= policy.max_class.breadth()` — audience gate.
2. `fact.classification.allowed_off_device(policy.max_classification)` — sensitivity gate, using
   the shipped `DataClassification::allowed_off_device` (`crates/protocol/src/artifact.rs:72`).

A derived classification may only **raise** sensitivity, never lower it (the existing rule in
`crates/codypendentd/src/routing.rs`). Record which gate refused in
`graph_publication.decision` (`withheld-class` vs `withheld-classification`) — an operator cannot
debug "nothing published" otherwise.

### 3.3 Exactly how an edge computes its inherited class

```
class(edge) = strictest(
    class(from_node),                       -- endpoint A's computed class
    class(to_node),                         -- endpoint B's computed class
    policy(from_repository).max_class,      -- both repositories bound a cross-repo edge
    policy(to_repository).max_class,
    class_floor(evidence_kind)              -- see below
)
classification(edge) = max_sensitivity(classification(from_node), classification(to_node))
```

`class_floor(evidence_kind)` exists because evidence carries more than the endpoints do. An edge
whose `evidence_artifact` names a file and byte range is a source-location disclosure; if
`publish_evidence_artifacts = 0` the artifact is dropped and the edge publishes normally, but if the
evidence *kind itself* is derived from unpublished content (an agent assertion over private
transcript, for instance) the floor is `private-local` and the edge does not publish at all.

Two rules that are easy to miss and both have to be tested:

- **A cross-repository edge is bounded by the stricter of the two repositories.** If repository A is
  `organization-knowledge` and B is `metadata-shared`, every A→B and B→A edge is `metadata-shared`.
  This is why `from_repository_id`/`to_repository_id` are denormalized onto the edge.
- **Narrowing must propagate.** `class_inputs_digest` is SHA-256 over the canonical tuple
  `(from_class, to_class, from_policy_version, to_policy_version, evidence_floor)`. A policy change
  bumps `policy_version`, which changes the digest, which makes the reclassification sweep find
  every affected edge. Without the digest, narrowing a node leaves its edges published at the old
  class — the single most likely real leak in M6.

Search indexes over published facts obey the same rule: an index entry's class is the `strictest`
over every contributing source. Model the contributing sources the way
`session_search_sources` does (`migrations/0040_session_library.sql`) — one row per
`(subject, source_type, source_id)` carrying the source's own hash and class — so the inherited
value is recomputable and auditable rather than a number someone wrote once.

### 3.4 Tombstone ordering

Design §6.4: reconnecting daemons consume tombstones **before** publishing new deltas. Concretely,
the publisher's outbound loop is:

1. `SELECT ... FROM graph_tombstone WHERE acknowledged_at IS NULL ORDER BY created_at` — send and
   acknowledge all of them.
2. Only if step 1 drained cleanly, seal a new `graph_publication_batch`.
3. A batch sealed while unacknowledged tombstones exist is a bug; assert it, do not tolerate it.

The failure this prevents: delete a symbol offline, reconnect, the incremental publisher re-derives
the symbol from a stale projection row and the consumer resurrects it.

---

## 4. Authority boundaries, per table

The local daemon stays authoritative for source, private history, artifacts, secrets and local
effects. In M6 there is no cloud yet, so the boundary is expressed as: *the publication path may
only ever read authoritative tables and write projection tables.*

| Table | May the publisher write it? | Authority note |
|---|---|---|
| `code_nodes`, `code_edges` | **No** | Authoritative. Publication is read-only over them. A publisher that writes here has inverted the boundary. |
| `sessions`, `events`, `artifacts` | **No** | Never inputs to graph publication at all under metadata-only policy. |
| `federated_repository_identity` | Yes, on explicit operator action | Establishing an identity is a local decision; nothing remote may create one. |
| `graph_publication_policy` | Yes, local operator only | **Narrowing-only intersection.** A future remote policy value is intersected with this row, never written into it. |
| `shared_graph_node/_edge` | Yes | Projection. Deleting every row here loses nothing authoritative. |
| `graph_publication`, `graph_tombstone` | Yes, append-only | Audit. Never updated except to stamp `acknowledged_at`/`remote_receipt`. |
| `campaigns` and friends | Yes | Coordination only. Every actual effect goes through the ordinary run/approval path. |

**Identity rules.** `owner_uid`, `established_by_uid`, `actor_uid`, `created_by_uid` and
`decided_by_uid` are all derived from `PeerPrincipal` (`crates/daemon/src/principal.rs:27`) — the
kernel's `SO_PEERCRED`. No federation type deserializes an owner, repository or policy field from
the wire. Rows predating an ownership column are adopted by the daemon uid, matching
`migrations/0031_multi_user.sql:17` and the `NamedResource::Artifact` arm in
`crates/daemon/src/server.rs`.

**Narrowing, concretely.** When M7 arrives and a control plane sends an organization publication
policy, the effective policy is:

```rust
let effective_max_class = local.max_class.strictest(remote.max_class);
let effective_max_classification = min_permissive(local.max_classification, remote.max_classification);
```

There is no code path in which `remote` replaces `local`. Write the test now, in M6, with a stub
remote policy — it is much harder to retrofit once M7's sync exists.

---

## 5. Unauthorized and absent must be indistinguishable — under traversal

Locally this is easier than in M7 because the only principal axis is the uid. The hard part is
**traversal**, and the plan's Task 6.2 test ("byte-equivalent absent/inaccessible seed results")
only covers the endpoints. Also cover the middle.

**Apply grants at seed selection *and* at every recursive step.** The idiomatic recursive CTE is
wrong by default:

```sql
-- WRONG: authorizes the seed, then walks freely.
WITH RECURSIVE reach(id) AS (
    SELECT shared_node_id FROM shared_graph_node WHERE shared_node_id = ?1 AND <authorized>
    UNION
    SELECT e.to_shared_node_id FROM shared_graph_edge e JOIN reach ON e.from_shared_node_id = reach.id
)
```

An unauthorized node in the middle is not returned, but its *edges* still extend the frontier, so
its existence is observable in the result set beyond it, in the hop counts, and in the path lengths.
The authorization predicate must appear in the recursive term as well:

```sql
    UNION
    SELECT e.to_shared_node_id
    FROM shared_graph_edge e
    JOIN reach ON e.from_shared_node_id = reach.id
    JOIN shared_graph_node n ON n.shared_node_id = e.to_shared_node_id
    WHERE <authorized(n)> AND <authorized_edge(e)>
```

Then the checklist:

- **Counts.** Every count runs inside the authorized set. A "3 of 47 results hidden" affordance is a
  disclosure; there are 3 results.
- **Traversal.** A path that transits an inaccessible node does not exist. Not "exists but is
  redacted" — the two are distinguishable by path length.
- **Pagination.** Cursors must not encode absolute offsets over the unfiltered set, or page 2's
  first item reveals how many rows page 1 skipped. Copy `session_library.rs`: a keyset cursor whose
  `query_hash` binds the principal (`crates/daemon/src/session_library.rs:121`), rejected as
  `InvalidCursor` when replayed against a different principal or query.
- **Error bodies.** One refusal for "no such node" and "not your node" — the shipped precedent is
  `node_not_found` in `crates/daemon/src/codegraph.rs`, which already says exactly this. Reuse it;
  do not mint a `federation.forbidden`.
- **Timing.** Aim for the same *class* of work, not constant time: authorize before you load, so an
  unauthorized query does the same "resolve, deny, return empty" work regardless of whether the row
  exists. Put the whole read on one transaction snapshot (as `search_sessions` does at
  `crates/daemon/src/session_library.rs:120`) so a concurrent policy change cannot leak rows selected
  under the old policy.

---

## 6. Crate and protocol wiring

**`crates/federation` layout** (per plan Task 6.1/6.2/6.3):

```
crates/federation/
  Cargo.toml            # workspace member — add to Cargo.toml [workspace].members
  src/lib.rs
  src/identity.rs       # federated_id derivation; NOT stable_repository_id
  src/publication.rs    # PublicationPolicy, class algebra, redaction
  src/authorization.rs  # authorized-set predicates shared by every query
  src/store.rs          # SharedGraphStore over the 0047 tables
  src/query.rs          # blast radius, migration plan, ownership (Task 6.2)
  src/campaign.rs       # Task 6.3
  tests/publication_it.rs
```

Dependency direction: `federation` depends on `protocol` and `sqlx`; it must **not** depend on
`daemon` (daemon sits below knowledge and would create a cycle). Wire it in `codypendentd`, the
assembly crate, exactly as the code-graph seam is wired (`crates/daemon/src/codegraph.rs:1-30`).

**Protocol additions** — `crates/protocol/src/federated_graph.rs`. Every new client-issued command
needs all seven steps from `00-conventions-and-traps.md` §1. The ones people forget:

- an arm in `role_permits` (`crates/daemon/src/commands.rs:3001`) with a *decided* floor. Federation
  reads are `Observer`; publication and campaign mutations are `Contributor` at minimum; changing
  `graph_publication_policy` is `Controller`;
- an entry in `every_client_issued_command_has_a_decided_role_floor`
  (`crates/daemon/src/commands.rs:5516`);
- a real `named_resources()` result (`crates/protocol/src/command.rs:985`) — the code graph is a
  daemon-wide store, so federation commands name `NamedResource::DaemonStore(DaemonStore::CodeGraph)`,
  plus `NamedResource::Workflow` for campaign runs. **Not an empty vec.**

**Version bump.** `PROTOCOL_V1` is `1.6` (`crates/protocol/src/version.rs:38`). M6's additions are
additive, so `major` stays 1 and `minor` advances; add the paragraph to that doc comment in the same
style as the existing entries.

**Golden vectors.** New `protocol-vectors/federated_graph.json` and `campaign.json` must be added to
the `modeled`/`notModeled` partition list in `extensions/vscode/test/protocol-vectors.test.ts:1371`
(these are TUI/CLI-only commands, so the exclusion list is the right home), and every new
`it(...)` drifts the `doc-count:vitest` markers that only the `extension` CI job settles.

---

## 7. Acceptance criteria

Numbered, objectively checkable, each tied to a test name. Task 6.4's exit gate is these plus the
common gate.

1. `federated_id_is_stable_across_checkout_paths` — two `federated_repository_identity` rows derived
   from the same `(root_commit, normalized_remote)` at different local paths produce the same
   `federated_id`, and `stable_repository_id` for the same two paths produces different values.
2. `absent_policy_publishes_nothing` — a repository with no `graph_publication_policy` row seals a
   batch with `fact_count = 0` and writes `decision = 'withheld-class'` rows.
3. `metadata_only_policy_publishes_no_names_paths_or_signatures` — with `max_class =
   'metadata-shared'` and the three field flags at 0, every `shared_graph_node` row has
   `qualified_name`, `source_path` and `signature_hash` NULL, and no published payload byte matches
   any symbol name or path present in the fixture repository.
4. `published_facts_never_contain_local_row_ids` — no `code_nodes.id`, `code_edges.id`,
   `SessionId`, `RunId` or absolute filesystem path appears anywhere in a sealed batch.
5. `edge_inherits_strictest_of_its_sources` — table-driven over all
   `(from_class, to_class, from_policy, to_policy, evidence_floor)` combinations; the stored `class`
   equals the MIN in every case.
6. `narrowing_a_node_tombstones_its_published_edges` — narrow one endpoint, run the
   reclassification sweep, assert a `graph_tombstone` row with `reason = 'narrowed'` per affected
   edge and that the edge's `class_inputs_digest` changed.
7. `unknown_class_is_treated_as_narrowest` — a batch carrying a class string this build does not
   know deserializes to `Unknown` and publishes nothing.
8. `republishing_an_identical_batch_is_idempotent` — same `(owner_uid, idempotency_key)` returns the
   original batch id and creates no second `graph_publication` row.
9. `tombstones_are_drained_before_a_new_batch_is_sealed` — with an unacknowledged tombstone present,
   sealing is refused; the offline-delete/reconnect scenario ends with the fact absent at the
   consumer.
10. `inaccessible_seed_and_absent_seed_are_byte_identical` — serialized responses compare equal,
    including error code, message, and cursor presence.
11. `hidden_intermediate_nodes_do_not_extend_reachability` — a graph `A → H → B` where `H` is
    inaccessible yields no `B` in the blast radius from `A`, and the result is byte-identical to the
    same query against a graph where `H` does not exist.
12. `pagination_cursor_is_bound_to_its_principal_and_query` — a cursor from principal 1000 replayed
    by 1001, or against a mutated filter, returns `InvalidCursor`, not a page.
13. `campaign_creates_one_child_run_per_repository` — N repositories yield N rows in `campaign_runs`
    with distinct `run_id`s, each created through `create_run_idempotent_owned`.
14. `approval_in_one_repository_does_not_approve_another` — the same `action_digest` approved for
    repository A leaves repository B's approval `pending`.
15. `campaign_retry_is_idempotent_across_partial_failure` — one repository denied, one succeeded;
    re-driving the campaign creates no duplicate `campaign_effects` row (the
    `(campaign_id, repository_id, effect_digest)` UNIQUE holds) and does not re-run the succeeded
    repository.
16. `campaign_repositories_do_not_share_worktrees_or_budgets` — distinct `worktree_path` values, and
    a budget exhausted in A does not alter B's ceiling.
17. `remote_policy_can_only_narrow_local_policy` — with a stub remote policy wider than local, the
    effective policy equals local; with one narrower, it equals remote.
18. `federation_commands_have_a_decided_role_floor` — the existing guard test at
    `crates/daemon/src/commands.rs:5516`, extended with every new variant.

---

## 8. Gotchas

1. **`stable_repository_id` is path-derived, not repository-derived.**
   `crates/knowledge/src/codegraph.rs:174` hashes the canonical *local path*. The doc comment
   correctly calls it stable — it means stable across daemon restarts, not across machines. Using it
   as the federated identity gives you a different id per clone and a collision between two unrelated
   repositories checked out at the same path on two machines. This is the single most likely
   design-level mistake in M6.

2. **`code_nodes.symbol_key` folds in `source_path`.**
   Since `migrations/0004_codegraph_source_path.sql:17`, identity is scoped to the file. Two
   semantically identical symbols in different files are different nodes, and moving a file changes
   every node id in it. A published node id derived from `symbol_key` therefore *churns on file
   moves*, producing a tombstone-plus-republish storm on a large refactor. Either accept it and make
   the batch path efficient, or derive the published id from `(package, qualified_name, kind)` and
   accept the collisions that `source_path` was added to fix — decide deliberately and write the
   reason into the migration comment, because you cannot edit it afterwards.

3. **A rebuild retires symbols the scan no longer saw — but only when the walk was complete.**
   `ScanCoverage` (`crates/knowledge/src/codegraph.rs:229`) is `Complete` or `Truncated`; a
   truncated walk (file cap, early stop) retires nothing precisely so a partial scan cannot delete a
   valid graph. A publisher that diffs against the previous projection without checking coverage
   will read a truncated scan as mass deletion and emit a full tombstone sweep. **Gate publication
   on `ScanCoverage::Complete`.**

4. **`code_nodes` has no owner column.** It carries a repository, not an `owner_uid` — which is why
   `DaemonStore::CodeGraph` exists in `crates/protocol/src/command.rs` as a daemon-wide ownership
   axis with the repository gate applied *separately inside the seam*. Do not invent an `owner_uid`
   on the federation tables that pretends to authorize graph rows; authorize the store, then the
   repository, exactly as the shipped seam does.

5. **`0026_skill_executions.sql` / `0027_hooks.sql` have no writers.** Per `migrations/README.md`,
   neither table is ever written. If your ownership-aware reviewer suggestion or campaign audit
   reaches for a skill-execution or hook-dispatch record as evidence, there is none.

6. **`is_reserved_unsupported_command` (`crates/daemon/src/commands.rs:2971`) currently reserves the
   inbox, analytics, automation and bundle payloads.** M6 does not touch that list — but if you are
   implementing M6 before M3–M5, be aware that anything you build on top of the inbox or analytics
   contracts is talking to a `protocol.unsupported-payload` error.

7. **A federation query that returns a `confidence` value leaks.** `code_edges.confidence` is 0.45
   for syntax-inferred calls and higher for asserted ones (`crates/knowledge/src/codegraph.rs`); the
   *distribution* of confidences over a repository is a weak fingerprint of its size and structure.
   At `metadata-shared`, bucket it (`inferred`/`asserted`) rather than publishing the float.

8. **Empty string is not absence.** Withheld fields must be `NULL`/absent in both SQL and the
   serialized payload. `qualified_name = ''` is a distinguishable value and turns a redaction into
   an existence oracle for "this node has a name that was withheld".

9. **Cross-repository edges need both repositories enrolled.** An edge from an enrolled repository
   into a non-enrolled one has no `to_repository_id` policy to intersect with. Treat missing policy
   as `private-local` (publish nothing), not as unrestricted — the FK on
   `federated_repository_identity` gives you the enforcement point.

10. **`cargo clippy --workspace --all-targets --all-features` is the CI lint** (`.github/workflows/ci.yml:26`).
    A new crate with `#[cfg(unix)]`-gated helpers passes locally on macOS and fails on Linux CI; keep
    platform-gated code out of `crates/federation` entirely.
