//! `release`: releasing a repository's version, see the README.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::commands::check::{self, Outcome};
use crate::commands::link;
use crate::commands::start::{self, StartPlan};
use crate::links;
use crate::manifest::{Kind, Manifests};
use crate::process;
use crate::repo::{self, Repo};
use crate::ui::Ui;
use crate::version::Version;

/// How far a release has got, so that one which stopped part way carries on where it stopped.
struct Progress {
    merged: bool,
    tagged: bool,
    /// The crates still to publish, in the order Cargo publishes a workspace's.
    unpublished: Vec<String>,
    github_release: bool,
}

/// A repository next to this one which links the version being released, to be unlinked once it is published.
struct Unlink {
    repo: Repo,
    names: Vec<String>,
}

/// Releases the version on the repository's branch: checks it, squash merges it to main by pull request, tags main,
/// publishes from the tag, makes the GitHub release, unlinks it from the repositories next to it, and starts the next
/// version.
pub fn release(repo: &Repo, ui: &mut dyn Ui) -> Result<()> {
    let manifests = repo.manifests()?;
    if manifests.kind != Kind::Crate {
        bail!(
            "{} is an app, whose releases are not done by this tool yet: use its own release steps",
            repo.name
        );
    }
    let version = manifests.version()?;
    let branch = version.branch();
    if repo.branch()?.as_deref() != Some(branch.as_str()) {
        bail!("{} is not on {branch}, the branch of its version, {version}", repo.name);
    }
    repo.fetch()?;
    let progress = progress(repo, &manifests, version)?;

    let started = progress.merged || progress.tagged;
    if !started {
        let checks = check::check(repo, ui)?;
        let failed: Vec<String> = checks
            .iter()
            .filter_map(|check| match &check.outcome {
                Outcome::Fail(why) => Some(format!("{}: {why}", check.name)),
                _ => None,
            })
            .collect();
        if !failed.is_empty() {
            bail!("{} cannot be released yet:\n{}", repo.name, failed.join("\n"));
        }
    } else {
        ui.info(&format!(
            "{} {version} is part way through its release, carrying on",
            repo.name
        ));
    }

    let unlinks = unlinks(repo, &manifests, version)?;
    let next = match version.fixes() {
        Some(_) => None,
        None => Some(StartPlan {
            version: version.next_minor(),
            base: "origin/main".to_string(),
        }),
    };
    if let Some(next) = &next {
        let next_branch = next.version.branch();
        if repo.has_ref(&format!("refs/heads/{next_branch}"))?
            || repo.has_ref(&format!("refs/remotes/origin/{next_branch}"))?
        {
            bail!("{} already has the next version's branch, {next_branch}", repo.name);
        }
    }

    let done = |done: bool| if done { " (done)" } else { "" };
    let mut steps = vec![
        format!(
            "Squash merge {branch} into main by pull request{}",
            done(progress.merged)
        ),
        format!("Tag main {}, and push the tag{}", version.tag(), done(progress.tagged)),
        format!(
            "Publish {} to crates.io from the tag{}",
            manifests
                .crates
                .iter()
                .filter(|krate| krate.publishable)
                .map(|krate| krate.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            done(progress.unpublished.is_empty())
        ),
        format!(
            "Make the GitHub release {} with the ChangeLog's notes{}",
            version.tag(),
            done(progress.github_release)
        ),
    ];
    for unlink in &unlinks {
        steps.push(format!(
            "Unlink {} in {}, as it is then published",
            unlink.names.join(", "),
            unlink.repo.name
        ));
    }
    match &next {
        Some(next) => steps.extend(next.steps(repo)),
        None => steps.push(
            "Then, as this is a fix, merge main into the branch being developed by hand, so that it has the fix too"
                .to_string(),
        ),
    }
    if !ui.confirm(&format!("Release {} {version}", repo.name), &steps)? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }

    if !progress.merged {
        merge(repo, version, ui)?;
    }
    repo.fetch()?;
    if !progress.tagged {
        tag(repo, version, ui)?;
    }
    if !progress.unpublished.is_empty() {
        publish(repo, version, &manifests, &progress.unpublished, ui)?;
    }
    if !progress.github_release {
        github_release(repo, version, ui)?;
    }
    for unlink in &unlinks {
        link::remove_links(&unlink.repo, &unlink.names, ui)?;
    }
    match &next {
        Some(next) => start::apply_start(repo, next, ui)?,
        None => ui.info(&format!(
            "{} {version} released: merge main into the branch being developed, e.g. git switch vX.Y.0 && git merge origin/main",
            repo.name
        )),
    }
    ui.info(&format!("{} {version} released", repo.name));
    Ok(())
}

