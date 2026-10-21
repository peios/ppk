-- pack, fetch and closure on a real Peios kernel, with KACS enforcing.

-- From provium's and the kernel's integration suites; see provium.toml.
local kacs = require("helpers.kacs")
local sys = require("helpers.sys")
local token = require("helpers.token")

local PPK = "/ppk"
local BUSYBOX = "/busybox"

-- Files pushed into the guest arrive without an execute bit, and the guest
-- has no chmod, so set the mode with the system call.
local function make_executable(t, vm, path)
    local r = vm:syscall(sys.NR.fchmodat or 268, {
        args = { sys.AT_FDCWD, 0, tonumber("755", 8), 0 },
        bufs = { path .. "\0" },
        ptrs = { 1 },
    })
    t:assert_eq(r.errno, 0, "fchmodat " .. path)
end

local function boot(t)
    local vm = provium:vm("v", "kernel-only"):boot()
    vm:push_file("../../target/x86_64-unknown-linux-musl/release/ppk", PPK)
    vm:push_file("/usr/bin/busybox", BUSYBOX)
    make_executable(t, vm, PPK)
    make_executable(t, vm, BUSYBOX)
    return vm
end

local function ppk(vm, args)
    return vm:run(PPK, { args = args, timeout = "30s" })
end

local function busybox(vm, args)
    return vm:run(BUSYBOX, { args = args, timeout = "30s" })
end

-- Inode number and link count of `path`, from busybox's stat.
local function inode(vm, path)
    local r = busybox(vm, { "stat", "-c", "%i %h", path })
    r:assert_ok()
    local ino, links = r.stdout:match("^(%d+) (%d+)")
    return tonumber(ino), tonumber(links)
end

-- As SYSTEM: pack busybox into /build, fetch it into /system, and build a
-- closure of it there. Returns the closure's tree and the stored blob.
local function build_closure(t, vm)
    t:assert(vm:mkdir("/content/bin", { parents = true }) == nil)
    busybox(vm, { "cp", BUSYBOX, "/content/bin/busybox" }):assert_ok()
    make_executable(t, vm, "/content/bin/busybox")
    busybox(vm, { "ln", "-s", "busybox", "/content/bin/sh" }):assert_ok()

    local packed = ppk(vm, {
        "pack", "dev.peios.busybox@1.0", "/content", "--artifact-dir", "/build",
    })
    packed:assert_ok()

    local fetched = ppk(vm, {
        "fetch", "dev.peios.busybox@1.0", "--from", "/build", "--artifact-dir", "/system",
    })
    fetched:assert_ok()
    t:assert_eq(fetched.stdout, packed.stdout, "fetch reproduces the identity")

    local closed = ppk(vm, { "closure", "dev.peios.busybox@1.0", "--artifact-dir", "/system" })
    closed:assert_ok()
    local hex = closed.stdout:match("^sha256:(%x+)\n$")
    t:assert(hex ~= nil, "closure printed an identity: " .. closed.stdout)

    local blob = "/system/blobs/sha256/" .. vm:listdir("/system/blobs/sha256")[1].name
    return "/system/closures/" .. hex, blob
end

-- As SYSTEM: pack ppk itself and build its closure, so that someone other
-- than SYSTEM can run a copy that carries the store's descriptor. Returns
-- that copy.
local function ppk_for_everyone(t, vm)
    t:assert(vm:mkdir("/ppk-content/bin", { parents = true }) == nil)
    busybox(vm, { "cp", PPK, "/ppk-content/bin/ppk" }):assert_ok()
    make_executable(t, vm, "/ppk-content/bin/ppk")
    ppk(vm, { "pack", "dev.peios.ppk@0.1", "/ppk-content", "--artifact-dir", "/system" }):assert_ok()
    local closed = ppk(vm, { "closure", "dev.peios.ppk@0.1", "--artifact-dir", "/system" })
    closed:assert_ok()
    return "/system/closures/" .. closed.stdout:match("^sha256:(%x+)\n$") .. "/bin/ppk"
end

-- This guest's root mount is private, which would hide a mount leaking back
-- out of an environment. Share it, as a running system's often is.
local function share_root(t, vm)
    local MS_REC, MS_SHARED = 0x4000, 0x100000
    local shared = vm:syscall(sys.NR.mount, {
        args = { 0, 0, 0, MS_REC | MS_SHARED, 0 },
        bufs = { "/\0" },
        ptrs = { 1 },
    })
    t:assert_eq(shared.errno, 0, "make / shared")
end

