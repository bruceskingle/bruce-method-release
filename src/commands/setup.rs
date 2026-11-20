//! `setup`: making a repository's workflows and standard files from the method's templates, and keeping them so, see
//! the README's Setup.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use toml_edit::DocumentMut;

use crate::manifest::{self, AppSettings, Kind, StoreApp, StoreSettings};
use crate::process;
use crate::repo::Repo;
use crate::ui::Ui;

/// How setup treats a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Made wholly from its template: setup replaces it, and `check` fails while it differs.
    Owned,
    /// Made from its template, keeping the repository's own part, between markers, e.g. deny.toml's approvals.
    Merged,
    /// Written from its template only if it is missing, then the repository's own, e.g. its licences, whose years
    /// differ, or its release notes.
    Once,
}

/// Where a file is, compared with what setup would make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Missing,
    UpToDate,
    Differs,
    /// A `Once` file which is there, and so left as it is.
    Kept,
}

/// A file setup makes.
pub struct File {
    /// Its path, relative to the checkout.
    pub path: String,
    pub mode: Mode,
    pub state: State,
    /// What setup would write.
    pub content: String,
}

impl File {
    /// Whether setup would write it.
    pub fn to_write(&self) -> bool {
        matches!(self.state, State::Missing | State::Differs)
    }
}

/// A repository's setting on GitHub, and whether it is as the method wants it.
pub struct Setting {
    pub what: String,
    pub right: bool,
    /// The arguments to `gh` which make it right.
    pub fix: Vec<String>,
}

/// What setup would do to a repository.
pub struct Setup {
    pub files: Vec<File>,
    pub settings: Vec<Setting>,
    /// The secrets the workflows use, by name, and whether the repository has each.
    pub secrets: Vec<(String, bool)>,
}

/// A part of deny.toml which is the repository's own, which setup keeps: a list, kept between markers, or, before
/// setup has made the file, the whole of the list.
struct Kept {
    start: &'static str,
    end: &'static str,
    /// The line which opens the list, e.g. `exceptions = [`.
    list: &'static str,
}

/// The crates the repository approves by name, with their licences, in `[licenses]`.
const APPROVALS: Kept = Kept {
    start: "    # This repository's approvals, which setup keeps: each crate, the licences approved for it, and what pulls it in.",
    end: "    # The end of this repository's approvals.",
    list: "exceptions = [",
};

/// The advisories the repository ignores, each with why it does not apply, in `[advisories]`.
const IGNORED: Kept = Kept {
    start: "    # This repository's ignored advisories, which setup keeps: each one, and why it does not apply or cannot be fixed.",
    end: "    # The end of this repository's ignored advisories.",
    list: "ignore = [",
};

/// The secrets an app's release workflow uses, to sign and notarize the macOS app, see its template.
const APPLE_SECRETS: [&str; 5] = [
    "APPLE_CERTIFICATE",
    "APPLE_CERTIFICATE_PASSWORD",
    "APPLE_API_KEY",
    "APPLE_API_ISSUER",
    "APPLE_API_KEY_CONTENT",
];

/// The secrets an App Store workflow uses: the team, and the App Store Connect API key, with which it signs and
/// uploads, see its template.
const STORE_SECRETS: [&str; 4] = [
    "APPLE_TEAM_ID",
    "APPLE_API_KEY",
    "APPLE_API_ISSUER",
    "APPLE_API_KEY_CONTENT",
];

