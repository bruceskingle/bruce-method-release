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
    /// Apps for Apple's App Store, e.g. an iOS app and a Mac app, which a GitHub workflow builds, signs and uploads to
    /// App Store Connect.
    AppStore,
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

    /// The target the crate is built for, if its own `.cargo/config.toml` sets one, e.g. `xtensa-esp32s3-espidf` for
    /// a board's firmware. Such a crate is built for a device, so its tests cannot run on this computer, and running
    /// them would use its runner, which may flash a board.
    pub fn build_target(&self) -> Result<Option<String>> {
        for name in ["config.toml", "config"] {
            let file = self.dir().join(".cargo").join(name);
            if !file.exists() {
                continue;
            }
            let text = fs::read_to_string(&file).with_context(|| format!("could not read {}", file.display()))?;
            let config: DocumentMut = text
                .parse()
                .with_context(|| format!("could not parse {}", file.display()))?;
            return Ok(config
                .get("build")
                .and_then(|build| build.get("target"))
                .and_then(|target| target.as_str())
                .map(str::to_string));
        }
        Ok(None)
    }

    /// Whether the crate is built for a device, see build_target.
    pub fn for_device(&self) -> Result<bool> {
        Ok(self.build_target()?.is_some())
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
    /// The directory of the crate depended on, for a path dependency.
    pub path: Option<PathBuf>,
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
        let (crates, kind) = read_metadata(dir, None)?;
        Ok(Manifests {
            crates,
            kind: kind.unwrap_or(Kind::Crate),
        })
    }

    /// Reads every crate of the repository in `dir` whose Cargo.toml is one of `files`, its tracked Cargo.toml files,
    /// whether or not it is in the workspace at the root, e.g. firmware built on its own, or a repository with no
    /// Cargo.toml at its root, whose crates are each their own workspace. Says too which files could not be read, and
    /// why, rather than failing, as a crate which cannot be read should not hide the others.
    pub fn read_all(dir: &Path, files: &[PathBuf]) -> Result<(Manifests, Vec<String>)> {
        let mut crates: Vec<Crate> = Vec::new();
        let mut kind = None;
        let mut unreadable = Vec::new();
        let root = dir.join("Cargo.toml");
        let mut files: Vec<&PathBuf> = files.iter().collect();
        // The root's first, which gives the repository's kind, and covers its workspace's members at once.
        files.sort_by_key(|file| !same_file(file, &root));
        for file in files {
            if crates.iter().any(|krate| same_file(&krate.manifest, file)) {
                continue;
            }
            match read_metadata(dir, Some(file)) {
                Ok((found, found_kind)) => {
                    if same_file(file, &root) {
                        kind = found_kind;
                    }
                    for krate in found {
                        if !crates.iter().any(|known| same_file(&known.manifest, &krate.manifest)) {
                            crates.push(krate);
                        }
                    }
                }
                Err(error) => unreadable.push(format!("{}: {error:#}", file.display())),
            }
        }
        Ok((
            Manifests {
                crates,
                kind: kind.unwrap_or(Kind::Crate),
            },
            unreadable,
        ))
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

/// The crates of a workspace, from `cargo metadata`, run in `dir`, of the workspace whose Cargo.toml is `manifest`, or
/// else the one at `dir`'s root, and its kind, if its metadata gives one.
fn read_metadata(dir: &Path, manifest: Option<&Path>) -> Result<(Vec<Crate>, Option<Kind>)> {
    let mut args = vec!["metadata", "--format-version", "1", "--no-deps"];
    let manifest = manifest.map(|manifest| manifest.to_string_lossy().into_owned());
    if let Some(manifest) = &manifest {
        args.extend(["--manifest-path", manifest.as_str()]);
    }
    let json = process::output(dir, "cargo", &args)?;
    let metadata: Metadata = serde_json::from_str(&json).context("could not read cargo metadata's output")?;
    let kind = kind_of(&metadata.metadata)
        .or_else(|| metadata.packages.iter().find_map(|package| kind_of(&package.metadata)))
        .transpose()?;
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
                        path: dependency.path,
                    })
                    .collect(),
                name: package.name,
                manifest: package.manifest_path,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((crates, kind))
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
        Some("app-store") => Ok(Kind::AppStore),
        _ => Err(anyhow!(
            "bruce-method's kind should be \"crate\", \"app\" or \"app-store\", not {kind}"
        )),
    })
}

