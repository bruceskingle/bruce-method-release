//! `check`: the checks a release needs, changing nothing.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::links;
use crate::manifest::{Crate, Kind, Manifests};
use crate::process;
use crate::repo::Repo;
use crate::ui::Ui;
use crate::version::Version;

/// The result of one check.
pub struct Check {
    pub name: &'static str,
    pub outcome: Outcome,
}

pub enum Outcome {
    Pass,
    /// Failed, with why.
    Fail(String),
    /// Not done, with why, e.g. a repository with no deny.toml is not checked by cargo-deny.
    Skip(String),
}

impl Check {
    pub fn failed(&self) -> bool {
        matches!(self.outcome, Outcome::Fail(_))
    }
}

/// The ChangeLog, in which each release has a `# Release X.Y.Z` section.
pub const CHANGELOG: &str = "docs/ChangeLog.md";

/// The notes of a version in the ChangeLog, the text of its section, or None if it has none.
pub fn changelog_notes(dir: &Path, version: Version) -> Result<Option<String>> {
    let file = dir.join(CHANGELOG);
    if !file.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&file).with_context(|| format!("could not read {}", file.display()))?;
    Ok(notes_in(&text, version))
}

/// The text of a version's section in a ChangeLog's text, up to the next top level heading.
fn notes_in(text: &str, version: Version) -> Option<String> {
    let heading = format!("# Release {version}");
    let mut lines = text.lines().skip_while(|line| line.trim() != heading);
    lines.next()?;
    let notes: Vec<&str> = lines.take_while(|line| !line.starts_with("# ")).collect();
    Some(notes.join("\n").trim().to_string())
}

