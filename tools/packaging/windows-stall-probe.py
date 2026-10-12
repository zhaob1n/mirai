#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""Step a record in a release and report every GUI-thread stall, and how long close takes.

Run with native UCRT64 Python:

    windows-stall-probe.py EXE GAME.sgf [STEPS [GAP [TAIL]]]

Posts Home, then STEPS Right keys (default 4) GAP seconds apart (default 0.8), waits TAIL
seconds (default 2) and closes the window. Between keys a WM_NULL is sent; one that takes
over 100 ms to answer prints as a STALL. A screenshot cannot show a stall: a sent message
is served ahead of posted keys, so a stuck window is captured before its moves.
"""

import ctypes
from ctypes import wintypes as w
import subprocess
import sys
import time

WM_NULL, WM_CLOSE, WM_KEYDOWN, WM_KEYUP = 0x0000, 0x0010, 0x0100, 0x0101
SMTO_ABORTIFHUNG = 0x0002


def main():
    if not 3 <= len(sys.argv) <= 6:
        raise SystemExit(__doc__)
    exe, sgf = sys.argv[1], sys.argv[2]
    steps = int(sys.argv[3]) if len(sys.argv) > 3 else 4
    gap = float(sys.argv[4]) if len(sys.argv) > 4 else 0.8
    tail = float(sys.argv[5]) if len(sys.argv) > 5 else 2.0

    user = ctypes.WinDLL("user32", use_last_error=True)
    callback_type = ctypes.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
    user.EnumWindows.argtypes = [callback_type, w.LPARAM]
    user.IsWindowVisible.argtypes = [w.HWND]
    user.GetWindowThreadProcessId.argtypes = [w.HWND, ctypes.POINTER(w.DWORD)]
    user.PostMessageW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM]
    user.SendMessageTimeoutW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM, w.UINT, w.UINT,
                                         ctypes.POINTER(ctypes.c_size_t)]

    start = time.monotonic()

    def log(message):
        print(f"{time.monotonic() - start:7.2f}s {message}", flush=True)

    process = subprocess.Popen([exe, sgf])
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
        while not windows:
            if process.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError("no visible release window")
            user.EnumWindows(visit, 0)
            time.sleep(0.05)
        window = windows[0]
        log("window")

        def watch(seconds):
            end = time.monotonic() + seconds
            while time.monotonic() < end and process.poll() is None:
                result = ctypes.c_size_t()
                sent = time.monotonic()
                answered = user.SendMessageTimeoutW(window, WM_NULL, 0, 0, SMTO_ABORTIFHUNG,
                                                    60000, ctypes.byref(result))
                took = time.monotonic() - sent
                if took > 0.1 or not answered:
                    log(f"STALL {took:.2f}s{'' if answered else ' (no answer)'}")
                time.sleep(0.05)

        def key(vk, scan, name):
            param = 1 | scan << 16 | 1 << 24
            user.PostMessageW(window, WM_KEYDOWN, vk, param)
            user.PostMessageW(window, WM_KEYUP, vk, param | 0xC0000000)
            log(name)

        watch(3)
        key(0x24, 0x47, "Home")
        watch(gap)
        for step in range(steps):
            key(0x27, 0x4D, f"Right {step + 1}")
            watch(gap)
        watch(tail)
        log("WM_CLOSE")
        user.PostMessageW(window, WM_CLOSE, 0, 0)
        process.wait(timeout=90)
        log(f"exited {process.returncode}")
    finally:
        if process.poll() is None:
            log("still running; killed")
            process.kill()
            process.wait()


if __name__ == "__main__":
    main()
