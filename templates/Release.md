# Releasing {{repo}}

{{repo}} is developed and released with [bruce-method-release](https://github.com/bruceskingle/bruce-method-release),
whose README describes the method: each version is developed on its `vX.Y.Z` branch, squash merged into `main`, and
tagged `releaseX.Y.Z`.

```sh
bruce-method-release status    # where the version is
bruce-method-release check     # the checks a release needs, changing nothing
bruce-method-release release   # the release, after listing its steps and asking
```

Write the release's notes in [ChangeLog.md](ChangeLog.md) first, under `# Release X.Y.Z`, which the checks require and
which become the GitHub release's notes.
