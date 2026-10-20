---
title: Building a Closure
type: how-to
description: Compose packages into a closure with ppk closure, find its tree
  and manifest, and resolve a conflict between two packages.
related:
  - peios/packaging/closures
  - peios/packaging/fetching-a-package
---

Building a closure composes packages into one directory tree that is laid
out as a system root. Do this when you want a set of packages together in
one place, for example to check that they fit together without conflicts.
For how closures work, see [Closures](~peios/packaging/closures).

## Before you start

- Install `ppk`.
- Have every package in your artifact directory, by packing it with
  `ppk pack` or fetching it with `ppk fetch`.
- Have write access to the artifact directory. The default is
  `/system/artifacts`; to use a directory of your own, pass
  `--artifact-dir`.

## Build the closure

Run `ppk closure` with the identifier of each package:

```sh
ppk closure 'dev.peios.hello@1.0?arch=noarch' 'dev.peios.greetings@2.0?arch=noarch' --artifact-dir artifacts
```

`ppk` prints the closure's identity:

```text
sha256:fc893f180b8bdbb4604098a845d2b5006e84caf1336ab07239867bc8ec1b7ac5
```

## Check the result

The closure's tree is in the `closures` directory of the artifact
directory, named by the hash in the identity:

```sh
ls artifacts/closures/fc893f180b8bdbb4604098a845d2b5006e84caf1336ab07239867bc8ec1b7ac5/usr/bin
```

The tree holds the files of every package, at their usual paths:

```text
hello
```

The closure's manifest, which lists its packages, is beside the tree with
the same name and a `.manifest` suffix. Running the same command again
reuses the closure and prints the same identity.

## Resolving a conflict

If two packages give the same path different contents, `ppk closure`
refuses and names both packages:

```text
ppk: "usr/bin/hello": dev.peios.clash@1.0?arch=noarch and dev.peios.hello@1.0?arch=noarch provide different entries
```

Nothing is written. Decide which package should provide the path, and
build the closure without the other one, or change one of the packages so
that the two don't overlap and pack it as a new version.

## Using another closure directory

To keep closures somewhere other than the artifact directory, pass
`--closure-dir`. The directory must be on the same filesystem as the
artifact directory.
