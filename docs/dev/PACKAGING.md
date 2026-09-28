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
|`packaging/windows/Containerfile`|the Windows build environment: Fedora 45 with its MinGW GTK 4 stack, libadwaita cross-built from its tarball, and rustup's `x86_64-pc-windows-gnu`|
|`packaging/windows/stage.sh`|runs inside that image: builds both executables with icon and version resources, stages them with every DLL they load and the runtime data GTK and GStreamer look up, then writes a portable zip and an installer|
|`packaging/windows/installer.nsi`|per-user NSIS installer: `%LOCALAPPDATA%\Programs\mirai`, Start menu entry, `.sgf` association, uninstaller|
|`tools/packaging/windows-cross.sh`|runs that from the host with podman (or `CONTAINER_ENGINE=docker`); `package` (default), `debug`, `shell`, `run CMD`. Output in `target/windows/dist/`|
|`tools/packaging/windows-wine.sh`|runs a staged build under Wine in a prefix of its own, `target/windows/wine`; with the debug build and `MIRAI_HARNESS` this is the Windows screenshot recipe|

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
- **A new runtime-loaded module on Windows** — a GStreamer element, a gdk-pixbuf loader, a
  GIO module — → its list in `packaging/windows/stage.sh`. DLLs an executable imports are
  followed automatically; a module loaded by name is not, and a missing GStreamer element
  aborts the process rather than degrading.
- **libadwaita or Fedora moves** → `ADW_VERSION` and `ADW_SHA256`, or the `FROM` line, in
  the Containerfile, then `tools/packaging/windows-cross.sh debug` and a Wine run.

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

**Windows is cross-built from a Fedora container.** Fedora packages the whole GTK 4.22
stack for MinGW, so one Linux machine with podman builds the Windows release, the same way
locally and in CI; gvsbuild or MSYS2 would need a Windows machine. Fedora's `mingw64`
runtime is msvcrt-based, as is Rust's `x86_64-pc-windows-gnu`, so the Rust and C sides
share one CRT. Fedora has no MinGW libadwaita, so the image builds it from the release
tarball against that sysroot. `blueprint-compiler` runs on the build machine inside the
`CompositeTemplate` derive and needs native GTK, libadwaita and gobject-introspection
typelibs, which is the only reason those are in the image. The cargo registry lives in the
`mirai-windows-cargo` volume and the build in `target/windows/`, so an unchanged tree
rebuilds in seconds.

**The Windows package ships its runtime.** `stage.sh` walks each executable's and module's
imports (`objdump -p`) through the sysroot, strips what it copies, and adds what is
loaded by name: gdk-pixbuf loaders, the GStreamer elements stone sounds need, the Adwaita
icon theme (GTK's own resources lack ten of the icons mirai uses), compiled GSettings
schemas, and `gdbus.exe`, which GLib starts as a private session bus so a second launch
hands its files to the running instance. Fedora's GStreamer derives its plugin directory
from its build-time sysroot path, not from where its DLL sits, so on Windows mirai sets
`GST_PLUGIN_SYSTEM_PATH_1_0` itself, in `windows_package.rs`, the one module that sets up
the package's environment; without it GstPlay aborts on the first stone sound. The plugin
scanner is left out for the same reason, and GStreamer scans in-process. For each
language in `po/LINGUAS` it compiles mirai's catalogue and copies GTK's, GLib's and
libadwaita's (the image lifts Fedora's English-only `%_install_langs` to get them); on
Windows `i18n.rs` binds through GNU libintl's `libintl_*` entry points and the
wide-character `wbindtextdomain`, since the narrow one cannot name a profile directory
outside the ANSI code page. A release executable is a GUI-subsystem program; a debug one
keeps its console for logs and the harness.

**Zip and installer, both unsigned.** The zip runs from wherever it is unpacked
(`bin\mirai.exe`). The installer is per-user, so it needs no elevation, and offers mirai
for `.sgf` without taking the association from another program. It installs only into a
missing or empty folder or an earlier mirai installation, because upgrading and
uninstalling delete `bin`, `lib`, `share` and `doc` there. A running mirai holds `bin\`'s
DLLs and may hold an unsaved record, so both ask the user to close it rather than stop it;
`gdbus.exe` keeps running after mirai quits and is stopped, but only the one started from
that folder. The folder reaches PowerShell through an environment variable, not inside
its command line. Neither file is code-signed, so SmartScreen warns on first run.

**What Wine can and cannot prove.** Under Wine 11: the GUI and the harness; KataGo
discovery on `PATH` and live analysis with a Windows KataGo (the Eigen build); a silent
install, Start menu entry, registry entries and a clean silent uninstall; a second launch
handing its file to the first process over `gdbus.exe`; autosaves in
`%LOCALAPPDATA%`. Wine differs from Windows, without that being a defect of mirai, in
that it lacks DirectComposition and DWM blur (GTK logs a critical and falls back); its
fonts lack the `⚫`/`⚪` glyphs the analysis header uses; its `HKEY_CLASSES_ROOT` does not
include `HKCU\Software\Classes`, so the `.sgf` association only opens mirai once copied
to `HKLM`; it passes the host's `DBUS_SESSION_BUS_ADDRESS` through (`windows-wine.sh`
unsets it); it has no PowerShell, so the installer's process checks are untested; its
font fallback lacks simplified-only Chinese glyphs such as 胜, 谱 and 释; and it rejects
the `IP_RECVECN` socket option quinn sets, so neither `mirai-server.exe` nor a remote
profile can bind a socket. The network path is therefore unverified on Windows.

**Fox Go does not work on Windows.** `fox.rs` fetches through `gio::File::for_uri`, and
GIO serves `http(s)` URIs only through GVfs, which does not exist on Windows; the download
dialog reports the error. Fixing it means a different HTTP client, which is a dependency
decision.
