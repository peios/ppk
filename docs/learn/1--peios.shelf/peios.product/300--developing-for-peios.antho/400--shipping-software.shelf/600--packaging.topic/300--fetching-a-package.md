---
title: Fetching a Package
type: how-to
description: Copy a package from another artifact directory with ppk fetch,
  what it checks on the way, and what happens when the package is already
  present.
related:
  - peios/packaging/packages
  - peios/packaging/creating-a-package
---

Fetching copies a package from another artifact directory into yours and
checks every part of it on the way. Do this to use a package that was packed
somewhere else, such as on a build machine whose artifact directory you can
read.

## Before you start

- Install `ppk`.
- Have read access to the source artifact directory, as a local path.
- Have write access to your artifact directory. The default is
  `/system/artifacts`; to use a directory of your own, pass
  `--artifact-dir`.
- Know the package's identifier, such as `dev.peios.hello@1.0-1?arch=noarch`.

## Fetch the package

Run `ppk fetch` with the identifier and the source:

```sh
ppk fetch 'dev.peios.hello@1.0-1?arch=noarch' --from shared --artifact-dir artifacts
```

`ppk` prints the package's identity:

```text
sha256:73a137f612f39b8ee1ffcbb7f51b7b9dc370b8662323d72b9e6b73b9d54413f8
```

## Check the result

Compare the identity with the one `ppk pack` printed where the package was
made. Because the identity is the hash of the manifest, and the manifest
names every file by its hash, the same identity means the same package,
byte for byte.

## What fetch checks

`ppk fetch` trusts nothing it reads from the source:

- **The manifest** must hash to the identity in the source's ref.
- **Its form** must be exactly what `ppk pack` writes, naming the package
  you asked for, with every path relative and inside the package's root.
- **Each file's content** must hash to the name the manifest gives it.

If a check fails, `ppk fetch` stops with an error and doesn't record the
package. Only the files missing from your artifact directory are copied, so
fetching a new version transfers only what changed.

These checks prove that the package is the one the source's ref names. They
don't prove who published it, so fetch only from sources you trust.

## When the package is already present

Fetching a package that is already present with the same identity succeeds
and copies only what is missing. If your artifact directory has a different
package under the same name, version, and architecture, `ppk fetch` refuses
and writes nothing:

```text
ppk: dev.peios.hello@1.0-1?arch=noarch: already present as sha256:73a137f612f39b8ee1ffcbb7f51b7b9dc370b8662323d72b9e6b73b9d54413f8, but other has sha256:453a10dad4cb1902912919527452fa61c6420ae9152c1f02b57c1421a6d9d57b
```

A packed version never changes, so two different packages under one version
mean the two artifact directories disagree. Find out which one is right
before you use the package.
