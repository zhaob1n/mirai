# Packaging

Nothing here is published: the files exist so a package can be built and checked locally.
Commands are in the [README](../../README.md#requirements).

|Path|What|
|---|---|
|`data/`|desktop entry and AppStream metainfo, shared by every package; icons come from `crates/mirai/resources/icons/`|
|`packaging/aur/`|split PKGBUILD and its `.SRCINFO`: `mirai-git` (the GUI, desktop files, icons) and `mirai-server-git` (the headless host, which needs no GTK), from one build|

## Keeping it current

- **PKGBUILD changed** → `makepkg --printsrcinfo > .SRCINFO` in `packaging/aur/`.
- **A release** → add a `<release>` to the metainfo.
- **Linked libraries changed** → each package's `depends` lists the owner of every `NEEDED`
  entry of its binary (`readelf -d`, `pacman -Qqo`).

## Decisions

**The PKGBUILD is a VCS package** because there are no release tags. A tagged release would
add PKGBUILDs pinned to a tarball and checksum.

**Split, from one build.** `mirai-server` runs on the machine with the GPU and links no GTK;
installing it should not pull GTK in, and a client does not need it.

**No Flatpak.** A manifest is parked, unmaintained, on the `flatpak` branch. Granting the GPU
(`--device=dri`) is not enough: the runtime carries Mesa, rusticl OpenCL included, but not
the host's ROCm or CUDA, so the host's KataGo cannot run inside the sandbox. The branch runs
it outside through `flatpak-spawn --host`, which Flathub rejects as a sandbox escape. A
bundled OpenCL KataGo does run inside; measured on an RX 6800 XT (b10 network, 16 threads,
400 visits) it managed 123 visits/s against 472 for the host's ROCm build.
