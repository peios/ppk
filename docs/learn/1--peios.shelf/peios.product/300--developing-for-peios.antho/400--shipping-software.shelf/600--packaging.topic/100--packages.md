---
title: Packages
type: concept
description: What a Peios package is, covering manifests and blobs, how a
  package's identity is derived, package identifiers, the layout of the
  artifact directory, and why a packed version never changes.
related:
  - peios/packaging/creating-a-package
---

A package is how software is shipped on Peios: a set of files, symbolic
links, and directories with a name, a version, and an architecture. `ppk`
stores a package as a list of its paths and the content of each file, and
names every piece by a hash of its content. That design explains why a
packed version can never change and why files shared between packages take
no extra space.

## Manifests and blobs

A package has two kinds of part:

- **Blobs.** A blob is the content of one file, named by its SHA-256 hash.
  It records nothing else: not the file's path, name, or permissions.
- **The manifest.** A manifest names the package and lists every path in
  it. Each path is a file, given by its blob and whether it is executable;
  a symbolic link, given by its target; or an empty directory.

A directory that contains anything is implied by its contents, so the
manifest lists only empty ones. Of a file's permissions, the manifest keeps
only whether it is executable.

The manifest for a package holding one program and one document looks like
this:

```toml
name = "dev.peios.hello"
version = "1.0"
arch = "noarch"

[files]
"usr/bin/hello" = { blob = "sha256:de7a5d81f4ab0680f55966b56e7bebfd5da26f4f45d8cd6e2e22c6a0e523c37f", exec = true }
"usr/share/doc/hello/README" = { blob = "sha256:c90139667a35e240080ae250c1f99a5df3a6d509b0aa65b3d5550f87e20cb3a4" }
```

Paths are relative to the root of the system the package is installed
into.

## A package's identity

A package's identity is the SHA-256 hash of its manifest, written as
`sha256:` followed by the hash in hexadecimal. The manifest names every
blob by its hash, so the identity covers the content of every file as well
as the paths, names, and versions.

`ppk` writes a manifest in exactly one way: entries in a fixed order, with
fixed spacing and quoting. The same directory therefore always produces the
same manifest and the same identity. You can rebuild a package and compare
identities to check that the result is identical.

## Package identifiers

You name a package with an identifier of this form:

```text
<name>@<version>[?arch=<arch>]
```

`<name>` and `<version>` are the package's name and version. `<arch>` is
the processor architecture its content is built for, such as `x86_64` or
`aarch64`. Use `noarch` for a package whose content runs anywhere, such as
documentation or scripts.

If you leave out `?arch=`, the architecture is the one `ppk` itself was
built for. Give it explicitly for any package built for another machine,
and always for `noarch` packages. The characters each part can contain are
listed in `ppk(1)`.

## The artifact directory

`ppk` stores packages in an artifact directory, which is
`/system/artifacts` unless you name another. It holds three kinds of file:

| Path | Contents |
|---|---|
| `blobs/sha256/<hash>` | The content of one file |
| `manifests/sha256/<hash>` | One manifest, named by its hash |
| `refs/<name>/<version>/<arch>` | The identity of the package with that name, version, and architecture |

A ref is an index. It lets you find a package by name, but every manifest
records its own name, version, and architecture, so the manifests remain
the authority.

Because a blob is named by its content, a file that appears in several
packages, or in several versions of one package, is stored once. Packing a
new version adds blobs only for the files that changed.

An artifact directory is also where other machines get packages from. `ppk
fetch` copies a package out of one artifact directory into another, checking
every hash on the way; see
[Fetching a Package](~peios/packaging/fetching-a-package).

## Packed versions never change

Once a package is packed, its name, version, and architecture always mean
the same content. Packing the same version again with identical content
succeeds and changes nothing. Packing it with any change to its files is
refused, so that a version means one thing everywhere it is used. To ship a
change, give it a new version.

Every file in the artifact directory is read-only for the same reason: a
blob, manifest, or ref that changed after it was written would stop matching
what refers to it.
