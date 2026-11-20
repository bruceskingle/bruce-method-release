# bruce-method-release

Bruce's Method: Release. A command line tool for developing and releasing a set of Rust repositories that depend on
one another: the libraries are published to crates.io, and the apps use the published versions. It follows one way of
naming branches and tags and of moving work between them, so that every repository is released the same way, in the
right order, and the tag, the `main` branch and the published crate are always the same commit.

It is the first of the `bruce-method-*` tools, for the way of working described here.

**Status:** releases crates to crates.io; it released its own first version, 0.1.0. Releasing apps, whose installers
a GitHub workflow builds, is new in 0.2.0, see [Apps](#apps), and replaces Bruce's Smart Home's `scripts/release.sh`.

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
| `status` | The branch, version, uncommitted and unpushed changes, last release and links of this repository and of the libraries it links, with the links which can be removed. For an app, also the commits last tried and tagged, its GitHub release, and its workflow's latest runs. |
| `start [X.Y.Z]` | Starts a version's branch, sets its version in `Cargo.toml`, commits and pushes it. With no version, `vX.(Y+1).0` from `main`; with a fix's version, `vX.Y.(Z+1)` from the tag `releaseX.Y.Z`. |
| `check` | The checks a release needs, without changing anything; see below. |
| `release` | The checks, then the squash merge to `main` by pull request, the tag, publishing from the tag, the GitHub release, the unlinks next to it, and `start` of the next version. The repositories next to it which link the version released are unlinked, and those which take its crates by path, e.g. `{ version = "0.1.0", path = "../lib" }` or a path alone, take that version from crates.io instead, as its checkout moves on to the next version. One requiring a version which the release does not meet is left by path. |
| `try` | For an app: builds the installers of the branch's last commit with its workflow, and downloads them, releasing nothing; see [Apps](#apps). |
| `watch` | For an app: watches its workflow's latest run, of a `try` or a `release`, e.g. after Ctrl+C, and downloads the installers. |
| `download` | For an app: downloads the installer for this computer from the latest run, e.g. to try it on another computer. Not for the App Store's, which are installed from TestFlight. |
| `untag` | For an app: deletes the draft release and the release's tag, to make more changes before publishing. |
| `setup` | Makes the repository's workflows and standard files from the method's templates, keeps them so, and sets its GitHub settings as the method wants them; see [Setup](#setup). |
| `link <library>` | Links this repository to the library's checkout next to it. |
| `unlink [<library>]` | Removes a link, or every link which is no longer needed. Given a library, it also makes this repository's path dependencies on it dependencies on its latest release on crates.io, as `release` does. |
| `dependencies [-v]` | Lists the libraries next to this repository which it depends on, directly or through one another, in the order to release them, a line each: a tick for one which is ready, or a cross and what is to be done, e.g. `release 0.1.0, which bruce-smart-home needs`, after which `check` in that library says what stops it. With `-v`, each with how its users take it, by path, linked or from crates.io, and the versions asked of crates.io. Changes nothing. |
| `sync [<branch>]` | Brings this checkout, and the libraries it links, up to date with GitHub, by fast forward only, optionally switching it to another branch first, given by name or by version, e.g. `0.6.0` for `v0.6.0`; with none, a checkout on the branch of a version already released moves on to the latest version's branch, e.g. on a computer which tries each version's installers; then links the libraries next to it of which it requires a version not yet published, putting each on that version's branch, e.g. on a second computer, where the links are not. |

### What a release checks

- The repository is on its `vX.Y.Z` branch, with `X.Y.Z` in every crate's `Cargo.toml`, nothing uncommitted, and
  everything pushed.
- `releaseX.Y.Z` is not already a tag.
- No library is linked, nothing is taken by path from outside the repository, and every version required is on
  crates.io.
- `main` is merged into the branch, e.g. after a fix's release, so that the squash merge is the branch exactly.
- `docs/ChangeLog.md` has a `Release X.Y.Z` section, under a heading of any level, e.g. `# Release X.Y.Z`, or
  `## Release X.Y.Z` under a `# Change Log` title, which becomes the GitHub release's notes: its lines are unwrapped, as
  GitHub shows each line break, and its relative links made links to the docs on GitHub at the tag.
- `cargo fmt --check` passes, if the repository has a `rustfmt.toml`, `cargo clippy` with no warnings, `cargo test`, and
  `cargo deny check`, if it has a `deny.toml`.
- `cargo publish --dry-run` passes, for a library.
- The files `setup` makes are as it would make them, see [Setup](#setup).
- CI passed on GitHub for the branch's last commit, if it has workflows. For an app, a successful `try` of it is that
  run.

A library's crates are published in the order they depend on one another, e.g. a derive macro before the library
which uses it, from a worktree of the tag, so that what is published is exactly the tagged commit, with no links. After
publishing, the tool checks that the packages recorded the tag's commit, in their `.cargo_vcs_info.json`.

### Apps

An app's installers are built by a GitHub workflow, see [Settings](#settings), on GitHub's computers for each
platform, and tried before anyone else gets them, twice:

1. `try` builds the installers of the branch's last commit, tagged `testX.Y.Z`, and downloads them to
   `~/Downloads/<repo>-X.Y.Z-test`, releasing nothing. Try them, and change and try again until they are right. A
   successful try is also the CI run the release checks for.
2. `release` makes the checks, squash merges the branch into `main`, tags it `releaseX.Y.Z`, merges `main` back into
   the branch, which changes no files but means a later pull request shows only what is new, makes a draft GitHub
   release with the ChangeLog's notes, has the workflow build the installers from the tag and attach them to the draft,
   and downloads them to `~/Downloads/<repo>-X.Y.Z`. Then it stops, for them to be tried, and the draft's notes read.
3. `release` again publishes the draft as the latest release, and starts the next version.

To change something after step 2, `untag` deletes the draft and the tag; commit the change to the branch, `try` it, and
`release` again, which merges the new changes. A published release is never changed: a fix is a new version.

`watch` carries on watching a run after Ctrl+C, and `download` fetches only this computer's installer, e.g. on a
second computer which tries it. A release which stops part way, e.g. while its installers are built, carries on from
where it stopped, watching a run already going rather than starting another.

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

## Setup

`setup` makes a repository's workflows and standard files from templates in the tool, so that every repository of a
kind is set up the same way, and a change to the method reaches each of them by running `setup` again with a newer
version of the tool. It lists what it will do and asks, like the other commands, and changes nothing which is already
as it would make it. A file is one of three sorts:

| Sort | Files | What setup does |
|---|---|---|
| Made | `rustfmt.toml`, `.github/workflows/ci.yml` for a library, the release workflow for an app | Writes it wholly from its template, with a comment at the top saying so. It is never edited by hand: `check` fails while it differs, and `setup` replaces it. |
| Kept in part | `deny.toml` | Writes the licence policy, the same for every repository, from its template, and keeps the repository's own approvals, the crates approved by name, between the markers in its `exceptions`. |
| Started | `LICENSE-MIT`, `LICENSE-APACHE`, `docs/ChangeLog.md`, and a library's `docs/Release.md` | Writes it only if it is missing; then it is the repository's own. |

A repository's own steps in a made workflow are its **hooks**, local actions which it owns and which the workflow runs
if they are there:

| Hook | Run by | When |
|---|---|---|
| `.github/actions/before-bundle/action.yml` | An app's release workflow | Before the installers are built, e.g. to build programs which they include, and to set variables in `GITHUB_ENV` for the build. |

On GitHub, `setup` allows pull requests to be merged only by squash merge. It lists the secrets the workflows use, by
name only, e.g. the Apple signing secrets an app's workflow uses, and which of them the repository does not have; their
values are set with `gh secret set`, by whoever holds them.

An app's release workflow is made for a Dioxus desktop app, from its `Dioxus.toml` and `Cargo.lock`: the Dioxus CLI is
the version Cargo.lock has, so `check` fails after Dioxus is updated until `setup` is run, and the Windows installer is
checked to have put the programs in `external_bin` beside the app. `setup` does not yet make files for a repository of
separate crates.

`try` builds an app with the libraries it links, if it links any: the workflow checks out each linked library's branch
beside the app, as on the developer's computer, and writes the same patch, so each must be committed and pushed. A
release never takes linked libraries.

## Settings

What a repository releases is set in its `Cargo.toml`, under `[package.metadata.bruce-method]`, or
`[workspace.metadata.bruce-method]` for a workspace:

```toml
[package.metadata.bruce-method]
# "crate", published to crates.io, the default, or "app", whose installers are built by a GitHub workflow.
kind = "crate"
```

An app also names its workflow, `release.yml` unless set, and lists the installers it builds, each with its platform,
`<os>-<architecture>`, e.g. `macos-aarch64`, `macos-x64`, `windows-x64` or `windows-arm64`, and its file's name, with
`{version}` for the version:

```toml
[package.metadata.bruce-method]
kind = "app"
workflow = "release.yml"
installers = [
    { platform = "macos-aarch64", file = "MyApp_{version}_aarch64.dmg" },
    { platform = "windows-x64", file = "MyApp_{version}_x64-setup.exe" },
]
```

The workflow is started by hand (`on: workflow_dispatch`), with two inputs: `tag`, the tag to build, `testX.Y.Z` or
`releaseX.Y.Z`, and `attach`, a boolean, true to attach the installers to the draft GitHub release for the tag, which
it should refuse to do to a published one. It keeps each platform's installer as an artifact of the run named for the
platform, e.g. `macos-aarch64`. Bruce's Smart Home's `.github/workflows/release.yml` is one.

Apps for Apple's App Store are `kind = "app-store"`, which lists each app, an iOS app made by Xcode, or a Mac app made
with Dioxus, e.g. in a workspace's Cargo.toml:

```toml
[workspace.metadata.bruce-method]
kind = "app-store"
workflow = "release.yml"

[[workspace.metadata.bruce-method.apps]]
platform = "ios"
project = "ios/MyApp.xcodeproj"
scheme = "MyApp"
swift-package = "ios/MyAppKit"   # optional: a Swift package whose tests CI runs

[[workspace.metadata.bruce-method.apps]]
platform = "macos"
dioxus = "server"                # the Dioxus app's folder
features = ["store"]             # optional: the App Store build's Cargo features
entitlements = "server/store.entitlements"
category = "public.app-category.trivia-games"
exempt-encryption = true         # optional: its only encryption is exempt, e.g. HTTPS through Apple's frameworks
```

`setup` makes its release workflow, which builds each app on a GitHub Mac and signs it for App Store Connect, with
automatic signing and an App Store Connect API key with the Admin role, so that no certificate or profile is kept in
the repository's secrets; a Mac app made with Dioxus is put in an Xcode archive of its own, signed with its
entitlements, for xcodebuild to export as it would one of Xcode's. Pushing a tag runs it, so that it works before it is
on main, where the Actions tab could run it: a `testX.Y.Z` tag builds and signs the apps without uploading them, and a
`releaseX.Y.Z` tag uploads them to App Store Connect, where they appear in TestFlight once Apple has processed them. Its
CI runs on a Mac, with the iOS apps' Swift packages' tests.

Its release goes as an app's does, but with TestFlight in place of a draft's installers:

1. `try` tags the branch's last commit `testX.Y.Z` and pushes the tag, which builds the apps and signs them as they
   would be uploaded, then downloads them to `~/Downloads/<repo>-X.Y.Z-test`. A successful try is also the CI run the
   release checks for. It builds with the libraries from crates.io, so unlink any first.
2. `release` checks, squash merges into main, tags main `releaseX.Y.Z`, which uploads the apps, makes a draft GitHub
   release with the ChangeLog's notes, watches the upload, and stops.
3. Try the apps in TestFlight, and submit them for App Review in App Store Connect.
4. Once Apple has approved them, `release` again publishes the GitHub release and starts the next version. Until then,
   a rejection is fixed in the same version, as in the next paragraph.

To change something after step 2, `untag`, commit the change, `try` it, and `release` again: the next upload has a new
build number, the workflow's run number, which App Store Connect needs. A failed upload is run again by `release`.

Firmware, which attaches the builds for each board, is to follow.

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
