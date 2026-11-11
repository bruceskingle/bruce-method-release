//! Links: a repository's patches, in its git-ignored `.cargo/config.toml`, which build a library from the checkout
//! next to it rather than from crates.io, see the README.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, InlineTable, Item, Table};

/// The file a repository's links are kept in, ignored by git so that they are each developer's own.
pub const CONFIG: &str = ".cargo/config.toml";

/// The comment the tool starts a config file it makes with.
const HEADER: &str = "# Git-ignored: builds the libraries patched here from the checkouts next to this one, rather than their\n\
                      # published versions, to work on them together. Made by bruce-method-release's link, removed by its\n\
                      # unlink.\n";

/// A patch in a repository's config file, which builds a crate from a path rather than from crates.io.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    /// The crate patched.
    pub name: String,
    /// The path as written, relative to the repository.
    pub path: String,
}

impl Patch {
    /// The patched crate's directory.
    pub fn dir(&self, repository: &Path) -> PathBuf {
        repository.join(&self.path)
    }
}

/// The patches of crates.io crates by path in a config file's text.
pub fn patches_in(text: &str) -> Result<Vec<Patch>> {
    let document: DocumentMut = text.parse().context("could not read the config file")?;
    let Some(table) = document
        .get("patch")
        .and_then(|patch| patch.get("crates-io"))
        .and_then(Item::as_table_like)
    else {
        return Ok(Vec::new());
    };
    Ok(table
        .iter()
        .filter_map(|(name, item)| {
            let path = item.as_table_like()?.get("path")?.as_str()?;
            Some(Patch {
                name: name.to_string(),
                path: path.to_string(),
            })
        })
        .collect())
}

/// A config file's text with patches added, or replaced if the crate is already patched.
pub fn with_patches(text: &str, patches: &[Patch]) -> Result<String> {
    let new = text.trim().is_empty();
    let mut document: DocumentMut = text.parse().context("could not read the config file")?;
    let patch = document.entry("patch").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    let crates_io = patch
        .as_table_like_mut()
        .context("patch in the config file is not a table")?
        .entry("crates-io")
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .context("patch.crates-io in the config file is not a table")?;
    for added in patches {
        let mut entry = InlineTable::new();
        entry.insert("path", added.path.as_str().into());
        crates_io.insert(&added.name, Item::Value(entry.into()));
    }
    // A new file starts with what it is for, written here as comments on their own would come after the tables.
    Ok(if new {
        format!("{HEADER}\n{document}")
    } else {
        document.to_string()
    })
}

/// A config file's text with the named crates' patches removed, and the patch tables too once they are empty, or
/// None if nothing is left in it but comments, so that the file can be removed.
pub fn without_patches(text: &str, names: &[String]) -> Result<Option<String>> {
    let mut document: DocumentMut = text.parse().context("could not read the config file")?;
    if let Some(patch) = document.get_mut("patch").and_then(Item::as_table_like_mut) {
        if let Some(crates_io) = patch.get_mut("crates-io").and_then(Item::as_table_like_mut) {
            for name in names {
                crates_io.remove(name);
            }
            if crates_io.is_empty() {
                patch.remove("crates-io");
            }
        }
        if patch.is_empty() {
            document.remove("patch");
        }
    }
    Ok((!document.is_empty()).then(|| document.to_string()))
}

/// The patches in a repository's config file, none if it has no file.
pub fn read(repository: &Path) -> Result<Vec<Patch>> {
    let file = repository.join(CONFIG);
    if !file.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&file).with_context(|| format!("could not read {}", file.display()))?;
    patches_in(&text).with_context(|| file.display().to_string())
}

/// Adds patches to a repository's config file, making it if need be.
pub fn add(repository: &Path, patches: &[Patch]) -> Result<()> {
    let file = repository.join(CONFIG);
    let text = if file.exists() {
        fs::read_to_string(&file).with_context(|| format!("could not read {}", file.display()))?
    } else {
        String::new()
    };
    let new = with_patches(&text, patches).with_context(|| file.display().to_string())?;
    fs::create_dir_all(file.parent().expect("the config file is in a directory"))
        .with_context(|| format!("could not make the directory of {}", file.display()))?;
    fs::write(&file, new).with_context(|| format!("could not write {}", file.display()))
}

/// Removes the named crates' patches from a repository's config file, and the file, and its directory, once nothing
/// is left in them.
pub fn remove(repository: &Path, names: &[String]) -> Result<()> {
    let file = repository.join(CONFIG);
    let text = fs::read_to_string(&file).with_context(|| format!("could not read {}", file.display()))?;
    match without_patches(&text, names).with_context(|| file.display().to_string())? {
        Some(new) => fs::write(&file, new).with_context(|| format!("could not write {}", file.display())),
        None => {
            fs::remove_file(&file).with_context(|| format!("could not remove {}", file.display()))?;
            if let Some(dir) = file.parent() {
                // Only if it is empty: it may hold other settings.
                let _ = fs::remove_dir(dir);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(name: &str, path: &str) -> Patch {
        Patch {
            name: name.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn makes_a_config_file_with_a_patch() {
        let text = with_patches("", &[patch("bruce-db", "../bruce-db")]).unwrap();
        assert!(text.starts_with("# Git-ignored"));
        assert!(text.ends_with("[patch.crates-io]\nbruce-db = { path = \"../bruce-db\" }\n"));
        assert_eq!(patches_in(&text).unwrap(), vec![patch("bruce-db", "../bruce-db")]);
    }

    #[test]
    fn adds_to_and_keeps_other_settings() {
        let text = "[build]\ntarget = \"x\"\n\n[patch.crates-io]\na = { path = \"../a\" }\n";
        let new = with_patches(text, &[patch("b", "../b/b")]).unwrap();
        assert_eq!(
            new,
            "[build]\ntarget = \"x\"\n\n[patch.crates-io]\na = { path = \"../a\" }\nb = { path = \"../b/b\" }\n"
        );
    }

    #[test]
    fn removes_a_patch_then_the_tables_then_the_file() {
        let text = "[patch.crates-io]\na = { path = \"../a\" }\nb = { path = \"../b\" }\n";
        let one = without_patches(text, &["a".to_string()]).unwrap().unwrap();
        assert_eq!(patches_in(&one).unwrap(), vec![patch("b", "../b")]);
        assert_eq!(without_patches(&one, &["b".to_string()]).unwrap(), None);
        let kept = without_patches(
            "[build]\ntarget = \"x\"\n[patch.crates-io]\na = { path = \"../a\" }\n",
            &["a".to_string()],
        )
        .unwrap()
        .unwrap();
        assert_eq!(kept, "[build]\ntarget = \"x\"\n");
    }

    #[test]
    fn ignores_patches_which_are_not_paths() {
        let text = "[patch.crates-io]\na = { git = \"https://example.com/a\" }\n";
        assert!(patches_in(text).unwrap().is_empty());
    }
}
