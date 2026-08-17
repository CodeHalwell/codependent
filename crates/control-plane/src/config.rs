use std::net::SocketAddr;

/// Minimum accepted length (in bytes) for the JWT signing secret.
///
/// HS256 keys shorter than the hash output (32 bytes) reduce the effective
/// security of the MAC, so anything shorter is refused outright.
pub const MIN_JWT_SECRET_LEN: usize = 32;

/// Secrets that must never be accepted, regardless of length. These have all
/// shipped in source control at some point and are therefore public knowledge.
const BANNED_JWT_SECRETS: &[&str] =
    &["codypendent-control-plane-insecure-default-secret-key-32-bytes!"];

/// Substrings that mark a value as a placeholder rather than a real secret.
const BANNED_JWT_SECRET_MARKERS: &[&str] = &[
    "insecure",
    "changeme",
    "change-me",
    "placeholder",
    "example",
    "dummy",
    "default-secret",
];

/// Configuration failures. The control plane refuses to start on any of these:
/// a service that cannot authenticate must not boot, not boot insecurely.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "JWT_SECRET is not configured; refusing to start. Set JWT_SECRET to a \
         randomly generated value of at least {MIN_JWT_SECRET_LEN} bytes."
    )]
    MissingJwtSecret,

    #[error(
        "JWT_SECRET is too short ({actual} bytes); refusing to start. A minimum \
         of {MIN_JWT_SECRET_LEN} bytes is required."
    )]
    JwtSecretTooShort { actual: usize },

    #[error(
        "JWT_SECRET is a known or placeholder value; refusing to start. Generate \
         a fresh random secret (e.g. `openssl rand -hex 32`)."
    )]
    JwtSecretNotSecret,

    #[error("LISTEN_ADDR is not a valid socket address: {0}")]
    InvalidListenAddr(String),
}

#[derive(Debug, Clone)]
pub struct ControlPlaneConfig {
    pub database_url: Option<String>,
    pub listen_addr: SocketAddr,
    pub jwt_secret: String,
    pub storage: StorageConfig,
    pub cors_allowed_origins: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum StorageConfig {
    Memory,
    S3 {
        endpoint: Option<String>,
        bucket: String,
        region: String,
        access_key: String,
        secret_key: String,
        use_path_style: bool,
    },
}

/// Validate an operator-supplied JWT signing secret.
///
/// `raw` is `None` when the variable is entirely absent. Fails closed on absent,
/// blank, short, and known/placeholder values: there is deliberately no fallback
/// secret, because a shared default secret lets anyone forge a token for any
/// tenant.
pub fn resolve_jwt_secret(raw: Option<String>) -> Result<String, ConfigError> {
    let secret = raw.ok_or(ConfigError::MissingJwtSecret)?;
    let secret = secret.trim().to_string();

    if secret.is_empty() {
        return Err(ConfigError::MissingJwtSecret);
    }

    if secret.len() < MIN_JWT_SECRET_LEN {
        return Err(ConfigError::JwtSecretTooShort {
            actual: secret.len(),
        });
    }

    let lowered = secret.to_ascii_lowercase();
    if BANNED_JWT_SECRETS
        .iter()
        .any(|banned| lowered == banned.to_ascii_lowercase())
        || BANNED_JWT_SECRET_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker))
    {
        return Err(ConfigError::JwtSecretNotSecret);
    }

    Ok(secret)
}

impl ControlPlaneConfig {
    /// Build the configuration from the process environment.
    ///
    /// Returns `Err` — and therefore prevents startup — when the JWT signing
    /// secret is absent or unacceptable, or when `LISTEN_ADDR` is malformed.
    pub fn from_env() -> Result<Self, ConfigError> {
        let jwt_secret = resolve_jwt_secret(std::env::var("JWT_SECRET").ok())?;
        Self::from_env_with_jwt_secret(jwt_secret)
    }

