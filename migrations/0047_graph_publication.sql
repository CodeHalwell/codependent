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