/// What an app's release builds, from its `[package.metadata.bruce-method]`, or `[workspace.metadata.bruce-method]`:
/// the GitHub workflow which builds its installers, and the installers it builds.
///
/// The workflow is run by hand (`workflow_dispatch`), with two inputs: `tag`, the tag to build, `testX.Y.Z` or
/// `releaseX.Y.Z`, and `attach`, true to attach the installers to the draft GitHub release for the tag. It keeps each
/// platform's installer as an artifact of the run named for the platform, e.g. `macos-aarch64`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AppSettings {
    /// The workflow's file in `.github/workflows`, `release.yml` unless set.
    #[serde(default = "default_workflow")]
    pub workflow: String,
    pub installers: Vec<Installer>,
}

fn default_workflow() -> String {
    "release.yml".to_string()
}

/// An installer an app's workflow builds.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Installer {
    /// The platform it is for, `<os>-<architecture>`, e.g. `macos-aarch64`, `macos-x64`, `windows-x64` or
    /// `windows-arm64`, which also names the run's artifact with it in.
    pub platform: String,
    /// Its file's name, with `{version}` for the version, e.g. `BruceSmartHome_{version}_aarch64.dmg`.
    pub file: String,
}

impl Installer {
    /// Its file's name for a version.
    pub fn file_for(&self, version: Version) -> String {
        self.file.replace("{version}", &version.to_string())
    }
}

/// The `bruce-method` table of the Cargo.toml at the root of `dir`, the workspace's or else its package's, after
/// checking that it is of the kind given.
fn method_table(dir: &Path, kind: &str) -> Result<serde_json::Value> {
    let json = process::output(dir, "cargo", &["metadata", "--format-version", "1", "--no-deps"])?;
    let metadata: Metadata = serde_json::from_str(&json).context("could not read cargo metadata's output")?;
    let table = std::iter::once(&metadata.metadata)
        .chain(metadata.packages.iter().map(|package| &package.metadata))
        .find_map(|metadata| metadata.as_ref()?.get("bruce-method"))
        .ok_or_else(|| anyhow!("its Cargo.toml has no [package.metadata.bruce-method]"))?;
    if table.get("kind").and_then(|found| found.as_str()) != Some(kind) {
        bail!("its Cargo.toml's bruce-method table does not say kind = \"{kind}\"");
    }
    Ok(table.clone())
}

/// What an App Store repository's release builds, from its `[workspace.metadata.bruce-method]`, or
/// `[package.metadata.bruce-method]`: the GitHub workflow which builds, signs and uploads its apps, and the apps.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct StoreSettings {
    /// The workflow's file in `.github/workflows`, `release.yml` unless set.
    #[serde(default = "default_workflow")]
    pub workflow: String,
    pub apps: Vec<StoreApp>,
}

/// An app for the App Store, by its platform.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "platform", rename_all = "lowercase", deny_unknown_fields)]
pub enum StoreApp {
    /// An iOS app, made by Xcode.
    Ios {
        /// Its Xcode project, from the repository's root, e.g. `ios/QuizParty.xcodeproj`.
        project: String,
        /// The scheme which builds it.
        scheme: String,
        /// A Swift package of its, from the repository's root, whose tests CI runs, e.g. `ios/QuizPartyKit`.
        #[serde(default, rename = "swift-package")]
        swift_package: Option<String>,
        /// The GitHub runner which builds it, e.g. `xcode-27`, the image with the newest Xcode while `macos-latest`
        /// does not have it yet; `macos-latest` unless set.
        #[serde(default)]
        runner: Option<String>,
        /// The Xcode which builds it, from those on the runner, e.g. `27.2`, for the SDK App Store Connect asks for,
        /// as `/Applications/Xcode_27.2.app`; the runner's default unless set.
        #[serde(default)]
        xcode: Option<String>,
    },
    /// A Mac app made with Dioxus, which the workflow puts in an Xcode archive to sign and upload.
    Macos {
        /// The Dioxus app's folder, with its Dioxus.toml, from the repository's root, e.g. `server`.
        dioxus: String,
        /// The Cargo features of the App Store's build, e.g. one which leaves out what the App Sandbox does not allow.
        #[serde(default)]
        features: Vec<String>,
        /// Its entitlements file, from the repository's root, which must ask for the App Sandbox.
        entitlements: String,
        /// Its App Store category, as `LSApplicationCategoryType`, e.g. `public.app-category.trivia-games`.
        category: String,
        /// Whether its only encryption is exempt from export documentation, e.g. HTTPS through Apple's own
        /// frameworks, so that each build declares so (`ITSAppUsesNonExemptEncryption` false) and App Store Connect
        /// does not ask. It is false unless set, as it is a declaration only the app's author can make.
        #[serde(default, rename = "exempt-encryption")]
        exempt_encryption: bool,
        /// The app's name, as people see it, e.g. `Bruce's Quiz Party`, given to its bundle, in Finder and the menu
        /// bar, and, without spaces or punctuation, to the package exported, e.g. `BrucesQuizParty.pkg`. Without it,
        /// the app keeps the name dx gives it, its binary's in PascalCase, e.g. `BruceQuizParty`, which Dioxus.toml
        /// cannot change.
        #[serde(default)]
        name: Option<String>,
    },
}