    /// Build the configuration from the environment with an explicitly supplied
    /// signing secret. The secret is validated exactly as it is in [`Self::from_env`].
    pub fn from_env_with_jwt_secret(jwt_secret: impl Into<String>) -> Result<Self, ConfigError> {
        let jwt_secret = resolve_jwt_secret(Some(jwt_secret.into()))?;

        let listen_addr = match std::env::var("LISTEN_ADDR") {
            Ok(addr) => addr
                .parse()
                .map_err(|_| ConfigError::InvalidListenAddr(addr))?,
            Err(_) => "0.0.0.0:8080".parse().expect("static address is valid"),
        };

        Ok(Self {
            database_url: std::env::var("DATABASE_URL").ok(),
            listen_addr,
            jwt_secret,
            storage: match std::env::var("S3_BUCKET") {
                Ok(bucket) => StorageConfig::S3 {
                    endpoint: std::env::var("S3_ENDPOINT").ok(),
                    bucket,
                    region: std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
                    access_key: std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_default(),
                    secret_key: std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_default(),
                    use_path_style: std::env::var("S3_USE_PATH_STYLE")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false),
                },
                Err(_) => StorageConfig::Memory,
            },
            cors_allowed_origins: std::env::var("CORS_ALLOWED_ORIGINS")
                .map(|origins| origins.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_else(|_| vec!["*".to_string()]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialises the tests that mutate process-wide environment variables.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn absent_jwt_secret_is_refused() {
        assert_eq!(resolve_jwt_secret(None), Err(ConfigError::MissingJwtSecret));
    }

    #[test]
    fn blank_jwt_secret_is_refused() {
        assert_eq!(
            resolve_jwt_secret(Some("      ".to_string())),
            Err(ConfigError::MissingJwtSecret)
        );
    }

    #[test]
    fn short_jwt_secret_is_refused() {
        let short = "a1b2c3d4e5f6"; // 12 bytes
        assert_eq!(
            resolve_jwt_secret(Some(short.to_string())),
            Err(ConfigError::JwtSecretTooShort { actual: 12 })
        );
    }

    #[test]
    fn boundary_length_is_enforced_exactly() {
        let just_under = "b".repeat(MIN_JWT_SECRET_LEN - 1);
        assert!(resolve_jwt_secret(Some(just_under)).is_err());

        let exact = "b".repeat(MIN_JWT_SECRET_LEN);
        assert_eq!(resolve_jwt_secret(Some(exact.clone())), Ok(exact));
    }

    #[test]
    fn previously_hardcoded_default_secret_is_refused() {
        // The literal that used to be the silent fallback. It is public, so it
        // must never authenticate anything again.
        let old_default = "codypendent-control-plane-insecure-default-secret-key-32-bytes!";
        assert_eq!(
            resolve_jwt_secret(Some(old_default.to_string())),
            Err(ConfigError::JwtSecretNotSecret)
        );
    }

    #[test]
    fn placeholder_secrets_are_refused() {
        for placeholder in [
            "changeme-changeme-changeme-changeme-changeme",
            "this-is-an-example-secret-value-not-real",
            "PLACEHOLDER-PLACEHOLDER-PLACEHOLDER-VALUE",
        ] {
            assert_eq!(
                resolve_jwt_secret(Some(placeholder.to_string())),
                Err(ConfigError::JwtSecretNotSecret),
                "{placeholder} must be refused"
            );
        }
    }

    #[test]
    fn strong_secret_is_accepted_and_trimmed() {
        let secret = "  8f3c1d9a7b5e2f40c6a8d1e3b7f9024ce5a1b3d7f9024ce5  ";
        assert_eq!(
            resolve_jwt_secret(Some(secret.to_string())),
            Ok("8f3c1d9a7b5e2f40c6a8d1e3b7f9024ce5a1b3d7f9024ce5".to_string())
        );
    }

    #[test]
    fn from_env_refuses_to_build_without_a_configured_secret() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var("JWT_SECRET").ok();
        std::env::remove_var("JWT_SECRET");

        let result = ControlPlaneConfig::from_env();

        if let Some(prev) = previous {
            std::env::set_var("JWT_SECRET", prev);
        }

        assert!(
            matches!(result, Err(ConfigError::MissingJwtSecret)),
            "startup config must fail closed when JWT_SECRET is unset, got {result:?}"
        );
    }

    #[test]
    fn from_env_accepts_an_explicit_strong_secret() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let config =
            ControlPlaneConfig::from_env_with_jwt_secret("d41d8cd98f00b204e9800998ecf8427e-cp")
                .expect("strong secret must be accepted");
        assert_eq!(config.jwt_secret, "d41d8cd98f00b204e9800998ecf8427e-cp");
    }
}
