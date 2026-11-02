---
title: Composing an Environment
type: how-to
description: Lay out an environment's whole root in a directory with ppk
  compose-env from profiles or a list of packages, add the links and
  directories it needs outside /usr, share a layout between profiles, and
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
- Choose a directory for the root that doesn't exist yet. Every file in the
  root is a hard link to its blob, so the directory must be on the same
  mount as the artifact directory, unless you
  [compose on another mount](#compose-on-another-mount).

## Compose the root

1. Describe the environment in a profile, such as `env.toml`. For the
   format, see [Environments](~peios/packaging/environments):

   ```toml
   arch = "noarch"

   [packages]
   "dev.peios.hello" = "1.0-1"
   "dev.peios.greetings" = "2.0-1"
   ```

2. Run `ppk compose-env` with the profile and `--out`:

   ```sh
   ppk compose-env env.toml --artifact-dir artifacts --out root
   ```

`ppk` prints the identity of the packages' closure, the same one
`ppk closure` prints for them:

```text
sha256:fe5cf307f0b70379e1ec36b0d8dca659782446b0d13e7140c1257cd855ba61c4
```

`root/usr` holds the packages' files. Nothing is written to the artifact
directory.

To add packages to the profile's, or to compose without a profile, name
them after `-p`. One `-p` takes every package after it, up to the next
option, so the profile must come first:

```sh
ppk compose-env env.toml -p dev.peios.extra@1.0-1 --artifact-dir artifacts --out root
ppk compose-env -p dev.peios.hello@1.0-1 dev.peios.greetings@2.0-1 --artifact-dir artifacts --out root
```

These packages are all for the architecture `ppk` was built for. To name
another, give it in the package, as `dev.peios.hello@1.0-1?arch=noarch`, or
give `arch` in the profile.

## Add what lies outside usr

A package can only place files under `/usr`, so anything else the root
needs comes from the profile or the command line:

- A link, from the profile's `symlinks` or `--symlink <path>:<target>`.
  The target is written as given, unless it starts with a colon in the
  profile, or a second colon on the command line, which stands for `/usr`:
  `"bin" = ":bin"` and `bin::bin` both link `bin` to `usr/bin`.
- An empty directory, such as a mount point, from the profile's `dirs` or
  `--dir <path>`.
- The packages themselves, from the profile's `include_artifacts = true`
  or `--include-artifacts`, in the root's own artifact directory,
  `system/artifacts`, as hard links, so that `ppk` inside the environment
  finds them.
- `--copy-artifacts` does the same with copies, checking every manifest and
  blob as `ppk fetch` does, and wins over hard links.

Paths are relative to the root. The options add to what the profile
gives:

```sh
ppk compose-env env.toml --artifact-dir artifacts --out root --symlink bin::bin --dir tmp --include-artifacts
```

The root then holds `bin`, a link to `usr/bin`, an empty `tmp`, the
packages' artifacts in `system/artifacts`, and `usr`.

## Share a layout between profiles

When several environments need the same root layout, write it once as a
fragment, such as `layout.toml`:

```toml
dirs = ["proc", "sys", "dev", "tmp"]

[symlinks]
"bin" = ":bin"
"lib64" = ":lib"
```

Then include it from each profile. The path is relative to the profile's
own directory:

```toml
include = ["layout.toml"]

[packages]
"dev.peios.hello" = "1.0-1"
```

You can also give several profiles to `ppk compose-env`, which merges them
in the same way, so a profile that names only packages can be composed with
a layout:

```sh
ppk compose-env packages.toml layout.toml --artifact-dir artifacts --out root
```

If two of them disagree, `ppk` names both files and composes nothing:

```text
ppk: other.toml links "bin" to "usr/sbin", but layout.toml links it to "usr/bin"
```

## Compose on another mount

A hard link can't cross a mount, even between two mounts of one filesystem.
To compose a root on another mount than the artifact directory, such as a
fresh tmpfs, or in a sandboxed build step that has the artifact directory as
a read-only input, copy what would be linked:

- If the root has no artifact directory of its own, pass `--copy-files`.
  `root/usr` then holds a copy of each file, checked against its hash, and
  the root holds nothing else from the store:

  ```sh
  ppk compose-env env.toml --artifact-dir artifacts --out root --copy-files
  ```

- If it needs one, pass `--copy-artifacts`, which copies the packages into
  the root's `system/artifacts`. `usr` then hard links to that.

To see what a copy changes, including for security descriptors, see
[Copied files](~peios/packaging/closures#copied-files).

## If ppk refuses

`ppk compose-env` leaves nothing behind when it refuses. It checks all but
the hard links before it writes anything. It refuses a root that already
exists:

```text
ppk: root: already exists
```

It refuses a path under a link you also gave, because creating it would
follow the link out of the root:

```text
ppk: "bin/extra": under "bin", which is a symlink
```

It refuses another version of a package the profile holds, since an
environment holds one version of each:

```text
ppk: dev.peios.hello@2.0-1?arch=x86_64: the environment holds dev.peios.hello@1.0-1?arch=x86_64 already
```

It also refuses packages that conflict, in the same way as
[`ppk closure`](~peios/packaging/building-a-closure), and any path that
is `usr` or lies under it.
