---
title: Environments
type: concept
description: What an environment is, covering the closure mounted at /usr,
  what stays as it was, how an environment is kept to the processes inside
  it, and who can create one.
related:
  - peios/packaging/closures
  - peios/packaging/running-a-command-in-an-environment
  - peios/packaging/composing-an-environment
---

An environment is a view of the filesystem in which a closure is `/usr`.
Everything Peios ships lives under `/usr`, so a program in an environment
finds the exact set of packages the closure holds, while everything else,
such as the machine's own configuration, its state, and your home
directory, stays where it was. Different programs can run side by side with
different closures as their `/usr`.

## What changes and what doesn't

An environment starts from the filesystem as the process that creates it
sees it, and replaces only `/usr` with the closure. Directories such as
`/lcl`, `/var`, `/home`, and `/run` are the same ones, so a program in an
environment reads the machine's configuration and shares `/run` with the
rest of the machine.

Packages are laid out relative to `/usr` for this reason: a package's
`bin/hello` is `/usr/bin/hello` in any environment that includes it, and a
package can't place anything outside `/usr`.

## Kept to its own processes

An environment is a mount namespace: a private copy of the table that says
which filesystem is mounted where. `ppk run` creates one, mounts the
closure at `/usr` in it, and starts the command there. The command and
every process it starts see the closure; nothing outside sees any change,
and when the last of them ends, the environment ends with it.

Each `ppk run` creates a new environment, even for the same closure. The
closure itself is built once and reused.

## Laid out as files

An environment can also be laid out as a directory, as the whole root of a
filesystem, for a tool such as an image builder to take. `ppk compose-env`
does this. There is no running system around such a root to borrow the rest
of the filesystem from, so what lies outside `/usr`, such as links and
mount points, is given when the root is composed. It belongs to the
environment, not to any package.

## Who can create one

Any process can create a mount namespace of its own and mount a closure in
it, so `ppk run` needs no privilege. Building a closure needs write access
to the closure directory, but running one that already exists only reads
the store, so an ordinary user can run any closure an administrator has
built.

The closure is mounted as it is, not read-only: making a mount read-only
needs a privilege that ordinary users don't hold. Its files are protected by
their security descriptors instead, which grant everyone read and execute
and nothing more.
