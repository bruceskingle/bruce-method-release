//! `status`: where a repository, and the libraries it links, are.

use anyhow::Result;

use crate::commands::link::{self, Link};
use crate::github::{self, ReleaseState, Run};
use crate::manifest::{self, Kind};
use crate::repo::Repo;
use crate::version::Version;

/// Where a repository is.
pub struct RepoStatus {
    pub name: String,
    pub branch: Option<String>,
    /// Its version, or why it has none, e.g. its crates' versions differ.
    pub version: Result<Version, String>,
    pub uncommitted: usize,
    /// Commits ahead of, and behind, the branch it tracks on GitHub, as last fetched.
    pub ahead_behind: Option<(u32, u32)>,
    pub latest_release: Option<Version>,
    pub links: Vec<Link>,
    /// For an app, where the release of its version is, from GitHub.
    pub app: Option<AppStatus>,
}

/// Where the release of an app's version is.
pub struct AppStatus {
    /// The commit of the `testX.Y.Z` tag, last tried, if there is one.
    pub tried: Option<String>,
    /// The commit of the `releaseX.Y.Z` tag, if it is tagged.
    pub tagged: Option<String>,
    pub release: ReleaseState,
    /// The workflow's latest runs, newest first.
    pub runs: Vec<Run>,
}

/// The status of a repository and of the libraries it links, without fetching.
pub fn status(repo: &Repo) -> Result<Vec<RepoStatus>> {
    let links = link::links_of(repo)?;
    let mut libraries: Vec<Repo> = Vec::new();
    for link in &links {
        if let Some(name) = &link.library
            && !libraries.iter().any(|library| &library.name == name)
        {
            libraries.push(Repo::at(repo.sibling_dir(name)));
        }
    }
    let mut first = status_of(repo, links)?;
    if let Ok(version) = &first.version
        && repo.manifests()?.kind == Kind::App
    {
        first.app = Some(app_status(repo, *version)?);
    }
    let mut statuses = vec![first];
    for library in libraries {
        let links = link::links_of(&library)?;
        statuses.push(status_of(&library, links)?);
    }
    Ok(statuses)
}

fn status_of(repo: &Repo, links: Vec<Link>) -> Result<RepoStatus> {
    Ok(RepoStatus {
        name: repo.name.clone(),
        branch: repo.branch()?,
        version: repo
            .manifests()
            .and_then(|manifests| manifests.version())
            .map_err(|error| format!("{error:#}")),
        uncommitted: repo.uncommitted()?.len(),
        ahead_behind: repo.ahead_behind()?,
        latest_release: repo.releases()?.last().copied(),
        links,
        app: None,
    })
}

/// Where the release of an app's version is, asking GitHub, as the tags and runs are made there.
fn app_status(repo: &Repo, version: Version) -> Result<AppStatus> {
    let settings = manifest::app_settings(&repo.dir)?;
    let commit_of = |tag: String| -> Result<Option<String>> {
        let line = repo.git(&[
            "ls-remote",
            "--tags",
            "origin",
            &format!("refs/tags/{tag}^{{}}"),
            &format!("refs/tags/{tag}"),
        ])?;
        // An annotated tag's commit is on its ^{} line, which comes after the tag's own.
        Ok(line.lines().last().and_then(|line| line.get(..7)).map(str::to_string))
    };
    Ok(AppStatus {
        tried: commit_of(version.test_tag())?,
        tagged: commit_of(version.tag())?,
        release: github::release_state(repo, &version.tag())?,
        runs: github::runs(repo, &settings.workflow, None, 3)?,
    })
}
