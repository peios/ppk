---
title: Recording Dependencies and Metadata
type: how-to
description: Record what a package depends on, requires, provides and
  claims, and describe it, with ppk pack's options, then check the
  manifest and understand what ppk does with what it records.
related:
  - peios/packaging/packages
  - peios/packaging/creating-a-package
---

A package's manifest can say what the package needs, what it offers, and
what it is, as well as listing its files. Record these when you pack it, so
that whatever builds an environment from packages can choose the ones that
work together. For what a manifest holds, see
[Packages](~peios/packaging/packages#what-else-a-manifest-says).

## Before you start

- Be able to pack the package, as
  [Creating a Package](~peios/packaging/creating-a-package) describes.
- Know what the package needs at run time that isn't in its own files and
  that its programs don't link: the packages and commands it runs by name or
  loads itself. `ppk pack` finds the libraries its programs link, as
  [What ppk pack finds in the files](#what-ppk-pack-finds-in-the-files)
  describes.

## Record what the package needs and offers

Give `ppk pack` an option for each thing to record. The options that take
two values take them as two separate arguments, so neither value needs a
separator. An empty value means "any version" for a dependency or
requirement, and "no particular version" for something provided:

```sh
ppk pack 'org.git.kernel.dash@0.5.13.5-1?arch=x86_64' dash --artifact-dir artifacts \
  --depends org.gnu.glibc '>= 2.44' \
  --provides sh '' \
  --claims /usr/bin/sh dash
```

- `--depends <name> <constraint>` records a dependency on another package,
  by its name.
- `--requires <name> <constraint>` records a need for a capability rather
  than a particular package: a name such as the shell `sh`, or
  `pkgconfig(zlib)`, which any package can provide. A library that a
  program links is found for you.
- `--provides <name> <version>` records a capability this package offers.
- `--claims <path> <target>` records a link at an absolute path that the
  package provides besides its files, with the link's target. Use it for a
  name that one of several packages answers, such as `/usr/bin/sh`.

Give each option once for each package, capability or path. A name given
twice is an error.

## What ppk pack finds in the files

`ppk pack` reads each ELF program and library in the files, and adds what
they need and offer to what the options give:

- Each library a program or library links (its `DT_NEEDED`) becomes a
  requirement, such as `libtinfo.so.6`. A library the package itself
  provides isn't required.
- Each library's own name (its `DT_SONAME`) becomes a capability the package
  provides.
- For glibc's libraries and `libgcc_s.so.1`, the requirement has a floor: the
  newest symbol version the programs use. A program using `GLIBC_2.38`
  requires `libc.so.6 >= 2.38`. The package providing the library provides it
  at its own version, such as `2.44-1`.
- For `libstdc++.so.6`, each symbol version is a capability of its own, such
  as `elfver(libstdc++.so.6:GLIBCXX_3.4.21)`.

Files under `src/` and symbolic links aren't read, nor is anything that
isn't ELF.

Where an option names a capability that the files also give, the option's
entry is kept. The floors are the exception: they come only from the files,
so don't write them yourself. A floor written by hand tends to record what
the package was built against, not what it needs, and it would keep the
package off a system with an older glibc that would run it. To require a
newer glibc for a reason the symbols don't show, depend on the glibc package
with `--depends` and say why in the recipe.

The rules in full are under DERIVATION in `ppk(1)`.

## Write a constraint

A constraint is one or more expressions, separated by commas, that a
version must all satisfy:

| Constraint | Satisfied by |
|---|---|
| `>= 2.44` | `2.44` and later, whatever the revision: `2.44-1`, `2.44-3`, `2.45-1` |
| `>= 2.44-3` | `2.44-3` and later, but not `2.44-2` |
| `>= 1.0, < 2.0` | `1.0` and later, before `2.0` |
| `= 2.44-1` or `2.44-1` | Exactly `2.44-1` |
| `!= 2.43` | Anything but upstream version `2.43` |
| Empty | Any version |

The operators are `=`, `>`, `>=`, `<`, `<=`, and `!=`, and a version
without one means `=`. A version in a constraint can leave out its
revision, and the revision then isn't compared. That's usually what you
want: repackaging the same upstream version shouldn't change whether a
dependency is met. Versions compare as
[Package Versions](~peios/packaging/packages#package-versions) describes.

## Describe the package

Add the descriptive options to the same command:

```sh
  --description 'POSIX-compliant shell' \
  --homepage https://git.kernel.org/pub/scm/utils/dash/dash.git \
  --license BSD-3-Clause \
  --license-class free
```

- `--description` is one line of printable ASCII.
- `--homepage` is an `http` or `https` URL.
- `--license` is the package's license, conventionally an SPDX expression,
  and `--source-license` the license of the source it was built from, where
  that differs.
- `--license-class` is `free`, `firmware`, `proprietary`, or `unknown`, the
  default.

## Check the result

`ppk pack` prints the package's identity. Its manifest, in
`artifacts/manifests/sha256/` followed by that hash, records everything you
gave and everything found in the files, in one fixed order:

```toml
name = "org.git.kernel.dash"
version = "0.5.13.5-1"
arch = "x86_64"
description = "POSIX-compliant shell"
homepage = "https://git.kernel.org/pub/scm/utils/dash/dash.git"
license = "BSD-3-Clause"
license_class = "free"

[claims]
"/usr/bin/sh" = "dash"

[dependencies]
"org.gnu.glibc" = ">= 2.44"

[provides]
"sh" = ""

[requires]
"libc.so.6" = ">= 2.38"

[files]
"bin/dash" = { blob = "sha256:e4a76dc6517a94c9f1abde8582aa3909a1315e28a66a5c9f304c5a697b74c638", exec = true }
```

The requirement on `libc.so.6` comes from `bin/dash`, which uses symbols as
new as `GLIBC_2.38`. Each constraint is written in one form, whatever
spacing you gave it, and anything that wasn't given or found is left out.

## When an option is refused

`ppk pack` checks everything before it writes anything. Anything that breaks
a rule is a usage error, with exit status 2, and nothing is packed. A
dependency must name a package, so a capability given to `--depends` is
refused:

```text
ppk: dependencies: invalid package name 'sh': use a reverse-DNS name such as dev.peios.hello, of at most 64 characters, with at least 3 dot-separated labels of lowercase ASCII letters, digits, - and +
```

Use `--requires` for it instead. A constraint that doesn't parse names its
option:

```text
ppk: --depends org.gnu.glibc: constraint '>=2.44,' has an empty expression
```

A floor on one of glibc's libraries or `libgcc_s.so.1` fails, with exit
status 1, because only the files can say what it is. The message names what
you wrote and what the files need:

```text
ppk: requires libc.so.6: the written floor '>= 2.44' is refused, and the payload needs '>= 2.38' (bin/dash); a floor on libc.so.6 comes only from the payload's symbol versions
```

An ELF file that `ppk pack` can't read fails in the same way, naming the
file.

The full rules for each value are in `ppk(1)`.

## What ppk does with them

The dependencies, requirements, capabilities and claims you record are part
of the package and its identity, and `ppk` checks them when it reads a
manifest. `ppk` doesn't act on them yet: no command chooses packages to
satisfy them, and composing an environment doesn't create claimed links.
Name every package an environment needs, as
[Composing an Environment](~peios/packaging/composing-an-environment)
describes.

Changing any of them changes the package, so it needs a new version, as a
change to its files does.
