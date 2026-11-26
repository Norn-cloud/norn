//! The host's pinned configuration, read once from the environment. The same
//! variables as the Durable Object host's Worker (`apps/norn-cell-do`), so a
//! stage configures both the same way; nothing here is taken from a request.

use std::sync::{Arc, OnceLock};

use norn_cell::engine_release::{EngineVersion, STORAGE_VERSION, UPGRADES_FROM};

/// Stages a `fixtures` build may serve.
pub const FIXTURE_STAGES: &[&str] = &["smoke", "test"];
/// Environments a tenant cell may exist in when `NORN_TENANT_ENVIRONMENTS`
/// is unset.
const DEFAULT_ENVIRONMENTS: &str = "dev,prod";
/// SYNC-YIELD-2: the default sync step budget.
const STEP_BUDGET_MS: u64 = 1500;

/// The `RivetKit` release this host is built against: `rivetkit` and
/// `rivetkit-core` in this workspace's `Cargo.lock`, which a unit test
/// checks. `RivetKit` may commit its own `SQLite` schema migrations before
/// `on_start` admits the cell, so a `RivetKit` change ships only with a Norn
/// storage version bump (`docs/platform/engine-upgrades.md`, "`RivetKit`
/// upgrades").
pub const RIVETKIT_VERSION: &str = "3.1.1";

/// One `RivetKit` release of the release channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RivetkitRelease {
    /// The `RivetKit` version.
    pub rivetkit: &'static str,
    /// The first Norn storage version written under it. Every cell stamped
    /// at or above it, and below the next release's, was written by this
    /// `RivetKit`.
    pub from_storage: u32,
}

/// The allowed-upgrade list: every `RivetKit` release whose cells this host
/// opens, oldest first. The last is [`RIVETKIT_VERSION`], and each starts at
/// a storage version above the one before it, so a cell written under a
/// newer `RivetKit` carries a newer storage version and is refused
/// (`core.engine.downgrade`). Dropping the oldest entries retires their
/// releases: [`ADMISSION_FLOOR`] rises past their storage versions, so the
/// host refuses their cells (`core.engine.upgrade_unsupported`).
/// `scripts/ci/rivetkit_channel.py` (`core.sh rivet`) keeps the list
/// append-only against the pull request's base and requires the entry of a
/// new release at a storage version above the base's.
pub const RIVETKIT_CHANNEL: &[RivetkitRelease] = &[RivetkitRelease {
    rivetkit: "3.1.1",
    from_storage: 4,
}];

/// The oldest storage version a cell on this host may carry: the lowest
/// `from_storage` of the retained [`RIVETKIT_CHANNEL`]. A cell stamped below
/// it was written under a retired `RivetKit` release (or none), whose
/// `SQLite` schema this `RivetKit` may already have migrated at startup.
pub const ADMISSION_FLOOR: u32 = admission_floor(RIVETKIT_CHANNEL);

/// The window the Rivet host admits cells in: this engine's storage
/// version, and the higher of its own upgrade floor and [`ADMISSION_FLOOR`].
pub const RIVET_ENGINE: EngineVersion = EngineVersion {
    storage: STORAGE_VERSION,
    upgrades_from: if ADMISSION_FLOOR > UPGRADES_FROM {
        ADMISSION_FLOOR
    } else {
        UPGRADES_FROM
    },
};

/// The lowest `from_storage` of `channel`; an empty channel admits nothing.
pub(crate) const fn admission_floor(channel: &[RivetkitRelease]) -> u32 {
    let mut floor = u32::MAX;
    let mut rest = channel;
    while let Some((release, tail)) = rest.split_first() {
        if release.from_storage < floor {
            floor = release.from_storage;
        }
        rest = tail;
    }
    floor
}

/// Everything the host reads from its environment.
#[derive(Clone, Default)]
pub struct HostConfig {
    /// `NORN_HOST_ISSUER`: the token issuer; required without `fixtures`.
    pub issuer: Option<String>,
    /// `NORN_HOST_JWKS_URL`: the issuer's pinned key document.
    pub jwks_url: Option<String>,
    /// `NORN_LINEAGE_KEY`: the stage secret for lineage handles.
    pub lineage_key: Option<String>,
    /// `NORN_CHECKPOINT_KEY`: the stage secret (64 hex chars) authenticating open checkpoints.
    pub checkpoint_key: Option<String>,
    /// `NORN_SOURCES`: the cell's source connections (JSON array).
    pub sources: String,
    /// The variables the connections read (`NORN_SECRET_*`, `NORN_S3_*`,
    /// `NORN_SOURCE_ALLOWED_HOSTS`), as one JSON object.
    pub connect_vars: String,
    /// `NORN_OLAP_URL`: the tenant analytics engine (D102).
    pub olap_url: Option<String>,
    /// `NORN_OLAP_SIGNING_KEY`: the key the cell mints its per-call engine
    /// credentials with (OLAP-CRED-1). Not a connection variable, so no
    /// connector ever sees it.
    pub olap_signing_key: Option<String>,
    /// `NORN_OLAP_ENGINE`: the analytics engine the deployment runs
    /// (`duckdb`, the default, or `datafusion`); only its answers count.
    pub olap_engine: Option<String>,
    /// `NORN_STAGE`.
    pub stage: Option<String>,
    /// `NORN_TENANT_ENVIRONMENTS`.
    pub environments: Vec<String>,
    /// `ALLOWED_ORIGINS`.
    pub allowed_origins: Vec<String>,
    /// `NORN_SYNC_STEP_MS`, clamped to 250 ms .. 30 s.
    pub sync_step_ms: u64,
    /// The conformance seams are served: a `conformance` build with
    /// `NORN_STAGE=test` and `NORN_CONFORMANCE=synthetic`.
    pub conformance: bool,
}

