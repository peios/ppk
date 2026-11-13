---
title: Closures
type: concept
description: What a closure is, covering how packages are composed into one
  tree of hardlinks, how its inventory records why each package is there,
  how a closure is named and given names of its own, the conflicts that stop
  a closure being built, why an existing closure is reused, and what copying
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

## Why each package is there

Each closure records why it holds each package, in its inventory: a file at
the top of its tree, visible as `/usr/inventory` when it's mounted.
`ppk compose-env` writes the same file in the root's `usr`. It's a line
format:

```text
closure sha256:9c41…
org.openssl.libssl@3.5.8-1?arch=x86_64 sha256:… [se.curl.curl libssl.so.3]
se.curl.curl@8.22.0-1?arch=x86_64 sha256:… [* >=8.22]
```

The first line names the closure. Each other line is a package: its full
ID, its identity, and a bracket for each thing that wants it.

- **Who wants it:** `*` for a request made of the closure itself, such as a
  package named after `ppk run -p` or in a profile, or the name of another
  package in the closure.
- **What it asked for:** a capability, such as a library's soname, if the
  package wasn't asked for by name.
- **Where from:** `#` and an index or channel, such as `#stable`, for a
  request that named one.
- **Which versions would do:** one token per expression of the constraint,
  such as `>=8.22`, or none for any version.

A package is wanted by every request, dependency or requirement it
satisfies. Every package in a closure must be wanted by something, and
every request satisfied. Lines are sorted by package ID, and brackets by who
wants the package, `*` first.

This generated file has mode `0444` and inherits the tree's security
descriptor, like directories without manifest metadata.

## Naming a closure

A closure's identity is the SHA-256 hash of its inventory without the first
line. So the same packages, asked for the same way, always make the same
closure, whatever order you name them in. The same packages asked for
differently, such as `*` asking for curl exactly in one and only through
another package in the other, are two closures.

Closures are kept in the `closures` directory of the artifact directory
unless you name another. The tree is `closures/<hash>/`, and its manifest,
the same lines as the inventory without the first, is
`closures/<hash>.manifest`.

Because the name is known before anything is built, `ppk closure` reuses
a closure that already exists. It builds a new tree in a temporary
directory and moves it into place only when it's complete, so a closure
that exists is never half-built.

A closure can also be given a name of your own with `ppk name`:

```sh
ppk name SYSTEM sha256:9c41…   # SYSTEM now names that closure
ppk name SYSTEM                # print the closure SYSTEM names
ppk name                       # list every name
ppk name --remove SYSTEM       # remove the name; the closure stays
```

A name is a symbolic link beside the closures, replaced by renaming, so it
always names one closure or another. An installed system names the closure
that is its `/usr` `SYSTEM`, and the one its initramfs is built from
`INITRAMFS`.

## Identifying the mounted closure

`ppk list` checks that the inventory's identity is the hash of its other
lines before printing the packages. This checks the recorded packages and
requests; it doesn't authenticate the package IDs or verify the files in the
tree.

Run `ppk list` to print the current closure's full package IDs in byte
order, one per line. No artifact directory is needed. Run `ppk list --all`
to list every package indexed by the store's refs, also sorted by full ID.
A package whose ID and identity are both in the current closure has `*`
before its ID; other packages have a space there, so every ID starts in the
same column.

A missing inventory makes `ppk list` fail with an explanation. With
`--all`, a missing inventory leaves every package unmarked. A malformed
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
