CREATE TABLE users (
    id uuid PRIMARY KEY,
    -- Display only. NOT an identity key: email collisions must never link two
    -- humans (design §5.1 requires proof of both identities to link).
    display_name text NOT NULL,
    primary_email text,
    state text NOT NULL CHECK (state IN ('active', 'suspended', 'deleted')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- One row per external identity. A human may have several (design §5.1).
CREATE TABLE user_identities (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id),
    -- 'github' | 'oidc'
    provider text NOT NULL,
    -- The OIDC issuer, or 'https://github.com'. Part of the identity key
    -- because subject values are only unique WITHIN an issuer — two OIDC
    -- tenants can both mint sub='1'.
    issuer text NOT NULL,
    subject text NOT NULL,
    -- Verified email at link time, retained for audit. Never used to match.
    email_at_link text,
    linked_at timestamptz NOT NULL DEFAULT now(),
    -- The audit record id proving both identities were authenticated when the
    -- link was made. NOT NULL: an unlinked-provenance link is not permitted.
    link_audit_id uuid NOT NULL,
    UNIQUE (provider, issuer, subject)
);

-- Refresh credentials for browser sessions. Only the hash is stored, so a
-- database compromise does not yield usable tokens.
CREATE TABLE user_refresh_tokens (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id),
    token_hash bytea NOT NULL UNIQUE,
    -- Rotation chain: a replayed (already-rotated) refresh token means theft,
    -- and must revoke the whole chain rather than just failing.
    rotated_from uuid REFERENCES user_refresh_tokens(id),
    issued_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    user_agent_digest bytea
);
CREATE INDEX ON user_refresh_tokens (user_id) WHERE revoked_at IS NULL;

-- OAuth/OIDC in-flight state. Rows are single-use and short-lived; the PKCE
-- verifier hash and nonce are what make an authorization-code interception
-- useless.
CREATE TABLE auth_flows (
    state text PRIMARY KEY,
    provider text NOT NULL,
    issuer text NOT NULL,
    pkce_verifier_hash bytea NOT NULL,
    nonce text NOT NULL,
    redirect_uri text NOT NULL,
    -- Set when the flow is a LINK rather than a login; the already-authenticated
    -- user whose account the new identity attaches to.
    linking_user_id uuid REFERENCES users(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz
);
