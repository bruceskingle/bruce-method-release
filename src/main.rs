//! The command line of bruce-method-release, see the README.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use bruce_method_release::commands::check::Outcome;
use bruce_method_release::commands::dependencies::{self, Verdict};
use bruce_method_release::commands::link::{self, State};
use bruce_method_release::commands::{check, release, start, status, sync};
use bruce_method_release::ui::Ui;
use bruce_method_release::{Repo, Version};

/// Developing and releasing a set of Rust repositories which depend on one another, cloned side by side.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// The repository to work in, by default the one the current directory is in.
    #[arg(long, global = true)]
    repo: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Shows the repository's branch, version, changes and links, and those of the libraries it links.
    Status,
    /// Starts a version's branch: with no version, the next minor version from main; with a fix's, from its release.
    Start { version: Option<Version> },
    /// Runs the checks a release needs, changing nothing.
    Check,
    /// Releases the version on the repository's branch, and starts the next one.
    Release,
    /// Builds a library from its checkout next to this repository, while both are developed.
    Link { library: String },
    /// Removes the links to a library, or else every link no longer needed.
    Unlink { library: Option<String> },
    /// Brings the checkout, and the libraries it links, up to date with GitHub, by fast forward only.
    Sync { branch: Option<String> },
    /// Lists the libraries next to this repository which it depends on, in the order to release them, each with a tick
    /// if it is ready or a cross and what is to be done, changing nothing.
    Dependencies {
        /// Lists who takes each library and how, and the versions asked of crates.io, too.
        #[arg(short, long)]
        verbose: bool,
    },
}

/// The command line's way of reporting and asking.
struct Terminal;

impl Ui for Terminal {
    fn info(&mut self, message: &str) {
        println!("{message}");
    }

