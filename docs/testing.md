# Testing ppk

## Host tests

```sh
cargo test
```

These cover every command on the build host's own filesystem.

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

```sh
cargo build --release --target x86_64-unknown-linux-musl
cd tests/vm
provium .
```

Add `--json` to see each test's `t:log` output.

## An interactive shell

`tools/vm-shell` boots the same kernel with a busybox shell and `ppk` on
the serial console, so you can try commands by hand:

```sh
tools/vm-shell
```

It builds the static `ppk`, packs it with busybox into an initramfs, and
starts QEMU in the terminal. `/demo/busybox` in the guest is a directory to
pack, and the shell prints commands to try. Exit the shell to power the
guest off, or press <kbd>Ctrl</kbd>+<kbd>A</kbd> then <kbd>X</kbd>. Nothing in
the guest survives. Pass `--kernel` or set `PPK_VM_KERNEL` to boot another
kernel; `--help` lists the options.

The shell runs as SYSTEM on `rootfs`, the same as the VM tests, so the
limits that follow apply to it too.

### What the tests do and don't show

The guest's root is the kernel's `rootfs`, which KACS manages. Its root
grants full access to SYSTEM only, and the agent runs as SYSTEM, so every
check passes on access. The tests therefore show that `ppk` runs unsigned on
Peios, that KACS lets it hardlink read-only blobs, and that an executable
form runs from a closure tree. They don't show that anyone but SYSTEM can
use a closure: blobs inherit the SYSTEM-only descriptor, and a hardlink
shares it.
