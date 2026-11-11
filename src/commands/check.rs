//! `check`: the checks a release needs, changing nothing.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::links;
use crate::manifest::Kind;
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
            let ran = process::run(&repo.dir, "cargo", &["metadata", "--format-version", "1", "--locked"])?;
            if ran.success {
                Outcome::Pass
            } else {
                Outcome::Fail(ran.tail(3))
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

    let cargo = |args: &[&str], ui: &mut dyn Ui| -> Result<Outcome> {
        ui.info(&format!("  {}", process::describe("cargo", args)));
        let ran = process::run(&repo.dir, "cargo", args)?;
        Ok(if ran.success {
            Outcome::Pass
        } else {
            Outcome::Fail(format!("\n{}", ran.tail(20)))
        })
    };

    let formatted = repo.dir.join("rustfmt.toml").exists() || repo.dir.join(".rustfmt.toml").exists();
    let outcome = if formatted {
        cargo(&["fmt", "--all", "--check"], ui)?
    } else {
        Outcome::Skip("no rustfmt.toml, so not formatted with rustfmt".to_string())
    };
    add("Formatted", outcome, ui);

    let outcome = cargo(
        &["clippy", "--workspace", "--all-targets", "--", "--deny", "warnings"],
        ui,
    )?;
    add("No clippy warnings", outcome, ui);

    let outcome = cargo(&["test", "--workspace"], ui)?;
    add("Tests pass", outcome, ui);

    let outcome = if repo.dir.join("deny.toml").exists() {
        cargo(&["deny", "check"], ui)?
    } else {
        Outcome::Skip("no deny.toml".to_string())
    };
    add("Licences, advisories and sources", outcome, ui);

    let outcome = if manifests.kind != Kind::Crate {
        Outcome::Skip("not published to crates.io".to_string())
    } else if !manifests.crates.iter().any(|krate| krate.publishable) {
        Outcome::Skip("no crate may be published".to_string())
    } else {
        cargo(&["publish", "--dry-run", "--workspace", "--allow-dirty"], ui)?
    };
    add("Would publish", outcome, ui);

    let outcome = if !repo.dir.join(".github/workflows").exists() {
        Outcome::Skip("no GitHub workflows".to_string())
    } else {
        ci_outcome(repo, &head)?
    };
    add("CI passed on GitHub", outcome, ui);

    Ok(checks)
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
