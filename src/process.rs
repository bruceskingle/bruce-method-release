//! Running the programs the tool drives: git, gh and cargo.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// What a program which ran wrote, and whether it succeeded.
pub struct Ran {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    /// The last lines of what it wrote to stderr, or else stdout, to explain a failure briefly.
    pub fn tail(&self, lines: usize) -> String {
        let text = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        let all: Vec<&str> = text.trim_end().lines().collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }
}

/// Runs a program in a directory, with nothing on its stdin so that it cannot stop to ask anything, and returns what it
/// wrote, whether or not it succeeded. Fails only if the program cannot be run at all.
pub fn run(dir: &Path, program: &str, args: &[&str]) -> Result<Ran> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not run {program}, is it installed?"))?;
    Ok(Ran {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Runs a program in a directory and returns what it wrote to stdout, without the trailing newline, failing with what
/// it wrote to stderr if it fails.
pub fn output(dir: &Path, program: &str, args: &[&str]) -> Result<String> {
    let ran = run(dir, program, args)?;
    if !ran.success {
        bail!(
            "{} failed in {}: {}",
            describe(program, args),
            dir.display(),
            ran.tail(20)
        );
    }
    Ok(ran.stdout.trim_end().to_string())
}

/// Runs a program in a directory with its output shown as it goes, for long steps such as publishing, failing if it
/// fails.
pub fn visible(dir: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("could not run {program}, is it installed?"))?;
    if !status.success() {
        bail!("{} failed in {}", describe(program, args), dir.display());
    }
    Ok(())
}

/// The command line, for messages.
pub fn describe(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}
