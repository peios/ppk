---
title: Environments
type: concept
description: What an environment is, covering the closure mounted at /usr,
  what stays as it was, how an environment is kept to the processes inside
  it, how a profile and its fragments describe one, and who can create
  one.
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

## Described by a profile

An environment profile is a TOML file that describes an environment
exactly, so that it can be named and reused rather than spelt out on the
command line each time:

```toml
dirs = ["proc", "sys", "tmp"]

[packages]
"net.busybox.busybox" = "1.37.0-1"
"dev.peios.prelude" = "0.0.7-1"

[symlinks]
"init" = ":sbin/prelude"
```

`packages` maps each package's name to its exact version, so a profile
holds one version of each package and always describes the same
environment. Whatever chooses the versions, such as a tool that picks the
newest, does so when it writes the profile. `symlinks` and `dirs` are what
the root holds outside `/usr`, where a leading `:` in a link's target
stands for `usr`. `include_artifacts = true` gives a composed root its
packages in an artifact directory of its own. Every key is optional.
`arch` sets the architecture of every package, the one `ppk` was built for
by default.

A profile can `include` other profiles, called fragments, so that what
several environments share, such as the layout of a root, is written once:

```toml
include = ["root-layout.toml"]

[packages]
"net.busybox.busybox" = "1.37.0-1"
```

Each path is relative to the file that names it, and a fragment can include
others. The profile holds everything its fragments hold. Where two of them
hold the same thing, it's held once, and where they disagree, as with two
versions of one package or two targets for one link, it's an error naming
both files. Nothing decides which one wins: you change one of them.

`ppk compose-env` and `ppk run` both take profiles, and several given
together merge as fragments do. A run keeps the rest of the filesystem as
it is, so it uses only the profiles' packages.

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