/// Reads an App Store repository's settings from the Cargo.toml at the root of `dir`.
pub fn store_settings(dir: &Path) -> Result<StoreSettings> {
    let settings: StoreSettings = serde_json::from_value(method_table(dir, "app-store")?)
        .context("its bruce-method table should list its apps, see the README's Settings")?;
    if settings.apps.is_empty() {
        bail!("its bruce-method table lists no apps");
    }
    Ok(settings)
}

/// Reads an app's settings from the Cargo.toml at the root of `dir`, failing if it is not an app's or does not say
/// which installers it builds.
pub fn app_settings(dir: &Path) -> Result<AppSettings> {
    let table = method_table(dir, "app")?;
    let settings: AppSettings = serde_json::from_value(table)
        .context("its [package.metadata.bruce-method] should list its installers, see the README's Settings")?;
    if settings.installers.is_empty() {
        bail!("its [package.metadata.bruce-method] lists no installers");
    }
    Ok(settings)
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
    for_each_dependency_table(&mut document, &mut |table| {
        set_requirements(table, versions, &mut changes)
    });
    Ok((document.to_string(), changes))
}

/// Makes each of the named crates' path dependencies in a Cargo.toml's text a dependency on crates.io, once the
/// version is published, in every kind of dependency table, as with_requirements: the path is removed, and the version
/// given is kept, or else, for a path alone, the version published is required, e.g. `{ path = "../bruce-db" }` to
/// `"0.2.0"`. One requiring a version which the one published does not meet is left as it is. A table left with only
/// its version becomes that version, so that it reads as one written by hand would.
/// Returns the new text and what was changed.
pub fn with_published(text: &str, versions: &HashMap<String, Version>) -> Result<(String, Vec<String>)> {
    let mut document: DocumentMut = text.parse().context("could not read the Cargo.toml")?;
    let mut changes = Vec::new();
    for_each_dependency_table(&mut document, &mut |table| publish_paths(table, versions, &mut changes));
    Ok((document.to_string(), changes))
}

/// Calls f with each dependency table of a Cargo.toml: the crate's own, of each kind, the workspace's, and those for
/// particular targets.
fn for_each_dependency_table(document: &mut DocumentMut, f: &mut dyn FnMut(&mut dyn TableLike)) {
    let root = document.as_table_mut();
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = root.get_mut(key).and_then(Item::as_table_like_mut) {
            f(table);
        }
    }
    if let Some(table) = root
        .get_mut("workspace")
        .and_then(Item::as_table_like_mut)
        .and_then(|workspace| workspace.get_mut("dependencies"))
        .and_then(Item::as_table_like_mut)
    {
        f(table);
    }
    if let Some(targets) = root.get_mut("target").and_then(Item::as_table_like_mut) {
        for (_, target) in targets.iter_mut() {
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(table) = target
                    .as_table_like_mut()
                    .and_then(|target| target.get_mut(key))
                    .and_then(Item::as_table_like_mut)
                {
                    f(table);
                }
            }
        }
    }
}