    /// Lists the steps and asks once, `y` and Return to go ahead; anything else, including Return alone, cancels.
    fn confirm(&mut self, title: &str, steps: &[String]) -> Result<bool> {
        println!();
        println!("{title}:");
        for (index, step) in steps.iter().enumerate() {
            println!("  {}. {step}", index + 1);
        }
        print!("Go ahead? [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        let answer = answer.trim().to_lowercase();
        Ok(answer == "y" || answer == "yes")
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Runs the command, returning whether it succeeded, e.g. false for a check which failed.
fn run(cli: Cli) -> Result<bool> {
    let dir = match cli.repo {
        Some(dir) => dir,
        None => std::env::current_dir()?,
    };
    let repo = Repo::containing(&dir)?;
    let ui = &mut Terminal;
    let mut succeeded = true;
    match &cli.command {
        Command::Status => print_status(&repo)?,
        Command::Start { version } => start::start(&repo, *version, ui)?,
        Command::Check => succeeded = print_checks(&check::check(&repo, ui)?),
        Command::Release => release::release(&repo, ui)?,
        Command::Link { library } => link::link(&repo, library, ui)?,
        Command::Unlink { library } => link::unlink(&repo, library.as_deref(), ui)?,
        Command::Sync { branch } => sync::sync(&repo, branch.as_deref(), ui)?,
        Command::Dependencies { verbose: true } => {
            succeeded = print_dependencies(&dependencies::dependencies(&repo, ui)?)
        }
        Command::Dependencies { verbose: false } => {
            succeeded = print_dependencies_briefly(&dependencies::dependencies(&repo, &mut Quiet)?)
        }
    }
    if !matches!(
        cli.command,
        Command::Status | Command::Unlink { .. } | Command::Release | Command::Dependencies { .. }
    ) {
        report_removable_links(&repo)?;
    }
    Ok(succeeded)
}

/// Says which of the repository's links are no longer needed, as every command does, see the README.
fn report_removable_links(repo: &Repo) -> Result<()> {
    let removable: Vec<_> = link::links_of(repo)?
        .into_iter()
        .filter(|link| link.state.removable())
        .collect();
    if !removable.is_empty() {
        println!();
        for link in &removable {
            println!(
                "{}'s link of {} is {}",
                repo.name,
                link.patch.name,
                link.state.describe()
            );
        }
        println!("Remove with: bruce-method-release unlink");
    }
    Ok(())
}

fn print_status(repo: &Repo) -> Result<()> {
    for each in status::status(repo)? {
        println!("{}", each.name);
        let version = match &each.version {
            Ok(version) => version.to_string(),
            Err(error) => error.clone(),
        };
        println!(
            "  branch {}, version {version}",
            each.branch.as_deref().unwrap_or("(none)")
        );
        let mut changes = Vec::new();
        if each.uncommitted > 0 {
            changes.push(format!("{} files not committed", each.uncommitted));
        }
        match each.ahead_behind {
            Some((ahead, behind)) => {
                if ahead > 0 {
                    changes.push(format!("{ahead} commits not pushed"));
                }
                if behind > 0 {
                    changes.push(format!("{behind} commits on GitHub not pulled, as last fetched"));
                }
            }
            None => changes.push("not on GitHub".to_string()),
        }
        if changes.is_empty() {
            changes.push("up to date with GitHub, as last fetched".to_string());
        }
        println!("  {}", changes.join(", "));
        match each.latest_release {
            Some(release) => println!("  last released {release}, tag {}", release.tag()),
            None => println!("  not released yet"),
        }
        for link in &each.links {
            let local = link
                .local
                .map(|version| version.to_string())
                .unwrap_or_else(|| "?".to_string());
            let marker = if link.state == State::Linked { "" } else { " !" };
            println!(
                "  link{marker}: {} {local} from {}, requires {}: {}",
                link.patch.name,
                link.patch.path,
                if link.requirements.is_empty() {
                    "nothing".to_string()
                } else {
                    link.requirements.join(", ")
                },
                link.state.describe()
            );
        }
    }
    Ok(())
}

/// A Ui which says nothing as it goes, for a command whose result alone is wanted, e.g. `dependencies` without `-v`.
struct Quiet;

impl Ui for Quiet {
    fn info(&mut self, _message: &str) {}

    fn confirm(&mut self, _title: &str, _steps: &[String]) -> Result<bool> {
        Ok(false)
    }
}

/// Lists the libraries in the order to release them, a line each, a green tick for one which is ready, a red cross
/// and what is to be done for one which is not, returning whether they are all ready. Colours only on a terminal.
fn print_dependencies_briefly(dependencies: &dependencies::Dependencies) -> bool {
    use std::io::IsTerminal;
    let colour = std::io::stdout().is_terminal();
    let (tick, cross) = if colour {
        ("\x1b[32m✓\x1b[0m", "\x1b[31m✗\x1b[0m")
    } else {
        ("✓", "✗")
    };
    let width = dependencies
        .libraries
        .iter()
        .map(|library| library.repo.name.len())
        .max()
        .unwrap_or(0);
    let mut ready = true;
    for library in &dependencies.libraries {
        let mark = if library.ready() { tick } else { cross };
        ready &= library.ready();
        println!("{mark} {:width$}  {}", library.repo.name, library.summary());
    }
    if !dependencies.circular.is_empty() {
        ready = false;
        println!(
            "{cross} {} use each other, so no order releases them",
            dependencies.circular.join(", ")
        );
    }
    if !dependencies.unreadable.is_empty() {
        println!(
            "! {} Cargo.toml files could not be read, see -v",
            dependencies.unreadable.len()
        );
    }
    ready
}

/// Lists the libraries in the order to release them, each with what is to be done and who uses it, returning whether
/// nothing needs releasing.
fn print_dependencies(dependencies: &dependencies::Dependencies) -> bool {
    println!();
    if dependencies.libraries.is_empty() {
        println!("{} depends on none of the libraries next to it", dependencies.repo);
        return true;
    }
    println!(
        "{} depends on these libraries next to it, in the order to release them:",
        dependencies.repo
    );
    let mut to_release = 0;
    for (index, library) in dependencies.libraries.iter().enumerate() {
        println!();
        let version = match &library.version {
            Ok(version) => format!("checkout {version}"),
            Err(error) => format!("checkout version unknown: {error}"),
        };
        let released = match library.latest_release {
            Some(release) => format!("released {release}"),
            None => "not released yet".to_string(),
        };
        println!("{}. {}, {version}, {released}", index + 1, library.repo.name);
        match &library.verdict {
            Verdict::Release(version) => {
                to_release += 1;
                println!("   RELEASE {version}");
            }
            Verdict::Published => println!("   nothing to release: every version needed is on crates.io"),
            Verdict::Mismatch(why) => {
                to_release += 1;
                println!("   CANNOT RELEASE THE CHECKOUT: {why}");
            }
        }
        for problem in &library.problems {
            println!("   ! {problem}");
        }
        // Uses alike are put on one line, e.g. a repository's examples which all take it by path.
        let mut groups: Vec<(&dependencies::Use, Vec<&str>)> = Vec::new();
        for each in &library.uses {
            match groups.iter_mut().find(|(first, _)| {
                first.user_repo == each.user_repo
                    && first.library_crate == each.library_crate
                    && first.requirement == each.requirement
                    && first.how == each.how
            }) {
                Some((_, crates)) => crates.push(&each.user_crate),
                None => groups.push((each, vec![&each.user_crate])),
            }
        }
        for (each, crates) in groups {
            let user = match crates.as_slice() {
                [only] if *only == each.user_repo => each.user_repo.clone(),
                [only] => format!("{only} ({})", each.user_repo),
                many => format!("{} crates of {}", many.len(), each.user_repo),
            };
            let requirement = if each.requirement == "*" {
                "any version".to_string()
            } else {
                each.requirement.clone()
            };
            let crate_name = if each.library_crate == library.repo.name {
                String::new()
            } else {
                format!("{} ", each.library_crate)
            };
            let on_crates_io = match (each.needs, each.published) {
                (Some(needs), Some(true)) => format!(", {needs} is on crates.io"),
                (Some(needs), Some(false)) => format!(", {needs} is not on crates.io"),
                (Some(needs), None) => format!(", could not ask crates.io for {needs}"),
                (None, _) => String::new(),
            };
            println!(
                "     {user} {} {crate_name}{requirement} {}{on_crates_io}",
                if crates.len() > 1 { "take" } else { "takes" },
                each.how.describe()
            );
        }
        if library.taken_from_checkout() {
            let mut users: Vec<&str> = library
                .uses
                .iter()
                .filter(|each| each.how != dependencies::How::CratesIo && each.published == Some(true))
                .map(|each| each.user_repo.as_str())
                .collect();
            users.sort();
            users.dedup();
            println!(
                "   to take it from crates.io instead: bruce-method-release unlink {} in {}",
                library.repo.name,
                users.join(", ")
            );
        }
    }
    println!();
    if !dependencies.circular.is_empty() {
        println!(
            "! {} use each other, so no order releases them",
            dependencies.circular.join(", ")
        );
    }
    for unreadable in &dependencies.unreadable {
        println!("! could not read {unreadable}");
    }
    if to_release == 0 {
        println!("Nothing to release: {} can be built from crates.io", dependencies.repo);
    } else {
        println!("{to_release} to release, in that order, then {}", dependencies.repo);
    }
    to_release == 0
}

/// Lists the checks' outcomes, returning whether they all passed.
fn print_checks(checks: &[check::Check]) -> bool {
    println!();
    for check in checks {
        match &check.outcome {
            Outcome::Pass => println!("  pass  {}", check.name),
            Outcome::Fail(why) => println!("  FAIL  {}: {why}", check.name),
            Outcome::Skip(why) => println!("  skip  {}: {why}", check.name),
        }
    }
    let failed = checks.iter().filter(|check| check.failed()).count();
    if failed == 0 {
        println!("Ready to release");
    } else {
        println!("{failed} checks failed");
    }
    failed == 0
}
