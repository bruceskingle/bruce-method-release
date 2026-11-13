# Releasing bruce-method-release

bruce-method-release releases itself, once it is installed:

```sh
cd ~/git/bruce-method-release
cargo install --path .
bruce-method-release check
bruce-method-release release
```

## Trying a version before releasing it

A change is tried on the real checkouts, e.g. bruce-smart-home, with the program built in this checkout, run by its
path, so that the version installed in `~/.cargo/bin` stays the released one, for real releases:

```sh
cd ~/git/bruce-method-release
cargo build
alias mr=~/git/bruce-method-release/target/debug/bruce-method-release
```

`cargo build` again after each change to the source; the alias lasts as long as the shell.

**Commands which change nothing** can be run anywhere, as often as wanted:

```sh
cd ~/git/bruce-smart-home
mr dependencies     # the libraries it depends on, in the order to release them, a tick or a cross each
mr dependencies -v  # and who takes each, and how
mr status
mr check            # in a library, e.g. ~/git/bruce-embedded-flash
```

**`unlink`** changes only the Cargo.toml files of the repository it is run in, after listing what it will do and
asking. Answer `n` to see the plan and change nothing; answer `y` to make the change, then see it with `git diff`, and
undo it, if wanted, with git:

```sh
cd ~/git/bruce-esp-idf
mr unlink bruce-embedded-std     # its path dependencies on bruce-embedded-std, to the release on crates.io
git diff -- '*Cargo.toml'
git checkout -- '*Cargo.toml' '*Cargo.lock'     # to undo it
```

**`release`** is tried only as far as its question: it runs the checks, fetches, and asks GitHub and crates.io how far
the release has got, but changes nothing until it is answered. In a library whose checks pass, run it and answer `n`,
to see its steps, including the repositories next to it whose links and path dependencies it would change:

```sh
cd ~/git/bruce-embedded-flash
mr release     # answer n
```

Answering `y` releases the library for real: a crate published to crates.io cannot be taken back, so the steps after
the question, publishing, unlinking the repositories next to it and starting the next version, are tried only on a
real release, which is meant. Throwaway repositories do not help with those, as `cargo publish` from them would publish
to the real crates.io too.

## Using a version before releasing it

Nothing depends on bruce-method-release, so a new version need not be released before it is used: install it from
this checkout, and it replaces the one in `~/.cargo/bin`:

```sh
cd ~/git/bruce-method-release
cargo install --path .
bruce-method-release --version     # the version on the branch
```

This is how a new command, or a change to `release`, is used on the libraries' real releases before the tool's own
release, so that the steps after `release`'s question have been tried on them first. Release it once they have worked,
so that crates.io, and `cargo install bruce-method-release` on another computer, have it too.

To go back to the released version:

```sh
cargo install bruce-method-release --force
```

## The first version

Its first version, 0.1.0, is released by hand, by the steps in bruce-db's
[docs/Release.md](https://github.com/bruceskingle/bruce-db/blob/main/docs/Release.md), with this repository's name, as
there is no released version to do it with yet. Installing the version on the branch with `cargo install --path .`
and releasing with it would also do, and would try `release` on a real release.
