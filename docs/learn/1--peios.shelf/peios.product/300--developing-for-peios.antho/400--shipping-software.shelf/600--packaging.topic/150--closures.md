---
title: Closures
type: concept
description: What a closure is, covering how packages are composed into one
  tree of hardlinks, how a closure is named, the conflicts that stop a
  closure being built, why an existing closure is reused, and what copying
  a closure's files changes.
related:
  - peios/packaging/packages
  - peios/packaging/building-a-closure
---

A closure is a set of packages composed into one directory tree: the
contents of a `/usr`. Mounted at `/usr`, each package's files are at their
ordinary paths, such as `/usr/bin/hello`, so software runs from a closure
without any changes to where it looks for its files. Different closures
can hold different versions of the same package side by side.

## A tree of hardlinks

Unless you ask for copies, every package file in a closure is a hardlink
to its blob in the artifact directory: the same file on disk, not a copy.
Building a closure therefore copies no file content, and a file that
several closures share is stored once.

A hardlink shares everything about the file except its name, including
whether it is executable. That's why the artifact directory stores an
executable form of each executable blob, as described in
[Packages](~peios/packaging/packages). For the same reason, everything in
the artifact directory is read-only: a change made through one closure
would change the file in every closure that links it.

Symbolic links and directories aren't shared. `ppk closure` creates them
afresh in each tree. The root of the tree grants everyone read and execute,
and the directories and links inside it inherit that, so any user can reach
and run what a closure holds. Its files have the access their blobs grant,
described in [Packages](~peios/packaging/packages).

## Naming a closure

A closure has a manifest of its own, which lists the identities of its
packages:

```toml
packages = [
    "sha256:73a137f612f39b8ee1ffcbb7f51b7b9dc370b8662323d72b9e6b73b9d54413f8",
    "sha256:a1ffa25de2f7bfac1c4bb1663a5691cee317b6a05509db67b08c6ffd0b10db17",
]
```

The identities are sorted, and each appears once. The closure's identity
is the SHA-256 hash of this manifest, so the same set of packages always
makes the same closure, whatever order you name them in.

Closures are kept in the `closures` directory of the artifact directory
unless you name another. The tree is `closures/<hash>/` and the manifest is
`closures/<hash>.manifest`.

Because the name is known before anything is built, `ppk closure` reuses
a closure that already exists. It builds a new tree in a temporary
directory and moves it into place only when it's complete, so a closure
that exists is never half-built.

## Identifying the mounted closure

Each built closure also holds `inventory` at its top, visible as
`/usr/inventory` when mounted. `ppk compose-env` writes the same file in
the root's `usr`. `ppk run` exposes the inventory of the closure it mounts.
This generated file has mode `0444` and inherits the tree's security
descriptor, like directories without manifest metadata.

The inventory uses canonical TOML, following the package manifest format.
Its `closure` string records the closure identity. Its `[packages]` table
maps each quoted package identity to its full package ID, one per line,
sorted by identity. The inventory doesn't change how the closure is named.
`ppk list` reconstructs the closure manifest from these identities and
checks its hash against `closure` before printing the packages.
This checks the recorded set of identities; it doesn't authenticate the
package IDs or verify the files in the tree.

Run `ppk list` to print the current closure's full package IDs in byte
order, one per line. No artifact directory is needed. Run `ppk list --all`
to list every package indexed by the store's refs, also sorted by full ID.
Each line has `1`, a tab, and the ID when both ID and identity are in the
current closure; other packages have `0` in the first field.

A missing inventory makes `ppk list` fail with an explanation. With
`--all`, a missing inventory gives every package a `0` field. A malformed
inventory or mismatched closure identity makes either command fail.

An existing closure without an inventory is reused unchanged as a closure
built by an older ppk. To obtain an inventory, compose a fresh root or use
a separate closure directory. Reusing a closure keeps requiring only read
access to its tree.

## Conflicts

Packages in a closure share one tree, so they have to agree about every
path in it. Building a closure fails, before anything is written, when:

- **Two packages give one path different contents.** Two packages can
  ship the same file only if it is identical, such as a shared license
  text.
- **A path lies beneath another package's file or symbolic link.** One
  package can't put files inside something another package ships as a
  file. This also means a closure is never written through a symbolic
  link to somewhere outside it.

A package can put files in an empty directory that another package ships.

## The same mount

A hardlink can't cross from one mount to another, even between two mounts of
one filesystem, as the bind mounts of a build sandbox often are. So the
closure directory must be on the same mount as the artifact directory, and
`ppk closure` fails with nothing left behind if it isn't.

## Copied files

To build a closure on another mount, ask for copies with `--copy-files`. The
tree then has the same paths, content, and permissions, and the same
identity, but each file is a file of its own. `ppk` copies a file only when
you ask, rather than whenever a link fails, because a copy differs from a
hardlink in ways you should choose:

- **It takes space.** A copy is stored once per closure, not once per
  store.
- **It has a security descriptor of its own.** A hardlink has its blob's one
  descriptor, so the store controls access to every closure that links it.
  A copy gets the store's descriptor when it's made, so it grants what a
  hardlink would, but a later change to the store's descriptors doesn't
  reach it.
- **It is checked.** A copy has to read the blob, so `ppk` hashes it as
  [`ppk fetch`](~peios/packaging/fetching-a-package) does, and a blob that
  doesn't match its hash stops the build. Hardlinking reads no content, and
  checks none.

A closure is named by its packages, not by how its files got there, so
`ppk closure` reuses an existing closure whichever way it was built.