/// Runs every check, reporting each as it starts, after fetching, so that a repository's state is compared with
/// GitHub's as it is now. A check which cannot be done because an earlier one failed is skipped.
pub fn check(repo: &Repo, ui: &mut dyn Ui) -> Result<Vec<Check>> {
    let mut checks = Vec::new();
    let mut add = |name: &'static str, outcome: Outcome, ui: &mut dyn Ui| {
        if let Outcome::Fail(why) = &outcome {
            ui.info(&format!("  {name}: failed, {why}"));
        }
        checks.push(Check { name, outcome });
    };
    ui.info(&format!("Checking {}", repo.name));
    repo.fetch()?;
    let manifests = repo.manifests()?;
    let version = manifests.version();
    let head = repo.commit("HEAD")?;

    match &version {
        Ok(_) => add("Every crate has the same version", Outcome::Pass, ui),
        Err(error) => add(
            "Every crate has the same version",
            Outcome::Fail(format!("{error:#}")),
            ui,
        ),
    }
    let version = version.ok();

    let branch = repo.branch()?;
    add(
        "On the version's branch",
        match (version, &branch) {
            (Some(version), Some(branch)) if *branch == version.branch() => Outcome::Pass,
            (Some(version), branch) => Outcome::Fail(format!(
                "on {}, not {}",
                branch.as_deref().unwrap_or("no branch"),
                version.branch()
            )),
            (None, _) => Outcome::Skip("no version".to_string()),
        },
        ui,
    );

    let uncommitted = repo.uncommitted()?;
    add(
        "Everything committed",
        if uncommitted.is_empty() {
            Outcome::Pass
        } else {
            Outcome::Fail(format!("{} files changed", uncommitted.len()))
        },
        ui,
    );

    add(
        "Everything pushed",
        match repo.ahead_behind()? {
            Some((0, 0)) => Outcome::Pass,
            Some((ahead, 0)) => Outcome::Fail(format!("{ahead} commits not pushed")),
            Some((_, behind)) => Outcome::Fail(format!("{behind} commits on GitHub not pulled")),
            None => Outcome::Fail("the branch is not on GitHub".to_string()),
        },
        ui,
    );

    add(
        "main is merged into the branch",
        if !repo.has_ref("refs/remotes/origin/main")? {
            Outcome::Fail("there is no main branch on GitHub".to_string())
        } else if process::run(
            &repo.dir,
            "git",
            &["merge-base", "--is-ancestor", "origin/main", "HEAD"],
        )?
        .success
        {
            Outcome::Pass
        } else {
            Outcome::Fail(
                "main has changes, e.g. a fix's release, not merged into the branch: git merge origin/main".to_string(),
            )
        },
        ui,
    );

    add(
        "Not released already",
        match version {
            Some(version) => {
                let tag = version.tag();
                if repo.has_ref(&format!("refs/tags/{tag}"))? || repo.remote_tag_exists(&tag)? {
                    Outcome::Fail(format!("{tag} is already a tag"))
                } else {
                    Outcome::Pass
                }
            }
            None => Outcome::Skip("no version".to_string()),
        },
        ui,
    );

    let patches = links::read(&repo.dir)?;
    let linked = !patches.is_empty();
    add(
        "No links",
        if linked {
            Outcome::Fail(format!(
                "{} linked, release them first, then unlink",
                patches
                    .iter()
                    .map(|patch| patch.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        } else {
            Outcome::Pass
        },
        ui,
    );

    add(
        "Every dependency is on crates.io, as Cargo.lock has it",
        if linked {
            Outcome::Skip("linked".to_string())
        } else {
            let mut failed = Vec::new();
            for dir in repo.cargo_dirs(&manifests) {
                let ran = process::run(&dir, "cargo", &["metadata", "--format-version", "1", "--locked"])?;
                if !ran.success {
                    failed.push(in_dir(repo, &dir, &ran.tail(3)));
                }
            }
            if failed.is_empty() {
                Outcome::Pass
            } else {
                Outcome::Fail(failed.join("\n"))
            }
        },
        ui,
    );

    add(
        "The ChangeLog has the release's notes",
        match version {
            Some(version) => match changelog_notes(&repo.dir, version)? {
                Some(notes) if !notes.is_empty() => Outcome::Pass,
                Some(_) => Outcome::Fail(format!("the # Release {version} section of {CHANGELOG} is empty")),
                None => Outcome::Fail(format!("{CHANGELOG} has no # Release {version} section")),
            },
            None => Outcome::Skip("no version".to_string()),
        },
        ui,
    );

    let outcomes = if repo.is_workspace() {
        workspace_checks(repo, &manifests, ui)?
    } else {
        separate_crate_checks(repo, &manifests, ui)?
    };
    for (name, outcome) in outcomes {
        add(name, outcome, ui);
    }

    let outcome = if !repo.dir.join(".github/workflows").exists() {
        Outcome::Skip("no GitHub workflows".to_string())
    } else {
        ci_outcome(repo, &head)?
    };
    add("CI passed on GitHub", outcome, ui);

    Ok(checks)
}

/// Whether the repository is formatted with rustfmt, by having its settings at its root.
fn formatted(repo: &Repo) -> bool {
    repo.dir.join("rustfmt.toml").exists() || repo.dir.join(".rustfmt.toml").exists()
}

/// Runs cargo in a directory, reporting it as it starts, and passes if it succeeds.
fn cargo_in(repo: &Repo, dir: &Path, args: &[&str], ui: &mut dyn Ui) -> Result<Outcome> {
    cargo_in_each(repo, &[dir], args, ui)
}

/// Runs cargo in each of several directories, reporting each as it starts, and passes if every one succeeds, failing
/// with what each that failed wrote.
fn cargo_in_each(repo: &Repo, dirs: &[&Path], args: &[&str], ui: &mut dyn Ui) -> Result<Outcome> {
    let mut failed = Vec::new();
    for dir in dirs {
        let place = repo.relative(dir);
        if place == "." {
            ui.info(&format!("  {}", process::describe("cargo", args)));
        } else {
            ui.info(&format!("  {}, in {place}", process::describe("cargo", args)));
        }
        let ran = process::run(dir, "cargo", args)?;
        if !ran.success {
            failed.push(in_dir(repo, dir, &ran.tail(20)));
        }
    }
    Ok(if failed.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail(format!("\n{}", failed.join("\n")))
    })
}

/// What a command wrote in a directory, headed with the directory unless it is the checkout's root.
fn in_dir(repo: &Repo, dir: &Path, text: &str) -> String {
    let place = repo.relative(dir);
    if place == "." {
        text.to_string()
    } else {
        format!("{place}:\n{text}")
    }
}

/// The checks of a workspace's crates, by cargo run at its root.
fn workspace_checks(repo: &Repo, manifests: &Manifests, ui: &mut dyn Ui) -> Result<Vec<(&'static str, Outcome)>> {
    let dir = repo.dir.as_path();
    let mut outcomes = Vec::new();
    outcomes.push((
        "Formatted",
        if formatted(repo) {
            cargo_in(repo, dir, &["fmt", "--all", "--check"], ui)?
        } else {
            Outcome::Skip("no rustfmt.toml, so not formatted with rustfmt".to_string())
        },
    ));
    outcomes.push((
        "No clippy warnings",
        cargo_in(
            repo,
            dir,
            &["clippy", "--workspace", "--all-targets", "--", "--deny", "warnings"],
            ui,
        )?,
    ));
    outcomes.push(("Tests pass", cargo_in(repo, dir, &["test", "--workspace"], ui)?));
    outcomes.push((
        "Licences, advisories and sources",
        if repo.dir.join("deny.toml").exists() {
            cargo_in(repo, dir, &["deny", "check"], ui)?
        } else {
            Outcome::Skip("no deny.toml".to_string())
        },
    ));
    outcomes.push((
        "Would publish",
        if manifests.kind != Kind::Crate {
            Outcome::Skip("not published to crates.io".to_string())
        } else if !manifests.crates.iter().any(|krate| krate.publishable) {
            Outcome::Skip("no crate may be published".to_string())
        } else {
            cargo_in(repo, dir, &["publish", "--dry-run", "--workspace", "--allow-dirty"], ui)?
        },
    ));
    Ok(outcomes)
}

/// The checks of a repository of separate crates, by cargo run in each crate's directory, so that its own
/// `.cargo/config.toml` applies. A crate built for a device, e.g. a board's firmware, is checked by building it, as its
/// tests cannot run here; a library built for a device which others of the repository use by path, e.g. a platform
/// which needs a board's features to compile at all, is checked by building them, and published without building it
/// on its own.
fn separate_crate_checks(repo: &Repo, manifests: &Manifests, ui: &mut dyn Ui) -> Result<Vec<(&'static str, Outcome)>> {
    let mut on_computer = Vec::new();
    let mut on_device = Vec::new();
    for krate in &manifests.crates {
        if krate.for_device()? {
            on_device.push(krate);
        } else {
            on_computer.push(krate);
        }
    }
    let used_by_path = |krate: &Crate| {
        manifests.crates.iter().any(|other| {
            other.dependencies.iter().any(|dependency| {
                dependency.name == krate.name
                    && dependency
                        .path
                        .as_deref()
                        .is_some_and(|path| same_dir(path, krate.dir()))
            })
        })
    };
    let all: Vec<&Path> = manifests.crates.iter().map(|krate| krate.dir()).collect();
    let computer: Vec<&Path> = on_computer.iter().map(|krate| krate.dir()).collect();
    let built: Vec<&Path> = on_device
        .iter()
        .filter(|krate| !used_by_path(krate))
        .map(|krate| krate.dir())
        .collect();

    let mut outcomes = Vec::new();
    outcomes.push((
        "Formatted",
        if formatted(repo) {
            cargo_in_each(repo, &all, &["fmt", "--all", "--check"], ui)?
        } else {
            Outcome::Skip("no rustfmt.toml, so not formatted with rustfmt".to_string())
        },
    ));
    outcomes.push((
        "No clippy warnings",
        if computer.is_empty() {
            Outcome::Skip("every crate is built for a device, so is checked by building it".to_string())
        } else {
            cargo_in_each(
                repo,
                &computer,
                &["clippy", "--all-targets", "--", "--deny", "warnings"],
                ui,
            )?
        },
    ));
    outcomes.push((
        "Builds for its device",
        if built.is_empty() {
            Outcome::Skip("no crate is built for a device".to_string())
        } else {
            cargo_in_each(repo, &built, &["build", "--release"], ui)?
        },
    ));
    outcomes.push((
        "Tests pass",
        if computer.is_empty() {
            Outcome::Skip("every crate is built for a device, where its tests cannot run from here".to_string())
        } else {
            cargo_in_each(repo, &computer, &["test"], ui)?
        },
    ));
    let deny = repo.dir.join("deny.toml");
    outcomes.push((
        "Licences, advisories and sources",
        if deny.exists() {
            let deny = deny.to_string_lossy().into_owned();
            cargo_in_each(repo, &all, &["deny", "check", "--config", &deny], ui)?
        } else {
            Outcome::Skip("no deny.toml".to_string())
        },
    ));
    let publishable: Vec<&Crate> = manifests.crates.iter().filter(|krate| krate.publishable).collect();
    let outcome = if manifests.kind != Kind::Crate {
        Outcome::Skip("not published to crates.io".to_string())
    } else if publishable.is_empty() {
        Outcome::Skip("no crate may be published".to_string())
    } else {
        let mut failed = Vec::new();
        for krate in publishable {
            let mut args = vec!["publish", "--dry-run", "--allow-dirty"];
            if krate.for_device()? {
                args.push("--no-verify");
            }
            if let Outcome::Fail(why) = cargo_in(repo, krate.dir(), &args, ui)? {
                failed.push(why);
            }
        }
        if failed.is_empty() {
            Outcome::Pass
        } else {
            Outcome::Fail(failed.join(""))
        }
    };
    outcomes.push(("Would publish", outcome));
    Ok(outcomes)
}

/// Whether two paths name the same directory, allowing for either being through a symbolic link.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Whether every workflow run on GitHub for a commit finished and succeeded.
fn ci_outcome(repo: &Repo, commit: &str) -> Result<Outcome> {
    let json = process::output(
        &repo.dir,
        "gh",
        &[
            "run",
            "list",
            "--commit",
            commit,
            "--json",
            "workflowName,status,conclusion",
        ],
    )?;
    let runs: Vec<serde_json::Value> = serde_json::from_str(&json).context("could not read gh's list of runs")?;
    if runs.is_empty() {
        return Ok(Outcome::Fail(format!(
            "no run for the last commit, {}",
            &commit[..7.min(commit.len())]
        )));
    }
    let not_passed: Vec<String> = runs
        .iter()
        .filter(|run| run["conclusion"] != "success")
        .map(|run| {
            let state = if run["status"] == "completed" {
                &run["conclusion"]
            } else {
                &run["status"]
            };
            format!(
                "{} {}",
                run["workflowName"].as_str().unwrap_or("?"),
                state.as_str().unwrap_or("?")
            )
        })
        .collect();
    Ok(if not_passed.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail(not_passed.join(", "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_versions_notes() {
        let text = "# ChangeLog\n\n# Release 0.2.0\n\n## New\n- A.\n\n# Release 0.1.0\n\n- First.\n";
        assert_eq!(notes_in(text, "0.2.0".parse().unwrap()).unwrap(), "## New\n- A.");
        assert_eq!(notes_in(text, "0.1.0".parse().unwrap()).unwrap(), "- First.");
        assert_eq!(notes_in(text, "0.3.0".parse().unwrap()), None);
    }
}
