//! `status`: where a repository, and the libraries it links, are.

use anyhow::Result;

use crate::commands::link::{self, Link};
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
    let mut statuses = vec![status_of(repo, links)?];
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
    })
}
