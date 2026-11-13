# bruce-method-release

Bruce's Method: Release. A command line tool for developing and releasing a set of Rust repositories that depend on
one another: the libraries are published to crates.io, and the apps use the published versions. It follows one way of
naming branches and tags and of moving work between them, so that every repository is released the same way, in the
right order, and the tag, the `main` branch and the published crate are always the same commit.

It is the first of the `bruce-method-*` tools, for the way of working described here.

**Status:** releases crates to crates.io; it released its own first version, 0.1.0. Apps, which build installers,
are still released by their own steps.

## Installing

Install the version published on crates.io with Cargo:

```sh
cargo install bruce-method-release
```

This builds it and puts the program in `~/.cargo/bin`, which is on the `PATH` of anyone with Cargo, so that it can be
run from any repository's checkout, e.g. `cd ~/git/bruce-db` then `bruce-method-release status`. Run the same command
again to update to a newer version, and `bruce-method-release --version` to see which is installed. It needs Git, the
GitHub CLI and cargo-deny too, see [Needs](#needs).

To use a version not yet published, e.g. to try a change, install it from its checkout instead, see
[Developing](#developing).

## The method

### Branches and tags

| Name | What it is |
|---|---|
| `vX.Y.Z` | The branch version `X.Y.Z` is developed on, with `X.Y.Z` in `Cargo.toml` from the start. |
| `releaseX.Y.Z` | An annotated tag on `main`, of the commit released as `X.Y.Z`. |
| `main` | Only releases, each one a squash merge of its `vX.Y.Z` branch. |
| anything else | A feature branch, made from a `vX.Y.Z` branch and squash merged back into it. |

A release squash merges `vX.Y.Z` into `main` with a pull request, tags the result `releaseX.Y.Z`, and publishes from
the tag. Then the next version's branch, `vX.(Y+1).0`, is started from `main` straight away, so there is always a
branch to develop on.

A fix to a version already released is made on a `vX.Y.(Z+1)` branch from its tag, only when one is needed, and
released the same way. `main` then has the fix, and is merged into the `vX.(Y+1).0` branch being developed, so that it
has the fix too.

### Versions

Each repository has its own version, and every crate in a repository has that version. A repository's version goes up
when it is released, whatever the others do: a library may be at 0.1.0 while the app which uses it is at 0.5.0.

Before 1.0, Cargo treats a change in the middle number as a break: a crate which asks for `"0.1.0"` accepts 0.1.1 but
not 0.2.0. The method follows that: `vX.(Y+1).0` is the next version, which may change anything, and `vX.Y.(Z+1)` a
fix, which may not.

### Where the repositories are

The repositories are cloned side by side in one directory, e.g. `~/git/bruce-db` and `~/git/bruce-smart-home`, and the
tool finds each one as a sibling of the repository it is run in. It never names the directory, so that a whole set can
be cloned somewhere else, e.g. into `/tmp/git`, to try a release from start to finish without touching the real
checkouts. A repository's directory has the same name as the repository on GitHub.

A checkout next to it counts as one of the set if it follows the method, on a `vX.Y.Z` branch or with a `releaseX.Y.Z`
tag, so that a clone of someone else's crate kept alongside, e.g. a fork being tried, is left alone. Its crates are
found from every `Cargo.toml` git tracks in it, not only those of the workspace at its root, so that a crate built on
its own, e.g. a device's firmware, and a repository with no `Cargo.toml` at its root, are seen too.

### Depending on a library being developed

An app, or a library, depends on the published versions of the libraries it uses, e.g. `bruce-db = "0.1.0"`, so that
it builds anywhere, from crates.io alone. To work on a library and something which uses it together, the user is
*linked* to the library's checkout next to it. A link has two parts:

- **The version required, in `Cargo.toml`**, set to the version on the library's branch, e.g. `bruce-db = "0.2.0"`.
  It is committed, and so is shared: it says that this branch needs that version, which is not published yet.
- **A patch, in the git-ignored `.cargo/config.toml`**, which builds the library from the checkout next to it:

  ```toml
  [patch.crates-io]
  bruce-db = { path = "../bruce-db" }
  ```

  It is each developer's own, and says nothing to anyone else.

Once the library's version is released, the version required is on crates.io and the patch is no longer needed, so it
is taken out: *unlinked*. Every command checks for links whose library has since been released, and says so, for
`unlink` to remove them. A patch which no longer matches the version required, e.g. after the library's checkout has moved on to its next
branch, is reported too: Cargo ignores such a patch with only a warning, and quietly builds from crates.io instead.

Nothing is released while it links a library which is not: the library is released first, and the tool says which.
This means a release never depends on code which is not published.

While a branch requires a version which is not published, it builds only where the library's checkout is next to it
and linked, so its CI on GitHub fails until the library is released. That is a true account of the branch: it could
not be released either.

### Two developers, or more

The patch is each developer's own, so each links on their own workstation, and the shared part, the version required,
is committed once, by whoever links first; a second developer's link finds it already set.

Working on a library and an app which uses it:

1. Link the app to the library, which checks out the library's `vX.Y.Z` branch next to it if need be.
2. Make a feature branch in the library from `vX.Y.Z`, and one in the app from its own branch.
3. Develop and test them together.
4. Squash merge each feature branch back into its `vX.Y.Z` branch, resolving any conflicts with the other developers'
   work there, in the usual way.

Whoever releases the library is offered the unlink at once, as the release ends by checking the repositories next to
it. Anyone else is offered it by their next command after the release, whatever it is.

## Commands

Each command is run in a repository's checkout, or given one with `--repo`, and checks that the repositories are in
the state it expects before changing anything, stopping with an explanation at the first problem. A command which
changes anything, e.g. `start` or `release`, first lists what it will do and asks once:

```
Start bruce-db 0.2.0:
  1. Create the branch v0.2.0 in bruce-db from origin/main
  2. Set the version of every crate in it to 0.2.0, and update Cargo.lock
  3. Commit "Start v0.2.0" and push the branch to GitHub
Go ahead? [y/N]
```

`y` and Return goes ahead; anything else, including Return alone, cancels, changing nothing. A release which stops
part way, e.g. at a network failure, carries on from where it stopped when run again.

| Command | What it does |
|---|---|
| `status` | The branch, version, uncommitted and unpushed changes, last release and links of this repository and of the libraries it links, with the links which can be removed. |
| `start [X.Y.Z]` | Starts a version's branch, sets its version in `Cargo.toml`, commits and pushes it. With no version, `vX.(Y+1).0` from `main`; with a fix's version, `vX.Y.(Z+1)` from the tag `releaseX.Y.Z`. |
| `check` | The checks a release needs, without changing anything; see below. |
| `release` | The checks, then the squash merge to `main` by pull request, the tag, publishing from the tag, the GitHub release, the unlinks next to it, and `start` of the next version. The repositories next to it which link the version released are unlinked, and those which take its crates by path, e.g. `{ version = "0.1.0", path = "../lib" }` or a path alone, take that version from crates.io instead, as its checkout moves on to the next version. One requiring a version which the release does not meet is left by path. |
| `link <library>` | Links this repository to the library's checkout next to it. |
| `unlink [<library>]` | Removes a link, or every link which is no longer needed. Given a library, it also makes this repository's path dependencies on it dependencies on its latest release on crates.io, as `release` does. |
| `dependencies [-v]` | Lists the libraries next to this repository which it depends on, directly or through one another, in the order to release them, a line each: a tick for one which is ready, or a cross and what is to be done, e.g. `release 0.1.0, which bruce-smart-home needs`, after which `check` in that library says what stops it. With `-v`, each with how its users take it, by path, linked or from crates.io, and the versions asked of crates.io. Changes nothing. |
| `sync [<branch>]` | Brings this checkout, and the libraries it links, up to date with GitHub, by fast forward only, optionally switching it to another branch first; then links the libraries next to it of which it requires a version not yet published, putting each on that version's branch, e.g. on a second computer, where the links are not. |

### What a release checks

- The repository is on its `vX.Y.Z` branch, with `X.Y.Z` in every crate's `Cargo.toml`, nothing uncommitted, and
  everything pushed.
- `releaseX.Y.Z` is not already a tag.
- No library is linked, and every version required is on crates.io.
- `main` is merged into the branch, e.g. after a fix's release, so that the squash merge is the branch exactly.
- `docs/ChangeLog.md` has a `# Release X.Y.Z` section, which becomes the GitHub release's notes.
- `cargo fmt --check` passes, if the repository has a `rustfmt.toml`, `cargo clippy` with no warnings, `cargo test`, and
  `cargo deny check`, if it has a `deny.toml`.
- `cargo publish --dry-run` passes, for a library.
- CI passed on GitHub for the branch's last commit, if it has workflows.

A library's crates are published in the order they depend on one another, e.g. a derive macro before the library
which uses it, from a worktree of the tag, so that what is published is exactly the tagged commit, with no links. After
publishing, the tool checks that the packages recorded the tag's commit, in their `.cargo_vcs_info.json`.

### Separate crates

A repository with no `Cargo.toml` at its root is a set of separate crates, each its own workspace, e.g. a platform's
library and a crate for each board it supports, each built for its board's chip by its own `.cargo/config.toml`,
which Cargo reads only when run in the crate's directory. Its crates are those of every `Cargo.toml` git tracks, all
with the same version, and each is checked, published and given the next version in its own directory:

- A crate whose `.cargo/config.toml` sets a `[build] target` is built for a device. It is checked by
  `cargo build --release`, not clippy, and its tests are not run, as they cannot run on the computer, and its runner
  may flash a board.
- A library built for a device which other crates of the repository use by path, e.g. a platform which compiles only
  with a board's features, is checked by building them, and published with `--no-verify`, as it may not build on its
  own.
- The other crates are checked as a workspace's are, with clippy and their tests.

## Settings

What a repository releases is set in its `Cargo.toml`, under `[package.metadata.bruce-method]`, or
`[workspace.metadata.bruce-method]` for a workspace:

```toml
[package.metadata.bruce-method]
# "crate", published to crates.io, the default, or "app", whose installers are built by a GitHub workflow.
kind = "crate"
```

Crates come first. Apps, which build installers and attach them to a draft GitHub release, and firmware, which
attaches the builds for each board, follow, from the steps in Bruce's Smart Home's `scripts/release.sh`, which is used
until then.

## Needs

- Git, the GitHub CLI `gh` signed in with `gh auth login`, and Cargo with a crates.io token from `cargo login`.
- `cargo-deny`, for its checks.

It runs the same on macOS, Linux and Windows. It is a library with a command line on top, so that a desktop window can
be put on it later.

## Developing

The tool is used from the checkout of the repository it works on, e.g. `cd ~/git/bruce-db` then
`bruce-method-release status`, so it is installed where the shell finds it from any directory:

```sh
cd ~/git/bruce-method-release
cargo install --path .
```

This builds the crate in this checkout, optimised, and copies the program to `~/.cargo/bin`, which is on the `PATH`
of anyone with Cargo. The copy installed is what was built at the time: after changing the source, run
`cargo install --path .` again to use the change. Once a version is published, `cargo install bruce-method-release`
installs it from crates.io instead.

While developing, `cargo build` and `cargo run` work too, but are less handy:

- `cargo build` leaves the program in `target/debug/bruce-method-release` in this checkout, to be run by that path
  from the repository to work on, e.g. `~/git/bruce-method-release/target/debug/bruce-method-release status`.
- `cargo run -- status` runs it in this checkout, so it works on bruce-method-release itself, unless given another
  repository, e.g. `cargo run -- --repo ../bruce-db status`.

Commands which change anything are tried on throwaway repositories, each with a local bare repository as its
`origin`, never on real checkouts, see [AGENTS.md](AGENTS.md). The tool releases itself: install the version on its
branch with `cargo install --path .`, then run `bruce-method-release release` in this checkout.

License
=======

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

Contribution
------------

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
