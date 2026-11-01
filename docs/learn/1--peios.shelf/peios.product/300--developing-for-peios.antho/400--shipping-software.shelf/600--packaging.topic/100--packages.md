---
title: Packages
type: concept
description: What a Peios package is, covering manifests and blobs, how a
  package's identity is derived, package identifiers and names, the layout
  of the artifact directory, and why a packed version never changes.
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
  It doesn't record the file's path or name.
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
"bin/hello" = { blob = "sha256:de7a5d81f4ab0680f55966b56e7bebfd5da26f4f45d8cd6e2e22c6a0e523c37f", exec = true }
"share/doc/hello/README" = { blob = "sha256:c90139667a35e240080ae250c1f99a5df3a6d509b0aa65b3d5550f87e20cb3a4" }
```

Paths are relative to `/usr`, where everything Peios ships lives, including
the configuration it ships in `/usr/etc`. This package's `bin/hello` is
`/usr/bin/hello`. A package can't ship anything outside `/usr`: a machine's
own configuration and state live outside it, and stay the machine's. An
absolute symbolic link target is read from the root of the system, so a
link to `/usr/lib/libz.so.1` names a file in the same or another package.

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
and always for `noarch` packages. The characters a version and an
architecture can contain are listed in `ppk(1)`.

## Package names

A package's name is reverse-DNS: the domain of whoever owns the software,
backwards, then the product. `ppk` refuses any other name, wherever one
reaches it, including in a fetched manifest.

| Software | Name | Example |
|---|---|---|
| Peios's own | `dev.peios.<product>` | `dev.peios.hello` |
| A testing-only Peios product | `dev.peios.testing.<product>` | `dev.peios.testing.fixture` |
| Anyone else's | The upstream project's domain | `org.gnu.bash`, `io.github.eudev-project.eudev` |
| Part of a split package | The complete name of the base, with a suffix | `org.gnu.glibc-devel` |

Use the upstream project's own domain for software that Peios only packages.
A name doesn't rename anything: a program keeps its own executable, service,
and other externally defined names.

A name has at least three labels, separated by dots, and at most 64
characters. Each label holds lowercase ASCII letters, digits, `+`, and `-`,
starts with a letter or digit, ends with a letter, digit, or `+`, and has no
`--`. The first label holds only letters, as a top-level domain does, and
neither of the first two holds a `+`.

`ppk` can only check a name's form. It can't tell whether a domain is the
right one for the software, or whether a split keeps its base's name, so
those are for whoever writes the package.

## The artifact directory

`ppk` stores packages in an artifact directory, which is
`/system/artifacts` unless you name another. It holds these kinds of file:

| Path | Contents |
|---|---|
| `blobs/sha256/<hash>` | The content of one file that isn't executable |
| `blobs/sha256/<hash>.exec` | The content of one executable file |
| `manifests/sha256/<hash>` | One manifest, named by its hash |
| `refs/<name>/<version>/<arch>` | The identity of the package with that name, version, and architecture |

A blob is stored in the form its manifest entries need: plain, executable,
or both if the same content is shipped both ways. The form is part of the
stored file, not of the blob's name, so a package's identity doesn't depend
on it.

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

## Who can read a package

Everyone on a machine can read and run what's in its artifact directory.
`ppk` gives every file it stores a security descriptor that grants Everyone
read and execute, and SYSTEM and Administrators full control. The
directories it creates there grant the same, so any user can reach the
packages.

A package is the same on every machine it's installed on, because its
content is named by hash. It can't hold anything that belongs to one
machine or one user, and anything in it is readable by every user of every
machine that stores it.

> [!IMPORTANT]
> Never put a secret, such as a key or a password, in a package. Keep
> secrets in the machine's own configuration instead.
