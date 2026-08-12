//! Offline-safe GitHub release checks (§5.1).

use std::path::Path;
use std::time::Duration;

use semver::Version;
use serde::Deserialize;

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/sezeryavuz/reporch/releases/latest";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Cargo,
    Homebrew,
    Npm,
    Shell,
}

impl InstallMethod {
    pub const fn upgrade_command(self) -> &'static str {
        match self {
            Self::Cargo => "cargo install reporch --locked",
            Self::Homebrew => "brew upgrade reporch",
            Self::Npm => "npm install --global reporch@latest",
            Self::Shell => {
                "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/sezeryavuz/reporch/releases/latest/download/reporch-installer.sh | sh"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    Available {
        current: Version,
        latest: Version,
        url: String,
    },
    Current {
        current: Version,
    },
    Unavailable {
        reason: String,
    },
}

impl UpdateStatus {
    pub fn message_for(&self, method: InstallMethod) -> String {
        match self {
            Self::Available {
                current,
                latest,
                url,
            } => format!(
                "reporch {latest} is available; this binary is {current}.\n\nUpgrade with:\n\n  {}\n\nRelease: {url}",
                method.upgrade_command()
            ),
            Self::Current { current } => {
                format!("reporch {current} is the latest published release.")
            }
            Self::Unavailable { reason } => format!(
                "Could not check for updates. This is non-fatal and the console remains fully usable.\n\n{reason}"
            ),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    html_url: String,
}

pub fn parse_release(body: &str, current: &str) -> Result<UpdateStatus, String> {
    let response: ReleaseResponse =
        serde_json::from_str(body).map_err(|error| format!("invalid GitHub response: {error}"))?;
    let current = Version::parse(current)
        .map_err(|error| format!("invalid current version `{current}`: {error}"))?;
    let tag = response
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&response.tag_name);
    let latest =
        Version::parse(tag).map_err(|error| format!("invalid release tag `{tag}`: {error}"))?;
    if latest > current {
        Ok(UpdateStatus::Available {
            current,
            latest,
            url: response.html_url,
        })
    } else {
        Ok(UpdateStatus::Current { current })
    }
}

/// Query GitHub with a bounded wait. Every external failure is data for the notice screen, never
/// an application error: update availability has no bearing on task safety.
pub fn check_latest(current: &str) -> UpdateStatus {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .build();
    let agent: ureq::Agent = config.into();
    let response = agent
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(
            "User-Agent",
            &format!("reporch/{current} (+{})", crate::REPO_URL),
        )
        .call();
    let body = match response {
        Ok(mut response) => match response.body_mut().read_to_string() {
            Ok(body) => body,
            Err(error) => {
                return UpdateStatus::Unavailable {
                    reason: format!("GitHub's response could not be read: {error}"),
                }
            }
        },
        Err(error) => {
            return UpdateStatus::Unavailable {
                reason: format!("GitHub releases API: {error}"),
            }
        }
    };
    parse_release(&body, current).unwrap_or_else(|reason| UpdateStatus::Unavailable { reason })
}

/// Infer how this executable was installed from its resolved path. An unrecognised layout uses
/// the cargo-dist shell installer command, which is valid for any supported platform.
pub fn detect_install_method(executable: &Path) -> InstallMethod {
    let resolved = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    let text = resolved.to_string_lossy();
    if text.contains("/Cellar/reporch/")
        || text.contains("/homebrew/")
        || text.contains("/linuxbrew/")
    {
        return InstallMethod::Homebrew;
    }
    if text.contains("/node_modules/") || text.contains("/npm/") {
        return InstallMethod::Npm;
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| reporch_store::home_dir().map(|home| home.join(".cargo")));
    if cargo_home.is_some_and(|home| resolved.starts_with(home.join("bin"))) {
        InstallMethod::Cargo
    } else {
        InstallMethod::Shell
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = include_str!("../fixtures/github-release-latest.json");

    #[test]
    fn newer_release_reports_the_detected_upgrade_command() {
        let release = parse_release(RELEASE, "0.1.0").unwrap();
        let message = release.message_for(InstallMethod::Cargo);
        assert!(message.contains("15.2.0"), "{message}");
        assert!(message.contains("cargo install reporch"), "{message}");
    }

    #[test]
    fn current_and_malformed_responses_are_non_fatal_statuses() {
        let current = parse_release(RELEASE, "15.2.0").unwrap();
        assert!(matches!(current, UpdateStatus::Current { .. }));
        assert!(parse_release("not json", "0.1.0").is_err());
    }

    #[test]
    fn install_method_detection_uses_real_executable_layouts() {
        assert_eq!(
            detect_install_method(Path::new("/opt/homebrew/Cellar/reporch/0.1.0/bin/reporch")),
            InstallMethod::Homebrew
        );
        assert_eq!(
            detect_install_method(Path::new("/usr/local/lib/node_modules/reporch/bin/reporch")),
            InstallMethod::Npm
        );
    }
}
