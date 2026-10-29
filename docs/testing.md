# Testing ppk

## Building

[pekit](https://learn.peios.org/pekit) builds ppk as a static,
position-independent musl program with full RELRO, which an initramfs runs
before any C library is there:

```sh
pekit build ppk
```

The rust library's pinned Rust and the gcc library's Bootlin toolchain
build it, so no Rust or C toolchain is needed on the host. `pekit.lock`
pins the libraries.

### Building against ppk

A pekit workspace that takes ppk's crates by git, as peiso does, imports
ppk's GCSL library at the same commit as its `Cargo.lock` pins, and gives
its workspace ppk's settings:

```gcsl
import "github.com/peios/ppk" as ppk

rust.workspace(dir("."), rust.vendored(file("Cargo.lock")), crates = ppk.crate_settings())
```

`crate_settings()` places the kernel file that the vendored `peios-uapi`
crate reads through `#[path]` where that path reaches.

## Host tests

```sh
tools/check
```

This checks style with the pinned PSC tools, then runs `pekit test`. It
builds ppk, runs its unit tests, integration tests and documentation
tests, and checks it with Clippy, with every warning denied. The tests
cover every command on a writable `/tmp` in a sandbox, not on the host's
own filesystem. Prepare the style tools once, outside the repository:

```sh
python3 tools/prepare-style-tools.py ~/.cache/peios/ppk-style
export PEIOS_STYLE_BIN=~/.cache/peios/ppk-style/bin
```

GitHub CI checks style only. Builds and tests run with pekit on the build
server and with `tools/check` locally.

## Peios VM tests

`tests/vm/` runs `ppk` on a real Peios kernel with KACS enforcing, using
[provium](https://learn.peios.org/provium). The kernel boots with no
initramfs of its own: provium's agent is PID 1 and the whole userspace, and
each test pushes in a static `ppk` and the host's static `busybox`.

### Before you start

- Install provium with its agent overlay, and the host access its preflight
  check asks for (KVM, vhost-vsock, network privileges).
- Have a statically linked `busybox` at `/usr/bin/busybox`.
- Have a Peios kernel with vsock support. `tests/vm/provium.toml` names one
  from the cached full-system test image; change `kernel` to use another.
- Install the musl target: `rustup target add x86_64-unknown-linux-musl`.

### Run the tests

The test takes ppk from Cargo's musl build, where `tests/vm/ppk.test.lua`
looks for it:

```sh
cargo build --release --target x86_64-unknown-linux-musl
cd tests/vm
provium .
```

Add `--json` to see each test's `t:log` output.

## An interactive shell

`tools/vm-shell` builds a Peios ISO from `ppk` packages and live-boots it
in QEMU under UEFI, with a busybox shell on the serial console, so you can
try commands by hand:

```sh
tools/vm-shell
```

It needs the `kernel`, `peiosutils`, `peiso`, `pekit`, `pkgs` and
`prelude` repositories beside `ppk`, OVMF, `mkfs.vfat`, mtools and
`xorriso` on the host, Docker with the kernel build's pinned image, and a
static musl Peios SDK for building `seed-sd`. Prepare the SDK
once with prelude's `tools/prepare-sdk.py`, as prelude's `docs/building.md`
describes, into `~/.cache/peios/prelude-sdk-musl`, or pass `--sdk` or set
`PEIOS_SDK` to use another. The script needs `peios-sdk` checked out at the
commit it names; a temporary worktree at that commit keeps your own
checkout as it is.

On the host, `pekit` builds five packages from their recipes in `pkgs`,
into a store of the run's own:

- `dev.peios.kernel`, the kernel built from the `kernel` checkout beside
  `pkgs` by its own build, in Docker;
- `net.busybox.busybox`, busybox built statically from its release
  tarball, with a link for each applet in `bin` and `sbin`;
- `dev.peios.ppk`, with the static `bin/ppk`;
- `dev.peios.prelude`, with `sbin/prelude`, `bin/seed-sd` and
  `libexec/prelude/hook-log.sh`, as prelude installs them;
- `dev.peios.live-root`, with the prelude hook
  `libexec/prelude/hooks.d/ppk-live-root`, from `boot/ppk-live-root`.

The recipes for the kernel, `ppk`, live-root and prelude build from the
checkouts beside `pkgs`, uncommitted changes included, so what boots is
what you have checked out. Their versions do not change with their
contents, which is why each run packs into a store of its own. `pekit`
keeps each build in its cache and reruns only those whose recipe or
source changed, in a work directory it keeps, so they redo only what
changed. The kernel's first build takes about half an hour, and later ones
take seconds while the kernel is unchanged. To boot another kernel image
instead, pass `--kernel` or set `PPK_VM_KERNEL`. prelude's recipe takes
the SDK from `PEIOS_SDK`, which the script sets from `--sdk`.

The script then writes a spec for `peiso`: the kernel is the one in
`dev.peios.kernel`, the live system is busybox and `ppk`, and the
initramfs holds those two, which the hook runs, with prelude and
live-root. `peiso` composes the initramfs's root: the closure
of its four packages as `/usr` and, around it:

- `/init`, a link to `usr/sbin/prelude`;
- empty `/proc`, `/sys` and `/tmp`;
- `/system/medium`, the identifier of the ISO it boots from.

`mkirf` turns that tree into the initramfs, writing the hook into prelude's
hook sequence, and `mkuki` makes the initramfs, the kernel and its command
line into a unified kernel image. `peiso` writes the ISO with the image as
its UEFI boot loader and, beside it:

- `/artifacts`, a store holding every package;
- `/live.toml`, the live system's environment profile, which names busybox
  and `ppk`, links `/bin` and `/sbin` to `usr/bin` and `usr/sbin`, and
  gives an empty `/tmp`;
- `/peios-medium`, the same identifier as the initramfs's.

Pass `--mkirf`,
`--mkuki`, `--peiso` or `--pekit` to use another of those tools, and
`--pkgs` to use another recipe workspace. QEMU then boots the ISO in the
terminal, under OVMF, with `rd.shell` on the kernel command line.

At boot:

1. prelude mounts `/proc`, `/sys` and `/dev`, and gives `/dev` its security
   descriptors with `seed-sd`.
2. The `ppk-live-root` hook finds the ISO: it mounts each disk, disc and
   stick with an ISO9660 filesystem at `/mnt/medium` in turn, and keeps the
   one whose `/peios-medium` matches `/system/medium`, waiting up to ten
   seconds for one to appear. In QEMU it is `/dev/sr0`. The hook mounts a
   tmpfs at `/mnt/live` and gives it a descriptor with `seed-sd`: a tmpfs
   refuses all access, even to SYSTEM, until its root has one. It then
   runs `ppk compose-env` with the medium's `live.toml` and `artifacts`
   and `--copy-artifacts`, so the system's root has its own copy of its
   packages, adds `/proc`, `/sys` and `/dev` for prelude to move its mounts
   onto, unmounts the medium, and bind-mounts the root onto `/mnt/rootfs`.
   The root lives on its own tmpfs because prelude deletes what is left on
   the initramfs's filesystem once it hands over.
3. prelude moves `/proc`, `/sys` and `/dev` into the new root, switches to
   it, and starts its first fallback init that exists, `/sbin/init`, which
   is busybox's `init`.

busybox's `init` complains that `/etc/init.d/rcS` is missing, then offers a
shell on the console when you press <kbd>Enter</kbd>. In the system, `/usr`
is the closure of busybox and `ppk`, and `ppk` finds the system's packages
in `/system/artifacts`. Run `poweroff -f` to power the guest off, or press
<kbd>Ctrl</kbd>+<kbd>A</kbd> then <kbd>X</kbd>. Nothing in the guest
survives. Pass `--kernel` or set `PPK_VM_KERNEL` to boot another kernel;
`--help` lists the options.

If boot fails, prelude opens its recovery shell in the initramfs instead.
That shell has no controlling terminal, so <kbd>Ctrl</kbd>+<kbd>C</kbd>
does nothing, and exiting it halts the guest but leaves QEMU running.

Everything runs as SYSTEM, so every access check passes. The system's
store gets the descriptors `seed-sd` gave its tmpfs and `ppk` gives what it
writes. The initramfs's own store has none, because it was packed on the
host and the archive doesn't carry extended attributes. To see what an
ordinary user can do, use the VM tests.

### What the tests do and don't show

The guest's root is the kernel's `rootfs`, which KACS manages. Its root
grants full access to SYSTEM only, and the agent runs as SYSTEM. The tests
show that `ppk` runs unsigned on Peios, that KACS lets it hardlink
read-only blobs, and that an executable form runs from a closure tree.

One test then acts as an ordinary user, through the kernel suite's
`token.as_principal`. The user holds only the privilege that bypasses
traverse checking, as an ordinary user does; without it, this guest's
SYSTEM-only `/` would hide every path. The test checks that the user is
refused a file `ppk` didn't write, can run a closure's program and read a
blob, and is refused writing a blob or adding a file to a closure.

Two tests cover `ppk run`, as SYSTEM and as the same ordinary user, who
runs a copy of `ppk` packed into its own closure. Both first make the
guest's root mount shared, because a private root would hide a mount
leaking out of the environment, and then check that the guest's own `/usr`
is still empty after the command has seen the closure there. A third
checks that `ppk run` with no command starts the closure's own shell.

The syscall and token helpers come from provium's and the kernel's
integration suites, so the VM tests need the `provium` and `kernel`
repositories checked out beside `ppk`.
