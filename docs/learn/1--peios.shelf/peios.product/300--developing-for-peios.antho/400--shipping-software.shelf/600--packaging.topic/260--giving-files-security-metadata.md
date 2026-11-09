---
title: Giving Files Security Metadata
type: how-to
description: Give a package's files and directories security descriptors
  of their own and sign its firmware when you pack it with ppk pack, check
  the manifest, and understand what ppk does where it can't set them.
related:
  - peios/packaging/packages
  - peios/packaging/creating-a-package
  - peios/packaging/recording-dependencies-and-metadata
---

Most files in a package get the security descriptor that `ppk` gives
everything it stores. Some need one of their own: a program that only one
service can run, or a directory that only one service can write. Firmware
needs a signature, which the kernel checks before it gives the firmware to
a device. Record both when you pack the package, and `ppk` gives each file
and directory its metadata wherever it places it. For what a manifest holds,
see [Security metadata](~peios/packaging/packages#security-metadata).

## Before you start

- Be able to pack the package, as
  [Creating a Package](~peios/packaging/creating-a-package) describes.
- Have each descriptor as a self-relative security descriptor in
  hexadecimal, with an owner, a group, and a DACL. `ppk` takes the
  descriptor's bytes, not SDDL.
- Have each firmware file's signature: the 3310-byte file that a signer
  writes beside the file, named with `.peios.sig` added.

## Give a file or directory a descriptor

Give `ppk pack` the `--descriptor` option with the path, as the manifest
lists it, and the descriptor. The two values are separate arguments:

```sh
ppk pack 'dev.peios.gxwi@1.0-1?arch=x86_64' gxwi --artifact-dir artifacts \
  --descriptor bin/gxwi-server 010004801400000020000000000000002c000000...
```

The descriptor is shortened here. Give the option once for each path. The
path is relative to `/usr`, so `bin/gxwi-server` is `/usr/bin/gxwi-server`.

- A file keeps its descriptor in every closure and environment, because
  each placed file is a hard link to a stored file that has it, or a copy
  given it.
- A directory with a descriptor is listed in the manifest even if it has
  contents. It's created with its descriptor before anything inside it, so
  what's created in it inherits from that descriptor.
- A symbolic link can't have a descriptor of its own.

## Sign a file

Put each signature in the directory you pack, beside the file it signs,
with `.peios.sig` added to the file's name:

```text
gxwi/lib/firmware/acme/fw.bin
gxwi/lib/firmware/acme/fw.bin.peios.sig
```

`ppk pack` doesn't pack the signature as a file. It records it in the
signed file's entry, and the file gets it as its `security.peios.sig`
attribute wherever it's placed. A signature must be exactly 3310 bytes and
start with the version byte, 1. The file it signs must be a regular file in
the same package, and not an ELF file: an ELF file carries its signature in
its own `.peios.sig` section. No other path can end in `.peios.sig`.

`ppk` doesn't check what a signature signs. That's the kernel's job, when it
loads the file.

## Check the result

`ppk pack` prints the package's identity. In its manifest, in
`artifacts/manifests/sha256/` followed by that hash, each file and directory
with security metadata has it in its entry, after its blob or `dir`:

```toml
"bin/gxwi-server" = { blob = "sha256:07f7ab476bc3a83fad639d34a012cb4a5f859441f0d24c11627ca96696839012", exec = true, descriptor = "010004801400000020000000000000002c000000..." }
"lib/firmware/acme/fw.bin" = { blob = "sha256:07f7ab476bc3a83fad639d34a012cb4a5f859441f0d24c11627ca96696839012", signature = "sha256:ec87928fa1c24dacf5ebe2eaf8f7adf473de9ef65f8b7648ead92e9f7672155b" }
```

The descriptor is written in lowercase, whatever case you gave it, and is
shortened here. The signature is named by its hash, and stored as a blob.

## Packing on a system other than Peios

On Peios, `ppk` gives each stored file, copy, and directory its metadata as
it creates it, and fails if it can't. On another system, such as the Linux
host that builds packages, it writes the `security.peios.sd` and
`security.peios.sig` attributes only where that system lets it, which
usually needs privilege. Where it can't, it packs the files without them
and warns:

```text
ppk: warning: 2 files and directories lack security metadata this system would not let ppk give; their manifests record it
```

The package is still complete: its manifest records the metadata, and
`ppk fetch` on Peios gives each stored file what its entry records. `ppk
closure`, `ppk compose-env`, and `ppk run` warn in the same way when they
place files that lack it.

> [!NOTE]
> On Peios, a stored file's descriptor can't be read back as it was given,
> so `ppk` doesn't give a descriptor to a stored file that was stored
> elsewhere without one. Fetch the package on Peios, or copy its files with
> `--copy-files`, so that each gets its descriptor as it's created. A
> missing signature is given whenever the file is placed.

## When something is refused

`ppk pack` checks everything before it writes anything. A descriptor that
isn't hexadecimal, or isn't a whole self-relative descriptor, is a usage
error, with exit status 2:

```text
ppk: --descriptor bin/gxwi-server: security descriptor has no owner or no group
```

These fail with exit status 1, and nothing is packed:

- A descriptor for a path that isn't a file or directory of the package, or
  for a symbolic link.
- A signature that isn't 3310 bytes, or doesn't start with version 1.
- A signature beside nothing, or beside a symbolic link, or an ELF file:

  ```text
  ppk: "bin/tool.peios.sig": signs "bin/tool", an ELF file, which carries its signature in its .peios.sig section
  ```

- Anything else whose name ends in `.peios.sig`.

`ppk closure` and `ppk compose-env` refuse two packages that give one path
entries that differ only in their security metadata, as they refuse any
other two different entries. A stored file whose signature attribute isn't
the one its entry names is an error, because it isn't the file its name
says. The full rules are under SECURITY METADATA in `ppk(1)`.
