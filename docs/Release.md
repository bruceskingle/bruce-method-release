# Releasing bruce-method-release

bruce-method-release releases itself, once it is installed:

```sh
cd ~/git/bruce-method-release
cargo install --path .
bruce-method-release check
bruce-method-release release
```

Its first version, 0.1.0, is released by hand, by the steps in bruce-db's
[docs/Release.md](https://github.com/bruceskingle/bruce-db/blob/main/docs/Release.md), with this repository's name, as
there is no released version to do it with yet. Installing the version on the branch with `cargo install --path .`
and releasing with it would also do, and would try `release` on a real release.