/// Works out what setup would do, changing nothing. With `github`, it also asks GitHub for the repository's settings
/// and secrets; `check` leaves them out, as it checks only the checkout.
pub fn plan(repo: &Repo, github: bool) -> Result<Setup> {
    if !repo.is_workspace() {
        bail!(
            "{} is a repository of separate crates, e.g. a platform's boards, which setup does not make files for yet",
            repo.name
        );
    }
    let manifests = repo.manifests()?;
    let version = manifests.version()?;
    let mut files = Vec::new();
    let mut add = |path: &str, mode: Mode, content: String| -> Result<()> {
        let state = state_of(&repo.dir.join(path), mode, &content)?;
        files.push(File {
            path: path.to_string(),
            mode,
            state,
            content,
        });
        Ok(())
    };

    add(
        "rustfmt.toml",
        Mode::Owned,
        render(RUSTFMT, &[("header", &header("rustfmt.toml", Mode::Owned))])?,
    )?;
    let app = matches!(manifests.kind, Kind::App | Kind::AppStore);
    let (multiple, why) = if app {
        (
            "allow",
            "An app's frameworks, e.g. Dioxus, pull in several versions of some crates, which cannot be helped.",
        )
    } else {
        (
            "warn",
            "Several versions of one crate in the build are shown, to be tidied when they can be.",
        )
    };
    let approvals = kept(&repo.dir.join("deny.toml"), &APPROVALS)?;
    let ignored = kept(&repo.dir.join("deny.toml"), &IGNORED)?;
    add(
        "deny.toml",
        Mode::Merged,
        render(
            DENY,
            &[
                ("header", &header("deny.toml", Mode::Merged)),
                ("approvals", &approvals),
                ("ignored", &ignored),
                ("multiple_versions", multiple),
                ("multiple_versions_why", why),
            ],
        )?,
    )?;

    let mut secrets = Vec::new();
    if manifests.kind == Kind::AppStore {
        let settings = manifest::store_settings(&repo.dir)?;
        let workflow = format!(".github/workflows/{}", settings.workflow);
        let content = app_store_release(repo, &settings, &header("release-app-store.yml", Mode::Owned))?;
        add(&workflow, Mode::Owned, content)?;
        add(
            ".github/workflows/ci.yml",
            Mode::Owned,
            app_store_ci(&settings, &header("ci-app-store.yml", Mode::Owned))?,
        )?;
        secrets.extend(STORE_SECRETS.iter().map(|name| name.to_string()));
    } else if app {
        let settings = manifest::app_settings(&repo.dir)?;
        let workflow = format!(".github/workflows/{}", settings.workflow);
        let content = dioxus_release(repo, &settings, &header("release-dioxus.yml", Mode::Owned))?;
        add(&workflow, Mode::Owned, content)?;
        secrets.extend(APPLE_SECRETS.iter().map(|name| name.to_string()));
    } else {
        add(
            ".github/workflows/ci.yml",
            Mode::Owned,
            render(CI, &[("header", &header("ci.yml", Mode::Owned))])?,
        )?;
    }

    let year = current_year().to_string();
    let holder = repo.git(&["config", "user.name"]).unwrap_or_default();
    add(
        "LICENSE-MIT",
        Mode::Once,
        render(LICENSE_MIT, &[("year", &year), ("holder", &holder)])?,
    )?;
    add("LICENSE-APACHE", Mode::Once, LICENSE_APACHE.to_string())?;
    add(
        "docs/ChangeLog.md",
        Mode::Once,
        render(CHANGELOG, &[("version", &version.to_string())])?,
    )?;
    if !app {
        add("docs/Release.md", Mode::Once, render(RELEASE, &[("repo", &repo.name)])?)?;
    }

    let (settings, secrets) = if github {
        (github_settings(repo)?, secrets_present(repo, secrets)?)
    } else {
        (Vec::new(), secrets.into_iter().map(|name| (name, true)).collect())
    };
    Ok(Setup {
        files,
        settings,
        secrets,
    })
}

/// Makes the repository's files from the templates, and its GitHub settings as the method wants them, after listing
/// what it will do and asking. Secrets are only listed, by name, as their values are the maintainer's to set.
pub fn setup(repo: &Repo, ui: &mut dyn Ui) -> Result<()> {
    let plan = plan(repo, true)?;
    let mut steps = Vec::new();
    for file in plan.files.iter().filter(|file| file.to_write()) {
        let how = match (file.state, file.mode) {
            (State::Missing, _) => "Create",
            (_, Mode::Merged) => "Update, keeping this repository's own parts,",
            _ => "Replace",
        };
        steps.push(format!("{how} {} from its template", file.path));
    }
    for setting in plan.settings.iter().filter(|setting| !setting.right) {
        steps.push(format!("On GitHub: {}", setting.what));
    }
    for (name, present) in &plan.secrets {
        if !present {
            ui.info(&format!(
                "The workflow uses the secret {name}, which {} does not have; set it with: gh secret set {name}",
                repo.name
            ));
        }
    }
    if steps.is_empty() {
        ui.info(&format!("{} is set up as the templates make it", repo.name));
        return Ok(());
    }
    if !ui.confirm(&format!("Set up {}", repo.name), &steps)? {
        ui.info("Cancelled, nothing changed");
        return Ok(());
    }
    for file in plan.files.iter().filter(|file| file.to_write()) {
        let path = repo.dir.join(&file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("could not create {}", parent.display()))?;
        }
        fs::write(&path, &file.content).with_context(|| format!("could not write {}", path.display()))?;
        ui.info(&format!("{}: wrote {}", repo.name, file.path));
    }
    for setting in plan.settings.iter().filter(|setting| !setting.right) {
        let args: Vec<&str> = setting.fix.iter().map(String::as_str).collect();
        process::output(&repo.dir, "gh", &args)?;
        ui.info(&format!("{}: {}", repo.name, setting.what));
    }
    ui.info("Review the changes with git diff, then commit them");
    Ok(())
}

