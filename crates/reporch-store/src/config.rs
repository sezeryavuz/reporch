//! `~/.reporch/config.toml` (§8).
//!
//! Every field has a default, so a missing or partial config is normal rather than an error. The
//! file is written back on first run with the defaults spelled out and commented, because a
//! config the user cannot discover is a config they cannot change.

use serde::{Deserialize, Serialize};

use crate::{Paths, StoreError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub scan: ScanConfig,
    pub session: SessionConfig,
    pub limits: LimitsConfig,
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 1,
            scan: ScanConfig::default(),
            session: SessionConfig::default(),
            limits: LimitsConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScanConfig {
    /// Roots to scan. Empty means "the user's home directory" (§5.3).
    pub roots: Vec<String>,
    /// Bounded depth. Spike #9 found nothing below 6 on a real machine; 8 leaves headroom.
    pub max_depth: usize,
    /// Directory names never descended into (§5.3).
    pub skip: Vec<String>,
    /// Whether to use `cache/repos.json`. An optimisation, not load-bearing — the measured scan
    /// is 0.27s (§5.3 Deviation).
    pub use_cache: bool,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            max_depth: 8,
            skip: [
                "node_modules",
                "target",
                ".venv",
                "venv",
                "vendor",
                "dist",
                "build",
                "Library",
                "Applications",
                ".Trash",
                "Pictures",
                "Movies",
                "Music",
                ".cache",
                ".npm",
                ".cargo",
                ".rustup",
                ".pyenv",
                ".nvm",
                ".gradle",
                ".m2",
                "__pycache__",
                ".next",
                ".pnpm-store",
                "Containers",
                "go",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            use_cache: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionConfig {
    /// Default model for new tasks. Applied to every repo alike (§5.6).
    pub default_model: String,
    /// Per-session spend ceiling (§10). Note it overshoots and then kills the process — spike #5
    /// measured $0.186 against a $0.05 ceiling — so it is a bound, not an exact cap.
    pub max_budget_usd_per_session: f64,
    /// How a ping is delivered. `stdin` is the default and the only one implemented in v1;
    /// `peer` is reserved for v1.1 interop with user-started sessions (§6.4 Deviation).
    pub transport: Transport,
    /// Whether spawned sessions may load the operator's MCP servers.
    ///
    /// Default `false`, i.e. `--strict-mcp-config`. Fourteen MCP servers inside nine unattended
    /// `bypassPermissions` sessions is a materially larger attack surface than the task needs,
    /// and measurement showed the cost saving is not the reason (D006).
    pub allow_mcp_servers: bool,
    /// Whether spawned sessions may load the operator's skills and slash commands. Default
    /// `true`: they are genuinely useful inside the user's repos (D006).
    pub allow_skills: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            default_model: "opus".into(),
            max_budget_usd_per_session: 10.0,
            transport: Transport::Stdin,
            allow_mcp_servers: false,
            allow_skills: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdin,
    Peer,
}

impl Transport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Transport::Stdin => "stdin",
            Transport::Peer => "peer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    /// §6.3: at most one ping per session per this many seconds.
    pub ping_min_interval_secs: u64,
    /// §6.3: at most this many pings to one session, per task.
    pub ping_max_per_session: u32,
    /// §6.3: the global per-task ceiling. Breaching it aborts the task.
    pub ping_max_per_task: u32,
    /// §6.3 row 4: how long a worker may hold an unacked brief before it is surfaced.
    pub stale_inbound_secs: u64,
    /// §12.3: a task-level wall-clock timeout, after which the task moves to `delayed` and is
    /// surfaced rather than burning budget overnight.
    pub task_timeout_secs: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            ping_min_interval_secs: 30,
            ping_max_per_session: 20,
            ping_max_per_task: 100,
            stale_inbound_secs: 180,
            task_timeout_secs: 4 * 60 * 60,
        }
    }
}

impl LimitsConfig {
    pub fn to_ping_limits(&self) -> reporch_core::protocol::PingLimits {
        reporch_core::protocol::PingLimits {
            min_interval_ms: self.ping_min_interval_secs.saturating_mul(1000),
            max_per_session: self.ping_max_per_session,
            max_per_task: self.ping_max_per_task,
            stale_inbound_ms: self.stale_inbound_secs.saturating_mul(1000),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// Redraw tick, milliseconds. Also the input poll timeout — without one the loop spins at
    /// 100% CPU (docs/ratatui-notes.md).
    pub tick_ms: u64,
    /// Whether to check GitHub for a newer release on the top-level menu.
    pub check_updates: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            tick_ms: 250,
            check_updates: true,
        }
    }
}

/// The commented default written on first run.
const DEFAULT_CONFIG_TOML: &str = r#"# reporch configuration
#
# Everything here has a default; delete a line to go back to it. The full specification of
# what each setting means is in enhanced_brief.md, cited by section.

schema_version = 1

[scan]
# Where to look for git repositories (§5.3). Empty means your home directory.
roots = []
max_depth = 8
# A repo is a leaf: the scan stops descending as soon as it finds a `.git`.
use_cache = true

[session]
# Applied to every repo's session alike — master and workers (§5.6).
default_model = "opus"

# Per-session spend ceiling (§10). Claude Code enforces this between iterations, so it
# overshoots by up to one iteration, and the session EXITS when it binds rather than pausing.
max_budget_usd_per_session = 10.0

