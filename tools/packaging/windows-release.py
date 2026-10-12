#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""Build mirai on native MSYS2 UCRT64 and package a relocatable NSIS installer.

From the repository root, in an UCRT64 shell::

    python tools/packaging/windows-release.py \\
        --prefix "$(cygpath -m "$MINGW_PREFIX")" \\
        --target-dir target --version VERSION

That builds a locked x86_64-pc-windows-gnu release with the prefix's cargo/rustc/gcc
(ahead of rustup and MSVC), installs the shared ``just`` file list, bundles the runtime
those binaries import, and writes::

    target/dist/mirai-VERSION-windows-x86_64-setup.exe

Every tool comes from the prefix's ``bin/``. The staged layout is the MSYS2 prefix
layout. Libraries relocate from their own DLL location; this tool does not bake the
build prefix into the tree and does not set Fedora's ``GST_PLUGIN_SYSTEM_PATH`` or
``G_TLS_GNUTLS_PRIORITY``.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TRIPLE = "x86_64-pc-windows-gnu"
OWNED_ROOTS = ("bin", "lib", "libexec", "share", "etc")
MARKER_REL = "share/mirai/install.marker"
MARKER_TEXT = "mirai-owned-install 1\n"
APP_ID = "io.github.zhaob1n.Mirai"

REQUIRED_PLUGINS = (
    "coreelements",
    "typefindfunctions",
    "playback",
    "gio",
    "audioconvert",
    "audioresample",
    "audiofx",
    "volume",
    "wavparse",
    "autodetect",
    "wasapi2",
)
REQUIRED_HELPERS = (
    "gdbus.exe",
    "gspawn-win64-helper.exe",
    "gspawn-win64-helper-console.exe",
)
PIXBUF_LOADERS = (
    "libpixbufloader-jpeg.dll",
    "libpixbufloader-png.dll",
    "libpixbufloader-tiff.dll",
    "pixbufloader_svg.dll",
)
REQUIRED_DOMAINS = ("gtk40", "glib20", "libadwaita")
OPTIONAL_DOMAINS = ("gdk-pixbuf", "gsettings-desktop-schemas")
SCHEMA_FILES = (
    "org.gnome.desktop.enums.xml",
    "org.gnome.desktop.interface.gschema.xml",
    "org.gnome.desktop.a11y.interface.gschema.xml",
    "org.gtk.gtk4.Settings.ColorChooser.gschema.xml",
    "org.gtk.gtk4.Settings.Debug.gschema.xml",
    "org.gtk.gtk4.Settings.EmojiChooser.gschema.xml",
    "org.gtk.gtk4.Settings.FileChooser.gschema.xml",
    "org.gtk.gtk4.Inspector.gschema.xml",
)
JUST_FILES = (
    "bin/mirai.exe",
    "bin/mirai-server.exe",
    "share/licenses/mirai/LICENSE",
    "share/licenses/mirai-server/LICENSE",
    "share/doc/mirai/README.md",
    "share/doc/mirai/README.zh-CN.md",
    "share/doc/mirai/GUIDE.md",
    "share/doc/mirai/GUIDE.zh-CN.md",
    "share/doc/mirai-server/server.example.toml",
    f"share/icons/hicolor/scalable/apps/{APP_ID}.svg",
    f"share/icons/hicolor/symbolic/apps/{APP_ID}-symbolic.svg",
)

# Windows DLLs that are part of the OS, not the MSYS2 prefix. An import that is neither
# here nor in the prefix is an error: the old stage script treated every miss as a
# system DLL and shipped a tree that failed at runtime.
SYSTEM_DLLS = {
    "advapi32.dll",
    "authz.dll",
    "avrt.dll",
    "bcrypt.dll",
    "bcryptprimitives.dll",
    "cabinet.dll",
    "cfgmgr32.dll",
    "clbcatq.dll",
    "combase.dll",
    "comctl32.dll",
    "comdlg32.dll",
    "coml2.dll",
    "credui.dll",
    "crypt32.dll",
    "cryptbase.dll",
    "cryptsp.dll",
    "cryptui.dll",
    "d3d11.dll",
    "d3d12.dll",
    "d3dcompiler_47.dll",
    "dcomp.dll",
    "dbghelp.dll",
    "dhcpcsvc.dll",
    "dhcpcsvc6.dll",
    "dnsapi.dll",
    "dsound.dll",
    "dwmapi.dll",
    "dwrite.dll",
    "dxgi.dll",
    "dxva2.dll",
    "gdi32.dll",
    "gdiplus.dll",
    "hid.dll",
    "imagehlp.dll",
    "imm32.dll",
    "iphlpapi.dll",
    "kernel32.dll",
    "kernelbase.dll",
    "mf.dll",
    "mfplat.dll",
    "mfreadwrite.dll",
    "mmdevapi.dll",
    "mpr.dll",
    "msasn1.dll",
    "msctf.dll",
    "msimg32.dll",
    "msvcrt.dll",
    "netapi32.dll",
    "netutils.dll",
    "normaliz.dll",
    "nsi.dll",
    "ntdll.dll",
    "ncrypt.dll",
    "ole32.dll",
    "oleacc.dll",
    "oleaut32.dll",
    "opengl32.dll",
    "powrprof.dll",
    "profapi.dll",
    "propsys.dll",
    "psapi.dll",
    "rpcrt4.dll",
    "schannel.dll",
    "sechost.dll",
    "secur32.dll",
    "setupapi.dll",
    "shcore.dll",
    "shell32.dll",
    "shlwapi.dll",
    "ucrtbase.dll",
    "umpdc.dll",
    "urlmon.dll",
    "user32.dll",
    "userenv.dll",
    "usp10.dll",
    "uxtheme.dll",
    "version.dll",
    "wevtapi.dll",
    "winhttp.dll",
    "wininet.dll",
    "winmm.dll",
    "winscard.dll",
    "winspool.drv",
    "wldap32.dll",
    "ws2_32.dll",
    "wsock32.dll",
    "wtsapi32.dll",
    "windowscodecs.dll",
}

