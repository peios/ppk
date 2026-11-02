---
title: Creating a Package
type: how-to
description: Stage a directory of files and pack it with ppk pack, then
  confirm the package's identity, pack for another architecture, and ship a
  changed package as a new version.
related:
  - peios/packaging/packages
---

Packing turns a directory of files into a package in an artifact directory.
Do this when you have built software and want to ship it as a named,
versioned package. For what a package contains, see
[Packages](~peios/packaging/packages).

## Before you start

- Install `ppk`.
- Choose the package's version, the architecture its content is built for,
  and its name, which must follow the
  [rules for package names](~peios/packaging/packages#package-names).
- Have write access to the artifact directory. The default is
  `/system/artifacts`; to use a directory of your own, pass
  `--artifact-dir`.

## Pack a directory

1. Stage the package's files in a directory, laid out as they will appear
   under `/usr`. For example, a program installed as `/usr/bin/hello` goes
   at `hello/bin/hello`.
2. Make sure each program in the directory is marked executable. The
   package records which files are executable and nothing else about their
   permissions.
3. Run `ppk pack` with the package's identifier and the directory:

   ```sh
   ppk pack 'dev.peios.hello@1.0-1?arch=noarch' hello --artifact-dir artifacts
   ```

   Quote the identifier, because some shells treat `?` as a wildcard.
   `ppk` prints the package's identity:

   ```text
   sha256:73a137f612f39b8ee1ffcbb7f51b7b9dc370b8662323d72b9e6b73b9d54413f8
   ```

## Check the result

Read the package's ref, which is named by its name, version, and
architecture:

```sh
cat artifacts/refs/dev.peios.hello/1.0-1/noarch
```

It holds the identity that `ppk pack` printed:

```text
sha256:73a137f612f39b8ee1ffcbb7f51b7b9dc370b8662323d72b9e6b73b9d54413f8
```

The manifest is `artifacts/manifests/sha256/` followed by the same hash.
It lists every path in the package.

## Packing for another architecture

If the content is built for a particular processor, name its architecture
in the identifier:

```sh
ppk pack 'dev.peios.hello@1.0-1?arch=aarch64' hello-aarch64 --artifact-dir artifacts
```

Without `?arch=`, `ppk` uses the architecture it was built for itself. The
same name and version can be packed once for each architecture.

## Shipping a change

A packed version never changes. If you pack the same version again with
different files, `ppk` refuses and exits with status 1:

```text
ppk: dev.peios.hello@1.0-1?arch=noarch: already packed as sha256:73a137f612f39b8ee1ffcbb7f51b7b9dc370b8662323d72b9e6b73b9d54413f8; a packed version cannot be replaced
```

Pack the changed files as a new version instead, such as
`dev.peios.hello@1.1-1?arch=noarch`. Only the files that changed are stored
again. Packing a version again with identical files succeeds and prints the
same identity.
