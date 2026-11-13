---
title: Choosing Packages for an Environment
type: how-to
description: Write a profile with ppk resolve from the packages and
  capabilities you need and the artifact directories that hold them,
  following their dependencies and requirements, and understand how it
  chooses and why it can refuse.
related:
  - peios/packaging/environments
  - peios/packaging/recording-dependencies-and-metadata
  - peios/packaging/composing-an-environment
---

`ppk resolve` chooses an exact version of every package an environment
needs, from what you ask for and the packages in one or more artifact
directories, and writes them as a profile. It follows each package's
dependencies and requirements, as
[Recording Dependencies and Metadata](~peios/packaging/recording-dependencies-and-metadata)
records them. Composing a profile resolves its requests anyway, so use
`ppk resolve` when you want the answer fixed: a profile of exact versions
always composes the same environment, whatever else the artifact
directories come to hold. Compose it as
[Composing an Environment](~peios/packaging/composing-an-environment)
describes.

## Before you start

- Install `ppk`.
- Have every package the environment could need in one or more artifact
  directories. `ppk resolve` only reads them.

## Write the profile

Name the packages you need after `-p`, and the capabilities after `-c`.
Each request is one argument: a name, then optionally a constraint. Quote
a request with a constraint, so that the shell passes it whole. Give the
artifact directories after `--from`:

```sh
ppk resolve -p org.gnu.make 'org.gnu.gcc >= 16' -c sh --from stage1 --out root.toml
```

`root.toml` then holds one version of each package chosen:

```toml
arch = "x86_64"

[packages]
"org.git.kernel.dash" = "0.5.13.5-1"
"org.gnu.gcc" = "16.1.0-1"
"org.gnu.glibc" = "2.44-1"
"org.gnu.make" = "4.4.1-1"
```

Without `--out`, the profile goes to standard output. Like `-p`, `-c` and
`--from` take every argument after them, up to the next option. The
packages are for the architecture `ppk` was built for unless you give
`--arch`.

The profile holds only packages. To give it a root layout, include a
fragment that has one, or give both to `ppk compose-env`, as
[Share a layout between profiles](~peios/packaging/composing-an-environment#share-a-layout-between-profiles)
shows:

```sh
ppk compose-env root.toml layout.toml --artifact-dir stage1 --out root
```

## How it chooses

- A package you name, or that a chosen package depends on, is chosen by
  its name, at the newest version that satisfies the constraint.
- A capability, requested or required, is met by a package of that name,
  at its own version, or by one that provides it, at the version it
  provides. A capability provided without a version meets any constraint.
- Packages are settled before capabilities, at every step. So if you name
  a package that provides a capability, it's the one that meets it,
  wherever the request appears.
- Once chosen, a package is never reconsidered. Everything later that
  needs it must accept the version chosen.

Because nothing is reconsidered, the order matters when two things need
different versions of one package. A constraint you give on the command
line is settled before what the packages need, so naming the package with
a tighter constraint is how you steer the choice.

## If ppk refuses

`ppk resolve` writes nothing when it refuses. When something needs a
version of a package other than the one already chosen, it names both,
and says whether another version would have satisfied both:

```text
ppk: org.example.b@1.0-1 depends on org.example.c < 2, but org.example.c@2.0-1 was chosen for org.example.a@1.0-1 depends on org.example.c >= 1; org.example.c@1.5-1 would satisfy both, but finding it needs backtracking, which ppk doesn't do
```

Request the version that would satisfy both, here
`-p 'org.example.c < 2'`, or change a constraint.

When packages of several names provide a capability, `ppk` doesn't pick
one for you. Request the one you want by name:

```text
ppk: org.gnu.make@4.4.1-1 requires sh, but net.busybox.busybox, org.git.kernel.dash all provide it; request one of them by name
```

It also refuses when no version satisfies a request, when no package
provides a capability, and when one package is different in two of the
directories. A version can be spelt two ways, such as `1.0~a-1` and
`1.0~alpha-1`. No constraint can tell those apart, so if the version it
would choose has both, it refuses and asks you to remove one.
