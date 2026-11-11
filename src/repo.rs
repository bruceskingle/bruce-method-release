//! A repository's checkout, and the checkouts next to it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::manifest::Manifests;
use crate::process;
use crate::version::Version;

/// A repository's checkout, whose directory has the repository's name, next to the other repositories' checkouts.
#[derive(Clone, Debug)]
pub struct Repo {
    pub dir: PathBuf,
    pub name: String,
}

impl Repo {
    /// The repository whose checkout a directory is in.
    pub fn containing(dir: &Path) -> Result<Repo> {
        let top = process::output(dir, "git", &["rev-parse", "--show-toplevel"])
            .with_context(|| format!("{} is not in a git checkout", dir.display()))?;
        Ok(Repo::at(PathBuf::from(top)))
    }

    /// The repository checked out in a directory.
    pub fn at(dir: PathBuf) -> Repo {
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Repo { dir, name }
    }

    /// The directory the checkouts are in, side by side.
    pub fn parent(&self) -> &Path {
        self.dir.parent().unwrap_or(Path::new("/"))
    }

    /// The directory a repository is, or would be, checked out in next to this one.
    pub fn sibling_dir(&self, name: &str) -> PathBuf {
        self.parent().join(name)
    }

    /// The checkouts next to this one, of Rust repositories, not including this one.
    pub fn siblings(&self) -> Result<Vec<Repo>> {
        let mut siblings = Vec::new();
        for entry in
            fs::read_dir(self.parent()).with_context(|| format!("could not list {}", self.parent().display()))?
        {
            let dir = entry?.path();
            if dir != self.dir && dir.join(".git").exists() && dir.join("Cargo.toml").exists() {
                siblings.push(Repo::at(dir));
            }
        }
        siblings.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(siblings)
    }

    /// Runs git in the checkout, returning what it wrote.
    pub fn git(&self, args: &[&str]) -> Result<String> {
        process::output(&self.dir, "git", args)
    }

    /// The crates of the repository.
    pub fn manifests(&self) -> Result<Manifests> {
        Manifests::read(&self.dir).with_context(|| format!("{}: could not read its Cargo.toml", self.name))
    }

    /// The branch checked out, or None if none is, e.g. a tag.
    pub fn branch(&self) -> Result<Option<String>> {
        let branch = self.git(&["branch", "--show-current"])?;
        Ok((!branch.is_empty()).then_some(branch))
    }

    /// The files changed and not committed, as `git status --short` lists them.
    pub fn uncommitted(&self) -> Result<Vec<String>> {
        Ok(self
            .git(&["status", "--porcelain"])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// How many commits the branch is ahead of, and behind, the one it tracks on GitHub, as last fetched, or None if
    /// it tracks none.
    pub fn ahead_behind(&self) -> Result<Option<(u32, u32)>> {
        let ran = process::run(
            &self.dir,
            "git",
            &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        )?;
        if !ran.success {
            return Ok(None);
        }
        let mut counts = ran.stdout.split_whitespace().map(|count| count.parse().unwrap_or(0));
        Ok(Some((counts.next().unwrap_or(0), counts.next().unwrap_or(0))))
    }

    /// Fetches from GitHub, with the tags.
    pub fn fetch(&self) -> Result<()> {
        self.git(&["fetch", "--quiet", "--tags", "origin"]).map(|_| ())
    }

    /// Whether a ref, e.g. `refs/remotes/origin/v0.2.0`, exists, as last fetched.
    pub fn has_ref(&self, name: &str) -> Result<bool> {
        Ok(process::run(&self.dir, "git", &["rev-parse", "--quiet", "--verify", name])?.success)
    }

    /// Whether a tag is on GitHub, asking GitHub rather than relying on the last fetch.
    pub fn remote_tag_exists(&self, tag: &str) -> Result<bool> {
        Ok(!self
            .git(&["ls-remote", "--tags", "origin", &format!("refs/tags/{tag}")])?
            .is_empty())
    }

    /// The commit a ref names.
    pub fn commit(&self, name: &str) -> Result<String> {
        self.git(&["rev-parse", &format!("{name}^{{commit}}")])
    }

    /// The versions released, from the `releaseX.Y.Z` tags, oldest first.
    pub fn releases(&self) -> Result<Vec<Version>> {
        let mut versions: Vec<Version> = self
            .git(&["tag", "--list", "release*"])?
            .lines()
            .filter_map(Version::from_tag)
            .collect();
        versions.sort();
        Ok(versions)
    }

    /// The development branches on GitHub, as last fetched, oldest first.
    pub fn remote_branches(&self) -> Result<Vec<Version>> {
        let mut versions: Vec<Version> = self
            .git(&["for-each-ref", "--format=%(refname:strip=3)", "refs/remotes/origin/v*"])?
            .lines()
            .filter_map(Version::from_branch)
            .collect();
        versions.sort();
        Ok(versions)
    }

    /// The URL of the repository on GitHub.
    pub fn origin_url(&self) -> Result<String> {
        self.git(&["remote", "get-url", "origin"])
    }

    /// The URL another repository of the same owner would have, e.g. for cloning it next to this one.
    pub fn sibling_url(&self, name: &str) -> Result<String> {
        let url = self.origin_url()?;
        let suffix = if url.ends_with(".git") { ".git" } else { "" };
        let base = url.trim_end_matches(".git");
        match base.rfind(['/', ':']) {
            Some(index) => Ok(format!("{}{name}{suffix}", &base[..=index])),
            None => bail!("{}: cannot tell another repository's URL from {url}", self.name),
        }
    }

    /// Checks that nothing is changed and not committed.
    pub fn require_clean(&self) -> Result<()> {
        let uncommitted = self.uncommitted()?;
        if !uncommitted.is_empty() {
            bail!(
                "{} has changes not committed, commit or remove them first:\n{}",
                self.name,
                uncommitted.join("\n")
            );
        }
        Ok(())
    }
}

/// Whether a version of a crate is on crates.io, asking it with `cargo info`, from a directory outside any checkout so
/// that no patch or lock file stands in for it.
pub fn is_published(name: &str, version: Version) -> Result<bool> {
    let ran = process::run(
        &std::env::temp_dir(),
        "cargo",
        &["info", "--quiet", &format!("{name}@{version}")],
    )?;
    if ran.success {
        return Ok(true);
    }
    if ran.stderr.contains("could not find") {
        return Ok(false);
    }
    bail!("could not ask crates.io for {name} {version}: {}", ran.tail(5))
}
