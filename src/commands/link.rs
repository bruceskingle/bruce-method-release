//! `link` and `unlink`: building a library from the checkout next to this one while both are developed, and going
//! back to the published version once the library is released, see the README.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::links::{self, Patch};
use crate::manifest::Manifests;
use crate::process;
use crate::repo::{self, Repo};
use crate::ui::Ui;
use crate::version::Version;

/// A link of a repository to a library, and whether it is still needed.
#[derive(Clone, Debug)]
pub struct Link {
    pub patch: Patch,
    /// The repository the patched crate is in, if it is there.
    pub library: Option<String>,
    /// The version of the crate in the checkout patched in.
    pub local: Option<Version>,
    /// The versions of the crate the repository requires.
    pub requirements: Vec<String>,
    pub state: State,
}

/// Whether a link is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Needed: the version required is not on crates.io, and is built from the checkout.
    Linked,
    /// Not needed: the version required has been released.
    Released,
    /// Not working: the checkout's version is not one the repository requires, so Cargo ignores the patch, with only
    /// a warning, and builds from crates.io.
    Stale,
    /// Not needed: the repository does not depend on the crate.
    Unused,
    /// Not working: the crate is not where the patch says.
    Missing,
}

impl State {
    /// Whether unlink removes it when no library is named.
    pub fn removable(&self) -> bool {
        *self != State::Linked
    }

    pub fn describe(&self) -> &'static str {
        match self {
            State::Linked => "linked",
            State::Released => "released, the link can be removed",
            State::Stale => {
                "not used, as the checkout's version is not the one required, so it is built from crates.io"
            }
            State::Unused => "not used, as nothing depends on it",
            State::Missing => "not found where the link says",
        }
    }
}

/// A repository's links, and the state of each, asking crates.io whether the versions required are released.
pub fn links_of(repo: &Repo) -> Result<Vec<Link>> {
    let patches = links::read(&repo.dir)?;
    if patches.is_empty() {
        return Ok(Vec::new());
    }
    let manifests = repo.manifests()?;
    patches
        .into_iter()
        .map(|patch| link_of(repo, &manifests, patch))
        .collect()
}

fn link_of(repo: &Repo, manifests: &Manifests, patch: Patch) -> Result<Link> {
    let dir = patch.dir(&repo.dir);
    let requirements = manifests.requirements_of(&patch.name);
    let library = dir
        .exists()
        .then(|| Repo::containing(&dir).ok())
        .flatten()
        .map(|library| library.name);
    let local = dir
        .exists()
        .then(|| Manifests::read(&dir).ok())
        .flatten()
        .and_then(|library| library.find(&patch.name).map(|krate| krate.version));
    let state = match local {
        None => State::Missing,
        Some(_) if requirements.is_empty() => State::Unused,
        Some(local) => {
            let lowest: Vec<Version> = requirements
                .iter()
                .filter_map(|requirement| Version::lowest(requirement))
                .collect();
            let mut released = !lowest.is_empty();
            for version in &lowest {
                released &= repo::is_published(&patch.name, *version)?;
            }
            if released {
                State::Released
            } else if requirements
                .iter()
                .any(|requirement| local.satisfies(requirement) == Some(false))
            {
                State::Stale
            } else {
                State::Linked
            }
        }
    };
    Ok(Link {
        patch,
        library,
        local,
        requirements,
        state,
    })
}

/// What `link` will do.
pub struct LinkPlan {
    pub library: String,
    pub library_dir: PathBuf,
    /// The URL to clone the library from, if it is not next to this repository.
    pub clone: Option<String>,
    /// The branch to put the library's checkout on first, after cloning it, or because it is on another branch.
    pub branch: Option<Version>,
    /// The patches to add, known once the library is checked out.
    pub patches: Vec<Patch>,
    /// The versions to require of the library's crates.
    pub versions: HashMap<String, Version>,
    /// The changes to the repository's Cargo.toml files, known once the library is checked out.
    pub changes: Vec<String>,
    /// Whether .gitignore needs the config file adding to it.
    pub ignore: bool,
}