/// The files `check` finds not as setup would make them, each with why, e.g. `.github/workflows/ci.yml differs`.
pub fn drift(repo: &Repo) -> Result<Vec<String>> {
    Ok(plan(repo, false)?
        .files
        .into_iter()
        .filter_map(|file| match file.state {
            State::Missing => Some(format!("{} is missing", file.path)),
            State::Differs => Some(format!("{} differs", file.path)),
            State::UpToDate | State::Kept => None,
        })
        .collect())
}

fn state_of(path: &Path, mode: Mode, content: &str) -> Result<State> {
    if !path.exists() {
        return Ok(State::Missing);
    }
    if mode == Mode::Once {
        return Ok(State::Kept);
    }
    let current = fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    // Git may check a file out with Windows line endings.
    Ok(if current.replace("\r\n", "\n") == content {
        State::UpToDate
    } else {
        State::Differs
    })
}

const CI: &str = include_str!("../../templates/ci.yml");
const DENY: &str = include_str!("../../templates/deny.toml");
const RUSTFMT: &str = include_str!("../../templates/rustfmt.toml");
const RELEASE_DIOXUS: &str = include_str!("../../templates/release-dioxus.yml");
const RELEASE_APP_STORE: &str = include_str!("../../templates/release-app-store.yml");
const CI_APP_STORE: &str = include_str!("../../templates/ci-app-store.yml");
const STORE_SETUP_STEPS: &str = include_str!("../../templates/app-store/setup-steps.yml");
const STORE_EXPORT_STEPS: &str = include_str!("../../templates/app-store/export-steps.yml");
const STORE_JOB_IOS: &str = include_str!("../../templates/app-store/job-ios.yml");
const STORE_JOB_MACOS: &str = include_str!("../../templates/app-store/job-macos.yml");
const LICENSE_MIT: &str = include_str!("../../templates/LICENSE-MIT");
const LICENSE_APACHE: &str = include_str!("../../templates/LICENSE-APACHE");
const RELEASE: &str = include_str!("../../templates/Release.md");
const CHANGELOG: &str = include_str!("../../templates/ChangeLog.md");

/// The comment at the top of a generated file, saying where it came from and how to change it.
fn header(template: &str, mode: Mode) -> String {
    let own = if mode == Mode::Merged {
        ", except for this repository's approvals and ignored advisories below, which setup keeps"
    } else {
        ""
    };
    comment(&format!(
        "Made by bruce-method-release's setup from its {template} template{own}. Change the template, or this \
         repository's hooks, not this file: setup replaces it, and the release checks fail while it differs."
    ))
}

/// Text as `#` comment lines, wrapped at 120 characters, as the files are.
fn comment(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::from("#");
    for word in text.split_whitespace() {
        if line.len() + 1 + word.len() > 120 && line != "#" {
            lines.push(std::mem::replace(&mut line, String::from("#")));
        }
        line.push(' ');
        line.push_str(word);
    }
    lines.push(line);
    lines.join("\n")
}

