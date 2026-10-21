---
title: Running a Command in an Environment
type: how-to
description: Run a command or a shell with ppk run so that a set of packages
  is its /usr, pass arguments to it, and read its exit status.
related:
  - peios/packaging/environments
  - peios/packaging/building-a-closure
---

`ppk run` runs a command in an environment whose `/usr` is the closure of
the packages you name. Do this to use a set of packages without changing
`/usr` for anything else on the machine. For how environments work, see
[Environments](~peios/packaging/environments).

## Before you start

- Install `ppk`.
- Have every package in the artifact directory. If their closure doesn't
  exist yet, you also need write access to the closure directory; to run an
  existing closure you need only read access.

## Run the command

Run `ppk run` with the packages after `-p`, then `--`, then the command
and its arguments:

```sh
ppk run -p 'dev.peios.hello@1.0?arch=noarch' -- /usr/bin/hello
```

One `-p` takes every package after it. To use the packages of an
environment profile, give the profile first; packages after `-p` are added
to its own. A run keeps the rest of the filesystem as it is, so a
profile's links and directories don't apply:

```sh
ppk run env.toml -p dev.peios.extra@1.0 -- /usr/bin/hello
```

The command runs with the packages at `/usr`:

```text
Hello from Peios
```

Everything after `--` is the command, so its own options aren't read as
`ppk`'s. A command without a `/` is looked up through `PATH` inside the
environment.

## Start a shell instead

Leave out `--` and the command, and `ppk run` starts `/usr/bin/sh` in the
environment:

```sh
ppk run -p net.busybox.busybox@1.37.0-1
```

The shell is the closure's own, not yours, so one of the packages must
provide `bin/sh`. Commands you run from it share its environment.

## Check the result

`ppk run` exits with the command's own exit status. If the command can't be
started, the status is 127 when it isn't found and 126 when it isn't
executable, and `ppk` says why:

```text
ppk: /usr/bin/nothing: No such file or directory
```

Outside the command, `/usr` hasn't changed.
