# Packaging

Every install goes through the `justfile`; the Arch packages call it too. User-facing
commands are in the [README](../../README.md#installing).

## Installing with `just`

|Path|What|
|---|---|
|`justfile`|`check`, `build`, `install [mirai\|mirai-server\|all]`, `uninstall [mirai\|mirai-server\|all]`; honours `PREFIX` and `DESTDIR`. The one list of installed files|
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
|`packaging/aur/<pkg>/`|one directory per AUR repository, `mirai-git` and `mirai-server-git`: PKGBUILD, `.SRCINFO` and the 0BSD `LICENSE` covering them|
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
cp packaging/aur/$pkg/{PKGBUILD,.SRCINFO,LICENSE,.gitignore} ~/aur/$pkg/
cd ~/aur/$pkg && git add -A && git commit -m '…' && git push
```

### Decisions

**VCS packages.** There are no release tags. `pkgver()` reads the version from the clone at
build time, and the AUR guidelines forbid commits that only bump a VCS `pkgver`. A tagged
release would add PKGBUILDs pinned to a tarball and checksum.

**Two pkgbases, not a split package.** `makedepends` belong to the whole `pkgbase`: split,
building `mirai-server` on a headless host would install GTK.

**The package files are 0BSD**, as Arch asks of AUR package sources.

**No `check()`.** The test suite guards commits upstream (every commit passes it); rerunning
it on each user's machine adds a second release-profile compile of the whole workspace and
proves nothing a build from a tested commit does not. The chroot test's smoke run covers
what packaging can break.

**`!lto`.** The chroot's `makepkg.conf` enables LTO, which compiles the C inside `ring` and
`zstd-sys` to GCC bitcode that rust-lld, rustc's default linker, cannot read; every C
symbol is then undefined. A host `makepkg.conf` with `!lto` (CachyOS's) hides this.

**The chroot test keeps its own package cache.** On CachyOS, `[cachyos]` ships forks of
Arch packages under Arch's exact file names (its `pacman` copies Arch's `pkgver`, and the
`pkgrel`s coincide now and then); a chroot sharing the host cache finds that build and
rejects its signature. The `_v3` repositories carry a different architecture suffix and
never collide.