impl LinkPlan {
    /// What it will do, for the confirmation.
    pub fn steps(&self, repo: &Repo) -> Vec<String> {
        let mut steps = Vec::new();
        if let Some(version) = &self.branch {
            match &self.clone {
                Some(url) => steps.push(format!(
                    "Clone {url} into {}, on its branch {}",
                    self.library_dir.display(),
                    version.branch()
                )),
                None => steps.push(format!("Switch {} to its branch {}", self.library, version.branch())),
            }
            steps.push(format!(
                "In {}'s Cargo.toml files, require version {version} of the crates of {} which it uses, if they do not",
                repo.name, self.library
            ));
            steps.push(format!(
                "Patch them in {}'s {}, to build them from the checkout",
                repo.name,
                links::CONFIG
            ));
        } else {
            for change in &self.changes {
                steps.push(format!("In {}, require {change}", repo.name));
            }
            for patch in &self.patches {
                steps.push(format!(
                    "Patch {} in {}'s {}, built from {}",
                    patch.name,
                    repo.name,
                    links::CONFIG,
                    patch.path
                ));
            }
        }
        if self.ignore {
            steps.push(format!(
                "Add /{} to {}'s .gitignore, as it is each developer's own",
                links::CONFIG,
                repo.name
            ));
        }
        steps
    }
}

/// Works out what linking a repository to a library next to it will do: to the version on the library's checkout, or,
/// given a version, e.g. the one the repository already requires, to that version's branch, switching the checkout
/// to it first if need be.
pub fn plan_link(repo: &Repo, library: &str, wanted: Option<Version>) -> Result<LinkPlan> {
    let library_dir = repo.sibling_dir(library);
    let ignore = !process::run(&repo.dir, "git", &["check-ignore", "--quiet", links::CONFIG])?.success;
    let mut plan = LinkPlan {
        library: library.to_string(),
        library_dir: library_dir.clone(),
        clone: None,
        branch: None,
        patches: Vec::new(),
        versions: HashMap::new(),
        changes: Vec::new(),
        ignore,
    };
    if !library_dir.exists() {
        let url = repo.sibling_url(library)?;
        let heads = process::output(&repo.dir, "git", &["ls-remote", "--heads", &url, "v*"])
            .with_context(|| format!("could not find {library} at {url}"))?;
        let branches: Vec<Version> = heads
            .lines()
            .filter_map(|line| line.rsplit("refs/heads/").next().and_then(Version::from_branch))
            .collect();
        let branch = match wanted {
            Some(wanted) if branches.contains(&wanted) => wanted,
            Some(wanted) => bail!("{url} has no branch {}", wanted.branch()),
            None => *branches
                .iter()
                .max()
                .with_context(|| format!("{url} has no vX.Y.Z branch to link to"))?,
        };
        plan.clone = Some(url);
        plan.branch = Some(branch);
        return Ok(plan);
    }
    if let Some(wanted) = wanted {
        let checkout = Repo::at(library_dir.clone());
        if checkout.branch()?.as_deref() != Some(wanted.branch().as_str()) {
            checkout.require_clean()?;
            checkout.fetch()?;
            let branch = wanted.branch();
            if !checkout.has_ref(&format!("refs/heads/{branch}"))?
                && !checkout.has_ref(&format!("refs/remotes/origin/{branch}"))?
            {
                bail!("{library} has no branch {branch}, here or on GitHub");
            }
            plan.branch = Some(wanted);
            return Ok(plan);
        }
    }
    fill_plan(repo, &mut plan)?;
    Ok(plan)
}

