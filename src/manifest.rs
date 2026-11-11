//! What a repository's Cargo.toml files say, from `cargo metadata`, and changes to them which keep their layout and
//! comments.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use toml_edit::{DocumentMut, Item, TableLike, Value};

use crate::process;
use crate::version::Version;

/// What a repository releases, from `kind` in `[package.metadata.bruce-method]` or `[workspace.metadata.bruce-method]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Crates published to crates.io, the default.
    Crate,
    /// An app, whose installers are built by a GitHub workflow.
    App,
}

/// A crate of a repository.
#[derive(Clone, Debug)]
pub struct Crate {
    pub name: String,
    pub version: Version,
    /// The crate's Cargo.toml.
    pub manifest: PathBuf,
    /// Whether it may be published to crates.io, false for `publish = false`.
    pub publishable: bool,
    pub dependencies: Vec<Dependency>,
}

impl Crate {
    /// The directory the crate is in.
    pub fn dir(&self) -> &Path {
        self.manifest.parent().unwrap_or(Path::new("."))
    }
}

/// A dependency of a crate, by the name of the crate depended on, not any name it is renamed to.
#[derive(Clone, Debug)]
pub struct Dependency {
    pub name: String,
    /// The version required, as Cargo has it, e.g. `^0.1.0`.
    pub requirement: String,
    /// Whether it is a path dependency, e.g. on another crate of the same workspace.
    pub by_path: bool,
}

/// The crates of a repository, and what it releases.
#[derive(Clone, Debug)]
pub struct Manifests {
    pub crates: Vec<Crate>,
    pub kind: Kind,
}