/// A template with each `{{name}}` replaced by its value, failing if any is left, e.g. one misspelt. GitHub's own
/// expressions, `${{ ... }}`, have spaces, so are left alone.
pub fn render(template: &str, values: &[(&str, &str)]) -> Result<String> {
    let mut text = template.to_string();
    for (name, value) in values {
        // A value which is empty leaves no blank line where it was on a line of its own.
        if value.is_empty() {
            text = text.replace(&format!("{{{{{name}}}}}\n"), "");
        }
        text = text.replace(&format!("{{{{{name}}}}}"), value);
    }
    if let Some(start) = text.find("{{")
        && text[start + 2..].starts_with(|c: char| c.is_ascii_lowercase())
    {
        let end = text[start..].find("}}").map_or(text.len(), |end| start + end + 2);
        bail!("the template has no value for {}", &text[start..end]);
    }
    Ok(text)
}

/// A part of the repository's deny.toml which is its own: what is between setup's markers, or, before setup has made it,
/// the whole of the list.
fn kept(path: &Path, part: &Kept) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let text = fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let between = |start: usize, end: usize| lines[start + 1..end].join("\n");
    let kept = if let (Some(start), Some(end)) = (
        lines.iter().position(|line| *line == part.start),
        lines.iter().position(|line| *line == part.end),
    ) {
        (start < end).then(|| between(start, end))
    } else if let Some(start) = lines.iter().position(|line| line.trim_start().starts_with(part.list))
        && !lines[start].contains(']')
    {
        lines[start + 1..]
            .iter()
            .position(|line| line.trim() == "]")
            .map(|end| between(start, start + 1 + end))
    } else {
        None
    };
    Ok(kept
        .map(|kept| kept.trim_matches('\n').to_string())
        .filter(|kept| !kept.trim().is_empty())
        .unwrap_or_default())
}

/// A Dioxus desktop app's release workflow, from its settings, Cargo.lock's version of Dioxus, and Dioxus.toml's
/// programs installed beside it.
fn dioxus_release(repo: &Repo, settings: &AppSettings, header: &str) -> Result<String> {
    let dioxus_toml = repo.dir.join("Dioxus.toml");
    if !dioxus_toml.exists() {
        bail!(
            "{} is an app with no Dioxus.toml: setup makes release workflows only for Dioxus desktop apps yet",
            repo.name
        );
    }
    let dx_version = locked_version(&repo.dir.join("Cargo.lock"), "dioxus")?
        .with_context(|| format!("{}'s Cargo.lock has no dioxus", repo.name))?;
    let text = fs::read_to_string(&dioxus_toml).with_context(|| format!("could not read {}", dioxus_toml.display()))?;
    let document: DocumentMut = text.parse().context("could not read Dioxus.toml")?;
    let beside: Vec<String> = document
        .get("bundle")
        .and_then(|bundle| bundle.get("external_bin"))
        .and_then(|bins| bins.as_array())
        .map(|bins| {
            bins.iter()
                .filter_map(|bin| bin.as_str())
                .filter_map(|bin| Path::new(bin).file_name())
                .map(|name| format!("'{}.exe'", name.to_string_lossy()))
                .collect()
        })
        .unwrap_or_default();
    let package = repo
        .manifests()?
        .crates
        .first()
        .map(|krate| krate.name.clone())
        .unwrap_or_default();
    let exe_pattern = format!("*{}*", package.split('-').collect::<Vec<_>>().join("*"));
    let mut matrix = Vec::new();
    for installer in &settings.installers {
        let (name, runner) = runner_of(&installer.platform)?;
        matrix.push(format!(
            "          - name: {name}\n            runner: {runner}\n            artifact: {}",
            installer.platform
        ));
    }
    render(
        RELEASE_DIOXUS,
        &[
            ("header", header),
            ("repo", &repo.name),
            ("dx_version", &dx_version),
            ("matrix", &matrix.join("\n")),
            ("beside", &beside.join(", ")),
            ("exe_pattern", &exe_pattern),
        ],
    )
}

