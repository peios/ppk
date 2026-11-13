---
title: Publishing a Repository
type: how-to
description: Publish an artifact directory as a repository with ppk repo, with
  a signed catalog of every version, the leading index of the newest, release
  snapshots and the channels that follow them, and take a version back by
  yanking or deleting it.
related:
  - peios/packaging/packages
  - peios/packaging/fetching-a-package
---

A repository is an artifact directory that says what it publishes. Its
**catalog** lists every version it holds. Its **indexes** each hold one
version of each package for each architecture: `leading` always holds the
newest, and a **snapshot** holds a release, unchanged for ever. Its
**channels**, such as `stable`, each follow an index or another channel.

One file, the **pointer file**, says what the catalog, each index and each
channel are, and it is the only file that is signed. Everything else is
named by its hash, from there.

## Before you start

- Have the packages in an artifact directory, as
  [Creating a Package](~peios/packaging/creating-a-package) and
  [Fetching a Package](~peios/packaging/fetching-a-package) describe.
- Have an ML-DSA-65 private key for the repository, such as OpenSSL makes:

  ```sh
  openssl genpkey -algorithm ML-DSA-65 -out repo.key
  ```

  Keep it out of the repository and out of version control. Every `ppk repo`
  command that publishes reads it from `--key`, or from `$PPK_REPO_KEY`.

## Publish

Run `ppk repo update`:

```sh
ppk repo update --artifact-dir artifacts --key repo.key
```

It writes the catalog and `leading`, signs the pointer file, and prints it:

```text
sequence 1
signed 2026-11-13T13:11:46Z
catalog sha256:09cc9d05541917d9894cb2fc71a05373509789c0218b9ca5b7284ed6cca28619
index leading sha256:f5e090f9de6441810fab9e15909770d071687375e9f35c35d73244302c877ab6
```

The sequence goes up by one each time you publish. Run `ppk repo update`
again after adding packages.

`leading` holds only what can be installed from it. A package whose
dependencies or requirements `leading` can't satisfy is left out, with a
warning naming what it lacks, and so is anything that then lacks it. To make
that an error instead, pass `--strict`.

## Make a release

Take a snapshot of `leading` as the release's index, then point channels at
it:

```sh
ppk repo snapshot 2026.10.0 --artifact-dir artifacts --key repo.key
ppk repo channel 2026.10 2026.10.0 --artifact-dir artifacts --key repo.key
ppk repo channel stable 2026.10 --artifact-dir artifacts --key repo.key
ppk repo channel unstable leading --artifact-dir artifacts --key repo.key
```

A snapshot never changes, so its name must be new. To fix a release, take a
new snapshot, `2026.10.1`, and move the release's channel to it. When a
release gets no more fixes, mark its channel ended, so that a machine
following it can tell:

```sh
ppk repo channel 2026.10 2026.10.1 --ended --artifact-dir artifacts --key repo.key
```

## Take a version back

To stop new resolutions choosing a version, yank it. Anything that asks for
exactly that version still gets it:

```sh
ppk repo yank 'org.example.tool@1.2-1?arch=x86_64' --artifact-dir artifacts --key repo.key
```

`--undo` restores it. To remove a version altogether, delete it, giving a
reason:

```sh
ppk repo delete 'org.example.tool@1.2-1?arch=x86_64' --reason 'legal takedown' \
    --artifact-dir artifacts --key repo.key
```

That removes its ref, its manifest, and every file of it that no other
package shares. The catalog keeps a tombstone with the reason, so that
anything still asking for it can say why it's gone. Snapshots that hold it
aren't changed; publish a new one without it to keep a release installable.

## Check what a repository publishes

`ppk repo show` prints the pointer file. Give the repository's public key to
check its signature too:

```sh
ppk repo public-key --key repo.key
ppk repo show --artifact-dir artifacts --public-key 'ml-dsa-65:…'
```

For the files a repository holds, see `ppk(1)`.
