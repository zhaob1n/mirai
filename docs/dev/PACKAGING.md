# Packaging

Nothing here is published: the files exist so a package can be built and checked locally.
Commands are in the [README](../../README.md#requirements).

|Path|What|
|---|---|
|`justfile`|`check`, `build`, `install [mirai\|mirai-server\|all]`, `uninstall [mirai\|mirai-server\|all]`; honours `PREFIX` and `DESTDIR`. The one list of installed files|
|`data/`|templates of the desktop entry and AppStream metainfo (`*.in`), shared by every package; `just install` merges the translations from `po/` into them. Icons come from `crates/mirai/resources/icons/`|
|`po/`|one gettext catalogue per language in `po/LINGUAS`, compiled by `just install` into `share/locale` ([TRANSLATING](TRANSLATING.md))|
|`packaging/aur/`|split PKGBUILD and its `.SRCINFO`: `mirai-git` (the GUI, desktop files, icons) and `mirai-server-git` (the headless host, which needs no GTK), from one build, installed with `just`|
|`tools/packaging/makepkg-local.sh`|builds that PKGBUILD from this checkout's HEAD instead of GitHub, rewriting only `source=`; `--prepare DIR` writes the PKGBUILD without building|
|`tools/packaging/arch-chroot-test.sh`|builds the PKGBUILD in a clean devtools chroot with its own package cache, installs into a fresh one, smoke-runs both binaries, runs namcap|

## Keeping it current

- **An installed file added or moved** → the `justfile` (install and uninstall); packages
  follow.
- **PKGBUILD changed** → `makepkg --printsrcinfo > .SRCINFO` in `packaging/aur/`, then
  `tools/packaging/arch-chroot-test.sh`. Its namcap run always flags the debug package's
  `.build-id` symlinks and an unused `ld-linux-x86-64.so.2`; both are false positives.
- **A release** → add a `<release>` to the metainfo template.
- **A language added** → nothing here: `just install` and `uninstall` read `po/LINGUAS`.
- **Linked libraries changed** → each package's `depends` lists the owner of every `NEEDED`
  entry of its binary (`readelf -d`, `pacman -Qqo`), plus `hicolor-icon-theme` for the
  GUI's icons.

## Decisions

**The PKGBUILD is a VCS package** because there are no release tags. A tagged release would
add PKGBUILDs pinned to a tarball and checksum.

**`!lto`.** The chroot's `makepkg.conf` enables LTO, which compiles the C inside `ring` and
`zstd-sys` to GCC bitcode that rust-lld, rustc's default linker, cannot read; every C
symbol is then undefined. A host `makepkg.conf` with `!lto` (CachyOS's) hides this.

**The chroot test keeps its own package cache.** On CachyOS, `[cachyos]` ships forks of
Arch packages under Arch's exact file names (its `pacman` copies Arch's `pkgver`, and the
`pkgrel`s coincide now and then); a chroot sharing the host cache finds that build and
rejects its signature. The `_v3` repositories carry a different architecture suffix and
never collide.

**Split, from one build.** `mirai-server` runs on the machine with the GPU and links no GTK;
installing it should not pull GTK in, and a client does not need it.

**`just install`, not a package per distribution.** `cargo install` places only binaries,
not the desktop entry, metainfo and icons, so the install lives in a `justfile`. Debian,
Ubuntu and Fedora build from source with it into `/usr/local`; a `.deb` or `.rpm` would be
worth its upkeep only once there are releases and users on those systems. The install
recipes never build, so `sudo just install` does not leave a root-owned `target/`, and they
refresh an existing icon cache, which would otherwise hide the new icon. The translated
desktop entry, metainfo and catalogues are merged with `msgfmt` into a scratch directory
and installed from there, which keeps that promise.

**The locale directory is found at run time, not built in.** `mirai` looks for its
catalogues in `<prefix>/share/locale` beside its own `<prefix>/bin`, so the one release build
works under whatever `PREFIX` it is later installed to, `just prefix=$HOME/.local install`
included.

**No Flatpak.** A manifest is parked, unmaintained, on the `flatpak` branch. Granting the GPU
(`--device=dri`) is not enough: the runtime carries Mesa, rusticl OpenCL included, but not
the host's ROCm or CUDA, so the host's KataGo cannot run inside the sandbox. The branch runs
it outside through `flatpak-spawn --host`, which Flathub rejects as a sandbox escape. A
bundled OpenCL KataGo does run inside; measured on an RX 6800 XT (b10 network, 16 threads,
400 visits) it managed 123 visits/s against 472 for the host's ROCm build.

**Fox lookup talks to libsoup directly.** `fox.rs` used to fetch through
`gio::File::for_uri`, but GIO serves `https://` only through GVfs's daemon — itself
libsoup — which a Linux desktop need not install, and the Arch package never depended on.
Calling libsoup keeps the request on the GLib main context and the desktop's proxy
settings; the price is libsoup's own dependencies, among them nghttp2, libpsl and sqlite3
for its cookie and HSTS databases, which no build option removes.
