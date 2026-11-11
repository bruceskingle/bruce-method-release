//! `sync`: bringing a checkout, and the libraries it links, up to date with GitHub, e.g. on a second computer.

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::commands::link::{self, LinkPlan};
use crate::links;
use crate::repo::{self, Repo};
use crate::ui::Ui;
use crate::version::Version;

/// One checkout's part of a sync.
struct Update {
    repo: Repo,
    /// The branch to switch to first, if any.
    switch: Option<String>,
    /// The commits to fast forward by.
    behind: u32,
}

/// Brings a checkout and the libraries it links up to date with GitHub, by fast forward only, so that commits made on
/// this computer are never lost or merged by accident, optionally switching the checkout to another branch first,
/// then links the libraries next to it whose versions it requires are not published.
pub fn sync(repo: &Repo, branch: Option<&str>, ui: &mut dyn Ui) -> Result<()> {
    let mut repos = vec![repo.clone()];
    for patch in links::read(&repo.dir)? {
        let dir = patch.dir(&repo.dir);
        if dir.exists()
            && let Ok(library) = Repo::containing(&dir)
            && !repos.iter().any(|known| known.dir == library.dir)
        {
            repos.push(library);
        }
    }

    let mut updates = Vec::new();
    for (index, each) in repos.iter().enumerate() {
        each.require_clean()?;
        ui.info(&format!("Fetching {}", each.name));
        each.fetch()?;
        let switch = match branch {
            Some(branch) if index == 0 && each.branch()?.as_deref() != Some(branch) => {
                if !each.has_ref(&format!("refs/heads/{branch}"))?
                    && !each.has_ref(&format!("refs/remotes/origin/{branch}"))?
                {
                    bail!("{} has no branch {branch}, here or on GitHub", each.name);
                }
                Some(branch.to_string())
            }
            _ => None,
        };
        let behind = if switch.is_some() {
            0
        } else {
            match each.ahead_behind()? {
                Some((0, behind)) => behind,
                Some((ahead, 0)) => {
                    ui.info(&format!(
                        "{} has {ahead} commits not pushed, which sync leaves alone",
                        each.name
                    ));
                    0
                }
                Some((ahead, behind)) => bail!(
                    "{}'s branch has {ahead} commits here and {behind} on GitHub which are not on the other: merge or rebase it by hand",
                    each.name
                ),
                None => 0,
            }
        };
        if switch.is_some() || behind > 0 {
            updates.push(Update {
                repo: each.clone(),
                switch,
                behind,
            });
        }
    }

    let links = needed_links(repo)?;

    let mut steps = Vec::new();
    for update in &updates {
        if let Some(branch) = &update.switch {
            steps.push(format!(
                "Switch {} to {branch}, up to date with GitHub",
                update.repo.name
            ));
        }
        if update.behind > 0 {
            steps.push(format!(
                "Fast forward {} by {} commits from GitHub",
                update.repo.name, update.behind
            ));
        }
    }
    for plan in &links {
        steps.extend(plan.steps(repo));
    }
    if steps.is_empty() {
        ui.info("Up to date with GitHub, nothing to do");
        return Ok(());
    }
    if !ui.confirm(&format!("Sync {}", repo.name), &steps)? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    for update in &updates {
        if let Some(branch) = &update.switch {
            update.repo.git(&["switch", "--quiet", branch])?;
            if let Some((0, behind)) = update.repo.ahead_behind()?
                && behind > 0
            {
                update.repo.git(&["merge", "--quiet", "--ff-only", "@{upstream}"])?;
            }
        } else {
            update.repo.git(&["merge", "--quiet", "--ff-only", "@{upstream}"])?;
        }
        ui.info(&format!(
            "{}: on {}",
            update.repo.name,
            update.repo.git(&["log", "-1", "--format=%h %s"])?
        ));
    }
    for plan in links {
        link::apply_link(repo, plan, ui)?;
    }
    Ok(())
}

/// The links a checkout needs: to each library next to it, not already linked, of which it requires a version which
/// is not on crates.io, as on a second computer, where the developer's own links are not.
///
/// Only the crates of the repositories next to it are considered, so that crates.io is asked about few of its
/// dependencies.
fn needed_links(repo: &Repo) -> Result<Vec<LinkPlan>> {
    let mut owners: HashMap<String, String> = HashMap::new();
    for sibling in repo.siblings()? {
        if let Ok(manifests) = sibling.manifests() {
            for krate in manifests.crates.into_iter().filter(|krate| krate.publishable) {
                owners.insert(krate.name, sibling.name.clone());
            }
        }
    }
    let patched: Vec<String> = links::read(&repo.dir)?.into_iter().map(|patch| patch.name).collect();
    let manifests = repo.manifests()?;
    // Each library with the version required of it, whose branch its checkout is put on.
    let mut libraries: Vec<(String, Version)> = Vec::new();
    for (name, library) in &owners {
        if patched.contains(name) || libraries.iter().any(|(known, _)| known == library) {
            continue;
        }
        for requirement in manifests.requirements_of(name) {
            if let Some(version) = Version::lowest(&requirement)
                && !repo::is_published(name, version)?
            {
                libraries.push((library.clone(), version));
                break;
            }
        }
    }
    libraries.sort();
    libraries
        .iter()
        .map(|(library, version)| link::plan_link(repo, library, Some(*version)))
        .collect()
}
