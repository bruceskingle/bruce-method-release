//! `dependencies`: the libraries next to a repository which it depends on, directly or through one another, in the
//! order they are to be released, and which of them need releasing for it to be built from crates.io alone.
//!
//! Only the repositories next to it which follow the method are libraries, see Repo::follows_method, so that a
//! checkout of someone else's crate is not taken for one. It changes nothing, and asks crates.io only about the
//! versions the repositories require of each other.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::links;
use crate::manifest::Manifests;
use crate::repo::{self, Repo};
use crate::ui::Ui;
use crate::version::Version;

/// How a crate takes a library's crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum How {
    /// From the library's checkout, by a path in its Cargo.toml.
    Path,
    /// From the library's checkout, by a link, a patch in its git-ignored `.cargo/config.toml`, see the README.
    Link,
    /// From crates.io.
    CratesIo,
}

impl How {
    pub fn describe(&self) -> &'static str {
        match self {
            How::Path => "by path",
            How::Link => "linked",
            How::CratesIo => "from crates.io",
        }
    }
}

/// One crate's dependency on a crate of a library.
#[derive(Clone, Debug)]
pub struct Use {
    /// The repository of the crate which depends on the library.
    pub user_repo: String,
    pub user_crate: String,
    /// The library's crate depended on.
    pub library_crate: String,
    /// The version required, as Cargo has it, `*` for a path alone.
    pub requirement: String,
    pub how: How,
    /// The version which must be on crates.io for the requirement to be met from there: the lowest it accepts, or for
    /// a path alone, the latest released, or else the checkout's.
    pub needs: Option<Version>,
    /// Whether that version is on crates.io, None if crates.io could not be asked.
    pub published: Option<bool>,
}

/// What is to be done with a library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Its checkout's version is needed and not on crates.io: release it.
    Release(Version),
    /// Every version needed is on crates.io, though some users may still take it from its checkout.
    Published,
    /// A version is needed which is not on crates.io, and is not the checkout's, so releasing the checkout would not
    /// meet it: why.
    Mismatch(String),
}

/// A library next to the repository, which it depends on directly or through other libraries.
#[derive(Clone, Debug)]
pub struct Library {
    pub repo: Repo,
    /// The version of its publishable crates in the checkout, or why it has none.
    pub version: Result<Version, String>,
    pub latest_release: Option<Version>,
    pub uses: Vec<Use>,
    pub verdict: Verdict,
    /// Anything which would stop `release` releasing it, e.g. a Cargo.toml it cannot read.
    pub problems: Vec<String>,
}

impl Library {
    /// Whether any user takes it from its checkout, by path or by link, when the version it needs is on crates.io,
    /// so that the user could be changed to take it from there, by `unlink`.
    pub fn taken_from_checkout(&self) -> bool {
        !self.users_from_checkout().is_empty()
    }

    /// The repositories which take it from its checkout when the version they need is on crates.io, see
    /// taken_from_checkout.
    pub fn users_from_checkout(&self) -> Vec<&str> {
        let mut users: Vec<&str> = self
            .uses
            .iter()
            .filter(|each| each.how != How::CratesIo && each.published == Some(true))
            .map(|each| each.user_repo.as_str())
            .collect();
        users.sort();
        users.dedup();
        users
    }

    /// The repositories which need a version of it which is not on crates.io.
    pub fn waiting(&self) -> Vec<&str> {
        let mut users: Vec<&str> = self
            .uses
            .iter()
            .filter(|each| each.published == Some(false))
            .map(|each| each.user_repo.as_str())
            .collect();
        users.sort();
        users.dedup();
        users
    }

    /// Whether nothing is to be done with it: every version needed is on crates.io, taken from there, and release can
    /// read it.
    pub fn ready(&self) -> bool {
        self.verdict == Verdict::Published && !self.taken_from_checkout()
    }

    /// What is to be done with it, in a few words, e.g. "release 0.1.0, which bruce-smart-home needs".
    pub fn summary(&self) -> String {
        if self.version.is_err() && self.verdict != Verdict::Published {
            return "its crates have different versions, see check there".to_string();
        }
        match &self.verdict {
            Verdict::Release(version) => {
                let mut summary = format!("release {version}, which {} needs", self.waiting().join(", "));
                if let Some(problem) = self.problems.first() {
                    summary.push_str(&format!(", but {problem}"));
                }
                summary
            }
            Verdict::Mismatch(why) => match self.problems.first() {
                Some(problem) => format!("{why}; {problem}"),
                None => why.clone(),
            },
            Verdict::Published if self.taken_from_checkout() => format!(
                "{} take it by path though it is on crates.io: run unlink {} there",
                self.users_from_checkout().join(", "),
                self.repo.name
            ),
            Verdict::Published => "ready".to_string(),
        }
    }
}

