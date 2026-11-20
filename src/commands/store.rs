//! An App Store app's release: its apps are built and signed by a GitHub workflow, which pushing a tag runs, tried
//! without uploading them, then uploaded to App Store Connect, tried in TestFlight, and submitted for App Review there,
//! see the README. `try`, `watch` and `release` come here for one; `untag` is the same as for any app.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::commands::app::{self, Merge};
use crate::github::{self, ReleaseState, Run};
use crate::links;
use crate::manifest::{self, StoreSettings};
use crate::repo::Repo;
use crate::ui::Ui;
use crate::version::Version;

/// An App Store repository's settings and version, after checking that it is on its version's branch.
struct Store {
    settings: StoreSettings,
    version: Version,
}

impl Store {
    fn of(repo: &Repo) -> Result<Store> {
        let settings = manifest::store_settings(&repo.dir)
            .with_context(|| format!("{} does not release to the App Store", repo.name))?;
        Ok(Store {
            settings,
            version: app::version_on_branch(repo)?,
        })
    }

    fn workflow(&self) -> &str {
        &self.settings.workflow
    }
}

/// Builds and signs the apps of what is on the version's branch, without uploading them: moves the `testX.Y.Z` tag to
/// it and pushes it, which runs the workflow, then downloads the apps, as they would be uploaded. A successful try is
/// also the CI run which `check` asks for.
pub fn try_apps(repo: &Repo, ui: &mut dyn Ui) -> Result<()> {
    let store = Store::of(repo)?;
    repo.fetch()?;
    let manifests = repo.manifests()?;
    app::require_buildable(repo, &manifests)?;
    if !links::read(&repo.dir)?.is_empty() {
        bail!(
            "{} links libraries from their checkouts, which its App Store workflow does not build with: release them, \
             and unlink them first",
            repo.name
        );
    }
    let version = store.version;
    let test_tag = version.test_tag();
    let dir = app::download_dir(repo, version, true)?;
    let head = repo.git(&["rev-parse", "--short", "HEAD"])?;
    let steps = vec![
        format!(
            "Tag {} ({head}) {test_tag}, moving the tag if it is already there, and push it, which starts the {} workflow",
            version.branch(),
            store.workflow()
        ),
        "Build the apps and sign them for App Store Connect, without uploading them, which takes about 10 to 20 \
         minutes, or longer when the Dioxus CLI is not in GitHub's cache"
            .to_string(),
        format!("Download them to {}, replacing what is there", dir.display()),
    ];
    if !ui.confirm(&format!("Try {} {version}", repo.name), &steps)? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    let previous = github::latest_run_id(repo, store.workflow(), &test_tag)?;
    push_test_tag(repo, &test_tag, ui)?;
    let run = github::wait_for_run(repo, store.workflow(), &test_tag, previous)?;
    github::watch(repo, &run, ui)?;
    download_tried(repo, &run, &dir, ui)
}

/// Tags HEAD for a try and pushes the tag, so that the workflow runs. Pushing a tag which GitHub already has on the
/// same commit changes nothing there, and runs nothing, so it is deleted there first.
fn push_test_tag(repo: &Repo, test_tag: &str, ui: &mut dyn Ui) -> Result<()> {
    ui.info(&format!("{}: tagging HEAD {test_tag}, and pushing it", repo.name));
    repo.git(&["tag", "--force", test_tag, "HEAD"])?;
    let head = repo.commit("HEAD")?;
    if repo.remote_tag_commit(test_tag)?.as_deref() == Some(head.as_str()) {
        repo.git(&[
            "push",
            "--quiet",
            "origin",
            "--delete",
            &format!("refs/tags/{test_tag}"),
        ])?;
    }
    repo.git(&["push", "--quiet", "--force", "origin", &format!("refs/tags/{test_tag}")])?;
    Ok(())
}

/// Downloads the apps a try built, each platform's into a folder of its own.
fn download_tried(repo: &Repo, run: &Run, dir: &Path, ui: &mut dyn Ui) -> Result<()> {
    app::remove_dir(dir)?;
    github::download_run(repo, run, None, dir)?;
    ui.info(&format!(
        "The apps are in {}, signed as they would be uploaded; release uploads them to App Store Connect",
        dir.display()
    ));
    Ok(())
}

/// Watches the workflow's latest run, of a try or a release, e.g. after stopping watching with Ctrl+C, and, for a try,
/// downloads the apps when it has finished.
pub fn watch(repo: &Repo, ui: &mut dyn Ui) -> Result<()> {
    let store = Store::of(repo)?;
    let version = store.version;
    let Some(run) = github::runs(repo, store.workflow(), None, 1)?.into_iter().next() else {
        bail!("{} has no runs of the {} workflow", repo.name, store.workflow());
    };
    if run.head_branch == version.test_tag() {
        github::watch(repo, &run, ui)?;
        download_tried(repo, &run, &app::download_dir(repo, version, true)?, ui)
    } else if run.head_branch == version.tag() {
        github::watch(repo, &run, ui)?;
        uploaded_next(ui);
        Ok(())
    } else {
        bail!(
            "the latest {} run, {}, is from {}, not from a try or release of {version}",
            store.workflow(),
            run.id,
            run.head_branch
        )
    }
}