/// How far the release of a version has got, from GitHub and crates.io.
fn progress(repo: &Repo, manifests: &Manifests, version: Version) -> Result<Progress> {
    let branch = version.branch();
    let merged = process::output(
        &repo.dir,
        "gh",
        &[
            "pr", "list", "--head", &branch, "--base", "main", "--state", "merged", "--json", "number",
        ],
    )?;
    let merged = !serde_json::from_str::<Vec<serde_json::Value>>(&merged)?.is_empty();
    let tagged = repo.remote_tag_exists(&version.tag())?;
    let mut unpublished = Vec::new();
    for krate in manifests.crates.iter().filter(|krate| krate.publishable) {
        if !repo::is_published(&krate.name, version)? {
            unpublished.push(krate.name.clone());
        }
    }
    let github_release = process::run(&repo.dir, "gh", &["release", "view", &version.tag()])?.success;
    Ok(Progress {
        merged,
        tagged,
        unpublished,
        github_release,
    })
}

/// The repositories next to this one which link its crates at the version being released.
fn unlinks(repo: &Repo, manifests: &Manifests, version: Version) -> Result<Vec<Unlink>> {
    let mut unlinks = Vec::new();
    for sibling in repo.siblings()? {
        let patches = links::read(&sibling.dir)?;
        if patches.is_empty() {
            continue;
        }
        let theirs = sibling.manifests()?;
        let names: Vec<String> = patches
            .into_iter()
            .filter(|patch| manifests.find(&patch.name).is_some())
            .filter(|patch| {
                let requirements = theirs.requirements_of(&patch.name);
                !requirements.is_empty()
                    && requirements
                        .iter()
                        .all(|requirement| version.satisfies(requirement) == Some(true))
            })
            .map(|patch| patch.name)
            .collect();
        if !names.is_empty() {
            unlinks.push(Unlink { repo: sibling, names });
        }
    }
    Ok(unlinks)
}

/// Squash merges the version's branch into main by pull request, opening one if there is none.
fn merge(repo: &Repo, version: Version, ui: &mut dyn Ui) -> Result<()> {
    let branch = version.branch();
    let open = process::output(
        &repo.dir,
        "gh",
        &[
            "pr", "list", "--head", &branch, "--base", "main", "--state", "open", "--json", "number",
        ],
    )?;
    if serde_json::from_str::<Vec<serde_json::Value>>(&open)?.is_empty() {
        ui.info(&format!("{}: opening a pull request from {branch} to main", repo.name));
        process::output(
            &repo.dir,
            "gh",
            &[
                "pr",
                "create",
                "--base",
                "main",
                "--head",
                &branch,
                "--title",
                &format!("Release {version}"),
                "--body",
                &format!("Release {version}"),
            ],
        )?;
    }
    ui.info(&format!("{}: squash merging {branch} into main", repo.name));
    process::output(
        &repo.dir,
        "gh",
        &[
            "pr",
            "merge",
            &branch,
            "--squash",
            "--subject",
            &format!("Release {version}"),
        ],
    )?;
    Ok(())
}

/// Tags main, after checking that the merge left main the same as the branch.
fn tag(repo: &Repo, version: Version, ui: &mut dyn Ui) -> Result<()> {
    if !process::run(&repo.dir, "git", &["diff", "--quiet", "origin/main", "HEAD"])?.success {
        bail!(
            "{}: main is not the same as {} after the merge, see git diff origin/main HEAD",
            repo.name,
            version.branch()
        );
    }
    let tag = version.tag();
    ui.info(&format!("{}: tagging main {tag}", repo.name));
    repo.git(&[
        "tag",
        "--annotate",
        &tag,
        "origin/main",
        "--message",
        &format!("{} {tag}", repo.name),
    ])?;
    repo.git(&["push", "--quiet", "origin", &tag])?;
    Ok(())
}

