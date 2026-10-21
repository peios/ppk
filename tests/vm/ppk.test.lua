-- pack, fetch and closure on a real Peios kernel, with KACS enforcing.

local SYS_FCHMODAT = 268
local AT_FDCWD = -100

local PPK = "/ppk"
local BUSYBOX = "/busybox"

-- Files pushed into the guest arrive without an execute bit, and the guest
-- has no chmod, so set the mode with the system call.
local function make_executable(t, vm, path)
    local r = vm:syscall(SYS_FCHMODAT, {
        args = { AT_FDCWD, 0, tonumber("755", 8), 0 },
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

test("ppk runs on Peios", function(t)
    local vm = boot(t)
    local r = ppk(vm, { "--version" })
    r:assert_ok()
    t:assert_contains(r.stdout, "ppk ")
end)

test("pack, fetch and closure work, and the closure runs", function(t)
    local vm = boot(t)
    t:assert(vm:mkdir("/content/bin", { parents = true }) == nil)
    busybox(vm, { "cp", BUSYBOX, "/content/bin/busybox" }):assert_ok()
    make_executable(t, vm, "/content/bin/busybox")

    local packed = ppk(vm, {
        "pack", "dev.peios.busybox@1.0", "/content", "--artifact-dir", "/build",
    })
    packed:assert_ok()
    local identity = packed.stdout

    local fetched = ppk(vm, {
        "fetch", "dev.peios.busybox@1.0", "--from", "/build", "--artifact-dir", "/system",
    })
    fetched:assert_ok()
    t:assert_eq(fetched.stdout, identity, "fetch reproduces the identity")

    local closed = ppk(vm, { "closure", "dev.peios.busybox@1.0", "--artifact-dir", "/system" })
    closed:assert_ok()
    local hex = closed.stdout:match("^sha256:(%x+)\n$")
    t:assert(hex ~= nil, "closure printed an identity: " .. closed.stdout)
    local tree = "/system/closures/" .. hex

    local stat = vm:stat(tree .. "/bin/busybox")
    t:log(string.format("closure file mode %o", stat.perm))
    t:assert_eq(stat.perm, tonumber("555", 8), "the executable form is linked")

    local blob_hex = vm:listdir("/system/blobs/sha256")[1].name
    local blob_ino, blob_links = inode(vm, "/system/blobs/sha256/" .. blob_hex)
    local file_ino = inode(vm, tree .. "/bin/busybox")
    t:assert_eq(file_ino, blob_ino, "the closure file is the blob, not a copy")
    t:assert_eq(blob_links, 2, "one link from the store, one from the closure")

    local ran = vm:run(tree .. "/bin/busybox", { args = { "echo", "hello from a closure" } })
    ran:assert_ok()
    t:assert_eq(ran.stdout, "hello from a closure\n")
end)