DLL_NAME = re.compile(r"^\s*DLL Name:\s*(.+?)\s*$", re.IGNORECASE)
HICOLOR_INDEX = """\
[Icon Theme]
Name=Hicolor
Comment=Fallback icon theme
Directories=scalable/apps,symbolic/apps,scalable/actions

[scalable/apps]
Size=48
MinSize=16
MaxSize=512
Context=Applications
Type=Scalable

[symbolic/apps]
Size=16
MinSize=8
MaxSize=512
Context=Applications
Type=Scalable

[scalable/actions]
Size=24
MinSize=8
MaxSize=512
Context=Actions
Type=Scalable
"""


def die(message: str) -> None:
    print(f"windows-release: {message}", file=sys.stderr)
    raise SystemExit(1)


def run(cmd: list[str], *, env: dict[str, str] | None = None, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    shown = " ".join(cmd)
    print(f"+ {shown}", flush=True)
    proc = subprocess.run(cmd, env=env, cwd=cwd, text=True, errors="replace")
    if proc.returncode != 0:
        die(f"command failed ({proc.returncode}): {cmd[0]}")
    return proc


def prefix_tool(prefix: Path, name: str) -> Path:
    tool = prefix / "bin" / f"{name}.exe"
    if not tool.is_file():
        die(f"missing {tool}; install the UCRT64 package that provides {name}")
    return tool


def nsis_path(path: Path) -> str:
    # MSYS2 Python uses forward slashes; NSIS 3.13's native include/file lookup
    # requires Windows separators, even though Python can open either form.
    return str(path.resolve()).replace("/", "\\")


def linguas() -> list[str]:
    langs = []
    for line in (ROOT / "po" / "LINGUAS").read_text(encoding="utf-8").splitlines():
        lang = line.split("#", 1)[0].strip()
        if lang:
            langs.append(lang)
    if not langs:
        die("po/LINGUAS has no languages")
    return langs


def version_quad(version: str) -> str:
    core = version.split("+", 1)[0].split("-", 1)[0]
    nums: list[str] = []
    for part in core.split(".")[:3]:
        if not part.isdigit():
            break
        nums.append(str(int(part)))
    while len(nums) < 3:
        nums.append("0")
    return ".".join(nums[:3] + ["0"])


def valid_owned(rel: str) -> bool:
    if not rel or len(rel) > 220 or rel.endswith("/") or "//" in rel or ".." in rel:
        return False
    if any(ch in rel for ch in '\\:*?"<>|$'):
        return False
    root, sep, rest = rel.partition("/")
    return sep == "/" and root in OWNED_ROOTS and rest != "" and "." not in rel.split("/") and ".." not in rel.split("/")


def nsis_quoted(text: str) -> str:
    if any(ch in text for ch in '"\n\r'):
        die(f"path cannot be quoted for NSIS: {text}")
    return '"' + text.replace("$", "$$") + '"'


class PacmanIndex:
    def __init__(self, prefix: Path) -> None:
        self.prefix = prefix
        self.prefix_name = prefix.name
        db = prefix.parent / "var" / "lib" / "pacman" / "local"
        if not db.is_dir():
            die(f"pacman database not found at {db}; cannot attach runtime copyright files")
        self.owners: dict[str, Path] = {}
        self.packages: dict[Path, dict[str, object]] = {}
        for pkg in db.iterdir():
            if not pkg.is_dir():
                continue
            rels = self._files(pkg / "files")
            if not rels:
                continue
            for rel in rels:
                self.owners.setdefault(rel.lower(), pkg)
            self.packages[pkg] = {"rels": rels, **self._desc(pkg / "desc")}

    def _section(self, path: Path, name: str) -> list[str]:
        if not path.is_file():
            return []
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        out: list[str] = []
        capture = False
        for line in lines:
            if line == f"%{name}%":
                capture = True
                continue
            if line.startswith("%") and line.endswith("%"):
                if capture:
                    break
                continue
            if capture and line:
                out.append(line.replace("\\", "/"))
        return out

    def _files(self, path: Path) -> list[str]:
        rels = []
        for line in self._section(path, "FILES"):
            prefix = self.prefix_name + "/"
            if line.startswith(prefix) and not line.endswith("/"):
                rels.append(line[len(prefix) :])
        return rels

    def _desc(self, path: Path) -> dict[str, object]:
        def one(name: str) -> str:
            values = self._section(path, name)
            return values[0] if values else ""

        return {
            "name": one("NAME"),
            "version": one("VERSION"),
            "url": one("URL"),
            "licenses": self._section(path, "LICENSE"),
        }

    def note(self, src: Path, noted: set[str]) -> None:
        try:
            rel = src.resolve().relative_to(self.prefix.resolve()).as_posix()
        except ValueError:
            return
        if src.name in {"loaders.cache", "icon-theme.cache"} and rel.lower() not in self.owners:
            return
        noted.add(rel)


def copy_file(src: Path, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dest, follow_symlinks=True)


def copy_tree(src: Path, dest: Path, index: PacmanIndex, noted: set[str]) -> None:
    if not src.is_dir():
        die(f"missing {src}")
    for dirpath, _dirnames, filenames in os.walk(src, followlinks=False):
        source_dir = Path(dirpath)
        for name in filenames:
            file_src = source_dir / name
            if file_src.is_symlink():
                file_src = file_src.resolve()
            rel = source_dir.relative_to(src) / name
            copy_file(file_src, dest / rel)
            index.note(source_dir / name, noted)


def pe_imports(objdump: Path, pe: Path) -> list[str]:
    proc = subprocess.run(
        [str(objdump), "-p", str(pe)],
        text=True,
        errors="replace",
        capture_output=True,
    )
    if proc.returncode != 0:
        die(f"objdump failed on {pe}: {proc.stderr.strip()}")
    names = [match.group(1).strip() for match in (DLL_NAME.match(line) for line in proc.stdout.splitlines()) if match]
    if not names:
        die(f"{objdump} reported no DLL imports for {pe}; it cannot read this PE")
    return names


def is_system_dll(name: str) -> bool:
    low = name.lower()
    return low.startswith("api-ms-") or low.startswith("ext-ms-") or low in SYSTEM_DLLS


def bundle_imports(
    pe: Path,
    dest_dir: Path,
    *,
    objdump: Path,
    bin_index: dict[str, Path],
    prefix: Path,
    index: PacmanIndex,
    noted: set[str],
    seen: set[tuple[str, str]],
) -> None:
    dest_dir.mkdir(parents=True, exist_ok=True)
    for dll in pe_imports(objdump, pe):
        key = (str(dest_dir), dll.lower())
        if key in seen:
            continue
        seen.add(key)
        src = bin_index.get(dll.lower())
        if src is None:
            local = next((item for item in dest_dir.iterdir() if item.name.lower() == dll.lower()), None)
            src = local if local and local.is_file() else None
        if src is not None:
            dest = dest_dir / src.name
            if src.resolve() != dest.resolve():
                copy_file(src, dest)
                index.note(src, noted)
            bundle_imports(
                src,
                dest_dir,
                objdump=objdump,
                bin_index=bin_index,
                prefix=prefix,
                index=index,
                noted=noted,
                seen=seen,
            )
            continue
        if is_system_dll(dll):
            continue
        die(f"{pe} imports {dll}, which is not in {prefix / 'bin'} and is not a Windows system DLL")


def sanitize_loaders_cache(src: Path, dest: Path, loaders: set[str]) -> None:
    text = src.read_text(encoding="utf-8", errors="replace")
    lines = []
    keep = True
    cached = set()
    for line in text.splitlines():
        if line.startswith("# LoaderDir"):
            lines.append("# LoaderDir = lib/gdk-pixbuf-2.0/2.10.0/loaders")
            continue
        if line.startswith('"') and line.endswith('"') and ".dll" in line.lower():
            inner = line[1:-1]
            if ":\\" in inner or inner.startswith("/") or inner.startswith("\\\\") or inner.startswith("Z:"):
                die(f"gdk-pixbuf loader cache has an absolute path: {line}")
            name = Path(inner.replace("\\", "/")).name.lower()
            keep = name in loaders
            if keep:
                cached.add(name)
        if keep:
            lines.append(line)
    if missing := loaders - cached:
        die(f"gdk-pixbuf cache is missing shipped loaders: {', '.join(sorted(missing))}")
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")


def prefix_needles(prefix: Path) -> list[bytes]:
    text = str(prefix.resolve())
    return [
        text.encode(),
        text.replace("/", "\\").encode(),
        b"file:///ucrt64",
    ]


def assert_no_sysroot(path: Path, prefix: Path, *, extra: tuple[bytes, ...] = ()) -> None:
    data = path.read_bytes()
    for needle in [*prefix_needles(prefix), *extra]:
        if needle and needle in data:
            die(f"{path} contains a build-machine path ({needle!r}); refusing to ship it")


def assert_tree_relocatable(stage: Path, prefix: Path) -> None:
    needles = [needle for needle in prefix_needles(prefix) if needle]
    for path in stage.rglob("*"):
        if not path.is_file() or path.suffix.lower() in {".dll", ".exe", ".mo"}:
            continue
        data = path.read_bytes()
        for needle in needles:
            if needle in data:
                die(f"{path} contains a build-machine path ({needle!r}); refusing to ship it")


def build_icon(prefix: Path, res: Path) -> Path:
    svg = ROOT / "crates" / "mirai" / "resources" / "icons" / "hicolor" / "scalable" / "apps" / f"{APP_ID}.svg"
    if not svg.is_file():
        die(f"missing icon source {svg}")
    rsvg = prefix_tool(prefix, "rsvg-convert")
    pngs: list[Path] = []
    for size in (16, 24, 32, 48, 64, 128, 256):
        png = res / f"icon-{size}.png"
        run([str(rsvg), "-w", str(size), "-h", str(size), "-o", str(png), str(svg)])
        pngs.append(png)
    ico = res / "mirai.ico"
    run([str(prefix_tool(prefix, "icotool")), "-c", "-o", str(ico), *[str(png) for png in pngs]])
    if not ico.is_file():
        die("icotool did not write the installer icon")
    return ico


def write_resources(prefix: Path, res: Path, version: str) -> dict[str, Path]:
    ico = build_icon(prefix, res)
    quad = version_quad(version)
    comma = quad.replace(".", ",")
    objects: dict[str, Path] = {}
    windres = prefix_tool(prefix, "windres")
    include = prefix / "include"
    for binary, description in (("mirai", "mirai"), ("mirai-server", "mirai-server")):
        rc = res / f"{binary}.rc"
        obj = res / f"{binary}.o"
        rc.write_text(
            "\n".join(
                [
                    "#include <winver.h>",
                    '1 ICON "mirai.ico"',
                    "1 VERSIONINFO",
                    f"FILEVERSION {comma}",
                    f"PRODUCTVERSION {comma}",
                    "FILEOS VOS_NT_WINDOWS32",
                    "FILETYPE VFT_APP",
                    "BEGIN",
                    '  BLOCK "StringFileInfo"',
                    "  BEGIN",
                    '    BLOCK "040904B0"',
                    "    BEGIN",
                    f'      VALUE "FileDescription", "{description}"',
                    f'      VALUE "FileVersion", "{version}"',
                    f'      VALUE "InternalName", "{binary}"',
                    '      VALUE "LegalCopyright", "Copyright (C) 2026 Huang Zhaobin. GPL-3.0-or-later."',
                    f'      VALUE "OriginalFilename", "{binary}.exe"',
                    '      VALUE "ProductName", "mirai"',
                    f'      VALUE "ProductVersion", "{version}"',
                    "    END",
                    "  END",
                    '  BLOCK "VarFileInfo"',
                    "  BEGIN",
                    "    VALUE \"Translation\", 0x409, 1200",
                    "  END",
                    "END",
                    "",
                ]
            ),
            encoding="utf-8",
        )
        cmd = [str(windres), "-I", str(res)]
        if include.is_dir():
            cmd.extend(["-I", str(include)])
        cmd.extend(["-O", "coff", "-o", str(obj), str(rc)])
        run(cmd)
        objects[binary] = obj
    return objects


def cargo_build(prefix: Path, target_dir: Path, objects: dict[str, Path]) -> Path:
    cargo = prefix_tool(prefix, "cargo")
    env = os.environ.copy()
    env["PATH"] = str(prefix / "bin") + os.pathsep + env.get("PATH", "")
    env["CARGO_TARGET_DIR"] = str(target_dir.resolve())
    for var, name in (("RUSTC", "rustc"), ("CC", "gcc"), ("CXX", "g++"), ("AR", "ar"), ("PKG_CONFIG", "pkg-config")):
        env[var] = str(prefix_tool(prefix, name))
    env.pop("RUSTUP_TOOLCHAIN", None)
    # The regenerated windres objects are external linker inputs. Keep cached
    # dependencies, but relink the applications with this run's version/icon.
    run([str(cargo), "clean", "--locked", "--release", "--target", TRIPLE, "-p", "mirai", "-p", "mirai-server"], env=env, cwd=ROOT)
    for package, binary in (("mirai", "mirai"), ("mirai-server", "mirai-server")):
        link = objects[binary].resolve().as_posix()
        cmd = [str(cargo), "rustc", "--locked", "--release", "--target", TRIPLE]
        cmd.extend(["-p", package, "--bin", binary, "--", "-C", f"link-arg={link}"])
        run(cmd, env=env, cwd=ROOT)
    return target_dir / TRIPLE / "release"


def just_install(prefix: Path, stage: Path, bin_dir: Path) -> None:
    env = os.environ.copy()
    env["PATH"] = str(prefix / "bin") + os.pathsep + env.get("PATH", "")
    env["DESTDIR"] = stage.resolve().as_posix()
    env["PREFIX"] = "/"
    env["MIRAI_BIN_DIR"] = bin_dir.resolve().as_posix()
    env["MIRAI_EXE_SUFFIX"] = ".exe"
    env["MIRAI_DESKTOP"] = "0"
    run([str(prefix_tool(prefix, "just")), "install"], env=env, cwd=ROOT)


def compile_schemas(prefix: Path, schema_dir: Path) -> None:
    run([str(prefix_tool(prefix, "glib-compile-schemas")), f"--targetdir={schema_dir}", str(schema_dir)])


def attach_licenses(stage: Path, index: PacmanIndex, noted: set[str]) -> None:
    # MSYS2 does not install copyright files for these. The texts are the upstream
    # license documents (GNU texts as shipped by other prefix packages, Mozilla's
    # MPL 2.0, and p11-kit's BSD terms). A metadata-only note is not a substitute.
    vendored = ROOT / "packaging" / "licenses"
    fallback = {
        "gstreamer": (
            ((vendored / "LGPL-2.1.txt", "COPYING"),),
            "GStreamer is free software; you can redistribute it and/or modify it\n"
            "under the terms of the GNU Lesser General Public License as published by\n"
            "the Free Software Foundation; either version 2.1 of the License, or (at\n"
            "your option) any later version. COPYING is that license, from the\n"
            "GStreamer 1.28 tree.\n",
        ),
        "gst-plugins-base": (
            ((vendored / "LGPL-2.1.txt", "COPYING"),),
            "gst-plugins-base is free software under the GNU Lesser General Public\n"
            "License, version 2.1 or later. COPYING is that license.\n",
        ),
        "gst-plugins-good": (
            ((vendored / "LGPL-2.1.txt", "COPYING"),),
            "gst-plugins-good is free software under the GNU Lesser General Public\n"
            "License, version 2.1 or later. COPYING is that license.\n",
        ),
        "gst-plugins-bad-libs": (
            ((vendored / "LGPL-2.1.txt", "COPYING"),),
            "gst-plugins-bad-libs is free software under the GNU Lesser General Public\n"
            "License, version 2.1 or later. COPYING is that license. This is the\n"
            "library package, not the GPL plugins.\n",
        ),
        "gst-plugins-bad": (
            ((vendored / "LGPL-2.1.txt", "COPYING"),),
            "The bundled WASAPI2 plugin is under the GNU Lesser General Public\n"
            "License, version 2.1 or later. COPYING is that license.\n",
        ),
        "gnutls": (
            ((ROOT / "LICENSE", "COPYING"), (vendored / "LGPL-2.1.txt", "COPYING.LESSERv2")),
            "The GnuTLS library is under the GNU LGPL version 2.1 or later\n"
            "(COPYING.LESSERv2). The applications are under the GNU GPL version 3 or\n"
            "later (COPYING), unless a source file says otherwise.\n"
            "https://gitlab.com/gnutls/gnutls/-/blob/3.8.13/README.md\n",
        ),
        "gmp": (
            (
                (vendored / "LGPL-3.0.txt", "COPYING.LESSERv3"),
                (ROOT / "LICENSE", "COPYINGv3"),
                (vendored / "GPL-2.0.txt", "COPYINGv2"),
            ),
            "The GNU MP library is dual licensed, under the GNU LGPL version 3\n"
            "(COPYING.LESSERv3, which incorporates COPYINGv3) or the GNU GPL version 2\n"
            "(COPYINGv2), at the recipient's choice, including later versions.\n"
            "https://gmplib.org/manual/Copying.html\n",
        ),
        "nettle": (
            (
                (vendored / "LGPL-3.0.txt", "COPYING.LESSERv3"),
                (ROOT / "LICENSE", "COPYINGv3"),
                (vendored / "GPL-2.0.txt", "COPYINGv2"),
            ),
            "Nettle is dual licensed under the GNU LGPL version 3 or later\n"
            "(COPYING.LESSERv3, which incorporates COPYINGv3) or the GNU GPL version 2\n"
            "or later (COPYINGv2).\n",
        ),
        "libidn2": (
            (
                (vendored / "LGPL-3.0.txt", "COPYING.LESSERv3"),
                (ROOT / "LICENSE", "COPYINGv3"),
                (vendored / "GPL-2.0.txt", "COPYINGv2"),
            ),
            "GNU libidn2 is dual licensed under the GNU LGPL version 3 or later\n"
            "(COPYING.LESSERv3, which incorporates COPYINGv3) or the GNU GPL version 2\n"
            "or later (COPYINGv2).\n",
        ),
        "p11-kit": (
            ((vendored / "BSD-3-Clause.txt", "COPYING"),),
            "p11-kit is under the 3-clause BSD license. COPYING is the upstream\n"
            "0.26.5 license, including the copyright notice.\n"
            "https://github.com/p11-glue/p11-kit/blob/0.26.5/COPYING\n",
        ),
        "ca-certificates": (
            ((vendored / "MPL-2.0.txt", "MPL-2.0.txt"), (vendored / "GPL-2.0.txt", "COPYING")),
            "The Mozilla CA certificate list is available under the Mozilla Public\n"
            "License 2.0 (MPL-2.0.txt). The package's remaining material is available\n"
            "under the GNU GPL version 2 or later (COPYING).\n",
        ),
    }
    used: set[Path] = set()
    for rel in noted:
        pkg = index.owners.get(rel.lower())
        if pkg is None:
            die(f"shipped prefix file has no pacman owner: {rel}")
        used.add(pkg)
    for pkg in sorted(used, key=lambda item: item.name):
        info = index.packages[pkg]
        rels: list[str] = info["rels"]  # type: ignore[assignment]
        license_rels = [
            rel
            for rel in rels
            if rel.lower().startswith("share/licenses/")
            or Path(rel).name.lower() in {"copying", "copyright", "license", "authors"}
            or Path(rel).name.lower().startswith(("copying.", "license."))
        ]
        copied = False
        for rel in license_rels:
            src = index.prefix / rel
            if not src.is_file():
                continue
            copy_file(src, stage / rel)
            copied = True
        if copied:
            continue
        short = str(info["name"])
        for lead in ("mingw-w64-ucrt-x86_64-", "mingw-w64-clang-x86_64-", "mingw-w64-x86_64-"):
            if short.startswith(lead):
                short = short[len(lead) :]
                break
        entry = fallback.get(short)
        if entry is None:
            die(f"{info['name']} ships no copyright file and has no vendored license text")
        files, notice = entry
        dest = stage / "share" / "licenses" / short
        dest.mkdir(parents=True, exist_ok=True)
        licenses = info["licenses"]
        recorded = ", ".join(str(item) for item in licenses) if licenses else "(not recorded)"
        url = str(info["url"]).rstrip("/")
        header = (
            f"Package: {info['name']}\n"
            f"Version: {info['version']}\n"
            f"URL: {url}\n"
            f"License: {recorded}\n\n"
        )
        (dest / "NOTICE").write_text(header + notice, encoding="utf-8")
        for src, filename in files:
            if not src.is_file():
                die(f"vendored license text missing: {src}")
            copy_file(src, dest / filename)


def write_marker(stage: Path) -> None:
    marker = stage / MARKER_REL
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.write_text(MARKER_TEXT, encoding="utf-8")


def write_files_nsh(stage: Path, nsh: Path) -> None:
    grouped: dict[str, list[Path]] = {}
    rels: list[str] = []
    for dirpath, _dirnames, filenames in os.walk(stage):
        for name in filenames:
            path = Path(dirpath) / name
            rel = path.relative_to(stage).as_posix()
            if not valid_owned(rel):
                die(f"staged path is outside the owned roots: {rel}")
            grouped.setdefault(str(Path(rel).parent).replace("\\", "/"), []).append(path)
            rels.append(rel)
    install: list[str] = ["!macro MiraiInstallFiles", "  SetOverwrite on"]
    for directory in sorted(grouped):
        inst = "$INSTDIR" if directory == "." else "$INSTDIR\\" + directory.replace("/", "\\")
        install.append(f'  SetOutPath "{inst}"')
        for path in sorted(grouped[directory], key=lambda item: item.name.lower()):
            install.append(f"  File {nsis_quoted(nsis_path(path))}")
    install.append("!macroend")
    uninstall = ["!macro MiraiUninstallFiles"]
    for rel in sorted(rels):
        uninstall.append(f'  Delete "$INSTDIR\\{rel.replace("/", "\\")}"')
    dirs = {
        directory.as_posix()
        for rel in rels
        for directory in Path(rel).parents
        if directory != Path(".")
    }
    for rel in sorted(dirs, key=lambda item: (-item.count("/"), item)):
        uninstall.append(f'  RMDir "$INSTDIR\\{rel.replace("/", "\\")}"')
    uninstall.append("!macroend")
    nsh.write_text("\n".join([*install, "", *uninstall, ""]), encoding="utf-8")


def stage_runtime(prefix: Path, stage: Path, objdump: Path) -> None:
    index = PacmanIndex(prefix)
    noted: set[str] = set()
    bin_index = {
        path.name.lower(): path
        for path in (prefix / "bin").iterdir()
        if path.is_file() and path.suffix.lower() == ".dll"
    }
    seen: set[tuple[str, str]] = set()

    def take(src: Path, dest: Path) -> None:
        if not src.is_file():
            die(f"missing runtime file {src}")
        copy_file(src, dest)
        index.note(src, noted)

    for helper in REQUIRED_HELPERS:
        src = prefix / "bin" / helper
        dest = stage / "bin" / helper
        take(src, dest)
        bundle_imports(dest, stage / "bin", objdump=objdump, bin_index=bin_index, prefix=prefix, index=index, noted=noted, seen=seen)

    pixbuf_root = prefix / "lib" / "gdk-pixbuf-2.0"
    versions = [path for path in pixbuf_root.iterdir() if path.is_dir()] if pixbuf_root.is_dir() else []
    if len(versions) != 1:
        die(f"expected one gdk-pixbuf module directory in {pixbuf_root}")
    loader_src = versions[0] / "loaders"
    loader_dest = stage / "lib" / "gdk-pixbuf-2.0" / versions[0].name / "loaders"
    for name in PIXBUF_LOADERS:
        loader = loader_src / name
        dest = loader_dest / name
        take(loader, dest)
        bundle_imports(dest, stage / "bin", objdump=objdump, bin_index=bin_index, prefix=prefix, index=index, noted=noted, seen=seen)
    cache_src = versions[0] / "loaders.cache"
    cache_dest = stage / "lib" / "gdk-pixbuf-2.0" / versions[0].name / "loaders.cache"
    sanitize_loaders_cache(cache_src, cache_dest, set(PIXBUF_LOADERS))
    index.note(cache_src, noted)
    assert_no_sysroot(cache_dest, prefix)

    gio_cache_lines = []
    for name, extension in (
        ("libgiognutls.dll", "gio-tls-backend"),
        ("libgiolibproxy.dll", "gio-proxy-resolver"),
    ):
        src = prefix / "lib" / "gio" / "modules" / name
        dest = stage / "lib" / "gio" / "modules" / name
        take(src, dest)
        bundle_imports(dest, stage / "bin", objdump=objdump, bin_index=bin_index, prefix=prefix, index=index, noted=noted, seen=seen)
        gio_cache_lines.append(f"{name}: {extension}\n")
    gio_cache = stage / "lib" / "gio" / "modules" / "giomodule.cache"
    # LF, not this Python's CRLF: GIO splits the cache on "\n" alone, so a CRLF cache
    # registers its modules under "gio-proxy-resolver\r" and libproxy never loads.
    gio_cache.write_text("".join(gio_cache_lines), encoding="utf-8", newline="\n")
    assert_no_sysroot(gio_cache, prefix)

    plugin_dir = prefix / "lib" / "gstreamer-1.0"
    staged_plugins = stage / "lib" / "gstreamer-1.0"
    for name in REQUIRED_PLUGINS:
        src = plugin_dir / f"libgst{name}.dll"
        dest = staged_plugins / src.name
        take(src, dest)
        bundle_imports(dest, stage / "bin", objdump=objdump, bin_index=bin_index, prefix=prefix, index=index, noted=noted, seen=seen)
    scanner_src = prefix / "libexec" / "gstreamer-1.0" / "gst-plugin-scanner.exe"
    scanner_dest = stage / "libexec" / "gstreamer-1.0" / "gst-plugin-scanner.exe"
    take(scanner_src, scanner_dest)
    # GStreamer's Win32 plugin loader prepends its DLL directory (bin/) to the
    # scanner child's PATH. Keep one import closure there, not beside the helper.
    bundle_imports(
        scanner_dest,
        stage / "bin",
        objdump=objdump,
        bin_index=bin_index,
        prefix=prefix,
        index=index,
        noted=noted,
        seen=seen,
    )

    trust = prefix / "lib" / "pkcs11" / "p11-kit-trust.dll"
    take(trust, stage / "lib" / "pkcs11" / trust.name)
    bundle_imports(
        stage / "lib" / "pkcs11" / trust.name,
        stage / "bin",
        objdump=objdump,
        bin_index=bin_index,
        prefix=prefix,
        index=index,
        noted=noted,
        seen=seen,
    )
    module = prefix / "share" / "p11-kit" / "modules" / "p11-kit-trust.module"
    take(module, stage / "share" / "p11-kit" / "modules" / module.name)
    assert_no_sysroot(stage / "share" / "p11-kit" / "modules" / module.name, prefix)
    ca_source = prefix / "share" / "pki" / "ca-trust-source"
    if not ca_source.is_dir():
        die(f"missing GnuTLS trust source {ca_source}")
    copy_tree(ca_source, stage / "share" / "pki" / "ca-trust-source", index, noted)
    # MSYS2 patches OpenSSL and curl to resolve their /ucrt64 paths against the DLL's own
    # directory, so libproxy's HTTPS (PAC) fetches read these from <install>/etc/ssl.
    for rel in ("etc/ssl/cert.pem", "etc/ssl/certs/ca-bundle.crt"):
        src = prefix / rel
        if not src.is_file():
            die(f"missing CA bundle {src}")
        take(src, stage / rel)

    for exe in ("mirai.exe", "mirai-server.exe"):
        bundle_imports(
            stage / "bin" / exe,
            stage / "bin",
            objdump=objdump,
            bin_index=bin_index,
            prefix=prefix,
            index=index,
            noted=noted,
            seen=seen,
        )

    schema_src = prefix / "share" / "glib-2.0" / "schemas"
    schema_dest = stage / "share" / "glib-2.0" / "schemas"
    schema_dest.mkdir(parents=True, exist_ok=True)
    for name in SCHEMA_FILES:
        src = schema_src / name
        if not src.is_file():
            die(f"missing GSettings schema {src}")
        take(src, schema_dest / name)
        assert_no_sysroot(schema_dest / name, prefix)
    compile_schemas(prefix, schema_dest)
    compiled = schema_dest / "gschemas.compiled"
    if not compiled.is_file():
        die("glib-compile-schemas produced no gschemas.compiled")
    assert_no_sysroot(compiled, prefix, extra=(b"/ucrt64",))

    for theme in ("Adwaita", "AdwaitaLegacy"):
        src = prefix / "share" / "icons" / theme
        if not src.is_dir():
            die(f"missing icon theme {src}")
        copy_tree(src, stage / "share" / "icons" / theme, index, noted)
    index_theme = stage / "share" / "icons" / "hicolor" / "index.theme"
    index_theme.parent.mkdir(parents=True, exist_ok=True)
    index_theme.write_text(HICOLOR_INDEX, encoding="utf-8", newline="\n")

    langs = linguas()
    # GTK embeds English annotations; other UI languages use external resources.
    emoji = prefix / "share" / "gtk-4.0" / "emoji"
    for lang in sorted({lang.replace("_", "-").split("-", 1)[0].split("@", 1)[0] for lang in langs}):
        src = emoji / f"{lang}.gresource"
        if src.is_file():
            take(src, stage / "share" / "gtk-4.0" / "emoji" / src.name)

    for lang in langs:
        catalog = stage / "share" / "locale" / lang / "LC_MESSAGES" / "mirai.mo"
        if not catalog.is_file():
            die(f"just install did not compile {catalog}")
        messages = catalog.parent
        for domain in (*REQUIRED_DOMAINS, *OPTIONAL_DOMAINS):
            src = prefix / "share" / "locale" / lang / "LC_MESSAGES" / f"{domain}.mo"
            if domain in REQUIRED_DOMAINS and not src.is_file():
                die(f"missing gettext catalogue {src}")
            if src.is_file():
                take(src, messages / src.name)

    attach_licenses(stage, index, noted)


def makensis(prefix: Path, stage: Path, res: Path, version: str, outfile: Path, ico: Path) -> None:
    nsh = res / "files.nsh"
    write_files_nsh(stage, nsh)
    license_file = stage / "share" / "licenses" / "mirai" / "LICENSE"
    script = ROOT / "packaging" / "windows" / "installer.nsi"
    defines = {
        "VERSION": version,
        "VERSION_QUAD": version_quad(version),
        "LICENSE": nsis_path(license_file),
        "ICON": nsis_path(ico),
        "OUTFILE": nsis_path(outfile),
        "FILES_NSH": nsis_path(nsh),
    }
    cmd = [str(prefix_tool(prefix, "makensis")), "-INPUTCHARSET", "UTF8", "-V2"]
    for name, value in defines.items():
        cmd.append(f"-D{name}={value}")
    cmd.append(nsis_path(script))
    outfile.parent.mkdir(parents=True, exist_ok=True)
    run(cmd)
    if not outfile.is_file():
        die(f"makensis did not write {outfile}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--prefix", required=True, type=Path, help="MSYS2 UCRT64 prefix (MINGW_PREFIX)")
    parser.add_argument("--target-dir", default=Path("target"), type=Path)
    parser.add_argument("--version", required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", args.version):
        die("--version must be MAJOR.MINOR.PATCH with an optional prerelease suffix")
    prefix = args.prefix.resolve()
    if not (prefix / "bin").is_dir():
        die(f"--prefix is not an MSYS2 prefix (no bin/): {prefix}")
    target_dir = args.target_dir if args.target_dir.is_absolute() else ROOT / args.target_dir
    target_dir = target_dir.resolve()
    res = target_dir / "resources"
    res.mkdir(parents=True, exist_ok=True)

    if os.name != "nt":
        die("run this with MSYS2 UCRT64's native Python")
    objects = write_resources(prefix, res, args.version)
    bin_dir = cargo_build(prefix, target_dir, objects)
    for name in ("mirai.exe", "mirai-server.exe"):
        if not (bin_dir / name).is_file():
            die(f"missing {bin_dir / name}")

    base = f"mirai-{args.version}-windows-x86_64"
    dist = target_dir / "dist"
    stage = dist / base
    dist.mkdir(parents=True, exist_ok=True)
    if stage.exists():
        shutil.rmtree(stage)

    just_install(prefix, stage, bin_dir)
    for rel in JUST_FILES:
        if not (stage / rel).is_file():
            die(f"just install did not produce {rel}")
    for lang in linguas():
        catalog = stage / "share" / "locale" / lang / "LC_MESSAGES" / "mirai.mo"
        if not catalog.is_file():
            die(f"just install did not produce {catalog}")

    stage_runtime(prefix, stage, prefix_tool(prefix, "objdump"))
    assert_tree_relocatable(stage, prefix)
    strip = prefix_tool(prefix, "strip")
    for path in stage.rglob("*"):
        if path.is_file() and path.suffix.lower() in {".dll", ".exe"}:
            run([str(strip), "--strip-unneeded", str(path)])
    write_marker(stage)

    print(stage)
    ico = res / "mirai.ico"
    outfile = dist / f"mirai-{args.version}-windows-x86_64-setup.exe"
    if outfile.exists():
        outfile.unlink()
    makensis(prefix, stage, res, args.version, outfile, ico)
    print(outfile)


if __name__ == "__main__":
    main()
