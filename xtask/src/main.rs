//! Repository-maintainer commands that must remain local and non-publishing (§13.1, §14).

use std::path::PathBuf;
use std::process::{Command, ExitCode};

const PUBLISHABLE_PACKAGES: &[(&str, &[&str])] = &[
    ("reporch-core", &[]),
    ("reporch-claude", &["reporch-core"]),
    ("reporch-git", &["reporch-core"]),
    ("reporch-store", &["reporch-core"]),
    ("reporch-scan", &["reporch-core", "reporch-store"]),
    (
        "reporch",
        &[
            "reporch-core",
            "reporch-claude",
            "reporch-git",
            "reporch-store",
            "reporch-scan",
        ],
    ),
];
const PACKAGE_VERIFY_ROOT: &str = "target/package/reporch-package-verify";

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSpec {
    program: &'static str,
    args: Vec<String>,
    env: Option<(&'static str, &'static str)>,
}

impl CommandSpec {
    fn cargo(args: &[&str]) -> Self {
        Self {
            program: "cargo",
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: None,
        }
    }

    fn cargo_owned(args: Vec<String>) -> Self {
        Self {
            program: "cargo",
            args,
            env: None,
        }
    }

    fn new(program: &'static str, args: Vec<String>) -> Self {
        Self {
            program,
            args,
            env: None,
        }
    }

    #[cfg(test)]
    fn has_arg(&self, expected: &str) -> bool {
        self.args.iter().any(|arg| arg == expected)
    }
}

fn verify_commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec::cargo(&["fmt", "--all", "--check"]),
        CommandSpec::cargo(&[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ]),
        CommandSpec::cargo(&["test", "--workspace", "--locked"]),
        CommandSpec {
            env: Some(("RUSTDOCFLAGS", "-D warnings")),
            ..CommandSpec::cargo(&["doc", "--workspace", "--no-deps", "--locked"])
        },
        CommandSpec::cargo(&["build", "--workspace", "--release", "--locked"]),
    ]
}

fn package_archive_commands() -> Vec<CommandSpec> {
    vec![CommandSpec::cargo(&[
        "package",
        "--locked",
        "--workspace",
        "--exclude",
        "xtask",
        "--no-verify",
    ])]
}

fn packaged_dir(package: &str) -> String {
    format!(
        "{PACKAGE_VERIFY_ROOT}/{package}-{}",
        env!("CARGO_PKG_VERSION")
    )
}

fn package_extraction_commands() -> Vec<CommandSpec> {
    PUBLISHABLE_PACKAGES
        .iter()
        .map(|(package, _)| {
            CommandSpec::new(
                "tar",
                vec![
                    "-xzf".to_owned(),
                    format!(
                        "target/package/{package}-{}.crate",
                        env!("CARGO_PKG_VERSION")
                    ),
                    "--strip-components=1".to_owned(),
                    "-C".to_owned(),
                    packaged_dir(package),
                ],
            )
        })
        .collect()
}

fn package_verification_commands() -> Vec<CommandSpec> {
    // Cargo's built-in package verification resolves a packaged path dependency from crates.io.
    // That cannot verify a group of unpublished, mutually dependent crates at one version. Build
    // each generated archive offline, patching only its internal dependencies to the other
    // generated archives' extracted contents. This verifies what would be published without
    // consulting older registry releases or publishing anything.
    let mut commands = Vec::new();
    for (package, dependencies) in PUBLISHABLE_PACKAGES {
        let mut args = Vec::new();
        for dependency in *dependencies {
            args.push("--config".to_owned());
            args.push(format!(
                "patch.crates-io.{dependency}.path=\"{}\"",
                packaged_dir(dependency)
            ));
        }
        args.extend([
            "check".to_owned(),
            "--manifest-path".to_owned(),
            format!("{}/Cargo.toml", packaged_dir(package)),
            "--all-targets".to_owned(),
            "--offline".to_owned(),
        ]);
        commands.push(CommandSpec::cargo_owned(args));
    }
    commands
}

fn prepare_package_verification_dirs() -> Result<(), String> {
    let root = workspace_root().join(PACKAGE_VERIFY_ROOT);
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(|error| {
            format!(
                "could not clear package verification directory {}: {error}",
                root.display()
            )
        })?;
    }
    for (package, _) in PUBLISHABLE_PACKAGES {
        let directory = workspace_root().join(packaged_dir(package));
        std::fs::create_dir_all(&directory).map_err(|error| {
            format!(
                "could not create package verification directory {}: {error}",
                directory.display()
            )
        })?;
    }
    Ok(())
}

