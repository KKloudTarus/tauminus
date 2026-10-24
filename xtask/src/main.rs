// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Project automation. Run `cargo xtask help` for the task list.
//!
//! Each task runs the same commands as CI, so a green `cargo xtask ci` locally
//! means the CI checks pass on this platform.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

mod flood;

const BINDINGS: &str = "apps/desktop/src/ipc/bindings.ts";

const HELP: &str = "\
Usage: cargo xtask <task>

Tasks:
  ci        Run every check CI runs: format, lint, types, tests, bindings, layers
  rust      Rust format check, clippy, and tests
  deps      Check that inner layers depend only on what they are allowed to
  web       Frontend format check, lint, type check, and tests
  bindings  Regenerate the TypeScript IPC bindings
  flood     Benchmark output: flood text <MiB> | yes <seconds> | report <hex> | note <hex>
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("flood") => flood::run(&args[1..]),
        task => run(task),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            #[expect(clippy::print_stderr, reason = "a CLI reports failures on stderr")]
            {
                eprintln!("xtask: {error:#}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(task: Option<&str>) -> Result<()> {
    let root = workspace_root();
    match task {
        Some("ci") => {
            check_layers(&root)?;
            rust(&root)?;
            check_bindings(&root)?;
            web(&root)
        }
        Some("deps") => check_layers(&root),
        Some("rust") => rust(&root),
        Some("web") => web(&root),
        Some("bindings") => bindings(&root),
        Some("help") | None => {
            #[expect(clippy::print_stdout, reason = "help text is the output")]
            {
                print!("{HELP}");
            }
            Ok(())
        }
        Some(other) => bail!("unknown task `{other}`\n\n{HELP}"),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

fn rust(root: &Path) -> Result<()> {
    cargo(root, &["fmt", "--all", "--check"])?;
    cargo(
        root,
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    cargo(root, &["test", "--workspace", "--locked"])
}

fn web(root: &Path) -> Result<()> {
    for script in ["format:check", "lint", "typecheck", "test"] {
        pnpm(root, &[script])?;
    }
    Ok(())
}

fn bindings(root: &Path) -> Result<()> {
    cargo(
        root,
        &[
            "test",
            "--package",
            "tauminus-desktop",
            "--locked",
            "export_typescript_bindings",
        ],
    )
}

/// The test suite regenerates the bindings; a diff means the committed file is
/// stale.
fn check_bindings(root: &Path) -> Result<()> {
    run_command(
        Command::new("git")
            .args(["diff", "--exit-code", "--", BINDINGS])
            .current_dir(root),
    )
    .with_context(|| format!("{BINDINGS} is out of date; run `cargo xtask bindings` and commit it"))
}

/// Normal dependencies each inner crate may have. The compiler already stops
/// a crate from using what it does not list; this stops the list itself from
/// growing a runtime, serializer, or driver. Dev-dependencies are not checked.
const LAYER_RULES: &[(&str, &[&str])] = &[
    // regex and its parser: pure matching with no I/O, for highlight rules;
    // the domain checks and matches them with one engine (ADR 0020).
    // zmodem2: the Zmodem protocol as a state machine the caller drives,
    // with no I/O, for files a host sends with `sz` (ADR 0025). memchr:
    // the substring search that finds its offer in every output chunk.
    (
        "tauminus-domain",
        &["memchr", "regex", "regex-syntax", "thiserror", "zmodem2"],
    ),
    // zeroize: a dependency-free crate that wipes secrets when they drop.
    (
        "tauminus-app",
        &[
            "tauminus-domain",
            "thiserror",
            "tokio",
            "tracing",
            "zeroize",
        ],
    ),
];

fn check_layers(root: &Path) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .and_then(std::process::Child::wait_with_output)
        .context("failed to run cargo metadata")?;
    if !output.status.success() {
        bail!("cargo metadata failed with {}", output.status);
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("cargo metadata returned invalid JSON")?;
    let violations = layer_violations(&metadata);
    if violations.is_empty() {
        return Ok(());
    }
    bail!("layer rules broken:\n  {}", violations.join("\n  "))
}

/// O(p * d) for p packages with d dependencies each; both are small.
fn layer_violations(metadata: &serde_json::Value) -> Vec<String> {
    let mut violations = Vec::new();
    let packages = metadata["packages"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    for (crate_name, allowed) in LAYER_RULES {
        let Some(package) = packages.iter().find(|p| p["name"] == *crate_name) else {
            violations.push(format!("{crate_name} is missing from the workspace"));
            continue;
        };
        let dependencies = package["dependencies"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        for dependency in dependencies {
            if dependency["kind"] == "dev" {
                continue;
            }
            let name = dependency["name"].as_str().unwrap_or_default();
            if !allowed.contains(&name) {
                violations.push(format!("{crate_name} must not depend on {name}"));
            }
        }
    }
    violations
}

fn cargo(root: &Path, args: &[&str]) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    run_command(Command::new(cargo).args(args).current_dir(root))
}

fn pnpm(root: &Path, args: &[&str]) -> Result<()> {
    run_command(Command::new("pnpm").args(args).current_dir(root))
}

fn run_command(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to start {command:?}"))?;
    if !status.success() {
        bail!("{command:?} failed with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::layer_violations;

    fn metadata(domain_deps: &[(&str, Option<&str>)]) -> serde_json::Value {
        let deps: Vec<_> = domain_deps
            .iter()
            .map(|(name, kind)| serde_json::json!({ "name": name, "kind": kind }))
            .collect();
        serde_json::json!({ "packages": [
            { "name": "tauminus-domain", "dependencies": deps },
            { "name": "tauminus-app", "dependencies": [
                { "name": "tauminus-domain", "kind": null },
                { "name": "tokio", "kind": null },
            ] },
        ] })
    }

    #[test]
    fn allowed_dependencies_pass() {
        assert_eq!(
            layer_violations(&metadata(&[("thiserror", None)])),
            Vec::<String>::new()
        );
    }

    #[test]
    fn dev_dependencies_are_not_checked() {
        assert_eq!(
            layer_violations(&metadata(&[("proptest", Some("dev"))])),
            Vec::<String>::new()
        );
    }

    #[test]
    fn forbidden_normal_or_build_dependency_is_reported() {
        let found = layer_violations(&metadata(&[("serde", None), ("cc", Some("build"))]));
        assert_eq!(
            found,
            [
                "tauminus-domain must not depend on serde",
                "tauminus-domain must not depend on cc"
            ]
        );
    }

    #[test]
    fn missing_crate_is_reported() {
        let found = layer_violations(&serde_json::json!({ "packages": [] }));
        assert_eq!(found.len(), 2);
    }
}