/// The libraries a repository depends on, in the order to release them, each before those which use it.
#[derive(Clone, Debug)]
pub struct Dependencies {
    pub repo: String,
    pub libraries: Vec<Library>,
    /// Libraries which depend on each other in a circle, so that no order releases them, if any do.
    pub circular: Vec<String>,
    /// Cargo.toml files which could not be read, and why.
    pub unreadable: Vec<String>,
}

/// The libraries next to `repo` which it depends on, see the module documentation.
pub fn dependencies(repo: &Repo, ui: &mut dyn Ui) -> Result<Dependencies> {
    // Which repository next to this one each crate is in, by its name.
    let mut owners: HashMap<String, Repo> = HashMap::new();
    let mut all: HashMap<String, Manifests> = HashMap::new();
    let mut unreadable = Vec::new();
    for sibling in repo.siblings()? {
        if !sibling.follows_method()? {
            continue;
        }
        let (manifests, problems) = sibling.all_manifests()?;
        unreadable.extend(problems);
        for krate in manifests.crates.iter().filter(|krate| krate.publishable) {
            owners.insert(krate.name.clone(), sibling.clone());
        }
        all.insert(sibling.name.clone(), manifests);
    }
    let (own, problems) = repo.all_manifests()?;
    unreadable.extend(problems);
    all.insert(repo.name.clone(), own);

    // From the repository, the libraries it uses, and theirs in turn, with which uses which.
    let mut uses: Vec<Use> = Vec::new();
    let mut found: Vec<Repo> = Vec::new();
    let mut queue: Vec<Repo> = vec![repo.clone()];
    let mut seen: HashSet<String> = HashSet::from([repo.name.clone()]);
    let mut published_cache: HashMap<(String, Version), Option<bool>> = HashMap::new();
    while let Some(user) = queue.pop() {
        let Some(manifests) = all.get(&user.name) else { continue };
        let patched: Vec<String> = links::read(&user.dir)?.into_iter().map(|patch| patch.name).collect();
        for krate in &manifests.crates {
            for dependency in &krate.dependencies {
                let Some(owner) = owners.get(&dependency.name) else {
                    continue;
                };
                if owner.name == user.name {
                    continue;
                }
                let how = if dependency.by_path {
                    How::Path
                } else if patched.contains(&dependency.name) {
                    How::Link
                } else {
                    How::CratesIo
                };
                // A path alone needs a version on crates.io to change to: the latest released, or else the checkout's.
                let checkout = all
                    .get(&owner.name)
                    .and_then(|manifests| manifests.find(&dependency.name))
                    .map(|krate| krate.version);
                let needs = if dependency.requirement == "*" {
                    owner.releases()?.last().copied().or(checkout)
                } else {
                    Version::lowest(&dependency.requirement)
                };
                let published = match needs {
                    Some(needs) => {
                        let key = (dependency.name.clone(), needs);
                        if !published_cache.contains_key(&key) {
                            ui.info(&format!("Asking crates.io for {} {needs}", dependency.name));
                            let answer = repo::is_published(&dependency.name, needs).ok();
                            published_cache.insert(key.clone(), answer);
                        }
                        published_cache[&key]
                    }
                    None => None,
                };
                uses.push(Use {
                    user_repo: user.name.clone(),
                    user_crate: krate.name.clone(),
                    library_crate: dependency.name.clone(),
                    requirement: dependency.requirement.clone(),
                    how,
                    needs,
                    published,
                });
                if seen.insert(owner.name.clone()) {
                    found.push(owner.clone());
                    queue.push(owner.clone());
                }
            }
        }
    }

    let owner_of = |name: &str| owners.get(name).map(|owner| owner.name.clone());
    let mut libraries: Vec<Library> = Vec::new();
    for library in found {
        let library_uses: Vec<Use> = uses
            .iter()
            .filter(|each| owner_of(&each.library_crate).as_deref() == Some(library.name.as_str()))
            .cloned()
            .collect();
        let version = all
            .get(&library.name)
            .map(|manifests| Manifests {
                crates: manifests
                    .crates
                    .iter()
                    .filter(|krate| krate.publishable)
                    .cloned()
                    .collect(),
                kind: manifests.kind,
            })
            .ok_or_else(|| "it could not be read".to_string())
            .and_then(|publishable| publishable.version().map_err(|error| format!("{error:#}")));
        let mut problems = Vec::new();
        if let Err(error) = library.manifests().and_then(|manifests| manifests.version()) {
            let error = format!("{error:#}");
            problems.push(format!("release cannot read it: {error}"));
        }
        let verdict = verdict(&version, &library_uses);
        libraries.push(Library {
            latest_release: library.releases()?.last().copied(),
            repo: library,
            version,
            uses: library_uses,
            verdict,
            problems,
        });
    }

    let (libraries, circular) = in_release_order(libraries);
    Ok(Dependencies {
        repo: repo.name.clone(),
        libraries,
        circular,
        unreadable,
    })
}