/// What the cell needs to call the tenant analytics engine.
#[derive(Clone)]
pub struct Olap {
    /// `NORN_OLAP_SIGNING_KEY`.
    pub signing_key: Option<String>,
    /// `NORN_OLAP_ENGINE`.
    pub engine: Option<String>,
}

impl HostConfig {
    /// The analytics engine settings the cell is opened with.
    #[must_use]
    pub fn olap(&self) -> Olap {
        Olap {
            signing_key: self.olap_signing_key.clone(),
            engine: self.olap_engine.clone(),
        }
    }
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("issuer", &self.issuer)
            .field("jwks_url", &self.jwks_url)
            .field("olap_url", &self.olap_url)
            .field("stage", &self.stage)
            .field("environments", &self.environments)
            .field("conformance", &self.conformance)
            .finish_non_exhaustive()
    }
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

impl HostConfig {
    /// Read the configuration from the environment.
    #[must_use]
    pub fn from_env() -> Self {
        let stage = var("NORN_STAGE");
        let connect_vars = std::env::vars()
            .filter(|(name, _)| {
                name.starts_with("NORN_SECRET_")
                    || name.starts_with("NORN_S3_")
                    || name == "NORN_SOURCE_ALLOWED_HOSTS"
            })
            .map(|(name, value)| (name, serde_json::Value::String(value)))
            .collect::<serde_json::Map<String, serde_json::Value>>();
        let connect_vars = serde_json::Value::Object(connect_vars);
        let environments = var("NORN_TENANT_ENVIRONMENTS")
            .unwrap_or_else(|| DEFAULT_ENVIRONMENTS.to_owned())
            .split(',')
            .map(str::trim)
            .filter(|env| crate::router::environment_part(env))
            .map(str::to_owned)
            .collect();
        let sync_step_ms = var("NORN_SYNC_STEP_MS")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .map_or(STEP_BUDGET_MS, |ms| ms.clamp(250, 30_000));
        let conformance = cfg!(feature = "conformance")
            && stage.as_deref() == Some("test")
            && var("NORN_CONFORMANCE").as_deref() == Some("synthetic");
        Self {
            issuer: var("NORN_HOST_ISSUER"),
            jwks_url: var("NORN_HOST_JWKS_URL"),
            lineage_key: var("NORN_LINEAGE_KEY"),
            checkpoint_key: var("NORN_CHECKPOINT_KEY"),
            sources: var("NORN_SOURCES").unwrap_or_else(|| "[]".to_owned()),
            connect_vars: connect_vars.to_string(),
            olap_url: var("NORN_OLAP_URL"),
            olap_signing_key: var("NORN_OLAP_SIGNING_KEY"),
            olap_engine: var("NORN_OLAP_ENGINE"),
            stage,
            environments,
            allowed_origins: var("ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(str::to_owned)
                .collect(),
            sync_step_ms,
            conformance,
        }
    }
}

static CONFIG: OnceLock<Arc<HostConfig>> = OnceLock::new();

/// Install the process's configuration (once, before serving).
pub fn install(config: HostConfig) -> Arc<HostConfig> {
    Arc::clone(CONFIG.get_or_init(|| Arc::new(config)))
}

/// The installed configuration, or the environment's.
#[must_use]
pub fn get() -> Arc<HostConfig> {
    Arc::clone(CONFIG.get_or_init(|| Arc::new(HostConfig::from_env())))
}

/// The value of `NORN_RIVET_PLAINTEXT_ENGINE` that allows a plaintext engine
/// endpoint on a single machine's private bridge network (the compose stacks).
pub const PLAINTEXT_SINGLE_HOST: &str = "single-host";

/// Refuse an engine endpoint that would carry the admin token in cleartext
/// across a network. `RIVET_ENDPOINT` holds the token (it travels in every
/// request and in the node socket's handshake), so it must be `https` except
/// for a loopback address or when the operator states with
/// `NORN_RIVET_PLAINTEXT_ENGINE=single-host` that the engine is on the same
/// machine's bridge network. The error never repeats the endpoint, which
/// holds the token.
///
/// # Errors
/// A description of why the endpoint is refused.
pub fn check_engine_transport(endpoint: &str, plaintext: Option<&str>) -> Result<(), String> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|_unparsed| "RIVET_ENDPOINT is not a URL".to_owned())?;
    match url.scheme() {
        "https" => Ok(()),
        "http" => {
            let host = url.host_str().unwrap_or_default();
            let bare = host.trim_start_matches('[').trim_end_matches(']');
            let loopback = host == "localhost"
                || bare
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback());
            if loopback || plaintext == Some(PLAINTEXT_SINGLE_HOST) {
                Ok(())
            } else {
                Err(
                    "RIVET_ENDPOINT is plaintext http to a non-loopback engine, which would send \
                     the admin token in cleartext: use https, or set \
                     NORN_RIVET_PLAINTEXT_ENGINE=single-host \
                     when the engine is on this machine's bridge network"
                        .to_owned(),
                )
            }
        }
        _other => Err("RIVET_ENDPOINT must be https (or http to loopback)".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PLAINTEXT_SINGLE_HOST, RIVETKIT_CHANNEL, RIVETKIT_VERSION, check_engine_transport,
    };
    use norn_cell::engine_release::STORAGE_VERSION;

    #[test]
    fn engine_transport_refuses_cleartext_tokens_off_the_machine() {
        let ok =
            |endpoint: &str, flag: Option<&str>| check_engine_transport(endpoint, flag).is_ok();
        assert!(ok("https://default:t@10.0.0.2:6420", None));
        assert!(!ok("wss://default:t@lb.rivet.internal:6420", None));
        assert!(ok("http://default:t@127.0.0.1:6420", None));
        assert!(ok("http://default:t@[::1]:6420", None));
        assert!(ok("http://default:t@localhost:6420", None));
        assert!(ok("http://default:t@lb:6420", Some(PLAINTEXT_SINGLE_HOST)));
        assert!(!ok("http://default:t@10.0.0.2:6420", None));
        assert!(!ok("http://default:t@lb:6420", None));
        assert!(!ok("http://default:t@10.0.0.2:6420", Some("1")));
        assert!(!ok("ftp://default:t@10.0.0.2", None));
        assert!(!ok("not a url", None));
        let refused = check_engine_transport("http://default:secret-token@10.0.0.2:6420", None);
        assert!(refused.is_err_and(|message| !message.contains("secret-token")));
    }

    /// The version `Cargo.lock` locks `name` at.
    fn locked(name: &str) -> String {
        let lock = include_str!("../Cargo.lock");
        let entry = format!("name = \"{name}\"\n");
        let (_, rest) = lock
            .split_once(&entry)
            .unwrap_or_else(|| panic!("{name} is not in Cargo.lock"));
        rest.strip_prefix("version = \"")
            .and_then(|v| v.split('"').next())
            .unwrap_or_else(|| panic!("{name} has no version in Cargo.lock"))
            .to_owned()
    }

    /// A `RivetKit` change in `Cargo.lock` fails here until
    /// `RIVETKIT_VERSION` and `RIVETKIT_CHANNEL` name it, and the channel
    /// admits a new entry only at a storage version above the last one's:
    /// the `RivetKit` bump forces a storage version bump.
    #[test]
    fn rivetkit_pin_matches_the_lock_and_the_storage_version() {
        for name in ["rivetkit", "rivetkit-core"] {
            assert_eq!(
                locked(name),
                RIVETKIT_VERSION,
                "{name} changed in Cargo.lock: bump STORAGE_VERSION, append a RIVETKIT_CHANNEL entry at it, and update RIVETKIT_VERSION (docs/platform/engine-upgrades.md)"
            );
        }
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains(&format!("rivetkit = {{ version = \"={RIVETKIT_VERSION}\"")),
            "Cargo.toml must pin rivetkit at ={RIVETKIT_VERSION}"
        );
        let last = RIVETKIT_CHANNEL
            .last()
            .expect("the channel lists this release");
        assert_eq!(
            last.rivetkit, RIVETKIT_VERSION,
            "the last channel entry is this release"
        );
        assert!(
            last.from_storage <= STORAGE_VERSION,
            "the channel names storage version {} that this engine does not write ({STORAGE_VERSION})",
            last.from_storage
        );
        for pair in RIVETKIT_CHANNEL.windows(2) {
            assert!(
                pair[0].from_storage < pair[1].from_storage,
                "RivetKit {} starts at storage version {}, not above {}'s {}: a RivetKit change needs a storage version bump",
                pair[1].rivetkit,
                pair[1].from_storage,
                pair[0].rivetkit,
                pair[0].from_storage
            );
            assert_ne!(
                pair[0].rivetkit, pair[1].rivetkit,
                "a RivetKit release is listed once"
            );
        }
    }
}
