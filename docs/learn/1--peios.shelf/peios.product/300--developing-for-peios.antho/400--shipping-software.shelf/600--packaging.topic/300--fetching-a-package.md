---
title: Fetching a Package
type: how-to
description: Copy a package from another artifact directory with ppk fetch,
  what it checks on the way, and what happens when the package is already
  present.
related:
  - peios/packaging/packages
  - peios/packaging/creating-a-package
---

Fetching resolves a package and everything it needs from another artifact
directory, then copies the chosen packages into yours and checks them on the
way. Do this to use a package that was packed
somewhere else, such as on a build machine whose artifact directory you can
read.

## Before you start

- Install `ppk`.
- Have read access to the source artifact directory, as a local path.
- Have write access to your artifact directory. The default is
  `/system/artifacts`; to use a directory of your own, pass
  `--artifact-dir`.
- Know the package's name, such as `org.figlet.figlet`.

## Fetch the package

Run `ppk fetch` with the name and the source:

```sh
ppk fetch org.figlet.figlet --from shared --artifact-dir artifacts
```

`ppk` chooses the newest version that satisfies the request, follows its
dependencies and requirements, and fetches every chosen package. It prints
one identity per package fetched, in package-name order:

```text
sha256:73a137f612f39b8ee1ffcbb7f51b7b9dc370b8662323d72b9e6b73b9d54413f8
```

To constrain the version, quote the name and constraint, for example
`'org.figlet.figlet >= 2.2'`. A full ID such as
`'org.figlet.figlet@2.2.5-1?arch=x86_64'` pins that exact package, including
the version's spelling. Its dependencies are still resolved. Unqualified
requests choose for ppk's own architecture; `--arch` selects another. All
packages in one resolution must be for the same architecture. A full ID
with an explicit architecture can select that architecture on its own.

Resolution is greedy, as `ppk resolve` is. Missing dependencies, missing
capability providers and conflicting constraints fail with an error naming
what is needed, before copying starts.

To copy exactly one package without resolving anything it needs, pass
`--no-deps` and a full ID. This mode accepts IDs, with an optional
`?arch=…`, rather than bare names or constraints:

```sh
ppk fetch --no-deps 'dev.peios.hello@1.0-1?arch=noarch' --from shared --artifact-dir artifacts
```

## Check the result

Compare the identity with the one `ppk pack` printed where the package was
made. Because the identity is the hash of the manifest, and the manifest
names every file by its hash, the same identity means the same package,
byte for byte.

## Fetch from configured repositories

A machine keeps a list of the repositories it fetches from, each a file in
`/conf/ppk/repositories/`. Add one with `ppk repo add`, giving its public
key, so that its pointer file must be signed with it, and the channel it
follows:

```sh
ppk repo add peios /mnt/peios-repo --public-key 'ml-dsa-65:…' --track stable
```

That writes `/lcl/conf/ppk/repositories/peios.toml`, which `/conf/ppk` merges
with whatever the system ships. Then fetch without `--from`:

```sh
ppk fetch se.curl.curl
```

A request for a bare name, or a name with a constraint, follows the channel
its repository tracks, so this fetches the version `stable` holds, and what
it needs from `stable` too. Naming an index, as in `se.curl.curl#leading`, or
an exact version, takes that instead.

Each package name belongs to one repository. If two of them publish one, for
something you asked for or something it needs, `ppk` refuses rather than
choose. A repository's pointer file goes up in sequence each time it's
published, and `ppk` keeps the highest it has seen of each in
`/var/state/ppk`, refusing a repository that offers an older one. A mirror
might still be behind; `ppk repo list` shows when each was signed:

```text
peios /mnt/peios-repo #stable (sequence 42, signed 2026-11-13T13:11:46Z, signature checked)
```

## What fetch checks

`ppk fetch` trusts nothing it reads from the source:

- **The manifest** must hash to the identity in the source's ref.
- **Its form** must be exactly what `ppk pack` writes, naming the package
  you asked for, with every path relative and inside the package's root.
- **Each file's content** must hash to the name the manifest gives it.

If a check fails, `ppk fetch` stops with an error and doesn't record the
package. Only the files missing from your artifact directory are copied, so
fetching a new version transfers only what changed.

These checks prove that the package is the one the source's ref names. They
don't prove who published it, so fetch only from sources you trust.

## When the package is already present

By default, a chosen package already present with the same identity is
skipped without writes or output. Its dependencies are still resolved and
fetched if missing. With `--no-deps`, fetching it checks the package, copies
any missing files, and prints its identity as before. If your artifact
directory has a different package under the same name, version, and
architecture, `ppk fetch` refuses to replace it:

```text
ppk: dev.peios.hello@1.0-1?arch=noarch: already present as sha256:73a137f612f39b8ee1ffcbb7f51b7b9dc370b8662323d72b9e6b73b9d54413f8, but other has sha256:453a10dad4cb1902912919527452fa61c6420ae9152c1f02b57c1421a6d9d57b
```

A packed version never changes, so two different packages under one version
mean the two artifact directories disagree. Find out which one is right
before you use the package.

## Replace a package in a bootstrap's directory

A bootstrap builds its first packages with another system's tools, then
rebuilds them with its own at the same versions. Its artifact directory isn't
published until every package has been rebuilt, so nothing relies on the
first builds yet. To replace one, pass `--replace --no-deps` with its ID:

```sh
ppk fetch 'dev.peios.hello@1.0-1?arch=noarch' --from rebuilt --artifact-dir bootstrap --replace --no-deps
```

The ref then names the fetched package. The replaced manifest and its files
stay in the directory. Replacement requires `--no-deps` so that it affects
only the named package. Every check above still applies. Don't use
`--replace` on an artifact directory anything has fetched from: that breaks
the rule that a version never changes.
