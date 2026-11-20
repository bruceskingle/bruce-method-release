//! A repository's releases and workflow runs on GitHub, through the GitHub CLI, `gh`.

use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::process;
use crate::repo::Repo;
use crate::ui::Ui;

/// Where a GitHub release is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseState {
    /// There is no release for the tag.
    None,
    /// A draft, which only the repository's owners see, and which can still be changed or deleted.
    Draft,
    Published,
}

/// Where the GitHub release for a tag is.
pub fn release_state(repo: &Repo, tag: &str) -> Result<ReleaseState> {
    let ran = process::run(&repo.dir, "gh", &["release", "view", tag, "--json", "isDraft"])?;
    if !ran.success {
        if ran.stderr.contains("not found") {
            return Ok(ReleaseState::None);
        }
        bail!("could not ask GitHub for the release {tag}: {}", ran.tail(5));
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct View {
        is_draft: bool,
    }
    let view: View = serde_json::from_str(&ran.stdout).context("could not read gh's release")?;
    Ok(if view.is_draft {
        ReleaseState::Draft
    } else {
        ReleaseState::Published
    })
}

/// The names of the files attached to the GitHub release for a tag.
pub fn release_assets(repo: &Repo, tag: &str) -> Result<Vec<String>> {
    let names = process::output(
        &repo.dir,
        "gh",
        &["release", "view", tag, "--json", "assets", "--jq", ".assets[].name"],
    )?;
    Ok(names.lines().map(str::to_string).collect())
}

/// A run of a workflow, as `gh run list` describes it.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    #[serde(rename = "databaseId")]
    pub id: u64,
    /// `queued`, `in_progress` or `completed`, among others.
    pub status: String,
    /// Once completed, `success`, `failure` or `cancelled`, among others; empty until then.
    pub conclusion: String,
    /// The branch or tag the run was started from, e.g. `v0.6.0` for a try, or `release0.6.0` for a release.
    pub head_branch: String,
    pub head_sha: String,
    pub created_at: String,
    pub url: String,
}

impl Run {
    pub fn completed(&self) -> bool {
        self.status == "completed"
    }

    pub fn succeeded(&self) -> bool {
        self.conclusion == "success"
    }

    /// Where it is, e.g. `in_progress` or `failure`.
    pub fn state(&self) -> &str {
        if self.completed() {
            &self.conclusion
        } else {
            &self.status
        }
    }
}

/// The latest runs of a workflow, newest first, of those started from a branch or tag if one is given.
pub fn runs(repo: &Repo, workflow: &str, from: Option<&str>, limit: usize) -> Result<Vec<Run>> {
    let limit = limit.to_string();
    let mut args = vec![
        "run",
        "list",
        "--workflow",
        workflow,
        "--limit",
        &limit,
        "--json",
        "databaseId,status,conclusion,headBranch,headSha,createdAt,url",
    ];
    if let Some(from) = from {
        args.extend(["--branch", from]);
    }
    let json = process::output(&repo.dir, "gh", &args)?;
    serde_json::from_str(&json).context("could not read gh's list of runs")
}

/// Starts a workflow from a branch or tag, with its inputs, and returns its run, once GitHub lists it.
pub fn start_workflow(
    repo: &Repo,
    workflow: &str,
    from: &str,
    inputs: &[(&str, &str)],
    ui: &mut dyn Ui,
) -> Result<Run> {
    let previous = runs(repo, workflow, Some(from), 1)?.first().map(|run| run.id);
    ui.info(&format!("{}: starting the {workflow} workflow from {from}", repo.name));
    let fields: Vec<String> = inputs.iter().map(|(name, value)| format!("{name}={value}")).collect();
    let mut args = vec!["workflow", "run", workflow, "--ref", from];
    for field in &fields {
        args.extend(["--field", field]);
    }
    process::output(&repo.dir, "gh", &args)?;
    // GitHub lists the run a few seconds after it is asked for.
    for _ in 0..30 {
        thread::sleep(Duration::from_secs(2));
        if let Some(run) = runs(repo, workflow, Some(from), 1)?.into_iter().next()
            && Some(run.id) != previous
        {
            return Ok(run);
        }
    }
    bail!("the {workflow} run from {from} did not start, see the repository's Actions tab on GitHub")
}

/// Shows a run's progress until it finishes, failing, with how to see why, if it did not succeed. Stopping the tool,
/// e.g. with Ctrl+C, stops watching but not the run.
pub fn watch(repo: &Repo, run: &Run, ui: &mut dyn Ui) -> Result<()> {
    ui.info(&format!(
        "{}: watching run {}, {}, Ctrl+C stops watching but not the run",
        repo.name, run.id, run.url
    ));
    let id = run.id.to_string();
    if process::visible(&repo.dir, "gh", &["run", "watch", &id, "--exit-status"]).is_err() {
        bail!(
            "the run {id} did not succeed.\nTo see why: gh run view {id} --log-failed\n\
             To run the failed jobs again: gh run rerun {id} --failed"
        );
    }
    Ok(())
}

/// Downloads a run's artifacts into `dir`, each into a directory of its own name, or only the one named, into `dir`
/// itself.
pub fn download_run(repo: &Repo, run: &Run, artifact: Option<&str>, dir: &Path) -> Result<()> {
    let id = run.id.to_string();
    let dir = dir.to_string_lossy().into_owned();
    let mut args = vec!["run", "download", &id, "--dir", &dir];
    if let Some(artifact) = artifact {
        args.extend(["--name", artifact]);
    }
    process::output(&repo.dir, "gh", &args).map(|_| ())
}

/// Downloads the files attached to the GitHub release for a tag into `dir`, or only those matching a pattern, e.g.
/// `*_aarch64.dmg`, replacing any there already.
pub fn download_release(repo: &Repo, tag: &str, pattern: Option<&str>, dir: &Path) -> Result<()> {
    let dir = dir.to_string_lossy().into_owned();
    let mut args = vec!["release", "download", tag, "--dir", &dir, "--clobber"];
    if let Some(pattern) = pattern {
        args.extend(["--pattern", pattern]);
    }
    process::output(&repo.dir, "gh", &args).map(|_| ())
}