fn verify_packages() -> Result<(), String> {
    verify_protocol_copy()
        .and_then(|()| run_all(package_archive_commands()))
        .and_then(|()| prepare_package_verification_dirs())
        .and_then(|()| run_all(package_extraction_commands()))
        .and_then(|()| run_all(package_verification_commands()))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives directly under the workspace root")
        .to_path_buf()
}

fn run_all(commands: Vec<CommandSpec>) -> Result<(), String> {
    for command in commands {
        eprintln!("+ {} {}", command.program, command.args.join(" "));
        let mut process = Command::new(command.program);
        process.args(&command.args).current_dir(workspace_root());
        if let Some((key, value)) = command.env {
            process.env(key, value);
        }
        let status = process
            .status()
            .map_err(|error| format!("could not run {}: {error}", command.program))?;
        if !status.success() {
            return Err(format!(
                "{} {} failed with {}",
                command.program,
                command.args.join(" "),
                status.code().map_or_else(
                    || "a signal".to_string(),
                    |code| format!("exit code {code}")
                )
            ));
        }
    }
    Ok(())
}

fn verify_protocol_copy() -> Result<(), String> {
    let root = workspace_root();
    let documented = std::fs::read(root.join("docs/protocol.md"))
        .map_err(|error| format!("could not read docs/protocol.md: {error}"))?;
    let packaged = std::fs::read(root.join("crates/reporch/protocol.md"))
        .map_err(|error| format!("could not read crates/reporch/protocol.md: {error}"))?;
    if documented == packaged {
        Ok(())
    } else {
        Err("docs/protocol.md and crates/reporch/protocol.md differ; update both because the latter is embedded in the published binary".into())
    }
}

fn usage() {
    eprintln!(
        "xtask — local release verification only\n\n  cargo xtask verify   fmt, clippy, tests, docs, release build\n  cargo xtask package  build/verify local crate archives (never publishes)"
    );
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    if args.next().is_some() {
        usage();
        return ExitCode::from(2);
    }
    let result = match command.as_deref() {
        Some("verify") => verify_protocol_copy().and_then(|()| run_all(verify_commands())),
        Some("package") => verify_packages(),
        Some("help") | Some("--help") | Some("-h") | None => {
            usage();
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            usage();
            Err(format!("unknown xtask command `{other}`"))
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_runs_every_release_gate_with_locked_dependencies() {
        let commands = verify_commands();
        assert!(commands.iter().any(|command| command.has_arg("fmt")));
        assert!(commands.iter().any(|command| command.has_arg("clippy")));
        assert!(commands.iter().any(|command| command.has_arg("test")));
        assert!(commands.iter().any(|command| command.has_arg("doc")));
        assert!(commands.iter().any(|command| command.has_arg("--release")));
        assert!(commands
            .iter()
            .filter(|command| !command.has_arg("fmt"))
            .all(|command| command.has_arg("--locked")));
    }

    #[test]
    fn package_never_invokes_publish_and_covers_every_publishable_crate() {
        let archive_commands = package_archive_commands();
        let archive = &archive_commands[0];
        assert!(archive.has_arg("package"));
        assert!(archive.has_arg("--locked"));
        assert!(archive.has_arg("--workspace"));
        assert!(archive.has_arg("xtask"));
        assert!(archive.has_arg("--no-verify"));

        let extraction = package_extraction_commands();
        assert_eq!(extraction.len(), 6);
        assert!(extraction.iter().all(|command| {
            command.program == "tar"
                && command.has_arg("--strip-components=1")
                && command.args.iter().any(|arg| arg.ends_with(".crate"))
        }));

        let verification = package_verification_commands();
        assert_eq!(verification.len(), 6);
        assert!(verification.iter().all(|command| {
            command.has_arg("check")
                && command.has_arg("--all-targets")
                && command.has_arg("--offline")
        }));
        assert!(archive_commands
            .iter()
            .chain(extraction.iter())
            .chain(verification.iter())
            .all(|command| !command.has_arg("publish")));
    }

    #[test]
    fn packaged_protocol_matches_the_documented_protocol() {
        verify_protocol_copy().unwrap();
    }
}
