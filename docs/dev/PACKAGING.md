# Packaging

Every install goes through the `justfile`; the Arch and MSYS2 packages call it too. User-facing
commands are in the [README](../../README.md#-installing).

## Installing with `just`

|Path|What|
|---|---|
|`justfile`|`check`, `build`, `install [mirai\|mirai-server\|all]`, `uninstall [mirai\|mirai-server\|all]`; honours `PREFIX` and `DESTDIR`. The one list of installed files. `MIRAI_BIN_DIR`, `MIRAI_EXE_SUFFIX`, `MIRAI_PROFILE`, `CARGO_BUILD_TARGET` and `MIRAI_DESKTOP` stage a non-host build; unset, Linux is unchanged|
|`data/`|templates of the desktop entry and AppStream metainfo (`*.in`); `just install` merges the translations from `po/` into them. Icons come from `crates/mirai/resources/icons/`|
|`po/`|one gettext catalogue per language in `po/LINGUAS`, compiled by `just install` into `share/locale` ([TRANSLATING](TRANSLATING.md))|

When something changes:

- **An installed file added or moved** → `install` and `uninstall` in the `justfile`; the
  packages follow.
- **A language added** → nothing: `just install` and `uninstall` read `po/LINGUAS`.
- **A release** → add a `<release>` to the metainfo template.

**`just install`, not a package per distribution.** `cargo install` places only binaries,
not the desktop entry, metainfo and icons. The install recipes never build, so
`sudo just install` does not leave a root-owned `target/`, and they refresh an existing icon
cache, which would otherwise hide the new icon.

`just install` also places the repository `LICENSE` in `share/licenses/mirai/` and, for the
server, `share/licenses/mirai-server/`. The Arch packages pick that file up on their next
build; it is the same list, not a second one.

**One release build serves any `PREFIX`.** Catalogues are resolved at run time from the
install prefix; see [TRANSLATING.md](TRANSLATING.md#how-a-build-finds-its-catalogues).

**Kifu lookup talks to libsoup directly.** Fox, eWeiqi and Yike requests use libsoup because
GIO's `https://` support requires GVfs, which a Linux desktop need not install. libsoup brings
nghttp2, libpsl and sqlite3 with it; no build option removes them.

**No Flatpak.** A manifest is parked, unmaintained, on the `flatpak` branch. Granting the GPU
(`--device=dri`) is not enough: the runtime carries Mesa, rusticl OpenCL included, but not
the host's ROCm or CUDA, so the host's KataGo cannot run inside the sandbox. The branch runs
it outside through `flatpak-spawn --host`, which Flathub rejects as a sandbox escape. A
bundled OpenCL KataGo does run inside; measured on an RX 6800 XT (b10 network, 16 threads,
400 visits) it managed 123 visits/s against 472 for the host's ROCm build.

## Arch and the AUR

|Path|What|
|---|---|
|`packaging/aur/<pkg>/`|one directory per AUR repository, `mirai-git` and `mirai-server-git`: PKGBUILD and `.SRCINFO`|
|[`packaging/LICENSE`](../../packaging/LICENSE)|shared 0BSD license for the AUR and MSYS2 recipes|
|`tools/packaging/makepkg-local.sh`|builds one of those PKGBUILDs from this checkout's HEAD instead of GitHub, rewriting only `source=`; `--prepare DIR` writes the PKGBUILD without building|
|`tools/packaging/arch-chroot-test.sh`|builds each PKGBUILD in a clean devtools chroot with its own package cache, installs into a fresh one, smoke-runs both binaries, runs namcap|

A PKGBUILD changes when the build does: a dependency, a build step, an installed file the
`justfile` does not already cover. An upstream commit alone needs nothing.

- **Linked libraries changed** → each package's `depends` lists the owner of every `NEEDED`
  entry of its binary (`readelf -d`, `pacman -Qqo`), plus `hicolor-icon-theme` for the
  GUI's icons.

### Changing and publishing a PKGBUILD

1. Edit `packaging/aur/<pkg>/PKGBUILD`.
2. In that directory, `makepkg -od --noprepare` sets `pkgver` from GitHub's current HEAD;
   then `makepkg --printsrcinfo > .SRCINFO`.
3. Run `tools/packaging/arch-chroot-test.sh`. Its namcap run always flags the debug
   package's `.build-id` symlinks and an unused `ld-linux-x86-64.so.2`; both are false
   positives.
4. Commit here.
5. Copy the files into the AUR repository and push. It holds only those files, takes pushes
   to `master` alone, and refuses a pushed tip without a `.SRCINFO`. The commit message
   says what changed in the package, as here.

```sh
pkg=mirai-git      # or mirai-server-git
git -c init.defaultBranch=master clone ssh://aur@aur.archlinux.org/$pkg.git ~/aur/$pkg
cp packaging/aur/$pkg/{PKGBUILD,.SRCINFO,.gitignore} ~/aur/$pkg/
cp packaging/LICENSE ~/aur/$pkg/LICENSE
cd ~/aur/$pkg && git add -A && git commit -m '…' && git push
```

### Decisions

**VCS packages.** There are no release tags. `pkgver()` reads the version from the clone at
build time, and the AUR guidelines forbid commits that only bump a VCS `pkgver`. A tagged
release would add PKGBUILDs pinned to a tarball and checksum.

**Two pkgbases, not a split package.** `makedepends` belong to the whole `pkgbase`: split,
building `mirai-server` on a headless host would install GTK.

**The package files are 0BSD**, as Arch asks of AUR package sources. Keep one source
at `packaging/LICENSE`; copying it into a separately published AUR repository does not
require another platform-local copy in this tree.

**No `check()`.** The test suite guards commits upstream (every commit passes it); rerunning
it on each user's machine adds a second release-profile compile of the whole workspace and
proves nothing a build from a tested commit does not. The chroot test's smoke run covers
what packaging can break.

**`!lto`.** The chroot's `makepkg.conf` enables LTO, which compiles the C inside `ring` and
`zstd-sys` to GCC bitcode that rust-lld, rustc's default linker, cannot read; every C
symbol is then undefined. A host `makepkg.conf` with `!lto` (CachyOS's) hides this.

**The chroot test keeps its own package cache.** On CachyOS, `[cachyos]` ships forks of Arch packages under Arch's exact file names (its `pacman` copies Arch's `pkgver`, and the `pkgrel`s coincide now and then); a chroot sharing the host cache finds that build and rejects its signature. The `_v3` repositories carry a different architecture suffix and never collide.

## MSYS2 UCRT64

Native Windows builds are an MSYS2 **UCRT64** shell and `makepkg-mingw`. Not a Fedora
container, not rustup, not CLANG64. There is no portable zip in this tree.

|Path|What|
|---|---|
|`packaging/msys2/mirai-git/`|GUI package. `depends` includes `gtk4>=4.22`, `libadwaita>=1.9`, libsoup3, gettext-runtime|
|`packaging/msys2/mirai-server-git/`|headless server. `depends` is `cc-libs` only, so a server build does not install GTK|
|`justfile`|the one install list both packages call|

A real UCRT64 user, in that shell:

```sh
pacman -S --needed git
cd packaging/msys2/mirai-git    # or mirai-server-git
makepkg-mingw -si
```

`makepkg-mingw` reads `mingw_arch=('ucrt64')`, installs `depends` and `makedepends`, and
installs the package under `/ucrt64`. Version operators stay attached to the package name
(`gtk4>=4.22`, `libadwaita>=1.9`); pacman compares them with `vercmp`. KataGo and its model
stay outside the package. Stone sounds are `optdepends`: `gst-plugins-good` decodes the WAV
and `gst-plugins-bad` provides the WASAPI2 sink. The MSYS2 recipes are copyright Huang
Zhaobin and licensed 0BSD, like the AUR recipes.
This covers the packaging code only: mirai remains GPL-3.0-or-later, and bundled runtime
libraries retain their own licenses and copyright notices.

### Host triple

UCRT64 is GCC plus UCRT. Its makepkg fragment sets `RUST_CHOST="x86_64-pc-windows-gnu"`
([`makepkg_mingw.d.ucrt64.conf`](https://github.com/msys2/MSYS2-packages/blob/master/pacman/makepkg_mingw.d.ucrt64.conf)).
CLANG64 is LLVM and sets `RUST_CHOST="x86_64-pc-windows-gnullvm"`
([`makepkg_mingw.d.clang64.conf`](https://github.com/msys2/MSYS2-packages/blob/master/pacman/makepkg_mingw.d.clang64.conf)).
The environments are documented at <https://www.msys2.org/docs/environments/>. MSYS2's rust
package builds that `RUST_CHOST` as its host
([`mingw-w64-rust/PKGBUILD`](https://github.com/msys2/MINGW-packages/blob/master/mingw-w64-rust/PKGBUILD));
it is not a rustup toolchain: its `rustc -vV` reports host `x86_64-pc-windows-gnu` and
identifies as MSYS2.

The PKGBUILD therefore passes `--target x86_64-pc-windows-gnu` and refuses any other
`rustc --print host-tuple`, or a rustc whose `-vV` does not say MSYS2. Two consequences:
artifacts land in `target/x86_64-pc-windows-gnu/release/`, the same directory an explicit
`--target` build uses, and a rustup proxy — which is the only reader of `rust-toolchain.toml`
— cannot silently substitute upstream stable. `/ucrt64/bin` is put first on `PATH` for the
same reason.

### What `just install` takes

Linux with no variables set still installs `target/release/mirai` and the desktop entry.
A staged Windows tree:

```sh
DESTDIR=stage PREFIX=/ MIRAI_BIN_DIR=dir-of-exes MIRAI_EXE_SUFFIX=.exe just install
DESTDIR=stage PREFIX=/ MIRAI_EXE_SUFFIX=.exe just uninstall
```

`MIRAI_BIN_DIR` wins over `target/`. `MIRAI_DESKTOP=0` omits the desktop entry and AppStream
metainfo only. Translations, icons, docs and `share/licenses/*/LICENSE` always install.
Without `MIRAI_BIN_DIR`, the path is `$CARGO_TARGET_DIR/$CARGO_BUILD_TARGET/$MIRAI_PROFILE`
when `CARGO_BUILD_TARGET` is set, else `$CARGO_TARGET_DIR/$MIRAI_PROFILE` (`release` by default).
The recipes never build.

### What is not claimed

GTK's Windows backend is the limitation recorded when an earlier port was reverted, and it
is not fixed. GTK 4.22 composites through DirectComposition and draws a black band around
every window with its GPU renderers (gtk#7567). GTK 4.24, which UCRT64 ships, avoids the band
by drawing with Cairo on the CPU, and scrolling, resizing and animation can stutter. mirai
picks no renderer.

Wine is not a Windows test: it rejects the socket option quinn sets (`WSAEOPNOTSUPP`
10045), so `mirai-server` networking does not work there.