/// Works out the patches and Cargo.toml changes, once the library is checked out.
fn fill_plan(repo: &Repo, plan: &mut LinkPlan) -> Result<()> {
    let library = Repo::at(plan.library_dir.clone());
    let manifests = library.manifests()?;
    let version = manifests.version().with_context(|| library.name.clone())?;
    let branch = library.branch()?;
    if branch.as_deref() != Some(version.branch().as_str()) {
        bail!(
            "{} is on {}, not its development branch {}, switch it to the branch to link to first",
            library.name,
            branch.as_deref().unwrap_or("no branch"),
            version.branch()
        );
    }
    let ours = repo.manifests()?;
    let used: Vec<_> = manifests
        .crates
        .iter()
        .filter(|krate| krate.publishable && !ours.requirements_of(&krate.name).is_empty())
        .collect();
    if used.is_empty() {
        bail!(
            "{} does not depend on any crate of {} from crates.io",
            repo.name,
            library.name
        );
    }
    let library_root = library.dir.canonicalize()?;
    for krate in used {
        let within = krate.dir().canonicalize()?;
        let within = within.strip_prefix(&library_root).unwrap_or(Path::new(""));
        let mut path = format!("../{}", library.name);
        for part in within.components() {
            path.push('/');
            path.push_str(&part.as_os_str().to_string_lossy());
        }
        plan.patches.push(Patch {
            name: krate.name.clone(),
            path,
        });
        plan.versions.insert(krate.name.clone(), version);
    }
    let existing = links::read(&repo.dir)?;
    plan.patches.retain(|patch| !existing.contains(patch));
    plan.changes.clear();
    for file in ours.manifest_files(&repo.dir) {
        let text = std::fs::read_to_string(&file).with_context(|| format!("could not read {}", file.display()))?;
        let (_, changes) = crate::manifest::with_requirements(&text, &plan.versions)?;
        plan.changes.extend(changes);
    }
    Ok(())
}

/// Links a repository to a library next to it, as planned and confirmed.
pub fn apply_link(repo: &Repo, mut plan: LinkPlan, ui: &mut dyn Ui) -> Result<()> {
    if let Some(version) = plan.branch.take() {
        if let Some(url) = plan.clone.take() {
            ui.info(&format!("Cloning {url}"));
            let dir = plan.library_dir.to_string_lossy().into_owned();
            process::output(repo.parent(), "git", &["clone", "--quiet", &url, &dir])?;
        }
        process::output(&plan.library_dir, "git", &["switch", "--quiet", &version.branch()])?;
        fill_plan(repo, &mut plan)?;
    }
    for file in repo.manifests()?.manifest_files(&repo.dir) {
        crate::manifest::set_requirements_in(&file, &plan.versions)?;
    }
    if !plan.patches.is_empty() {
        links::add(&repo.dir, &plan.patches)?;
    }
    if plan.ignore {
        let file = repo.dir.join(".gitignore");
        let mut text = std::fs::read_to_string(&file).unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("/{}\n", links::CONFIG));
        std::fs::write(&file, text).with_context(|| format!("could not write {}", file.display()))?;
    }
    verify_linked(repo, &plan)?;
    if plan.changes.is_empty() {
        ui.info(&format!("{} is linked to {}", repo.name, plan.library));
    } else {
        ui.info(&format!(
            "{} is linked to {}: commit the Cargo.toml changes, which say this branch needs {}'s next version",
            repo.name, plan.library, plan.library
        ));
    }
    Ok(())
}

/// Checks that Cargo now builds the linked crates from the checkout, resolving the dependencies, which also brings
/// Cargo.lock up to date.
fn verify_linked(repo: &Repo, plan: &LinkPlan) -> Result<()> {
    let json = process::output(&repo.dir, "cargo", &["metadata", "--format-version", "1"])
        .context("Cargo could not resolve the dependencies with the link")?;
    let metadata: serde_json::Value = serde_json::from_str(&json)?;
    let library_root = plan.library_dir.canonicalize()?;
    for name in plan.versions.keys() {
        let from_checkout = metadata["packages"].as_array().into_iter().flatten().any(|package| {
            package["name"] == name.as_str()
                && package["source"].is_null()
                && package["manifest_path"]
                    .as_str()
                    .and_then(|path| Path::new(path).canonicalize().ok())
                    .is_some_and(|path| path.starts_with(&library_root))
        });
        if !from_checkout {
            bail!(
                "Cargo does not build {name} from {} after linking, see cargo tree -i {name}",
                library_root.display()
            );
        }
    }
    Ok(())
}

