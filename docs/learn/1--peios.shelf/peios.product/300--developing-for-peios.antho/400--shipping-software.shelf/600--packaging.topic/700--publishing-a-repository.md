---
title: Publishing a Repository
type: how-to
description: Publish an artifact directory as a repository with ppk repo, with
  a signed catalog of every version, the leading index of the newest, release
  snapshots and the channels that follow them, export a compressed copy and
  serve it over HTTP, and take a version back by yanking or deleting it.
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
provides sha256:f6ca384d90eb3223b077a93785c13ea3a9fb5ee53957f489e93cbd3b959e8015
index leading sha256:f5e090f9de6441810fab9e15909770d071687375e9f35c35d73244302c877ab6
```

The sequence goes up by one each time you publish. Run `ppk repo update`
again after adding packages.

Besides the catalog, `ppk repo update` writes each version's summary, the
part of its manifest that says what it needs and provides, and the
catalog's provides tree, which lists what provides each capability. A
machine choosing packages from the repository over HTTP reads those for
the names it needs, not every manifest the repository holds. A repository
published by an earlier `ppk` gets them the next
time you run `ppk repo update`, with the same packages.

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

## Install from a repository

Anything that resolves packages from a repository can ask for what an index
or channel holds, with `#` after the name:

```sh
ppk fetch 'se.curl.curl#stable' --from artifacts
ppk run -p 'se.curl.curl#2026.10.0' -- /usr/bin/curl --version
```

The version the index holds is chosen, and so is everything it depends on or
requires: a request for a release takes the release's versions of what it
needs too. A constraint after the index, as in `'se.curl.curl#stable >= 8'`,
must also hold. A closure records the index of each request that named one,
so its inventory shows what it was asked for from where.

A yanked version is chosen only by a request for exactly it, such as
`'org.example.tool = 1.2-1'`. A request that only a deleted version would
satisfy fails, giving the reason it was deleted.

## Export a compressed copy

Most of a repository is its blobs, the content of its packages' files. The
repository itself keeps them uncompressed, because machines and builds that
read it directly link them into closures. To serve it, export a copy whose
blobs are compressed:

```sh
ppk repo export --artifact-dir artifacts --to export --key repo.key
```

`ppk repo export` writes `export` as the repository as it was last
published, with each blob compressed with zstd under its own name, and
prints the pointer file it signed, which says `blobs zstd`. Nothing else is
compressed, and every package keeps its identity. `ppk` decompresses each
blob as it fetches it and checks it against its hash before storing it.

Run `ppk repo export` again after each `ppk repo update`. It compresses only
the blobs the export doesn't hold yet, and removes anything you deleted from
the repository. Compression uses zstd level 12 unless you pass `--level`,
from 1, the fastest, to 22, the smallest.

## Serve the repository over HTTP

Every file of a repository but `repository` and `repository.sig` is named by
its hash and never changes. A static web server can serve the repository's
directory, or its export, as it is, with no other software. Machines then
fetch from it by its URL, the URL of that directory:

```sh
ppk repo add peios https://pkgs.peios.org/ --public-key 'ml-dsa-65:…' --track stable
```

`ppk` reads each file at the URL followed by the file's path in the
directory, such as `https://pkgs.peios.org/repository` for the pointer file.
It doesn't need the server to list directories or answer range requests.

A copy of the repository on another server, such as a cache on your own
network, serves machines the same way. A cache can keep every file but
`repository` and `repository.sig` for ever. A machine given the cache's URL
before the repository's own tries the cache first and falls back to the
repository for anything the cache can't serve. Because every file is checked
against the key or its hash, a cache never has to be trusted for what it
sends.

## Check what a repository publishes

`ppk repo show` prints the pointer file. Give the repository's public key to
check its signature too:

```sh
ppk repo public-key --key repo.key
ppk repo show --artifact-dir artifacts --public-key 'ml-dsa-65:…'
```

`--artifact-dir` also takes the URL of a repository served over HTTP, to
check what a server is publishing.

For the files a repository holds, see `ppk(1)`.
