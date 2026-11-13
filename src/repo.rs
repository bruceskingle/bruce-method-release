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

    /// The checkouts next to this one, of Rust repositories, not including this one: those with a Cargo.toml, at their
    /// root or below it, as a repository of crates which are each their own workspace has none at its root.
    pub fn siblings(&self) -> Result<Vec<Repo>> {
        let mut siblings = Vec::new();
        for entry in
            fs::read_dir(self.parent()).with_context(|| format!("could not list {}", self.parent().display()))?
        {
            let dir = entry?.path();
            if dir == self.dir || !dir.join(".git").exists() {
                continue;
            }
            let sibling = Repo::at(dir);
            if sibling.dir.join("Cargo.toml").exists() || !sibling.tracked_manifests()?.is_empty() {
                siblings.push(sibling);
            }
        }
        siblings.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(siblings)
    }

    /// Runs git in the checkout, returning what it wrote.
    pub fn git(&self, args: &[&str]) -> Result<String> {
        process::output(&self.dir, "git", args)
    }

    /// Whether the repository is one workspace, with a Cargo.toml at its root, rather than separate crates, each its
    /// own workspace, e.g. a platform's library and its boards, each built for its own chip by its own
    /// `.cargo/config.toml`, which Cargo reads only when run in the crate's directory.
    pub fn is_workspace(&self) -> bool {
        self.dir.join("Cargo.toml").exists()
    }

    /// The crates of the repository: its workspace's, or, if it has separate crates, every crate git tracks.
    pub fn manifests(&self) -> Result<Manifests> {
        if self.is_workspace() {
            return Manifests::read(&self.dir).with_context(|| format!("{}: could not read its Cargo.toml", self.name));
        }
        let (manifests, unreadable) = self.all_manifests()?;
        if !unreadable.is_empty() {
            bail!("{}: could not read {}", self.name, unreadable.join("; "));
        }
        if manifests.crates.is_empty() {
            bail!("{}: has no Cargo.toml", self.name);
        }
        Ok(manifests)
    }

    /// The directories Cargo is run in for every crate of the repository, each with its own Cargo.lock: the root, for
    /// a workspace, or else each crate's.
    pub fn cargo_dirs(&self, manifests: &Manifests) -> Vec<PathBuf> {
        if self.is_workspace() {
            vec![self.dir.clone()]
        } else {
            manifests.crates.iter().map(|krate| krate.dir().to_path_buf()).collect()
        }
    }

    /// A directory of the checkout as it is named in messages, relative to the checkout.
    pub fn relative(&self, dir: &Path) -> String {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let root = self.dir.canonicalize().unwrap_or_else(|_| self.dir.clone());
        match dir.strip_prefix(&root) {
            Ok(relative) if relative.as_os_str().is_empty() => ".".to_string(),
            Ok(relative) => relative.display().to_string(),
            Err(_) => dir.display().to_string(),
        }
    }

    /// Whether the repository is developed and released by the method: it is on a version's branch, e.g. v0.2.0, or
    /// has a release's tag, e.g. release0.1.0, so that a checkout of someone else's crate next to it, e.g. a fork being
    /// tried, is not taken for one of the libraries to release.
    pub fn follows_method(&self) -> Result<bool> {
        if self.branch()?.as_deref().and_then(Version::from_branch).is_some() {
            return Ok(true);
        }
        Ok(!self.releases()?.is_empty())
    }

    /// The Cargo.toml files git tracks in the checkout, wherever they are, so that none in a directory git ignores,
    /// e.g. a build's, is taken for one of the repository's.
    pub fn tracked_manifests(&self) -> Result<Vec<PathBuf>> {
        Ok(self
            .git(&["ls-files", "--", "Cargo.toml", "*/Cargo.toml"])?
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| self.dir.join(line))
            .collect())
    }

    /// Every crate of the repository, in its workspace or not, see Manifests::read_all, and the Cargo.toml files which
    /// could not be read, and why.
    pub fn all_manifests(&self) -> Result<(Manifests, Vec<String>)> {
        Manifests::read_all(&self.dir, &self.tracked_manifests()?)
            .with_context(|| format!("{}: could not read its Cargo.toml files", self.name))
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
