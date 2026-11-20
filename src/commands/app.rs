//! An app's release: its installers are built by a GitHub workflow, tried, attached to a draft GitHub release, tried
//! again, and published, see the README. `try`, `watch`, `download` and `untag` are only for apps; `release` comes here
//! for one.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::commands::check;
use crate::commands::release;
use crate::commands::start::{self, StartPlan};
use crate::github::{self, ReleaseState, Run};
use crate::links;
use crate::manifest::{self, AppSettings, Installer, Manifests};
use crate::process;
use crate::repo::Repo;
use crate::ui::Ui;
use crate::version::Version;

/// An app's settings and version, after checking that it is on its version's branch.
struct App {
    settings: AppSettings,
    version: Version,
}

impl App {
    fn of(repo: &Repo) -> Result<App> {
        let settings = manifest::app_settings(&repo.dir).with_context(|| format!("{} is not an app", repo.name))?;
        let version = repo.manifests()?.version()?;
        let branch = version.branch();
        if repo.branch()?.as_deref() != Some(branch.as_str()) {
            bail!("{} is not on {branch}, the branch of its version, {version}", repo.name);
        }
        Ok(App { settings, version })
    }

    /// The installers missing from the draft release, by file name.
    fn missing(&self, repo: &Repo) -> Result<Vec<String>> {
        let attached = github::release_assets(repo, &self.version.tag())?;
        Ok(self
            .settings
            .installers
            .iter()
            .map(|installer| installer.file_for(self.version))
            .filter(|file| !attached.contains(file))
            .collect())
    }

    /// The installer for the computer the tool runs on.
    fn installer_here(&self) -> Result<&Installer> {
        let platform = this_platform()?;
        self.settings
            .installers
            .iter()
            .find(|installer| installer.platform == platform)
            .ok_or_else(|| anyhow::anyhow!("there is no installer for {platform}, this computer"))
    }
}

/// Where a version's installers are downloaded to, in the Downloads folder: `<repo>-<version>-test` for those of a
/// try, `<repo>-<version>` for those of the draft release.
pub fn download_dir(repo: &Repo, version: Version, tried: bool) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("there is no home folder, neither HOME nor USERPROFILE is set")?;
    let suffix = if tried { "-test" } else { "" };
    Ok(PathBuf::from(home)
        .join("Downloads")
        .join(format!("{}-{version}{suffix}", repo.name)))
}

/// The platform of the computer the tool runs on, as an installer names it, see `Installer::platform`: the machine's
/// own architecture, not that of the tool, which may be running under Rosetta, or Windows' x64 emulation.
pub fn this_platform() -> Result<String> {
    let here = std::env::temp_dir();
    match std::env::consts::OS {
        "macos" => {
            let arm = process::run(&here, "sysctl", &["-n", "hw.optional.arm64"])?
                .stdout
                .trim()
                == "1";
            Ok(if arm { "macos-aarch64" } else { "macos-x64" }.to_string())
        }
        "windows" => {
            let key = r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
            let ran = process::run(&here, "reg", &["query", key, "/v", "PROCESSOR_ARCHITECTURE"])?;
            Ok(if ran.stdout.contains("ARM64") {
                "windows-arm64"
            } else {
                "windows-x64"
            }
            .to_string())
        }
        "linux" => Ok(format!("linux-{}", std::env::consts::ARCH)),
        other => bail!("there are no installers for {other}"),
    }
}

/// Checks that what is on the branch can be built by the workflow: committed, pushed, and taking nothing by path from
/// outside the repository. Linked libraries are checked by linked_for_try.
fn require_buildable(repo: &Repo, manifests: &Manifests) -> Result<()> {
    repo.require_clean()?;
    match repo.ahead_behind()? {
        Some((0, 0)) => {}
        Some((ahead, 0)) => bail!("{} has {ahead} commits not pushed", repo.name),
        Some((_, behind)) => bail!("{} is {behind} commits behind GitHub, pull them first", repo.name),
        None => bail!("{}'s branch is not on GitHub, push it first", repo.name),
    }
    let outside = check::taken_by_path_from_outside(repo, manifests);
    if !outside.is_empty() {
        bail!(
            "{} takes {} by path from outside it, which the workflow would not find: release them, and take them from crates.io",
            repo.name,
            outside.join(", ")
        );
    }
    Ok(())
}

/// The libraries a try takes from their checkouts beside the app, as it links them, for the workflow to check out
/// beside it in the same way: each repository and its branch, e.g. `bruce-graphql@v0.6.0`, and the patch which links
/// them, as the app's git-ignored config file has it.
struct Linked {
    repos: Vec<(String, String)>,
    patch: String,
}