/// What is to be done with a library, from its checkout's version and how it is used.
fn verdict(version: &Result<Version, String>, uses: &[Use]) -> Verdict {
    let unpublished: Vec<&Use> = uses.iter().filter(|each| each.published == Some(false)).collect();
    if unpublished.is_empty() {
        return Verdict::Published;
    }
    let Ok(version) = version else {
        return Verdict::Mismatch(format!(
            "{} needed, and its checkout has no one version",
            needed(&unpublished)
        ));
    };
    let met = unpublished
        .iter()
        .all(|each| each.needs == Some(*version) || version.satisfies(&each.requirement) == Some(true));
    if met {
        Verdict::Release(*version)
    } else {
        Verdict::Mismatch(format!(
            "{} needed, which its checkout, {version}, is not",
            needed(&unpublished)
        ))
    }
}

/// The versions needed by uses, e.g. "0.2.0, 0.3.0".
fn needed(uses: &[&Use]) -> String {
    let mut versions: Vec<String> = uses
        .iter()
        .filter_map(|each| each.needs.map(|needs| needs.to_string()))
        .collect();
    versions.sort();
    versions.dedup();
    versions.join(", ")
}

/// The libraries ordered so that each comes before those which use it, by name where the order is free, and the names
/// of any which use each other in a circle, which are put last.
fn in_release_order(mut libraries: Vec<Library>) -> (Vec<Library>, Vec<String>) {
    libraries.sort_by(|a, b| a.repo.name.cmp(&b.repo.name));
    // For each library, the libraries it uses.
    let uses_of: HashMap<String, HashSet<String>> = libraries
        .iter()
        .map(|library| {
            let used: HashSet<String> = libraries
                .iter()
                .filter(|other| {
                    other.repo.name != library.repo.name
                        && other.uses.iter().any(|each| each.user_repo == library.repo.name)
                })
                .map(|other| other.repo.name.clone())
                .collect();
            (library.repo.name.clone(), used)
        })
        .collect();
    let mut ordered: Vec<Library> = Vec::new();
    let mut placed: HashSet<String> = HashSet::new();
    while !libraries.is_empty() {
        let ready = libraries
            .iter()
            .position(|library| uses_of[&library.repo.name].iter().all(|used| placed.contains(used)));
        match ready {
            Some(index) => {
                let library = libraries.remove(index);
                placed.insert(library.repo.name.clone());
                ordered.push(library);
            }
            None => break,
        }
    }
    let circular: Vec<String> = libraries.iter().map(|library| library.repo.name.clone()).collect();
    ordered.extend(libraries);
    (ordered, circular)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(name: &str, users: &[&str]) -> Library {
        Library {
            repo: Repo::at(std::path::PathBuf::from(format!("/git/{name}"))),
            version: Ok("0.1.0".parse().unwrap()),
            latest_release: None,
            uses: users
                .iter()
                .map(|user| Use {
                    user_repo: user.to_string(),
                    user_crate: user.to_string(),
                    library_crate: name.to_string(),
                    requirement: "^0.1.0".to_string(),
                    how: How::Path,
                    needs: Some("0.1.0".parse().unwrap()),
                    published: Some(false),
                })
                .collect(),
            verdict: Verdict::Published,
            problems: Vec::new(),
        }
    }

    #[test]
    fn orders_each_library_before_its_users() {
        // app uses esp and flash; esp uses std and record; flash uses record.
        let libraries = vec![
            library("esp", &["app"]),
            library("flash", &["app"]),
            library("record", &["esp", "flash"]),
            library("std", &["esp"]),
        ];
        let (ordered, circular) = in_release_order(libraries);
        let names: Vec<&str> = ordered.iter().map(|library| library.repo.name.as_str()).collect();
        assert_eq!(names, vec!["record", "flash", "std", "esp"]);
        assert!(circular.is_empty());
    }

    #[test]
    fn puts_libraries_in_a_circle_last() {
        let (ordered, circular) = in_release_order(vec![library("a", &["b"]), library("b", &["a"]), library("c", &[])]);
        assert_eq!(ordered[0].repo.name, "c");
        assert_eq!(circular, vec!["a", "b"]);
    }

    #[test]
    fn releases_the_checkout_only_when_it_is_the_version_needed() {
        let uses = library("lib", &["app"]).uses;
        assert_eq!(
            verdict(&Ok("0.1.0".parse().unwrap()), &uses),
            Verdict::Release("0.1.0".parse().unwrap())
        );
        assert!(matches!(
            verdict(&Ok("0.2.0".parse().unwrap()), &uses),
            Verdict::Mismatch(_)
        ));
        let mut published = uses.clone();
        published[0].published = Some(true);
        assert_eq!(verdict(&Ok("0.2.0".parse().unwrap()), &published), Verdict::Published);
    }

    #[test]
    fn a_path_alone_needs_the_version_it_is_given() {
        let mut uses = library("lib", &["app"]).uses;
        uses[0].requirement = "*".to_string();
        uses[0].needs = Some("0.2.0".parse().unwrap());
        assert_eq!(
            verdict(&Ok("0.2.0".parse().unwrap()), &uses),
            Verdict::Release("0.2.0".parse().unwrap())
        );
    }
}