impl Manifests {
    /// Reads the crates of the workspace whose root is in `dir`, without resolving any dependencies, so that it works
    /// whether or not they can be found.
    pub fn read(dir: &Path) -> Result<Manifests> {
        let json = process::output(dir, "cargo", &["metadata", "--format-version", "1", "--no-deps"])?;
        let metadata: Metadata = serde_json::from_str(&json).context("could not read cargo metadata's output")?;
        let kind = kind_of(&metadata.metadata)
            .or_else(|| metadata.packages.iter().find_map(|package| kind_of(&package.metadata)))
            .transpose()?
            .unwrap_or(Kind::Crate);
        let crates = metadata
            .packages
            .into_iter()
            .map(|package| {
                Ok(Crate {
                    version: package
                        .version
                        .parse()
                        .map_err(|error| anyhow!("crate {}: {error}", package.name))?,
                    publishable: package.publish.is_none_or(|registries| !registries.is_empty()),
                    dependencies: package
                        .dependencies
                        .into_iter()
                        .map(|dependency| Dependency {
                            name: dependency.name,
                            requirement: dependency.req,
                            by_path: dependency.path.is_some(),
                        })
                        .collect(),
                    name: package.name,
                    manifest: package.manifest_path,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Manifests { crates, kind })
    }

    /// The repository's version, which every one of its crates has.
    pub fn version(&self) -> Result<Version> {
        let first = self.crates.first().ok_or_else(|| anyhow!("there are no crates"))?;
        let differ: Vec<String> = self
            .crates
            .iter()
            .filter(|krate| krate.version != first.version)
            .map(|krate| format!("{} is {}", krate.name, krate.version))
            .collect();
        if !differ.is_empty() {
            bail!(
                "every crate should be version {}, as {} is, but {}",
                first.version,
                first.name,
                differ.join(", ")
            );
        }
        Ok(first.version)
    }

    /// The crate of this name, if the repository has one.
    pub fn find(&self, name: &str) -> Option<&Crate> {
        self.crates.iter().find(|krate| krate.name == name)
    }

    /// The versions of a crate required by this repository's crates from crates.io, not by path.
    pub fn requirements_of(&self, name: &str) -> Vec<String> {
        let mut requirements: Vec<String> = self
            .crates
            .iter()
            .flat_map(|krate| &krate.dependencies)
            .filter(|dependency| dependency.name == name && !dependency.by_path)
            .map(|dependency| dependency.requirement.clone())
            .collect();
        requirements.sort();
        requirements.dedup();
        requirements
    }

    /// Every Cargo.toml of the repository: the crates', and the workspace root's, if it is not also a crate's.
    pub fn manifest_files(&self, root: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self.crates.iter().map(|krate| krate.manifest.clone()).collect();
        let root_manifest = root.join("Cargo.toml");
        if root_manifest.exists() && !files.iter().any(|file| same_file(file, &root_manifest)) {
            files.insert(0, root_manifest);
        }
        files
    }
}

/// Whether two paths name the same file, allowing for one being through a symbolic link, e.g. macOS's /tmp.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    /// The workspace's metadata.
    metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    manifest_path: PathBuf,
    /// None for any registry, an empty list for `publish = false`.
    publish: Option<Vec<String>>,
    dependencies: Vec<PackageDependency>,
    metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct PackageDependency {
    name: String,
    req: String,
    path: Option<PathBuf>,
}

/// The kind of release from a `bruce-method` metadata table, if there is one.
fn kind_of(metadata: &Option<serde_json::Value>) -> Option<Result<Kind>> {
    let kind = metadata.as_ref()?.get("bruce-method")?.get("kind")?;
    Some(match kind.as_str() {
        Some("crate") => Ok(Kind::Crate),
        Some("app") => Ok(Kind::App),
        _ => Err(anyhow!(
            "bruce-method's kind should be \"crate\" or \"app\", not {kind}"
        )),
    })
}

/// A change made to a Cargo.toml, for saying what was done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub file: PathBuf,
    pub what: String,
}

/// Sets the version of the crate, or of the workspace's crates, which a Cargo.toml's text gives, and returns the new
/// text, or None if it gives no version of its own, e.g. `version.workspace = true`.
pub fn with_version(text: &str, version: Version) -> Result<Option<String>> {
    let mut document: DocumentMut = text.parse().context("could not read the Cargo.toml")?;
    let mut changed = false;
    for path in [&["package"][..], &["workspace", "package"][..]] {
        // Through TableLike, as indexing an Item mutably adds the key when it is missing.
        let mut item = Some(document.as_item_mut());
        for key in path {
            item = item
                .and_then(Item::as_table_like_mut)
                .and_then(|table| table.get_mut(key));
        }
        if let Some(value) = item
            .and_then(Item::as_table_like_mut)
            .and_then(|table| table.get_mut("version"))
            .and_then(Item::as_value_mut)
            && value.is_str()
        {
            set_string(value, &version.to_string());
            changed = true;
        }
    }
    Ok(changed.then(|| document.to_string()))
}

/// Sets the version required of each of the named crates, wherever a Cargo.toml's text depends on them, in every
/// kind of dependency table, including the workspace's and those for particular targets, and returns the new text
/// and what was changed, e.g. `bruce-db "0.1.0" to "0.2.0"`. A dependency which inherits its version from the
/// workspace is left alone, as the workspace's own table is changed.
pub fn with_requirements(text: &str, versions: &HashMap<String, Version>) -> Result<(String, Vec<String>)> {
    let mut document: DocumentMut = text.parse().context("could not read the Cargo.toml")?;
    let mut changes = Vec::new();
    let root = document.as_table_mut();
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = root.get_mut(key).and_then(Item::as_table_like_mut) {
            set_requirements(table, versions, &mut changes);
        }
    }
    if let Some(table) = root
        .get_mut("workspace")
        .and_then(Item::as_table_like_mut)
        .and_then(|workspace| workspace.get_mut("dependencies"))
        .and_then(Item::as_table_like_mut)
    {
        set_requirements(table, versions, &mut changes);
    }
    if let Some(targets) = root.get_mut("target").and_then(Item::as_table_like_mut) {
        for (_, target) in targets.iter_mut() {
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(table) = target
                    .as_table_like_mut()
                    .and_then(|target| target.get_mut(key))
                    .and_then(Item::as_table_like_mut)
                {
                    set_requirements(table, versions, &mut changes);
                }
            }
        }
    }
    Ok((document.to_string(), changes))
}

/// Sets the version required of each named crate in one dependency table.
fn set_requirements(table: &mut dyn TableLike, versions: &HashMap<String, Version>, changes: &mut Vec<String>) {
    for (key, item) in table.iter_mut() {
        let name = item
            .as_table_like()
            .and_then(|dependency| dependency.get("package"))
            .and_then(Item::as_str)
            .unwrap_or(key.get())
            .to_string();
        let Some(version) = versions.get(&name) else { continue };
        let new = version.to_string();
        let value = match item {
            Item::Value(value) if value.is_str() => Some(value),
            _ => item
                .as_table_like_mut()
                .and_then(|dependency| dependency.get_mut("version"))
                .and_then(Item::as_value_mut),
        };
        if let Some(value) = value
            && let Some(old) = value.as_str()
            && old != new
        {
            changes.push(format!("{name} \"{old}\" to \"{new}\""));
            set_string(value, &new);
        }
    }
}

