# ChangeLog

# Release 0.2.0

## New

- `dependencies`, the libraries next to a repository which it depends on, in the order to release them, and which
  need releasing for it to be built from crates.io alone.
- `release` makes the path dependencies on the version released, in the repositories next to it, dependencies on
  crates.io, as it unlinks their links, and `unlink <library>` does the same for its latest release.

## Improved

- A repository's crates are found from every `Cargo.toml` it tracks, so that a crate outside the workspace at its root,
  e.g. firmware, and a repository with no `Cargo.toml` at its root, are seen too.
- Only the checkouts next to a repository which follow the method count as its libraries.
- A repository of separate crates, with no `Cargo.toml` at its root, e.g. a platform and its boards, each built for
  its own chip, is checked and released crate by crate, in each crate's directory: a crate built for a device is
  checked by building it, and a library which needs a board's features by building the boards.

# Release 0.1.0

## New

- `status`, `start`, `check`, `release`, `link`, `unlink` and `sync`, for crates published to crates.io, see the
  README.
