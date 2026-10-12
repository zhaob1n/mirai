#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""Capture the installed release, which deliberately has no debug harness.

Run with native UCRT64 Python: windows-gui-smoke.py EXE OUTPUT.png [GAME.sgf].
With a record, Home followed by four Right keys exercises actual move navigation
and stone playback. This does not edit the record. Audio output still needs a
real device and an external recording; a screenshot cannot establish that.
"""

import ctypes
from ctypes import wintypes as w
from pathlib import Path
import struct
import subprocess
import sys
import time
import zlib


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def png(width, height, pixels):
    rows = bytearray()
    for y in range(height - 1, -1, -1):
        rows.append(0)  # PNG's unfiltered RGB scanline; GDI supplies bottom-up BGRX.
        row = pixels[y * width * 4:(y + 1) * width * 4]
        for x in range(0, len(row), 4):
            rows.extend((row[x + 2], row[x + 1], row[x]))
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def main():
    if len(sys.argv) not in (3, 4):
        raise SystemExit("usage: windows-gui-smoke.py EXE OUTPUT.png [GAME.sgf]")
    user = ctypes.WinDLL("user32", use_last_error=True)
    gdi = ctypes.WinDLL("gdi32", use_last_error=True)
    callback_type = ctypes.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
    user.EnumWindows.argtypes = [callback_type, w.LPARAM]
    user.IsWindowVisible.argtypes = [w.HWND]
    user.GetWindowThreadProcessId.argtypes = [w.HWND, ctypes.POINTER(w.DWORD)]
    user.GetWindowRect.argtypes = [w.HWND, ctypes.POINTER(w.RECT)]
    user.GetWindowDC.argtypes = [w.HWND]
    user.GetWindowDC.restype = w.HDC
    user.PrintWindow.argtypes = [w.HWND, w.HDC, w.UINT]
    user.PostMessageW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM]
    user.ReleaseDC.argtypes = [w.HWND, w.HDC]
    gdi.CreateCompatibleDC.argtypes = [w.HDC]
    gdi.CreateCompatibleDC.restype = w.HDC
    gdi.CreateCompatibleBitmap.argtypes = [w.HDC, ctypes.c_int, ctypes.c_int]
    gdi.CreateCompatibleBitmap.restype = w.HBITMAP
    gdi.SelectObject.argtypes = [w.HDC, w.HANDLE]
    gdi.SelectObject.restype = w.HANDLE
    gdi.GetDIBits.argtypes = [w.HDC, w.HBITMAP, w.UINT, w.UINT, w.LPVOID, w.LPVOID, w.UINT]
    gdi.DeleteObject.argtypes = [w.HANDLE]
    gdi.DeleteDC.argtypes = [w.HDC]

    exe, output = Path(sys.argv[1]), Path(sys.argv[2])
    with output.with_suffix(".log").open("wb") as log:
        process = subprocess.Popen([str(exe), *sys.argv[3:]], stdout=log, stderr=subprocess.STDOUT)
        try:
            windows = []

            @callback_type
            def visit(hwnd, _):
                pid = w.DWORD()
                user.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
                if pid.value == process.pid and user.IsWindowVisible(hwnd):
                    windows.append(hwnd)
                return True

            deadline = time.monotonic() + 30
            while not windows and time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError(f"Release exited before presenting a window: {process.returncode}")
                user.EnumWindows(visit, 0)
                time.sleep(0.1)
            if not windows:
                raise RuntimeError("No visible release window")
            window = windows[0]
            time.sleep(3)
            if len(sys.argv) == 4:
                # Posted to the window, not synthesized globally: a hosted runner's desktop
                # often refuses SetForegroundWindow, and then keybd_event goes elsewhere.
                for key, scan in [(0x24, 0x47), (0x27, 0x4D), (0x27, 0x4D), (0x27, 0x4D), (0x27, 0x4D)]:
                    extended = 1 | scan << 16 | 1 << 24  # Home, Right × 4
                    user.PostMessageW(window, 0x0100, key, extended)  # WM_KEYDOWN
                    user.PostMessageW(window, 0x0101, key, extended | 0xC0000000)  # WM_KEYUP
                    time.sleep(0.8)
            rect = w.RECT()
            if not user.GetWindowRect(window, ctypes.byref(rect)):
                raise ctypes.WinError(ctypes.get_last_error())
            width, height = rect.right - rect.left, rect.bottom - rect.top
            dc = user.GetWindowDC(window)
            mem = gdi.CreateCompatibleDC(dc)
            bitmap = gdi.CreateCompatibleBitmap(dc, width, height)
            old = gdi.SelectObject(mem, bitmap)
            try:
                if not user.PrintWindow(window, mem, 2):
                    raise ctypes.WinError(ctypes.get_last_error())
                gdi.SelectObject(mem, old)
                header = struct.pack("<IiiHHIIiiII", 40, width, height, 1, 32, 0,
                                     width * height * 4, 0, 0, 0, 0)
                info = ctypes.create_string_buffer(header)
                pixels = ctypes.create_string_buffer(width * height * 4)
                if gdi.GetDIBits(mem, bitmap, 0, height, pixels, info, 0) != height:
                    raise ctypes.WinError(ctypes.get_last_error())
                output.write_bytes(png(width, height, pixels.raw))
            finally:
                gdi.SelectObject(mem, old)
                gdi.DeleteObject(bitmap)
                gdi.DeleteDC(mem)
                user.ReleaseDC(window, dc)
            print(f"release-smoke: rendered {output} ({width} × {height})", flush=True)
            user.PostMessageW(window, 0x0010, 0, 0)  # Normal window-close path.
            process.wait(timeout=30)
            if process.returncode != 0:
                raise RuntimeError(f"Release exited {process.returncode}")
            print("release-smoke: closed normally", flush=True)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()


if __name__ == "__main__":
    main()