/// An App Store repository's release workflow: a job for each app, which builds, signs and exports it, and uploads it
/// when asked.
fn app_store_release(repo: &Repo, settings: &StoreSettings, header: &str) -> Result<String> {
    let setup_steps = render(STORE_SETUP_STEPS, &[("repo", &repo.name)])?;
    let setup_steps = setup_steps.trim_end();
    let mut jobs = Vec::new();
    let mut used: Vec<String> = Vec::new();
    for app in &settings.apps {
        let platform = match app {
            StoreApp::Ios { .. } => "ios",
            StoreApp::Macos { .. } => "macos",
        };
        // Each job's own ID, the platform's, numbered if there are two of a platform.
        let count = used.iter().filter(|job| job.starts_with(platform)).count();
        let job = if count == 0 {
            platform.to_string()
        } else {
            format!("{platform}-{}", count + 1)
        };
        used.push(job.clone());
        let export = |what: &str| {
            render(STORE_EXPORT_STEPS, &[("what", what), ("artifact", &job)]).map(|text| text.trim_end().to_string())
        };
        let rendered = match app {
            StoreApp::Ios { project, scheme, .. } => render(
                STORE_JOB_IOS,
                &[
                    ("job", &job),
                    ("project", project),
                    ("scheme", scheme),
                    ("setup_steps", setup_steps),
                    ("export_steps", &export("the iOS app")?),
                ],
            )?,
            StoreApp::Macos {
                dioxus,
                features,
                entitlements,
                category,
                exempt_encryption,
            } => {
                if !repo.dir.join(dioxus).join("Dioxus.toml").exists() {
                    bail!("{} has no Dioxus.toml in {dioxus}, the Mac app's folder", repo.name);
                }
                if !repo.dir.join(entitlements).exists() {
                    bail!("{} has no {entitlements}, the Mac app's entitlements", repo.name);
                }
                let dx_version = locked_version(&repo.dir.join("Cargo.lock"), "dioxus")?
                    .with_context(|| format!("{}'s Cargo.lock has no dioxus", repo.name))?;
                let features = if features.is_empty() {
                    String::new()
                } else {
                    format!(" --features {}", features.join(","))
                };
                let encryption = if *exempt_encryption {
                    "\n          plutil -replace ITSAppUsesNonExemptEncryption -bool NO \"$info\""
                } else {
                    ""
                };
                render(
                    STORE_JOB_MACOS,
                    &[
                        ("job", &job),
                        ("dioxus", dioxus),
                        ("dx_version", &dx_version),
                        ("features", &features),
                        ("entitlements", entitlements),
                        ("category", category),
                        ("encryption", encryption),
                        ("setup_steps", setup_steps),
                        ("export_steps", &export("the Mac app")?),
                    ],
                )?
            }
        };
        jobs.push(rendered.trim_end().to_string());
    }
    render(
        RELEASE_APP_STORE,
        &[
            ("header", header),
            (
                "jobs",
                &jobs.join(
                    "

",
                ),
            ),
        ],
    )
}

/// An App Store repository's CI: Rust's tests and the iOS apps' Swift packages', on a Mac, and cargo-deny's checks.
fn app_store_ci(settings: &StoreSettings, header: &str) -> Result<String> {
    let swift_tests: Vec<String> = settings
        .apps
        .iter()
        .filter_map(|app| match app {
            StoreApp::Ios {
                swift_package: Some(package),
                ..
            } => Some(format!("      - run: swift test --package-path {package}")),
            _ => None,
        })
        .collect();
    render(
        CI_APP_STORE,
        &[("header", header), ("swift_tests", &swift_tests.join("\n"))],
    )
}

/// The name shown for a platform's build, and the GitHub runner which builds it.
fn runner_of(platform: &str) -> Result<(&'static str, &'static str)> {
    Ok(match platform {
        "macos-aarch64" => ("macOS Apple Silicon", "macos-latest"),
        "macos-x64" => ("macOS Intel", "macos-15-intel"),
        "windows-x64" => ("Windows x64", "windows-latest"),
        "windows-arm64" => ("Windows ARM", "windows-11-arm"),
        other => bail!("setup does not know which GitHub runner builds {other}'s installer"),
    })
}

/// The version of a package which a Cargo.lock has, the first if it has several.
fn locked_version(lock: &Path, name: &str) -> Result<Option<String>> {
    let text = fs::read_to_string(lock).with_context(|| format!("could not read {}", lock.display()))?;
    let document: DocumentMut = text.parse().context("could not read Cargo.lock")?;
    Ok(document
        .get("package")
        .and_then(|packages| packages.as_array_of_tables())
        .and_then(|packages| {
            packages
                .iter()
                .find(|package| package.get("name").and_then(|item| item.as_str()) == Some(name))
        })
        .and_then(|package| package.get("version"))
        .and_then(|version| version.as_str())
        .map(str::to_string))
}

