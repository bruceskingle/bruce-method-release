//! The command line of bruce-method-release, see the README.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use bruce_method_release::commands::check::Outcome;
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
    }
    if !matches!(cli.command, Command::Status | Command::Unlink { .. } | Command::Release) {
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
