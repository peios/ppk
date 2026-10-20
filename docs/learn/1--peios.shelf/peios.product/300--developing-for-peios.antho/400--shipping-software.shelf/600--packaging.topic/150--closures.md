---
title: Closures
type: concept
description: What a closure is, covering how packages are composed into one
  tree of hardlinks, how a closure is named, the conflicts that stop a
  closure being built, and why an existing closure is reused.
related:
  - peios/packaging/packages
  - peios/packaging/building-a-closure
---

A closure is a set of packages composed into one directory tree, laid out
as a system root. Each package keeps its ordinary paths, such as
`/usr/bin/hello`, so software runs from a closure without any changes to
where it looks for its files. Different closures can hold different
versions of the same package side by side.

## A tree of hardlinks

Every file in a closure is a hardlink to its blob in the artifact
directory: the same file on disk, not a copy. Building a closure therefore
copies no file content, and a file that several closures share is stored
once.

A hardlink shares everything about the file except its name, including
whether it is executable. That's why the artifact directory stores an
executable form of each executable blob, as described in
[Packages](~peios/packaging/packages). For the same reason, everything in
the artifact directory is read-only: a change made through one closure
would change the file in every closure that links it.

Symbolic links and directories aren't shared. `ppk closure` creates them
afresh in each tree.

## Naming a closure

A closure has a manifest of its own, which lists the identities of its
packages:

```toml
packages = [
    "sha256:3eeece7f77f6140977b3b906292d9d30c6595d9c3b2bdad4c333a50745795309",
    "sha256:5d843b03aa4090b4d375f1c91f001aba9d5af2aaa03bab4ee2b49abc315c8845",
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

## The same filesystem

A hardlink can't cross from one filesystem to another, so the closure
directory must be on the same filesystem as the artifact directory.
`ppk closure` checks this before it writes anything.