/// Replaces a string value, keeping the spaces and comments around it.
fn set_string(value: &mut Value, new: &str) {
    let decor = value.decor().clone();
    *value = Value::from(new);
    *value.decor_mut() = decor;
}

/// Sets the version of a crate's Cargo.toml file, or of the workspace's, see with_version, if it gives one.
pub fn set_version(file: &Path, version: Version) -> Result<Option<Change>> {
    let text = fs::read_to_string(file).with_context(|| format!("could not read {}", file.display()))?;
    match with_version(&text, version).with_context(|| file.display().to_string())? {
        Some(new) if new != text => {
            fs::write(file, new).with_context(|| format!("could not write {}", file.display()))?;
            Ok(Some(Change {
                file: file.to_path_buf(),
                what: format!("version \"{version}\""),
            }))
        }
        _ => Ok(None),
    }
}

/// Sets the versions required of the named crates in a Cargo.toml file, see with_requirements.
pub fn set_requirements_in(file: &Path, versions: &HashMap<String, Version>) -> Result<Vec<Change>> {
    let text = fs::read_to_string(file).with_context(|| format!("could not read {}", file.display()))?;
    let (new, changes) = with_requirements(&text, versions).with_context(|| file.display().to_string())?;
    if new != text {
        fs::write(file, new).with_context(|| format!("could not write {}", file.display()))?;
    }
    Ok(changes
        .into_iter()
        .map(|what| Change {
            file: file.to_path_buf(),
            what,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(pairs: &[(&str, &str)]) -> HashMap<String, Version> {
        pairs
            .iter()
            .map(|(name, version)| (name.to_string(), version.parse().unwrap()))
            .collect()
    }

    #[test]
    fn sets_a_crates_version_keeping_its_comments() {
        let text = "[package]\nname = \"a\"\n# The version.\nversion = \"0.1.0\" # now\n\n[dependencies]\n";
        let new = with_version(text, "0.2.0".parse().unwrap()).unwrap().unwrap();
        assert_eq!(
            new,
            "[package]\nname = \"a\"\n# The version.\nversion = \"0.2.0\" # now\n\n[dependencies]\n"
        );
    }

    #[test]
    fn sets_a_workspaces_version_and_leaves_an_inherited_one() {
        let text = "[workspace.package]\nversion = \"0.1.0\"\n";
        assert_eq!(
            with_version(text, "0.2.0".parse().unwrap()).unwrap().unwrap(),
            "[workspace.package]\nversion = \"0.2.0\"\n"
        );
        let text = "[package]\nname = \"a\"\nversion.workspace = true\n";
        assert_eq!(with_version(text, "0.2.0".parse().unwrap()).unwrap(), None);
    }

    #[test]
    fn sets_requirements_written_every_way() {
        let text = r#"[dependencies]
anyhow = "1"
# The store.
bruce-db = "0.1.0"
progress = { package = "bruce-util-progress", version = "0.1.0", features = ["x"] }

[dev-dependencies.bruce-db]
version = "0.1.0"

[target.'cfg(unix)'.dependencies]
bruce-db = { version = "0.1.0" }

[workspace.dependencies]
bruce-db = "0.1"
"#;
        let (new, changes) = with_requirements(
            text,
            &versions(&[("bruce-db", "0.2.0"), ("bruce-util-progress", "0.2.0")]),
        )
        .unwrap();
        assert_eq!(
            new,
            r#"[dependencies]
anyhow = "1"
# The store.
bruce-db = "0.2.0"
progress = { package = "bruce-util-progress", version = "0.2.0", features = ["x"] }

[dev-dependencies.bruce-db]
version = "0.2.0"

[target.'cfg(unix)'.dependencies]
bruce-db = { version = "0.2.0" }

[workspace.dependencies]
bruce-db = "0.2.0"
"#
        );
        assert_eq!(changes.len(), 5);
        assert_eq!(changes[0], "bruce-db \"0.1.0\" to \"0.2.0\"");
    }

    #[test]
    fn leaves_other_crates_and_inherited_requirements_alone() {
        let text = "[dependencies]\nbruce-db = { workspace = true }\nother = \"0.1.0\"\n";
        let (new, changes) = with_requirements(text, &versions(&[("bruce-db", "0.2.0")])).unwrap();
        assert_eq!(new, text);
        assert!(changes.is_empty());
    }
}