/// Links a repository to a library next to it, see the README.
pub fn link(repo: &Repo, library: &str, ui: &mut dyn Ui) -> Result<()> {
    let plan = plan_link(repo, library, None)?;
    let steps = plan.steps(repo);
    if steps.is_empty() {
        ui.info(&format!("{} is already linked to {library}", repo.name));
        return Ok(());
    }
    if !ui.confirm(&format!("Link {} to {library}", repo.name), &steps)? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    apply_link(repo, plan, ui)
}

/// Removes a repository's links to a library, or else every link not needed. Given a library next to it which has
/// been released, its path dependencies on the library's crates are made dependencies on the latest release on
/// crates.io too, see plan_path_dependencies.
pub fn unlink(repo: &Repo, library: Option<&str>, ui: &mut dyn Ui) -> Result<()> {
    let all = links_of(repo)?;
    let chosen: Vec<&Link> = match library {
        Some(library) => all
            .iter()
            .filter(|link| link.library.as_deref() == Some(library) || link.patch.name == library)
            .collect(),
        None => all.iter().filter(|link| link.state.removable()).collect(),
    };
    let paths = match library {
        Some(library) => plan_released_paths(repo, library)?,
        None => None,
    };
    if chosen.is_empty() && paths.as_ref().is_none_or(|paths| paths.names.is_empty()) {
        match library {
            Some(library) => bail!(
                "{} has no link to {library}, and no path dependency on it which a release on crates.io meets",
                repo.name
            ),
            None => {
                ui.info(&format!("{} has no links which can be removed", repo.name));
                return Ok(());
            }
        }
    }
    let mut steps = Vec::new();
    for link in &chosen {
        steps.push(format!(
            "Remove the link of {} to {}: {}",
            link.patch.name,
            link.patch.path,
            link.state.describe()
        ));
        if link.state == State::Linked {
            steps.push(format!(
                "{} then cannot be built until {} {} is on crates.io, or its Cargo.toml requires a version which is",
                repo.name,
                link.patch.name,
                link.requirements.join(", ")
            ));
        }
    }
    if let Some(paths) = &paths
        && !paths.names.is_empty()
    {
        steps.push(paths.step());
        for left in &paths.left {
            steps.push(format!("Leave by path, as {left}"));
        }
    }
    if !ui.confirm(&format!("Unlink {}", repo.name), &steps)? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    if !chosen.is_empty() {
        remove_links(
            repo,
            &chosen.iter().map(|link| link.patch.name.clone()).collect::<Vec<_>>(),
            ui,
        )?;
    }
    if let Some(paths) = &paths {
        apply_path_dependencies(paths, ui)?;
    }
    Ok(())
}

/// The plan for a repository's path dependencies on the library next to it, by the library's repository name, at its
/// latest release, if it has one on crates.io.
fn plan_released_paths(repo: &Repo, library: &str) -> Result<Option<PathPlan>> {
    let dir = repo.sibling_dir(library);
    if !dir.join(".git").exists() {
        return Ok(None);
    }
    let library = Repo::at(dir);
    let Some(release) = library.releases()?.last().copied() else {
        return Ok(None);
    };
    let (theirs, _) = library.all_manifests()?;
    let Some(krate) = theirs.crates.iter().find(|krate| krate.publishable) else {
        return Ok(None);
    };
    if !repo::is_published(&krate.name, release)? {
        return Ok(None);
    }
    plan_path_dependencies(repo, &library, release)
}

/// A repository's path dependencies on a library's crates, in its Cargo.toml files, to be made dependencies on a
/// version of the library on crates.io, e.g. once it is released, see manifest::with_published.
#[derive(Clone, Debug)]
pub struct PathPlan {
    pub repo: Repo,
    pub library: String,
    pub version: Version,
    /// The library's crates taken by path which are to be taken from crates.io.
    pub names: Vec<String>,
    /// The uses left as they are, as they require another version, e.g. "app requires ^0.2.0 of lib".
    pub left: Vec<String>,
}