/// Makes the named crates' path dependencies in one dependency table dependencies on crates.io, see with_published.
fn publish_paths(table: &mut dyn TableLike, versions: &HashMap<String, Version>, changes: &mut Vec<String>) {
    for (key, item) in table.iter_mut() {
        let Some(dependency) = item.as_table_like_mut() else {
            continue;
        };
        let name = dependency
            .get("package")
            .and_then(Item::as_str)
            .unwrap_or(key.get())
            .to_string();
        let Some(version) = versions.get(&name) else { continue };
        // A version required which the one published does not meet is left by path, as crates.io cannot give it.
        if !dependency.contains_key("path")
            || dependency
                .get("version")
                .and_then(Item::as_str)
                .is_some_and(|required| version.satisfies(required) != Some(true))
        {
            continue;
        }
        dependency.remove("path");
        let required = match dependency.get("version").and_then(Item::as_str) {
            Some(required) => required.to_string(),
            None => {
                dependency.insert("version", toml_edit::value(version.to_string()));
                version.to_string()
            }
        };
        changes.push(format!("{name} from its checkout to \"{required}\" from crates.io"));
        // An inline table of the version alone becomes the version, keeping the spaces and comments around it, and
        // one with more is laid out afresh, as removing the path leaves the spaces around it, with the version first,
        // after the package if it is renamed, as Cargo.toml files usually have it, rather than last, where it was added.
        if let Item::Value(Value::InlineTable(inline)) = item {
            if inline.len() == 1 {
                let decor = inline.decor().clone();
                let mut plain = Value::from(required);
                *plain.decor_mut() = decor;
                *item = Item::Value(plain);
            } else {
                let mut ordered = toml_edit::InlineTable::new();
                for first in ["package", "version"] {
                    if let Some(value) = inline.get(first) {
                        ordered.insert(first, value.clone());
                    }
                }
                for (key, value) in inline.iter() {
                    if key != "package" && key != "version" {
                        ordered.insert(key, value.clone());
                    }
                }
                *ordered.decor_mut() = inline.decor().clone();
                ordered.fmt();
                *inline = ordered;
            }
        }
    }
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

/// Makes the named crates' path dependencies in a Cargo.toml file dependencies on crates.io, see with_published.
pub fn publish_paths_in(file: &Path, versions: &HashMap<String, Version>) -> Result<Vec<Change>> {
    let text = fs::read_to_string(file).with_context(|| format!("could not read {}", file.display()))?;
    let (new, changes) = with_published(&text, versions).with_context(|| file.display().to_string())?;
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
    fn makes_path_dependencies_published_ones() {
        let text = "[dependencies]\n\
                    # The store.\n\
                    bruce-db = { version = \"0.2.0\", path = \"../bruce-db\" } # local\n\
                    std = { path = \"../bruce-embedded-std\", features = [\"x\"] }\n\
                    other = { path = \"other\" }\n\
                    newer = { package = \"bruce-db\", version = \"0.3.0\", path = \"../bruce-db\" }\n\
                    \n\
                    [target.'cfg(unix)'.dependencies.progress]\n\
                    package = \"bruce-util-progress\"\n\
                    path = \"../progress\"\n";
        let (new, changes) = with_published(
            text,
            &versions(&[
                ("bruce-db", "0.2.0"),
                ("bruce-embedded-std", "0.1.0"),
                ("std", "9.9.9"),
                ("bruce-util-progress", "0.3.0"),
            ]),
        )
        .unwrap();
        assert_eq!(
            new,
            "[dependencies]\n\
             # The store.\n\
             bruce-db = \"0.2.0\" # local\n\
             std = { version = \"9.9.9\", features = [\"x\"] }\n\
             other = { path = \"other\" }\n\
             newer = { package = \"bruce-db\", version = \"0.3.0\", path = \"../bruce-db\" }\n\
             \n\
             [target.'cfg(unix)'.dependencies.progress]\n\
             package = \"bruce-util-progress\"\n\
             version = \"0.3.0\"\n"
        );
        assert_eq!(
            changes,
            vec![
                "bruce-db from its checkout to \"0.2.0\" from crates.io",
                "std from its checkout to \"9.9.9\" from crates.io",
                "bruce-util-progress from its checkout to \"0.3.0\" from crates.io",
            ]
        );
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
