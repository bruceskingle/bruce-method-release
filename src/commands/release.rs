//! `release`: releasing a repository's version, see the README.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::commands::check::{self, Outcome};
use crate::commands::link;
use crate::commands::start::{self, StartPlan};
use crate::links;
use crate::manifest::{Crate, Kind, Manifests};
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
/// publishes from the tag, makes the GitHub release, unlinks it from the repositories next to it, both their links and
/// their path dependencies on it, and starts the next version.
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
    let paths = path_plans(repo, version)?;
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
    for plan in &paths {
        if !plan.names.is_empty() {
            steps.push(plan.step());
        }
        for left in &plan.left {
            steps.push(format!("Leave by path in {}, as {left}", plan.repo.name));
        }
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
    for plan in &paths {
        link::apply_path_dependencies(plan, ui)?;
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

/// The repositories next to this one, developed by the method, which take its crates by path, to take the version
/// being released from crates.io instead once it is published, as their checkouts of it move on to the next version.
fn path_plans(repo: &Repo, version: Version) -> Result<Vec<link::PathPlan>> {
    let mut plans = Vec::new();
    for sibling in repo.siblings()? {
        if !sibling.follows_method()? {
            continue;
        }
        if let Some(plan) = link::plan_path_dependencies(&sibling, repo, version)? {
            plans.push(plan);
        }
    }
    Ok(plans)
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
    let commit = repo.commit(&tag)?;
    ui.info(&format!(
        "{}: publishing {} {version} from {tag}",
        repo.name,
        unpublished.join(", ")
    ));
    if repo.is_workspace() {
        let mut args = vec!["publish", "--workspace"];
        for krate in manifests
            .crates
            .iter()
            .filter(|krate| krate.publishable && !unpublished.contains(&krate.name))
        {
            args.push("--exclude");
            args.push(&krate.name);
        }
        process::visible(&worktree, "cargo", &args)?;
        for name in unpublished {
            check_recorded(&worktree.join("target/package"), name, version, &commit, ui)?;
        }
    } else {
        // Each crate from its own directory, so that its own .cargo/config.toml applies, those it depends on first.
        for krate in publishing_order(manifests, unpublished) {
            let dir = worktree.join(repo.relative(krate.dir()));
            let mut args = vec!["publish"];
            // A crate for a device is checked by building it, or the crates which use it, e.g. a platform's boards,
            // before the release, and may not compile on its own, without a board's features.
            if krate.for_device()? {
                args.push("--no-verify");
            }
            process::visible(&dir, "cargo", &args)?;
            check_recorded(&dir.join("target/package"), &krate.name, version, &commit, ui)?;
        }
    }
    remove_worktree(repo, &worktree)?;
    Ok(())
}

/// The crates of a repository of separate crates still to publish, each after those of them it depends on.
fn publishing_order<'a>(manifests: &'a Manifests, unpublished: &[String]) -> Vec<&'a Crate> {
    let mut left: Vec<&Crate> = manifests
        .crates
        .iter()
        .filter(|krate| unpublished.contains(&krate.name))
        .collect();
    let mut order = Vec::new();
    while !left.is_empty() {
        let waiting = |krate: &Crate, left: &[&Crate]| {
            krate.dependencies.iter().any(|dependency| {
                dependency.name != krate.name && left.iter().any(|other| other.name == dependency.name)
            })
        };
        // A cycle cannot be published anyway; taking the first then lets cargo say why.
        let next = left.iter().position(|krate| !waiting(krate, &left)).unwrap_or(0);
        order.push(left.remove(next));
    }
    order
}

/// Checks that crates.io recorded the tag's commit as the one a crate was published from, as the package Cargo made in
/// `package_dir` says.
fn check_recorded(package_dir: &Path, name: &str, version: Version, commit: &str, ui: &mut dyn Ui) -> Result<()> {
    match recorded_commit(package_dir, name, version)? {
        Some(recorded) if recorded == commit => Ok(()),
        Some(recorded) => {
            bail!(
                "crates.io recorded {name} {version} as from {recorded}, not {}'s commit {commit}",
                version.tag()
            )
        }
        None => {
            ui.info(&format!(
                "Could not find which commit {name} {version}'s package recorded"
            ));
            Ok(())
        }
    }
}

/// The commit a crate's package recorded, from the copy Cargo unpacked to check it before publishing, or else from the
/// package itself, which is all there is when it was published without checking that it builds.
fn recorded_commit(package_dir: &Path, name: &str, version: Version) -> Result<Option<String>> {
    let package = format!("{name}-{version}");
    let file = package_dir.join(&package).join(".cargo_vcs_info.json");
    let json = if file.exists() {
        fs::read_to_string(&file)?
    } else {
        let crate_file = package_dir.join(format!("{package}.crate"));
        if !crate_file.exists() {
            return Ok(None);
        }
        let path = crate_file.to_string_lossy().into_owned();
        let member = format!("{package}/.cargo_vcs_info.json");
        let ran = process::run(package_dir, "tar", &["-xzOf", &path, &member])?;
        if !ran.success {
            return Ok(None);
        }
        ran.stdout
    };
    let info: serde_json::Value = serde_json::from_str(&json)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Dependency;

    fn krate(name: &str, depends_on: &[&str]) -> Crate {
        Crate {
            name: name.to_string(),
            version: "0.1.0".parse().unwrap(),
            manifest: PathBuf::from(format!("{name}/Cargo.toml")),
            publishable: true,
            dependencies: depends_on
                .iter()
                .map(|name| Dependency {
                    name: name.to_string(),
                    requirement: "^0.1.0".to_string(),
                    by_path: true,
                    path: Some(PathBuf::from(name)),
                })
                .collect(),
        }
    }

    #[test]
    fn publishes_separate_crates_after_those_they_depend_on() {
        let manifests = Manifests {
            crates: vec![
                krate("app", &["lib", "log"]),
                krate("lib", &["derive"]),
                krate("derive", &[]),
            ],
            kind: Kind::Crate,
        };
        let all = ["app", "lib", "derive"].map(str::to_string);
        let order: Vec<&str> = publishing_order(&manifests, &all)
            .iter()
            .map(|krate| krate.name.as_str())
            .collect();
        assert_eq!(order, ["derive", "lib", "app"]);
        let some = ["app", "lib"].map(str::to_string);
        let order: Vec<&str> = publishing_order(&manifests, &some)
            .iter()
            .map(|krate| krate.name.as_str())
            .collect();
        assert_eq!(order, ["lib", "app"]);
    }
}