impl Linked {
    /// The workflow's `libraries` input, each `repository@branch`, separated by spaces.
    fn libraries(&self) -> String {
        self.repos
            .iter()
            .map(|(name, branch)| format!("{name}@{branch}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The libraries the app links, after checking that each is a checkout beside it, on a branch, with everything
/// committed and pushed, as the workflow builds what is on GitHub; None if it links none.
fn linked_for_try(repo: &Repo, ui: &mut dyn Ui) -> Result<Option<Linked>> {
    let patches = links::read(&repo.dir)?;
    if patches.is_empty() {
        return Ok(None);
    }
    let mut repos: Vec<(String, String)> = Vec::new();
    for patch in &patches {
        let library = Repo::containing(&patch.dir(&repo.dir)).with_context(|| {
            format!(
                "{} links {} from {}, which is not a checkout",
                repo.name, patch.name, patch.path
            )
        })?;
        if library.parent() != repo.parent() {
            bail!(
                "{} links {} from {}, which is not a checkout beside it, so the workflow could not check it out",
                repo.name,
                patch.name,
                library.dir.display()
            );
        }
        if repos.iter().any(|(name, _)| *name == library.name) {
            continue;
        }
        ui.info(&format!("Fetching {}, which {} links", library.name, repo.name));
        library.fetch()?;
        library.require_clean()?;
        let Some(branch) = library.branch()? else {
            bail!(
                "{} is not on a branch, which the workflow could check out",
                library.name
            );
        };
        match library.ahead_behind()? {
            Some((0, 0)) => {}
            Some((ahead, 0)) => bail!(
                "{} has {ahead} commits not pushed, which the workflow would not build",
                library.name
            ),
            Some((_, behind)) => bail!("{} is {behind} commits behind GitHub, pull them first", library.name),
            None => bail!("{}'s branch {branch} is not on GitHub, push it first", library.name),
        }
        repos.push((library.name, branch));
    }
    Ok(Some(Linked {
        repos,
        patch: links::with_patches("", &patches)?,
    }))
}

/// Builds the installers of what is on the version's branch, without releasing anything: moves the `testX.Y.Z` tag to
/// it, runs the workflow, and downloads the installers. A successful try is also the CI run which `check` asks for.
pub fn try_installers(repo: &Repo, ui: &mut dyn Ui) -> Result<()> {
    let app = App::of(repo)?;
    repo.fetch()?;
    let manifests = repo.manifests()?;
    require_buildable(repo, &manifests)?;
    let linked = linked_for_try(repo, ui)?;
    let version = app.version;
    let test_tag = version.test_tag();
    let dir = download_dir(repo, version, true)?;
    let head = repo.git(&["rev-parse", "--short", "HEAD"])?;
    let mut steps = vec![
        format!(
            "Tag {} ({head}) {test_tag}, moving the tag if it is already there, and push it",
            version.branch()
        ),
        format!(
            "Build the installers with the {} workflow, not attached to any release, which takes about 30 to 40 minutes",
            app.settings.workflow
        ),
        format!("Download them to {}, replacing what is there", dir.display()),
    ];
    if let Some(linked) = &linked {
        steps.insert(
            1,
            format!(
                "Build with the libraries it links, which the workflow checks out beside it: {}",
                linked
                    .repos
                    .iter()
                    .map(|(name, branch)| format!("{name}'s {branch}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    if !ui.confirm(&format!("Try {} {version}", repo.name), &steps)? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    repo.git(&["tag", "--force", &test_tag, "HEAD"])?;
    repo.git(&["push", "--quiet", "--force", "origin", &format!("refs/tags/{test_tag}")])?;
    let libraries = linked.as_ref().map(Linked::libraries);
    let mut inputs = vec![("tag", test_tag.as_str()), ("attach", "false")];
    if let (Some(linked), Some(libraries)) = (&linked, &libraries) {
        inputs.push(("libraries", libraries));
        inputs.push(("patch", &linked.patch));
    }
    let run = github::start_workflow(repo, &app.settings.workflow, &version.branch(), &inputs, ui)?;
    github::watch(repo, &run, ui)?;
    download_tried(repo, &run, &dir, ui)
}

/// Downloads every installer a try built.
fn download_tried(repo: &Repo, run: &Run, dir: &PathBuf, ui: &mut dyn Ui) -> Result<()> {
    remove_dir(dir)?;
    github::download_run(repo, run, None, dir)?;
    ui.info(&format!("The installers are in {}", dir.display()));
    Ok(())
}

/// Downloads every installer attached to the draft release, and says what is next.
fn download_drafted(repo: &Repo, version: Version, ui: &mut dyn Ui) -> Result<()> {
    let dir = download_dir(repo, version, false)?;
    remove_dir(&dir)?;
    github::download_release(repo, &version.tag(), None, &dir)?;
    let url = process::output(
        &repo.dir,
        "gh",
        &["release", "view", &version.tag(), "--json", "url", "--jq", ".url"],
    )?;
    ui.info(&format!(
        "The installers are in {}.\nTry them, and read the draft's notes, from the ChangeLog, at {url}\n\
         Then run release again to publish it, or untag to make more changes first.",
        dir.display()
    ));
    Ok(())
}

fn remove_dir(dir: &PathBuf) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("could not remove {}", dir.display()))?;
    }
    Ok(())
}

/// The run of the workflow last started, by a try or a release of the version.
enum Latest {
    Tried(Run),
    Released(Run),
}

fn latest_run(repo: &Repo, app: &App) -> Result<Latest> {
    let Some(run) = github::runs(repo, &app.settings.workflow, None, 1)?.into_iter().next() else {
        bail!("{} has no runs of the {} workflow", repo.name, app.settings.workflow);
    };
    let version = app.version;
    if run.head_branch == version.branch() {
        Ok(Latest::Tried(run))
    } else if run.head_branch == version.tag() {
        Ok(Latest::Released(run))
    } else {
        bail!(
            "the latest {} run, {}, is from {}, not from a try or release of {version}",
            app.settings.workflow,
            run.id,
            run.head_branch
        )
    }
}

/// Watches the workflow's latest run, of a try or a release, e.g. after stopping watching with Ctrl+C, and downloads
/// its installers when it has finished.
pub fn watch(repo: &Repo, ui: &mut dyn Ui) -> Result<()> {
    let app = App::of(repo)?;
    match latest_run(repo, &app)? {
        Latest::Tried(run) => {
            github::watch(repo, &run, ui)?;
            download_tried(repo, &run, &download_dir(repo, app.version, true)?, ui)
        }
        Latest::Released(run) => {
            github::watch(repo, &run, ui)?;
            download_drafted(repo, app.version, ui)
        }
    }
}

/// Downloads the installer for this computer from the workflow's latest run, of a try or a release, e.g. to try it on
/// another computer, into the same folder as `try` and `release` download all of them to.
pub fn download(repo: &Repo, ui: &mut dyn Ui) -> Result<()> {
    let app = App::of(repo)?;
    let installer = app.installer_here()?;
    let (run, tried) = match latest_run(repo, &app)? {
        Latest::Tried(run) => (run, true),
        Latest::Released(run) => (run, false),
    };
    if !run.completed() {
        bail!(
            "the latest run, {}, is {}, wait for it with: bruce-method-release watch",
            run.id,
            run.status
        );
    }
    if !run.succeeded() {
        bail!("the latest run, {}, did not succeed: {}", run.id, run.conclusion);
    }
    let dir = download_dir(repo, app.version, tried)?;
    if tried {
        let dir = dir.join(&installer.platform);
        remove_dir(&dir)?;
        ui.info(&format!("Downloading {} from run {}", installer.platform, run.id));
        github::download_run(repo, &run, Some(&installer.platform), &dir)?;
        ui.info(&format!("The installer is in {}", dir.display()));
    } else {
        let file = installer.file_for(app.version);
        ui.info(&format!("Downloading {file} from {}", app.version.tag()));
        github::download_release(repo, &app.version.tag(), Some(&file), &dir)?;
        ui.info(&format!("The installer is in {}", dir.display()));
    }
    Ok(())
}

/// Takes back a release not yet published, to make more changes: deletes the draft release and the release's tag. The
/// squash merge stays on main, and the next release merges the changes made since.
pub fn untag(repo: &Repo, ui: &mut dyn Ui) -> Result<()> {
    let app = App::of(repo)?;
    let tag = app.version.tag();
    repo.fetch()?;
    let state = github::release_state(repo, &tag)?;
    if state == ReleaseState::Published {
        bail!("{tag} has been published, so make a fix's version instead, see start");
    }
    let local = repo.has_ref(&format!("refs/tags/{tag}"))?;
    let remote = repo.remote_tag_exists(&tag)?;
    let mut steps = Vec::new();
    if state == ReleaseState::Draft {
        steps.push(format!("Delete the draft release {tag}, and its installers"));
    }
    if local || remote {
        steps.push(format!("Delete the tag {tag}, here and on GitHub"));
    }
    if steps.is_empty() {
        ui.info(&format!("{} has no draft release or tag {tag} to delete", repo.name));
        return Ok(());
    }
    if !ui.confirm(&format!("Untag {} {}", repo.name, app.version), &steps)? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    if state == ReleaseState::Draft {
        ui.info(&format!("{}: deleting the draft release {tag}", repo.name));
        process::output(&repo.dir, "gh", &["release", "delete", &tag, "--yes"])?;
    }
    delete_tag(repo, &tag, ui)?;
    ui.info(&format!(
        "Make the changes on {}, commit and push them, try them, then release again",
        app.version.branch()
    ));
    Ok(())
}

/// Deletes a tag here and on GitHub, wherever it is.
fn delete_tag(repo: &Repo, tag: &str, ui: &mut dyn Ui) -> Result<()> {
    ui.info(&format!("{}: deleting the tag {tag}", repo.name));
    if repo.has_ref(&format!("refs/tags/{tag}"))? {
        repo.git(&["tag", "--delete", tag])?;
    }
    if repo.remote_tag_exists(tag)? {
        repo.git(&["push", "--quiet", "origin", "--delete", &format!("refs/tags/{tag}")])?;
    }
    Ok(())
}

/// Releases an app's version, in two goes, each carrying on from wherever the last stopped. The first checks it, squash
/// merges it into main, tags it, makes a draft GitHub release, has the workflow build the installers and attach them to
/// it, downloads them, and stops for them to be tried. The second publishes the draft and starts the next version.
pub fn release(repo: &Repo, ui: &mut dyn Ui) -> Result<()> {
    let app = App::of(repo)?;
    let version = app.version;
    let tag = version.tag();
    repo.fetch()?;
    match github::release_state(repo, &tag)? {
        ReleaseState::Published => start_next(repo, version, ui),
        ReleaseState::Draft if app.missing(repo)?.is_empty() => publish(repo, &app, ui),
        _ => build(repo, &app, ui),
    }
}

/// The first go of an app's release, up to the installers on the draft release.
fn build(repo: &Repo, app: &App, ui: &mut dyn Ui) -> Result<()> {
    let version = app.version;
    let branch = version.branch();
    let tag = version.tag();
    let tagged = repo.remote_tag_exists(&tag)?;
    let merged = tagged || (same_tree(repo, "origin/main", "HEAD")? && release::pull_request_merged(repo, version)?);
    let state = github::release_state(repo, &tag)?;
    let running = github::runs(repo, &app.settings.workflow, Some(&tag), 1)?
        .into_iter()
        .find(|run| !run.completed());

    if merged {
        ui.info(&format!(
            "{} {version} is part way through its release, carrying on",
            repo.name
        ));
    } else {
        let failed: Vec<String> = check::check(repo, ui)?
            .iter()
            .filter_map(|check| match &check.outcome {
                check::Outcome::Fail(why) => Some(format!("{}: {why}", check.name)),
                _ => None,
            })
            .collect();
        if !failed.is_empty() {
            bail!("{} cannot be released yet:\n{}", repo.name, failed.join("\n"));
        }
    }

    let merged_back = merged && is_ancestor(repo, "origin/main", "HEAD")?;
    let test_tag = version.test_tag();
    let tried = repo.has_ref(&format!("refs/tags/{test_tag}"))? || repo.remote_tag_exists(&test_tag)?;
    let dir = download_dir(repo, version, false)?;
    let done = |done: bool| if done { " (done)" } else { "" };
    let mut steps = vec![
        format!("Squash merge {branch} into main by pull request{}", done(merged)),
        format!("Tag main {tag}, and push the tag{}", done(tagged)),
        format!(
            "Merge main back into {branch}, which changes no files, so that the next pull request shows only what is new{}",
            done(merged_back)
        ),
    ];
    if tried {
        steps.push(format!("Delete the tag {test_tag} of the last try"));
    }
    steps.push(format!(
        "Make the draft GitHub release {tag}, with the ChangeLog's notes{}",
        done(state != ReleaseState::None)
    ));
    steps.push(match &running {
        Some(run) => format!("Watch the run already building the installers, {}", run.url),
        None => format!(
            "Build the installers with the {} workflow from {tag}, and attach them to the draft, which takes about 30 to 40 minutes",
            app.settings.workflow
        ),
    });
    steps.push(format!(
        "Download them to {}, and stop for them to be tried, then release again publishes it",
        dir.display()
    ));
    if !ui.confirm(
        &format!("Release {} {version}, up to trying its installers", repo.name),
        &steps,
    )? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }

    if !merged {
        release::merge(repo, version, ui)?;
    }
    repo.fetch()?;
    if !tagged {
        release::tag(repo, version, ui)?;
    }
    if !is_ancestor(repo, "origin/main", "HEAD")? {
        ui.info(&format!("{}: merging main back into {branch}", repo.name));
        repo.git(&[
            "merge",
            "--quiet",
            "--no-edit",
            "origin/main",
            "--message",
            "Merge main after squash merge",
        ])?;
        repo.git(&["push", "--quiet"])?;
    }
    if tried {
        delete_tag(repo, &test_tag, ui)?;
    }
    if state == ReleaseState::None {
        release::github_release(repo, version, true, ui)?;
    }
    let run = match running {
        Some(run) => run,
        None => github::start_workflow(
            repo,
            &app.settings.workflow,
            &tag,
            &[("tag", &tag), ("attach", "true")],
            ui,
        )?,
    };
    github::watch(repo, &run, ui)?;
    let missing = app.missing(repo)?;
    if !missing.is_empty() {
        bail!(
            "the run {} succeeded, but the draft release {tag} has no {}",
            run.id,
            missing.join(", ")
        );
    }
    download_drafted(repo, version, ui)
}

/// The second go of an app's release: publishing the draft, once its installers have been tried.
fn publish(repo: &Repo, app: &App, ui: &mut dyn Ui) -> Result<()> {
    let version = app.version;
    let tag = version.tag();
    let mut steps = vec![format!(
        "Publish the draft release {tag}, with its installers, as tried from {}, as the latest release",
        download_dir(repo, version, false)?.display()
    )];
    let next = next_version(repo, version)?;
    match &next {
        Some(next) => steps.extend(next.steps(repo)),
        None => steps.push(
            "Then, as this is a fix, merge main into the branch being developed by hand, so that it has the fix too"
                .to_string(),
        ),
    }
    if !ui.confirm(&format!("Publish {} {version}", repo.name), &steps)? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    ui.info(&format!("{}: publishing {tag}", repo.name));
    process::output(&repo.dir, "gh", &["release", "edit", &tag, "--draft=false", "--latest"])?;
    finish(repo, version, next, ui)
}

/// Starts the next version, if a release was published but the next version not started, e.g. as it stopped.
fn start_next(repo: &Repo, version: Version, ui: &mut dyn Ui) -> Result<()> {
    let Some(next) = next_version(repo, version)? else {
        bail!("{} {version} is already released", repo.name);
    };
    if !ui.confirm(
        &format!("{} {version} is released, start the next version", repo.name),
        &next.steps(repo),
    )? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    finish(repo, version, Some(next), ui)
}

/// The next version to start after releasing one, or None for a fix, after which the version being developed carries
/// on, or if its branch is already there.
fn next_version(repo: &Repo, version: Version) -> Result<Option<StartPlan>> {
    if version.fixes().is_some() {
        return Ok(None);
    }
    let next = version.next_minor();
    let branch = next.branch();
    if repo.has_ref(&format!("refs/heads/{branch}"))? || repo.has_ref(&format!("refs/remotes/origin/{branch}"))? {
        return Ok(None);
    }
    Ok(Some(StartPlan {
        version: next,
        base: "origin/main".to_string(),
    }))
}

fn finish(repo: &Repo, version: Version, next: Option<StartPlan>, ui: &mut dyn Ui) -> Result<()> {
    match &next {
        Some(next) => start::apply_start(repo, next, ui)?,
        None if version.fixes().is_some() => ui.info(&format!(
            "{} {version} released: merge main into the branch being developed, e.g. git switch vX.Y.0 && git merge origin/main",
            repo.name
        )),
        None => {}
    }
    ui.info(&format!("{} {version} released", repo.name));
    Ok(())
}

/// Whether two commits have the same files.
fn same_tree(repo: &Repo, a: &str, b: &str) -> Result<bool> {
    Ok(process::run(&repo.dir, "git", &["diff", "--quiet", a, b])?.success)
}

/// Whether commit `a` is in the history of `b`.
fn is_ancestor(repo: &Repo, a: &str, b: &str) -> Result<bool> {
    Ok(process::run(&repo.dir, "git", &["merge-base", "--is-ancestor", a, b])?.success)
}
