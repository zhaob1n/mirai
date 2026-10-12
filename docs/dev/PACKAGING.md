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

CI sources each PKGBUILD in its own subshell with
`MINGW_PACKAGE_PREFIX=mingw-w64-ucrt-x86_64` and installs the union of top-level `depends`
and `makedepends`, preserving version constraints. `git` is the MSYS package. Release-only
additions are Python, NSIS, icoutils and librsvg for the installer.

The installer runtime then differs from that SDK in two ways, installed outside
setup-msys2's cache into `target/windows-packages`, which the build cache keeps:

- GTK is the D3D12 build from [zhaob1n/gtk-d3d12](https://github.com/zhaob1n/gtk-d3d12),
  pinned by file name and SHA-256 in `packaging/windows/gtk4-d3d12.sha256`. It replaces
  the official `gtk4` (`pacman -Rdd`, then `-U`; it provides `gtk4`, so libadwaita's
  dependency holds). To move the pin, replace that line with the new release asset's
  `SHA256SUMS` entry.
- `gst-plugins-good` and `-bad` install with `-Sdd`: the installer takes a handful of
  audio plugins, while `gst-plugins-bad`'s dependency closure alone added 71 packages and
  540 MiB to every run. `bundle_imports` fails the stage if a shipped plugin imports a DLL
  that is not there.

`windows-ci.sh packages` prints that list without requiring an MSYS2 shell, so the
workflow passes it to `setup-msys2`'s `install` input. Dependencies installed later in a
separate script would miss the action's package cache. Its
[implementation](https://github.com/msys2/setup-msys2/blob/ec48f7c5447b3140e2b088413ae3a55687bccb6e/index.js)
caches `var/cache/pacman/pkg`, not the mutable installation. `release: true` starts from
a fresh installer; `update: true` performs the core upgrade, restarts the MSYS2 runtime,
then completes `pacman -Syuu` on every run, even after a cache hit.

Cargo's registry/git downloads, release build outputs and those runtime packages have
their own cache, saved on every run. Restores prefer the same `pacman -Q` package versions
and Cargo manifests/lockfile, then fall back to any earlier build: Cargo rebuilds what a
newer rustc or lockfile invalidates. C objects in build-script outputs are not rebuilt for
a newer GCC alone. `CARGO_HOME` is isolated in the runner's temporary directory;
credentials, `target/dist` and `target/resources` are not cached. Incremental outputs are
disabled. Cache save runs even after a build/test failure, unless cancelled, so a fix can
reuse completed dependency builds; release artifacts are uploaded only after success.
Tests and packaging use the same explicit `RUSTC`, `CC`, `CXX`, `AR` and `PKG_CONFIG`
paths. Switching between unset and explicit values dirties the C build scripts and
rebuilds GTK/ring dependencies despite a cache hit.

### Windows release

`.github/workflows/windows-release.yml` runs on a native `windows-latest` UCRT64 runner:
`cargo test --release`, the installer, then an installed release GUI screenshot and an
installed-runtime/uninstall smoke. Everything is built in the release profile only.
`workflow_dispatch` uploads Actions artifacts only. A `vVERSION` tag matching
`[workspace.package].version` (optionally with a prerelease suffix) also attaches the
installer to the GitHub release, creating the release if missing. Existing assets are
preserved; only the matching Windows installer is replaced. The build job has read-only
permissions; only the separate publish job receives a write token.

`tools/packaging/windows-ci.sh` is the runner entry point.
`tools/packaging/windows-release.py` must run under UCRT64's native Python and takes every
tool from the prefix's `bin/`. It builds the checked-out source, stages through
`just install`, resolves runtime DLLs from UCRT64, and invokes
`packaging/windows/installer.nsi`:

```sh
python tools/packaging/windows-release.py --prefix "$(cygpath -m "$MINGW_PREFIX")" --target-dir target --version 0.1.0
```

The applications are cleaned before linking so they pick up the regenerated Windows
version/icon resources; dependency builds stay cached. NSIS inputs use Windows
backslashes: MSYS2 Python's canonical `D:/…` paths open in Python but fail NSIS 3.13's
`!include`/`File` lookup.

The output is `target/dist/mirai-VERSION-windows-x86_64-setup.exe`, not a portable zip.
The runtime is self-contained, but KataGo and its models are not bundled.
The installer is per-user and unsigned. Its uninstaller has a compiled-in owned-file list.
Upgrade requires the registered installation path and runs that version's uninstaller;
it never trusts a mutable manifest to choose deletion paths. Unrelated files and
application settings are not installer-owned.
Runtime copyright files come from the owning pacman packages. Shared, platform-neutral
texts in `packaging/licenses/` cover packages that omit them; GPL 3 reuses the root
`LICENSE`. No platform keeps its own copy. Unknown omissions fail the stage.
The runtime subset is deliberate:

- GIO ships GnuTLS and libproxy modules, their import closures, and a cache naming both.
  TLS alone makes HTTPS work but leaves libsoup's default resolver returning `direct://`,
  ignoring proxy settings.
- The scanner's DLL imports live only in `bin/`.
  [GStreamer's Win32 loader](https://gitlab.freedesktop.org/gstreamer/gstreamer/-/blob/1.28.7/subprojects/gstreamer/gst/gstpluginloader-win32.c)
  prepends its own DLL directory to the scanner child's `PATH`.
- No freedesktop MIME database or its catalogue:
  [Win32 GIO content types](https://gitlab.gnome.org/GNOME/glib/-/blob/2.90.0/gio/gcontenttype-win32.c)
  use the registry, and the SGF chooser filters by suffix.
- Pixbuf retains PNG, JPEG, TIFF and SVG fallback, with a cache containing only those
  loaders.
- Icons are Adwaita whole (about 800 files), without AdwaitaLegacy (1,861). Legacy holds
  full-colour PNGs of about 370 pre-symbolic names (`accessories-calculator`, `edit-copy`,
  …) at five sizes, a fallback GNOME keeps for applications that still ask for them. A
  `GTK_DEBUG=icontheme` trace of the main window and its dialogs resolved every themed
  lookup in Adwaita's `symbolic/`, none in Legacy. Cut whole themes, not names: an
  icon-name allowlist taken from mirai's source missed GTK/libadwaita controls and
  fallbacks.
- The staged hicolor `index.theme` lists `scalable/actions`. GTK walks that directory
  list for embedded resource icons too; without it every custom edit-bar icon falls back
  to `image-missing`.
- Audio bundles WASAPI2 alone and fails the stage without it. It is the sink
  `autoaudiosink` ranks first on Windows. DirectSound is not a fallback worth shipping:
  [it runs on the core audio APIs](https://learn.microsoft.com/en-us/windows/win32/coreaudio/about-the-windows-core-audio-apis),
  so it fails exactly when WASAPI2 does. On a desktop with every endpoint disabled, or with
  the Windows Audio service stopped, both sinks fail and `autoaudiosink` falls back to its fake
  sink. Only the listed plugins and their import closures come from `gst-plugins-bad`/`-good`.
- Emoji annotations follow `po/LINGUAS` (currently `zh.gresource`); GTK embeds English.
  Other system languages fall back to English emoji names, not missing glyphs.

### What is not claimed

GTK's Windows backend is the limitation recorded when an earlier port was reverted. GTK 4.22
composites through DirectComposition and draws a black band around every window with its
GPU renderers (gtk#7567); GTK 4.24, which UCRT64 ships, avoids the band by drawing with Cairo
on the CPU, and scrolling, resizing and animation can stutter. That is what the MSYS2
packages get. The installer ships the D3D12 build instead: D3D12 by default, falling back
to OpenGL and then Cairo; Vulkan is disabled. mirai picks no renderer.

CI runs on a hosted Windows runner with no sound device or physical GPU. Desktop GPU
performance, Chinese text rendering and audible output on real hardware are not
established by it. Wine is not a substitute either: it rejects the socket option quinn
sets (`WSAEOPNOTSUPP` 10045), so `mirai-server` networking does not work there.
