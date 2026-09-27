#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Loudness and brightness of the rendered stone-sound clips, for tuning crates/mirai/src/sound.rs.
#
#   cargo test -p mirai render_clips -- --ignored && tools/ui/sound-levels.py [/tmp/mirai-sounds]
#
# Per clip: peak (must stay under 1.0), then for every lid drop its loudness against the
# placement strike that opens the clip. Loudness is K-weighted (ITU-R BS.1770 shape) over
# 30 ms: peak says nothing about how loud a 5 ms drop sounds. Aim the first drop 1.5-4 dB
# under the placement. "centroid" is the drops' spectral centre: lower is warmer, higher is
# brighter and more brittle.
import sys
import wave
from pathlib import Path

import numpy as np

RATE = 48000


def kweight(x):
    spectrum = np.fft.rfft(x)
    f = np.fft.rfftfreq(len(x), 1 / RATE)
    shelf = 10 ** (4 / 20 / (1 + (1500 / np.maximum(f, 1)) ** 2))
    highpass = f**2 / (f**2 + 60**2)
    return np.fft.irfft(spectrum * shelf * highpass, len(x))


def main():
    folder = Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/mirai-sounds")
    for path in sorted(folder.glob("clip*.wav")):
        with wave.open(str(path)) as w:
            x = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2") / 32767
        k = kweight(x)
        hop = RATE // 200
        peaks = [np.abs(x[j : j + hop]).max() for j in range(0, len(x) - hop, hop)]
        drops = [
            j
            for j in range(1, len(peaks))
            if j * hop > 0.2 * RATE and peaks[j] > 0.03 and peaks[j] > 1.5 * peaks[j - 1]
        ]

        def loudness(j):
            return 10 * np.log10((k[j * hop : j * hop + RATE * 3 // 100] ** 2).mean())

        line = f"{path.name}  peak {np.abs(x).max():.2f}"
        if drops:
            tail = x[int(0.205 * RATE) :]
            power = np.abs(np.fft.rfft(tail)) ** 2
            f = np.fft.rfftfreq(len(tail), 1 / RATE)
            line += f"  centroid {(power * f).sum() / power.sum():4.0f} Hz  drops dB:"
            line += " ".join(f" {loudness(j) - loudness(0):+.1f}" for j in drops)
        print(line)


main()