/// Publishes the crates not yet published, from a worktree of the tag, so that what is published is exactly the
/// tagged commit, with no patches, as the git-ignored config file is not in it, then checks that crates.io recorded
/// that commit.
fn publish(
    repo: &Repo,
    version: Version,
    manifests: &Manifests,
    unpublished: &[String],
    ui: &mut dyn Ui,
) -> Result<()> {
    let tag = version.tag();
    let worktree = std::env::temp_dir().join(format!("bruce-method-release-{}-{version}", repo.name));
    remove_worktree(repo, &worktree)?;
    let path = worktree.to_string_lossy().into_owned();
    repo.git(&["worktree", "add", "--quiet", "--detach", &path, &tag])?;
    let mut args = vec!["publish", "--workspace"];
    for krate in manifests
        .crates
        .iter()
        .filter(|krate| krate.publishable && !unpublished.contains(&krate.name))
    {
        args.push("--exclude");
        args.push(&krate.name);
    }
    ui.info(&format!(
        "{}: publishing {} {version} from {tag}",
        repo.name,
        unpublished.join(", ")
    ));
    process::visible(&worktree, "cargo", &args)?;
    let commit = repo.commit(&tag)?;
    for name in unpublished {
        match recorded_commit(&worktree, name, version)? {
            Some(recorded) if recorded == commit => {}
            Some(recorded) => {
                bail!("crates.io recorded {name} {version} as from {recorded}, not {tag}'s commit {commit}")
            }
            None => ui.info(&format!(
                "Could not find which commit {name} {version}'s package recorded"
            )),
        }
    }
    remove_worktree(repo, &worktree)?;
    Ok(())
}

/// The commit a crate's package recorded, from the copy Cargo unpacked to check it before publishing.
fn recorded_commit(worktree: &Path, name: &str, version: Version) -> Result<Option<String>> {
    let file = worktree
        .join("target/package")
        .join(format!("{name}-{version}"))
        .join(".cargo_vcs_info.json");
    if !file.exists() {
        return Ok(None);
    }
    let info: serde_json::Value = serde_json::from_str(&fs::read_to_string(&file)?)?;
    Ok(info["git"]["sha1"].as_str().map(str::to_string))
}

/// Removes a worktree left by a release, if there is one.
fn remove_worktree(repo: &Repo, worktree: &PathBuf) -> Result<()> {
    if worktree.exists() {
        let path = worktree.to_string_lossy().into_owned();
        let _ = process::run(&repo.dir, "git", &["worktree", "remove", "--force", &path]);
        if worktree.exists() {
            fs::remove_dir_all(worktree).with_context(|| format!("could not remove {}", worktree.display()))?;
        }
        repo.git(&["worktree", "prune"])?;
    }
    Ok(())
}

/// Makes the GitHub release of the tag, with the ChangeLog's notes for the version.
fn github_release(repo: &Repo, version: Version, ui: &mut dyn Ui) -> Result<()> {
    let notes = check::changelog_notes(&repo.dir, version)?.unwrap_or_default();
    let file = std::env::temp_dir().join(format!("bruce-method-release-{}-{version}-notes.md", repo.name));
    fs::write(&file, notes).with_context(|| format!("could not write {}", file.display()))?;
    ui.info(&format!("{}: making the GitHub release {}", repo.name, version.tag()));
    let path = file.to_string_lossy().into_owned();
    let made = process::output(
        &repo.dir,
        "gh",
        &[
            "release",
            "create",
            &version.tag(),
            "--verify-tag",
            "--title",
            &format!("{} {version}", repo.name),
            "--notes-file",
            &path,
        ],
    );
    let _ = fs::remove_file(&file);
    made.map(|_| ())
}
