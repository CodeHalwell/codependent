CREATE TABLE organizations (
    id uuid PRIMARY KEY,
    slug text NOT NULL,
    display_name text NOT NULL,
    -- Organization-wide publication ceiling (design §12.3). Narrowing input to
    -- every repository under it.
    max_publication_class text NOT NULL DEFAULT 'metadata-shared',
    max_classification text NOT NULL DEFAULT 'internal',
    data_residency text,
    retention_days integer CHECK (retention_days IS NULL OR retention_days > 0),
    policy_version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now()
);
-- Case-insensitive uniqueness without the citext extension (self-hosters may
-- not be able to CREATE EXTENSION).
CREATE UNIQUE INDEX ON organizations (lower(slug));

CREATE TABLE teams (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    slug text NOT NULL,
    display_name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX ON teams (organization_id, lower(slug));

CREATE TABLE memberships (
    organization_id uuid NOT NULL REFERENCES organizations(id),
    user_id uuid NOT NULL REFERENCES users(id),
    state text NOT NULL CHECK (state IN ('invited', 'active', 'suspended')),
    joined_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, user_id)
);

CREATE TABLE team_members (
    team_id uuid NOT NULL REFERENCES teams(id),
    user_id uuid NOT NULL REFERENCES users(id),
    PRIMARY KEY (team_id, user_id)
);

CREATE TABLE repositories (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    -- M6's cross-machine identity (SHA-256 hex). The control plane never sees a
    -- local path-derived RepositoryId.
    federated_id text NOT NULL CHECK (char_length(federated_id) = 64),
    display_name text NOT NULL,
    -- Repository ceiling. Intersected with the organization's, never widening
    -- it — enforce in code AND with the CHECK-able invariant test in §5.
    max_publication_class text NOT NULL DEFAULT 'metadata-shared',
    max_classification text NOT NULL DEFAULT 'internal',
    policy_version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- Scoped to the organization on purpose: a global UNIQUE on federated_id
    -- turns registration into a cross-tenant existence oracle (a 409 proves
    -- another organization registered that repository). See §5.4.
    UNIQUE (organization_id, federated_id)
);

CREATE TABLE role_grants (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    -- Exactly one of user_id / team_id is set: a grant is to a principal or to
    -- a group, never both, never neither.
    user_id uuid REFERENCES users(id),
    team_id uuid REFERENCES teams(id),
    -- NULL = organization-wide scope.
    repository_id uuid REFERENCES repositories(id),
    role text NOT NULL CHECK (role IN (
        'observer', 'contributor', 'approver', 'maintainer', 'organization-admin'
    )),
    -- For 'approver': the action scope the grant is limited to (design §5.3
    -- requires an EXPLICIT repository/action scope). NULL for other roles.
    action_scope jsonb,
    granted_by uuid NOT NULL REFERENCES users(id),
    granted_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    revoked_at timestamptz,
    CHECK ((user_id IS NULL) <> (team_id IS NULL)),
    CHECK (role <> 'approver' OR action_scope IS NOT NULL)
);
CREATE INDEX ON role_grants (organization_id, user_id, repository_id) WHERE revoked_at IS NULL;
CREATE INDEX ON role_grants (organization_id, team_id, repository_id) WHERE revoked_at IS NULL;