/// Releases an App Store app's version, in two goes, each carrying on from wherever the last stopped. The first checks
/// it, squash merges it into main, tags it, which runs the workflow, uploading the apps to App Store Connect, and makes
/// a draft GitHub release, then stops for the apps to be tried in TestFlight and submitted for App Review. The second
/// publishes the GitHub release and starts the next version.
pub fn release(repo: &Repo, ui: &mut dyn Ui) -> Result<()> {
    let store = Store::of(repo)?;
    let version = store.version;
    let tag = version.tag();
    repo.fetch()?;
    match github::release_state(repo, &tag)? {
        ReleaseState::Published => app::start_next(repo, version, ui),
        ReleaseState::Draft if uploaded(repo, &store)? => publish(repo, &store, ui),
        _ => upload(repo, &store, ui),
    }
}

/// Whether the latest run from the release's tag succeeded, so that its apps are on App Store Connect.
fn uploaded(repo: &Repo, store: &Store) -> Result<bool> {
    Ok(github::runs(repo, store.workflow(), Some(&store.version.tag()), 1)?
        .first()
        .is_some_and(|run| run.completed() && run.succeeded()))
}

/// What a run from the release's tag needs, if it was tagged in an earlier go.
enum Uploading {
    /// The tag is pushed by this go, which runs the workflow.
    OnTagging,
    /// No run from the tag, e.g. as GitHub did not start one: one is started by hand.
    Start,
    /// A run still going, to watch.
    Watch(Run),
    /// A run which failed, whose failed jobs are run again.
    Rerun(Run),
}

/// The first go of an App Store app's release, up to its apps uploaded to App Store Connect.
fn upload(repo: &Repo, store: &Store, ui: &mut dyn Ui) -> Result<()> {
    let version = store.version;
    let tag = version.tag();
    let merge = Merge::of(repo, version, ui)?;
    let latest = github::runs(repo, store.workflow(), Some(&tag), 1)?.into_iter().next();
    let uploading = match latest {
        _ if !merge.tagged => Uploading::OnTagging,
        None => Uploading::Start,
        Some(run) if !run.completed() => Uploading::Watch(run),
        Some(run) if !run.succeeded() => Uploading::Rerun(run),
        // A run which succeeded, but the draft release not made: it is made, and the apps were uploaded.
        Some(run) => Uploading::Watch(run),
    };
    let tagging = format!(
        ", which starts the {} workflow, building the apps and uploading them to App Store Connect",
        store.workflow()
    );
    let mut steps = merge.steps(if merge.tagged { "" } else { &tagging });
    match &uploading {
        Uploading::OnTagging => steps.push(
            "Watch the run until the apps are uploaded, which takes about 10 to 20 minutes, or longer when the Dioxus \
             CLI is not in GitHub's cache"
                .to_string(),
        ),
        Uploading::Start => steps.push(format!(
            "Run the {} workflow from {tag}, uploading the apps to App Store Connect, and watch it",
            store.workflow()
        )),
        Uploading::Watch(run) => steps.push(format!("Watch the run uploading the apps, {}", run.url)),
        Uploading::Rerun(run) => steps.push(format!(
            "Run the failed jobs of {}, which failed, again, and watch it",
            run.url
        )),
    }
    steps.push(
        "Stop for the apps to be tried in TestFlight and submitted for App Review; once Apple approves them, release \
         again publishes the GitHub release"
            .to_string(),
    );
    if !ui.confirm(
        &format!("Release {} {version}, up to its apps on App Store Connect", repo.name),
        &steps,
    )? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    let previous = github::latest_run_id(repo, store.workflow(), &tag)?;
    merge.apply(repo, ui)?;
    let run = match uploading {
        Uploading::OnTagging => github::wait_for_run(repo, store.workflow(), &tag, previous)?,
        Uploading::Start => {
            github::start_workflow(repo, store.workflow(), &tag, &[("tag", &tag), ("upload", "true")], ui)?
        }
        Uploading::Watch(run) => run,
        Uploading::Rerun(run) => {
            github::rerun_failed(repo, &run, ui)?;
            run
        }
    };
    github::watch(repo, &run, ui)?;
    uploaded_next(ui);
    Ok(())
}

/// Says what is next once the apps are uploaded.
fn uploaded_next(ui: &mut dyn Ui) {
    ui.info(
        "The apps are uploaded to App Store Connect, and are in TestFlight once Apple has processed them, which it \
         emails about.\nTry them there, then submit them for App Review in App Store Connect. Once Apple has \
         approved them, run release again to publish the GitHub release and start the next version.\nTo make more \
         changes first, e.g. for a rejection, untag, then try and release again: the next upload has a new build \
         number.",
    );
}

/// The second go of an App Store app's release: publishing the GitHub release, once its apps have been tried.
fn publish(repo: &Repo, store: &Store, ui: &mut dyn Ui) -> Result<()> {
    let version = store.version;
    let tag = version.tag();
    let mut steps = vec![format!(
        "Publish the draft GitHub release {tag}, with its notes, as the latest release; the apps themselves are \
         released in App Store Connect"
    )];
    let next = app::next_version(repo, version)?;
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
    crate::process::output(&repo.dir, "gh", &["release", "edit", &tag, "--draft=false", "--latest"])?;
    app::finish(repo, version, next, ui)
}