/// The repository's settings on GitHub which the method needs: pull requests squash merged, as a release is one
/// commit on main, and no other way of merging, so that none is chosen by mistake.
fn github_settings(repo: &Repo) -> Result<Vec<Setting>> {
    let json = process::output(
        &repo.dir,
        "gh",
        &[
            "repo",
            "view",
            "--json",
            "squashMergeAllowed,mergeCommitAllowed,rebaseMergeAllowed",
        ],
    )?;
    let view: serde_json::Value = serde_json::from_str(&json).context("could not read gh's repository")?;
    let right = view["squashMergeAllowed"] == true
        && view["mergeCommitAllowed"] == false
        && view["rebaseMergeAllowed"] == false;
    Ok(vec![Setting {
        what: "allow pull requests to be merged only by squash merge".to_string(),
        right,
        fix: [
            "repo",
            "edit",
            "--enable-squash-merge",
            "--enable-merge-commit=false",
            "--enable-rebase-merge=false",
        ]
        .map(str::to_string)
        .to_vec(),
    }])
}

/// Which of the secrets the repository has on GitHub, asking only for their names.
fn secrets_present(repo: &Repo, names: Vec<String>) -> Result<Vec<(String, bool)>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let listed = process::output(
        &repo.dir,
        "gh",
        &["secret", "list", "--json", "name", "--jq", ".[].name"],
    )?;
    let have: Vec<&str> = listed.lines().collect();
    Ok(names
        .into_iter()
        .map(|name| {
            let present = have.contains(&name.as_str());
            (name, present)
        })
        .collect())
}

/// This year, by the computer's clock, for a new licence's copyright line.
fn current_year() -> i64 {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    year_of_day(seconds.div_euclid(86_400))
}

/// The year of a day counted from 1 January 1970, by the civil calendar, see Howard Hinnant's days_from_civil.
fn year_of_day(days: i64) -> i64 {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    yoe + era * 400 + i64::from(month <= 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_refuses_missing_values() {
        assert_eq!(
            render("a {{x}} ${{ github.x }}", &[("x", "1")]).unwrap(),
            "a 1 ${{ github.x }}"
        );
        assert_eq!(render("a\n{{x}}\nb", &[("x", "")]).unwrap(), "a\nb");
        assert!(render("a {{y}}", &[("x", "1")]).is_err());
    }

    #[test]
    fn wraps_comments() {
        let long = "word ".repeat(40);
        for line in comment(&long).lines() {
            assert!(line.len() <= 120 && line.starts_with("# "), "{line}");
        }
        assert_eq!(comment("a b"), "# a b");
    }

    #[test]
    fn finds_the_year() {
        assert_eq!(year_of_day(0), 1970);
        assert_eq!(year_of_day(20_454), 2026);
        assert_eq!(year_of_day(-1), 1969);
    }

    #[test]
    fn keeps_the_approvals() {
        let dir = std::env::temp_dir().join(format!("bruce-method-release-approvals-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deny.toml");
        fs::write(
            &path,
            "[licenses]\nexceptions = [\n    # Why.\n    { crate = \"a\", allow = [\"MIT\"] },\n]\n",
        )
        .unwrap();
        assert_eq!(
            kept(&path, &APPROVALS).unwrap(),
            "    # Why.\n    { crate = \"a\", allow = [\"MIT\"] },"
        );
        let made = render(
            DENY,
            &[
                ("header", "# H"),
                ("approvals", &kept(&path, &APPROVALS).unwrap()),
                ("ignored", ""),
                ("ignored", ""),
                ("multiple_versions", "warn"),
                ("multiple_versions_why", "Why."),
            ],
        )
        .unwrap();
        fs::write(&path, &made).unwrap();
        assert_eq!(
            kept(&path, &APPROVALS).unwrap(),
            "    # Why.\n    { crate = \"a\", allow = [\"MIT\"] },"
        );
        fs::write(&path, "[licenses]\nexceptions = []\n").unwrap();
        assert_eq!(kept(&path, &APPROVALS).unwrap(), "");
        fs::remove_dir_all(&dir).unwrap();
    }
}