-- `ppk run` of the busybox closure, with `command` after the separator.
local function run_args(command)
    local args = { "run", "-p", "dev.peios.busybox@1.0", "--artifact-dir", "/system", "--" }
    for _, arg in ipairs(command) do
        args[#args + 1] = arg
    end
    return args
end

test("ppk runs on Peios", function(t)
    local vm = boot(t)
    local r = ppk(vm, { "--version" })
    r:assert_ok()
    t:assert_contains(r.stdout, "ppk ")
end)

test("pack, fetch and closure work, and the closure runs", function(t)
    local vm = boot(t)
    local tree, blob = build_closure(t, vm)

    local stat = vm:stat(tree .. "/bin/busybox")
    t:assert_eq(stat.perm, tonumber("555", 8), "the executable form is linked")

    local blob_ino, blob_links = inode(vm, blob)
    local file_ino = inode(vm, tree .. "/bin/busybox")
    t:assert_eq(file_ino, blob_ino, "the closure file is the blob, not a copy")
    t:assert_eq(blob_links, 2, "one link from the store, one from the closure")

    local ran = vm:run(tree .. "/bin/busybox", { args = { "echo", "hello from a closure" } })
    ran:assert_ok()
    t:assert_eq(ran.stdout, "hello from a closure\n")
end)

test("an ordinary user can run a closure but cannot change the store", function(t)
    local vm = boot(t)
    local tree, blob = build_closure(t, vm)

    -- A test user in Everyone and Authenticated Users. Its one privilege
    -- bypasses traverse checking, as an ordinary user's does: this guest's
    -- `/` grants SYSTEM alone, so without it no path below `/` resolves.
    local bypass_traverse = kacs.PRIV.CHANGE_NOTIFY
    local user = { privs_present = bypass_traverse, privs_enabled = bypass_traverse }
    token.as_principal(t, vm, user, function(w)
        -- The control: a file ppk did not write keeps the SYSTEM-only
        -- descriptor the root filesystem gives everything.
        local _, denied = sys.open(w, PPK, sys.O.RDONLY)
        t:assert_eq(denied, sys.E.ACCES, "the user is not SYSTEM")

        local ran = w:run(tree .. "/bin/busybox", { args = { "echo", "run by a user" } })
        t:assert_eq(ran.stdout, "run by a user\n", ran.stderr)

        local fd, errno = sys.open(w, blob, sys.O.RDONLY)
        t:assert(fd ~= nil, "reading a blob: " .. sys.errname(errno or 0))
        sys.close(w, fd)

        local _, write_errno = sys.open(w, blob, sys.O.WRONLY)
        t:assert_eq(write_errno, sys.E.ACCES, "writing a blob is refused")

        local planted = tree .. "/bin/planted"
        local _, create_errno = sys.open(w, planted, sys.O.WRONLY | sys.O.CREAT, tonumber("644", 8))
        t:assert_eq(create_errno, sys.E.ACCES, "adding to a closure is refused")
    end)
end)

test("run shows the closure at /usr to its command alone", function(t)
    local vm = boot(t)
    build_closure(t, vm)
    -- This guest has no /usr, and an environment replaces one rather than
    -- creating it.
    t:assert(vm:mkdir("/usr") == nil)
    share_root(t, vm)

    local listed = ppk(vm, run_args({ "/usr/bin/busybox", "ls", "/usr/bin" }))
    listed:assert_ok()
    t:assert_eq(listed.stdout, "busybox\nsh\n")
    t:assert_eq(#vm:listdir("/usr"), 0, "the mount stays inside the environment")

    local failed = ppk(vm, run_args({ "/usr/bin/busybox", "false" }))
    t:assert_eq(failed.exit_code, 1, "the command's status is ppk's")
    local missing = ppk(vm, run_args({ "/usr/bin/nothing" }))
    t:assert_eq(missing.exit_code, 127, missing.stderr)
    t:assert_eq(missing.stderr, "ppk: /usr/bin/nothing: No such file or directory\n")
end)

test("an ordinary user can run a command in an environment", function(t)
    local vm = boot(t)
    build_closure(t, vm)
    local user_ppk = ppk_for_everyone(t, vm)
    -- Everyone can read a real system's /usr; one made here would otherwise
    -- inherit the SYSTEM-only descriptor of this guest's root.
    t:assert(vm:mkdir("/usr") == nil)
    local read_and_traverse = 0x1200A9
    local set = kacs.set_sd(vm, "/usr", kacs.grant(read_and_traverse))
    t:assert_eq(set.errno, 0, "set /usr's descriptor")
    share_root(t, vm)

    local bypass_traverse = kacs.PRIV.CHANGE_NOTIFY
    local user = { privs_present = bypass_traverse, privs_enabled = bypass_traverse }
    token.as_principal(t, vm, user, function(w)
        local r = w:run(user_ppk, { args = run_args({ "/usr/bin/busybox", "ls", "/usr/bin" }) })
        t:assert_eq(r.exit_code, 0, r.stderr)
        t:assert_eq(r.stdout, "busybox\nsh\n")
    end)
    t:assert_eq(#vm:listdir("/usr"), 0, "the user's mount stays inside the environment")
end)

test("run with no command starts the closure's shell", function(t)
    local vm = boot(t)
    build_closure(t, vm)
    t:assert(vm:mkdir("/usr") == nil)

    local r = vm:run(PPK, {
        args = { "run", "dev.peios.busybox@1.0", "--artifact-dir", "/system" },
        stdin = "echo $0; ls /usr/bin; exit 3\n",
        timeout = "30s",
    })
    t:assert_eq(r.exit_code, 3, r.stderr)
    t:assert_eq(r.stdout, "/usr/bin/sh\nbusybox\nsh\n")
end)
