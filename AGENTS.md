# Notes for Coding Agents

bruce-method-release is a Rust command line tool, and the library under it, for developing and releasing a set of Rust
repositories cloned side by side: the branch and tag names, the release steps, and links between a library and what
uses it while both are developed. The README is the description of the method; change it when the method changes.

## Building

- `cargo build` and `cargo test`. Format with `cargo fmt`, see `rustfmt.toml`; `cargo fmt --check` should pass.
- `cargo deny check` should pass: a new dependency outside the licences rule, see `deny.toml`, fails until it is
  approved and listed there by crate.
- Try commands which change anything on throwaway repositories, with local bare repositories as their origins, never
  on the real checkouts, and never `release` except on a real release the maintainer is making.
- It is published to crates.io, so a published version can never be changed or removed. Do not publish; the
  maintainer does, see [docs/Release.md](docs/Release.md).

## Conventions

- **Commands** take a `Repo` and a `Ui`, and work out what they will do before changing anything, then ask once with
  `Ui::confirm`, so that a desktop window can drive them later as the command line does now.
- **Repositories** are found as siblings of the one a command is run in, never by a fixed directory such as `~/git`.
- **Errors** are formatted with `{error:#}`, their chain of causes, not `{error:?}`.
- **Comments** are doc comments in full sentences, saying what a thing is for and why.
