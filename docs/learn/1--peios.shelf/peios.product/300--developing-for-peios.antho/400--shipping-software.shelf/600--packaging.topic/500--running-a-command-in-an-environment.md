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
the packages you request and everything they need. Do this to use a set of
packages without changing
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
ppk run -p org.figlet.figlet -- /usr/bin/figlet Peios
```

One `-p` takes every request after it, up to the next option. Requests accept
bare names, quoted names with constraints such as
`'org.figlet.figlet >= 2.2'`, and exact IDs such as
`'org.figlet.figlet@2.2.5-1?arch=x86_64'`. `ppk` resolves them against the
artifact directory, including dependencies and capability
requirements. It does not fetch missing packages: use `ppk fetch` first.
Unqualified requests use ppk's own architecture, or `--arch` if given.
An explicit architecture in a full ID selects that architecture when all
requests are qualified. One resolution holds one architecture.

To use the packages of an environment profile, give the profile first. With
`-p`, the profile's packages are exact pins in the same resolution. They
cannot be upgraded, and conflicting requests fail before composition. A
profile without `-p` stays exact: its dependencies are not resolved again.
A run keeps the rest of the filesystem as it is, so a
profile's links and directories don't apply:

```sh
ppk run env.toml -p dev.peios.extra -- /usr/bin/hello
```

The command runs with the chosen packages at `/usr`.

To compose exactly the IDs you name, pass `--no-deps`. This mode accepts
full IDs with an optional architecture, as before; names and constraints
are not accepted. Profiles remain exact in this mode too:

```sh
ppk run --no-deps -p org.figlet.figlet@2.2.5-1 -- /usr/bin/figlet Peios
```

If the named packages omit the program's loader or libraries, execution can
fail with `No such file or directory` even when the program itself exists.
The default resolution includes providers for the package's requirements.

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

The mounted closure includes its own `/usr/inventory`, so `ppk list`
inside the command reports that closure's packages. Include ppk and its
runtime packages if you want to run it there. `ppk list --all` reads the
store in the caller's root and marks membership in the mounted closure.
Closures built by an older ppk without an inventory are reused unchanged;
use a separate `--closure-dir` to build one with an inventory.
