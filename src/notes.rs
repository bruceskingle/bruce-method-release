//! A release's notes, from its section of the ChangeLog, and made ready for its GitHub release.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::repo::Repo;
use crate::version::Version;

/// The ChangeLog, in which each release has a `Release X.Y.Z` section, under a heading of any level, e.g.
/// `# Release 0.2.0` in a library's, or `## Release 0.5.0` under a `# Change Log` title in an app's.
pub const CHANGELOG: &str = "docs/ChangeLog.md";

/// The notes of a version in the ChangeLog in the checkout, the text of its section, or None if it has none.
pub fn changelog_notes(dir: &Path, version: Version) -> Result<Option<String>> {
    let file = dir.join(CHANGELOG);
    if !file.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&file).with_context(|| format!("could not read {}", file.display()))?;
    Ok(notes_in(&text, version))
}

/// The notes for a version's GitHub release, from the ChangeLog as it is at the release's tag, so that they are the
/// notes of what was released whatever the checkout has since: the section's lines unwrapped, as GitHub shows each line
/// break, and its relative links made links to the docs on GitHub at the tag, as the release's page is not in `docs/`.
/// None if the ChangeLog at the tag has no section for the version.
pub fn release_notes(repo: &Repo, version: Version) -> Result<Option<String>> {
    let tag = version.tag();
    let text = repo
        .git(&["show", &format!("{tag}:{CHANGELOG}")])
        .with_context(|| format!("{}: could not read {CHANGELOG} at {tag}", repo.name))?;
    let Some(notes) = notes_in(&text, version) else {
        return Ok(None);
    };
    let docs = format!("{}/blob/{tag}/docs/", repo.github_url()?);
    Ok(Some(absolute_links(&unwrap_markdown(&notes), &docs)))
}

/// The text of a version's section in a ChangeLog's text, after its heading, up to the next heading of the same level
/// or higher, so that its own subheadings, e.g. `### New`, are part of it.
pub fn notes_in(text: &str, version: Version) -> Option<String> {
    let title = format!("Release {version}");
    let mut lines = text.lines();
    let level = lines.by_ref().find_map(|line| {
        let (level, heading) = heading(line)?;
        (heading == title).then_some(level)
    })?;
    let notes: Vec<&str> = lines
        .take_while(|line| heading(line).is_none_or(|(other, _)| other > level))
        .collect();
    Some(notes.join("\n").trim().to_string())
}

/// A Markdown heading's level and text, e.g. (2, "Release 0.5.0") for `## Release 0.5.0`, or None if the line is not
/// a heading.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|&c| c == '#').count();
    if level == 0 {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((level, rest.trim()))
}

/// Joins the lines of each paragraph, list item and quote, which the docs wrap at 120 characters, as GitHub shows
/// each line break in a release's notes. A blank line, heading, list item or table row starts a new line, and code
/// blocks are left as they are.
pub fn unwrap_markdown(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    // The line being built, which the next line may be joined to.
    let mut held: Option<String> = None;
    let mut code = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            out.extend(held.take());
            code = !code;
            out.push(line.to_string());
            continue;
        }
        if code {
            out.push(line.to_string());
            continue;
        }
        if trimmed.is_empty() || line.starts_with('#') || is_list_item(trimmed) || trimmed.starts_with('|') {
            out.extend(held.replace(line.to_string()));
            continue;
        }
        if let Some(quoted) = line.strip_prefix('>') {
            let quoted = quoted.strip_prefix(' ').unwrap_or(quoted);
            match &mut held {
                Some(text) if text.starts_with('>') && !quoted.is_empty() && !text[1..].trim().is_empty() => {
                    text.push(' ');
                    text.push_str(quoted);
                }
                _ => out.extend(held.replace(line.to_string())),
            }
            continue;
        }
        match &mut held {
            Some(text) if !text.trim().is_empty() && !text.starts_with('#') && !text.trim_start().starts_with('|') => {
                text.push(' ');
                text.push_str(trimmed);
            }
            _ => out.extend(held.replace(line.to_string())),
        }
    }
    out.extend(held);
    out.join("\n")
}

/// Whether a line, without its indent, starts a list item: `-`, `*` or `+`, or a number and a full stop, then a space.
fn is_list_item(trimmed: &str) -> bool {
    if let Some(rest) = trimmed.strip_prefix(['-', '*', '+']) {
        return rest.starts_with([' ', '\t']);
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && trimmed[digits..].starts_with(". ")
}

/// Makes each relative link, e.g. `[the guide](help/index.md)`, a link under `base`, leaving links to anchors, e.g.
/// `(#new)`, and those with a scheme, e.g. `https:` or `mailto:`, as they are.
pub fn absolute_links(text: &str, base: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("](") {
        let (before, after) = rest.split_at(start + 2);
        out.push_str(before);
        match after.find(')') {
            Some(end) => {
                let target = &after[..end];
                if !target.is_empty() && !target.starts_with('#') && !target.contains(':') {
                    out.push_str(base);
                }
                out.push_str(target);
                rest = &after[end..];
            }
            None => rest = after,
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_section_at_any_level() {
        let library = "# Release 0.2.0\n## New\n- A.\n\n# Release 0.1.0\n- First.\n";
        assert_eq!(notes_in(library, "0.2.0".parse().unwrap()).unwrap(), "## New\n- A.");
        assert_eq!(notes_in(library, "0.1.0".parse().unwrap()).unwrap(), "- First.");
        assert_eq!(notes_in(library, "0.3.0".parse().unwrap()), None);

        let app = "# Change Log\n\nIntro.\n\n## Release 0.5.0\n\n### New\n\n- B.\n\n## Release 0.4.4\n\n- C.\n";
        assert_eq!(notes_in(app, "0.5.0".parse().unwrap()).unwrap(), "### New\n\n- B.");
        assert_eq!(notes_in(app, "0.4.4".parse().unwrap()).unwrap(), "- C.");
        assert_eq!(notes_in("#Release 0.5.0\n- D.", "0.5.0".parse().unwrap()), None);
    }

    #[test]
    fn unwraps_paragraphs_and_items() {
        let text = "### New\n\n- One item\n  wrapped.\n- Two.\n\nA paragraph\nwrapped\ntwice.\n\n> A quote\n> wrapped.\n\n\
                    ```\ncode\n  kept\n```\n| a | b |\n| c | d |\n1. First\n   wrapped.";
        assert_eq!(
            unwrap_markdown(text),
            "### New\n\n- One item wrapped.\n- Two.\n\nA paragraph wrapped twice.\n\n> A quote wrapped.\n\n\
             ```\ncode\n  kept\n```\n| a | b |\n| c | d |\n1. First wrapped."
        );
    }

    #[test]
    fn makes_relative_links_absolute() {
        let base = "https://github.com/o/r/blob/release0.5.0/docs/";
        assert_eq!(
            absolute_links("See [help](help/a.md), [x](#new) and [y](https://e.com/z).", base),
            "See [help](https://github.com/o/r/blob/release0.5.0/docs/help/a.md), [x](#new) and [y](https://e.com/z)."
        );
        assert_eq!(absolute_links("Odd ]( with no end", base), "Odd ]( with no end");
    }
}