# How reporch delivers a ping. "stdin" writes to the session's stdin, which is documented,
# synchronous and cannot be held pending an approval dialog (§6.4). "peer" is reserved for
# v1.1 interop with sessions you started yourself.
transport = "stdin"

# Whether spawned sessions load your MCP servers. Off by default: nine unattended sessions in
# bypassPermissions mode is not where you want your whole MCP surface (docs/decisions.md D006).
allow_mcp_servers = false

# Whether spawned sessions load your skills and slash commands. On by default — they are
# genuinely useful inside your own repositories.
allow_skills = true

[limits]
# Control-plane ping guardrails (§6.3). All of these are mandatory, not advisory.
ping_min_interval_secs = 30
ping_max_per_session = 20
# Breaching the task ceiling aborts the task and surfaces the loop, rather than pinging forever.
ping_max_per_task = 100
stale_inbound_secs = 180
# §12.3: after this, a task moves to `delayed` and is surfaced rather than running overnight.
task_timeout_secs = 14400

[ui]
tick_ms = 250
check_updates = true
"#;

impl Config {
    /// Load `config.toml`, falling back to defaults when it does not exist.
    pub fn load(paths: &Paths) -> Result<Self, StoreError> {
        let path = paths.config_file();
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).map_err(|e| StoreError::Config {
                path: path.clone(),
                detail: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(StoreError::io(&path, e)),
        }
    }

    /// Create `$REPORCH_HOME` and its subdirectories, writing a commented `config.toml` if there
    /// is none (§8). Idempotent.
    pub fn init_home(paths: &Paths) -> Result<Self, StoreError> {
        for dir in [
            paths.root().to_path_buf(),
            paths.cache_dir(),
            paths.logs_dir(),
            paths.projects_dir(),
            paths.worktrees_dir(),
        ] {
            std::fs::create_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
        }
        let cfg = paths.config_file();
        if !crate::atomic::is_file(&cfg) {
            crate::atomic::write_string(&cfg, DEFAULT_CONFIG_TOML)?;
        }
        Self::load(paths)
    }

    /// The scan roots, resolved: configured roots, or the home directory if none are set.
    pub fn scan_roots(&self) -> Vec<std::path::PathBuf> {
        if self.scan.roots.is_empty() {
            crate::paths::home_dir().into_iter().collect()
        } else {
            self.scan
                .roots
                .iter()
                .map(std::path::PathBuf::from)
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.session.transport, Transport::Stdin);
        assert!(!c.session.allow_mcp_servers, "D006: MCP off by default");
        assert!(c.session.allow_skills);
        assert_eq!(c.limits.ping_min_interval_secs, 30);
        assert_eq!(c.limits.ping_max_per_session, 20);
    }

    /// The shipped default file must actually parse into the struct defaults. If it drifts, users
    /// get a config that silently means something different from the code.
    #[test]
    fn the_written_default_matches_the_struct_default() {
        let parsed: Config = toml::from_str(DEFAULT_CONFIG_TOML).expect("default toml must parse");
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn missing_config_loads_defaults() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::at(d.path());
        assert_eq!(Config::load(&p).unwrap(), Config::default());
    }

    #[test]
    fn partial_config_fills_in_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::at(d.path());
        std::fs::create_dir_all(d.path()).unwrap();
        std::fs::write(
            p.config_file(),
            "[session]\nmax_budget_usd_per_session = 2.5\n",
        )
        .unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.session.max_budget_usd_per_session, 2.5);
        assert_eq!(c.session.default_model, "opus", "the rest defaults");
        assert_eq!(c.limits.ping_max_per_task, 100);
    }

    /// A typo in a setting name must be an error, not a silently ignored line. Someone who writes
    /// `ping_max_per_sesion = 5` and gets 20 has been actively misled.
    #[test]
    fn an_unknown_key_is_an_error_naming_the_file() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::at(d.path());
        std::fs::create_dir_all(d.path()).unwrap();
        std::fs::write(p.config_file(), "[limits]\nping_max_per_sesion = 5\n").unwrap();
        let err = Config::load(&p).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("config.toml"), "got: {s}");
    }

    #[test]
    fn init_home_creates_the_section_8_layout_and_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::at(d.path().join("home"));
        Config::init_home(&p).unwrap();
        for dir in [
            p.root().to_path_buf(),
            p.cache_dir(),
            p.logs_dir(),
            p.projects_dir(),
            p.worktrees_dir(),
        ] {
            assert!(dir.is_dir(), "missing {dir:?}");
        }
        assert!(p.config_file().is_file());

        // A second run must not clobber an edited config.
        std::fs::write(p.config_file(), "[ui]\ntick_ms = 999\n").unwrap();
        let c = Config::init_home(&p).unwrap();
        assert_eq!(c.ui.tick_ms, 999, "init_home must not overwrite");
    }

    #[test]
    fn limits_convert_to_the_protocol_type() {
        let l = LimitsConfig::default().to_ping_limits();
        assert_eq!(l.min_interval_ms, 30_000);
        assert_eq!(l.max_per_session, 20);
        assert_eq!(l.stale_inbound_ms, 180_000);
    }

    #[test]
    fn scan_roots_default_to_home() {
        let c = Config::default();
        let roots = c.scan_roots();
        assert_eq!(roots.len(), 1, "one root: the home directory");
    }
}