impl PathPlan {
    pub fn step(&self) -> String {
        format!(
            "In {}, take {} {} from crates.io rather than by path from {}",
            self.repo.name,
            self.names.join(", "),
            self.version,
            self.library
        )
    }
}

/// Works out which of a repository's path dependencies on the crates of the library next to it can be taken from
/// crates.io at version: those of a path alone, and those whose version required it meets. None if it has no path
/// dependencies on the library's crates. A path dependency is on the library only if its path is in the library's
/// checkout, so that a crate of the same name elsewhere, e.g. a fork, is left alone.
pub fn plan_path_dependencies(repo: &Repo, library: &Repo, version: Version) -> Result<Option<PathPlan>> {
    let (theirs, _) = library.all_manifests()?;
    let crates: Vec<String> = theirs
        .crates
        .iter()
        .filter(|krate| krate.publishable)
        .map(|krate| krate.name.clone())
        .collect();
    let library_dir = library.dir.canonicalize().unwrap_or(library.dir.clone());
    let (ours, _) = repo.all_manifests()?;
    let mut names: Vec<String> = Vec::new();
    let mut left: Vec<String> = Vec::new();
    for krate in &ours.crates {
        for dependency in &krate.dependencies {
            let Some(path) = &dependency.path else { continue };
            let path = path.canonicalize().unwrap_or(path.clone());
            if !crates.contains(&dependency.name) || !path.starts_with(&library_dir) {
                continue;
            }
            if dependency.requirement == "*" || version.satisfies(&dependency.requirement) == Some(true) {
                if !names.contains(&dependency.name) {
                    names.push(dependency.name.clone());
                }
            } else {
                left.push(format!(
                    "{} requires {} of {}",
                    krate.name, dependency.requirement, dependency.name
                ));
            }
        }
    }
    if names.is_empty() && left.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathPlan {
        repo: repo.clone(),
        library: library.name.clone(),
        version,
        names,
        left,
    }))
}

/// Makes the path dependencies of a plan dependencies on crates.io in each of the repository's Cargo.toml files, and
/// brings each changed crate's Cargo.lock up to date, saying what changed.
pub fn apply_path_dependencies(plan: &PathPlan, ui: &mut dyn Ui) -> Result<()> {
    if plan.names.is_empty() {
        return Ok(());
    }
    let versions: HashMap<String, Version> = plan.names.iter().map(|name| (name.clone(), plan.version)).collect();
    for file in plan.repo.tracked_manifests()? {
        let changes = crate::manifest::publish_paths_in(&file, &versions)?;
        if changes.is_empty() {
            continue;
        }
        let shown = file.strip_prefix(&plan.repo.dir).unwrap_or(&file).display().to_string();
        for change in &changes {
            ui.info(&format!("{}: {shown}: {}", plan.repo.name, change.what));
        }
        let manifest = file.to_string_lossy().into_owned();
        let ran = process::run(
            &plan.repo.dir,
            "cargo",
            &["metadata", "--format-version", "1", "--manifest-path", &manifest],
        )?;
        if !ran.success {
            ui.info(&format!(
                "{}: {shown} cannot resolve its dependencies now: {}",
                plan.repo.name,
                ran.tail(3)
            ));
        }
    }
    for left in &plan.left {
        ui.info(&format!("{}: left by path, as {left}", plan.repo.name));
    }
    Ok(())
}

/// Removes the named crates' patches from a repository, and brings its Cargo.lock up to date without them.
pub fn remove_links(repo: &Repo, names: &[String], ui: &mut dyn Ui) -> Result<()> {
    links::remove(&repo.dir, names)?;
    let ran = process::run(&repo.dir, "cargo", &["metadata", "--format-version", "1"])?;
    if ran.success {
        ui.info(&format!(
            "{}: unlinked {}, built from crates.io",
            repo.name,
            names.join(", ")
        ));
    } else {
        ui.info(&format!(
            "{}: unlinked {}, but Cargo cannot resolve its dependencies: {}",
            repo.name,
            names.join(", "),
            ran.tail(3)
        ));
    }
    Ok(())
}
