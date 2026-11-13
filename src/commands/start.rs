//! `start`: starting the branch a version is developed on.

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::manifest;
use crate::repo::Repo;
use crate::ui::Ui;
use crate::version::Version;

/// What `start` will do.
pub struct StartPlan {
    pub version: Version,
    /// What the branch starts from: `origin/main`, or a release's tag for a fix.
    pub base: String,
}

impl StartPlan {
    /// What it will do, for the confirmation.
    pub fn steps(&self, repo: &Repo) -> Vec<String> {
        vec![
            format!(
                "Create the branch {} in {} from {}",
                self.version.branch(),
                repo.name,
                self.base
            ),
            format!(
                "Set the version of every crate in it to {}, and update Cargo.lock",
                self.version
            ),
            format!(
                "Commit \"Start {}\" and push the branch to GitHub",
                self.version.branch()
            ),
        ]
    }
}

/// Works out the version to start and where from, after fetching: with no version, the next minor version after the
/// last release, from `main`; with a fix's version, `X.Y.(Z+1)`, from the tag of the release it fixes.
pub fn plan_start(repo: &Repo, version: Option<Version>) -> Result<StartPlan> {
    repo.require_clean()?;
    repo.fetch()?;
    let releases = repo.releases()?;
    let latest = releases.last().copied();
    let version = match (version, latest) {
        (Some(version), _) => version,
        (None, Some(latest)) => latest.next_minor(),
        (None, None) => bail!(
            "{} has no releaseX.Y.Z tag yet, so give the version to start, e.g. 0.1.0",
            repo.name
        ),
    };
    let base = match version.fixes() {
        Some(fixed) => {
            if !releases.contains(&fixed) {
                bail!("{} fixes {fixed}, but there is no tag {}", version, fixed.tag());
            }
            fixed.tag()
        }
        None => {
            if let Some(latest) = latest
                && version <= latest
            {
                bail!("{version} is not after the last release, {latest}");
            }
            if !repo.has_ref("refs/remotes/origin/main")? {
                bail!("{} has no main branch on GitHub", repo.name);
            }
            "origin/main".to_string()
        }
    };
    let branch = version.branch();
    if repo.has_ref(&format!("refs/heads/{branch}"))? || repo.has_ref(&format!("refs/remotes/origin/{branch}"))? {
        bail!(
            "{} already has a branch {branch}, switch to it with: git switch {branch}",
            repo.name
        );
    }
    Ok(StartPlan { version, base })
}

/// Starts the branch, as planned and confirmed.
pub fn apply_start(repo: &Repo, plan: &StartPlan, ui: &mut dyn Ui) -> Result<()> {
    let branch = plan.version.branch();
    ui.info(&format!("{}: creating {branch} from {}", repo.name, plan.base));
    repo.git(&["switch", "--quiet", "--no-track", "--create", &branch, &plan.base])?;
    let manifests = repo.manifests()?;
    // The crates of the workspace which depend on one another by path and version, e.g. a library on its derive
    // macro, require the new version of each other.
    let versions: HashMap<String, _> = manifests
        .crates
        .iter()
        .map(|krate| (krate.name.clone(), plan.version))
        .collect();
    let files = manifests.manifest_files(&repo.dir);
    for file in &files {
        manifest::set_version(file, plan.version)?;
        manifest::set_requirements_in(file, &versions)?;
    }
    // Cargo updates each Cargo.lock to the new versions: the workspace's, or each separate crate's.
    let dirs = repo.cargo_dirs(&manifests);
    for dir in &dirs {
        crate::process::output(dir, "cargo", &["metadata", "--format-version", "1", "--quiet"])?;
    }
    // The Cargo.toml files and Cargo.lock files by name, as a lock file the repository did not have is new.
    let mut add = vec!["add".to_string(), "--".to_string()];
    add.extend(files.iter().map(|file| file.to_string_lossy().into_owned()));
    for dir in &dirs {
        let lock = dir.join("Cargo.lock");
        let path = lock.to_string_lossy().into_owned();
        let ignored = crate::process::run(&repo.dir, "git", &["check-ignore", "--quiet", &path])?.success;
        if lock.exists() && !ignored {
            add.push(path);
        }
    }
    repo.git(&add.iter().map(String::as_str).collect::<Vec<_>>())?;
    repo.git(&["commit", "--quiet", "--message", &format!("Start {branch}")])?;
    repo.git(&["push", "--quiet", "--set-upstream", "origin", &branch])?;
    ui.info(&format!("{}: {branch} started and pushed", repo.name));
    Ok(())
}

/// Starts a version's branch, see plan_start.
pub fn start(repo: &Repo, version: Option<Version>, ui: &mut dyn Ui) -> Result<()> {
    let plan = plan_start(repo, version)?;
    if !ui.confirm(&format!("Start {} {}", repo.name, plan.version), &plan.steps(repo))? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    apply_start(repo, &plan, ui)
}
