---
title: Composing an Environment
type: how-to
description: Lay out an environment's whole root in a directory with ppk
  compose-env, add the links and directories it needs outside /usr, and
  include its packages.
related:
  - peios/packaging/environments
  - peios/packaging/building-a-closure
---

`ppk compose-env` lays out an environment's whole root in a new directory:
the closure of the packages you name as its `usr`, and whatever else you
give it outside `usr`. Do this when a tool needs the environment as files,
such as an image builder making an initramfs from it. For how environments
work, see [Environments](~peios/packaging/environments).

## Before you start

- Install `ppk`.
- Have every package in your artifact directory.
- Choose a directory for the root that doesn't exist yet, on the same
  filesystem as the artifact directory: every file in the root is a hard
  link to its blob.

## Compose the root

Run `ppk compose-env` with the packages and `--out`:

```sh
ppk compose-env 'dev.peios.hello@1.0?arch=noarch' 'dev.peios.greetings@2.0?arch=noarch' --artifact-dir artifacts --out root
```

`ppk` prints the identity of the packages' closure, the same one
`ppk closure` prints for them:

```text
sha256:854cb23deb27e55765ee2a3138a745efb7cbc2a4cf2e3e0d53e8106fc308a8c6
```

`root/usr` holds the packages' files. Nothing is written to the artifact
directory.

## Add what lies outside usr

A package can only place files under `/usr`, so anything else the root
needs comes from the command line:

- `--symlink <path>:<target>` creates a link. The target is written as
  given, unless it starts with a second colon, which stands for `/usr`:
  `bin::bin` links `bin` to `usr/bin`.
- `--dir <path>` creates an empty directory, such as a mount point.
- `--include-artifacts` gives the root its own artifact directory,
  `system/artifacts`, holding the packages themselves, so that `ppk` inside
  the environment finds them.

Paths are relative to the root and can be given more than once:

```sh
ppk compose-env 'dev.peios.hello@1.0?arch=noarch' 'dev.peios.greetings@2.0?arch=noarch' --artifact-dir artifacts --out root --symlink bin::bin --dir tmp --include-artifacts
```

The root then holds `bin`, a link to `usr/bin`, an empty `tmp`, the
packages' artifacts in `system/artifacts`, and `usr`.

## If ppk refuses

`ppk compose-env` checks everything before it writes anything, and leaves
nothing behind when it refuses. It refuses a root that already exists:

```text
ppk: root: already exists
```

It refuses a path under a link you also gave, because creating it would
follow the link out of the root:

```text
ppk: "bin/extra": under "bin", which is a symlink
```

It also refuses packages that conflict, in the same way as
[`ppk closure`](~peios/packaging/building-a-closure), and any path that
is `usr` or lies under it.
