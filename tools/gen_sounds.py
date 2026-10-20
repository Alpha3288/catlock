"""Synthesize the catlock sound effects (stdlib only, deterministic).

Run from the repo root: uv run --no-project python tools/gen_sounds.py
"""

import math
import os
import random
import struct
import subprocess
import tempfile
import wave
from array import array
from pathlib import Path

SR = 22050
TAU = 2 * math.pi
OUT = Path(__file__).resolve().parent.parent / "assets" / "sounds"


# ---------------------------------------------------------------- DSP helpers

def n_samples(seconds):
    return int(seconds * SR)


def noise(n, rng):
    return [rng.uniform(-1, 1) for _ in range(n)]


def biquad_coeffs(kind, freq, q):
    """RBJ cookbook coefficients, normalised by a0."""
    freq = min(max(freq, 20.0), SR * 0.45)
    w = TAU * freq / SR
    alpha = math.sin(w) / (2 * q)
    cw = math.cos(w)
    if kind == "bp":
        b0, b1, b2 = alpha, 0.0, -alpha
    elif kind == "lp":
        b0, b1, b2 = (1 - cw) / 2, 1 - cw, (1 - cw) / 2
    else:
        raise ValueError(kind)
    a0, a1, a2 = 1 + alpha, -2 * cw, 1 - alpha
    return b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0


def biquad(x, kind, freq, q=0.707):
    """Filter x; freq may be a constant or a per-sample list (for formants)."""
    y = []
    x1 = x2 = y1 = y2 = 0.0
    fixed = None if callable(freq) or isinstance(freq, list) else biquad_coeffs(kind, freq, q)
    for i, s in enumerate(x):
        c = fixed or biquad_coeffs(kind, freq[i], q)
        b0, b1, b2, a1, a2 = c
        o = b0 * s + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2
        x2, x1, y2, y1 = x1, s, y1, o
        y.append(o)
    return y


def onepole_lp(x, freq):
    a = 1 - math.exp(-TAU * freq / SR)
    y, o = [], 0.0
    for s in x:
        o += a * (s - o)
        y.append(o)
    return y


def lerp_curve(points, n):
    """Piecewise-linear curve over n samples from (position 0..1, value) points."""
    out = []
    for i in range(n):
        p = i / max(n - 1, 1)
        for (p0, v0), (p1, v1) in zip(points, points[1:]):
            if p <= p1:
                t = (p - p0) / (p1 - p0) if p1 > p0 else 1.0
                out.append(v0 + (v1 - v0) * t)
                break
        else:
            out.append(points[-1][1])
    return out


def harmonic_osc(f0, n_harm_fn, rolloff=1.0):
    """Band-limited additive oscillator driven by a per-sample f0 list.

    Additive synthesis avoids the aliasing a naive sawtooth would add when the
    pitch glides, which would otherwise read as a harsh digital buzz.
    """
    out, phase = [], 0.0
    for f in f0:
        phase += TAU * f / SR
        nh = max(1, min(n_harm_fn, int(SR * 0.45 / f)))
        out.append(sum(math.sin(h * phase) / h ** rolloff for h in range(1, nh + 1)))
    return out


def mix(*tracks):
    n = max(len(t) for t in tracks)
    return [sum(t[i] for t in tracks if i < len(t)) for i in range(n)]


def apply_env(x, env):
    return [a * b for a, b in zip(x, env)]


def fade(x, ms=8):
    """Short linear fades so the file never starts or ends on a click."""
    k = min(n_samples(ms / 1000), len(x) // 2)
    for i in range(k):
        g = i / k
        x[i] *= g
        x[-1 - i] *= g
    return x


def normalize(x, dbfs=-1.0):
    peak = max(abs(s) for s in x) or 1.0
    g = 10 ** (dbfs / 20) * 32767 / peak
    return [s * g for s in x]


def write_wav(name, x):
    x = normalize(fade(x))
    pcm = array("h", (int(round(s)) for s in x))
    with wave.open(str(OUT / name), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(pcm.tobytes())


# ------------------------------------------------------------------ the sounds

def make_warning():
    with tempfile.TemporaryDirectory() as d:
        tmp = os.path.join(d, "w.wav")
        subprocess.run(
            ["espeak-ng", "-v", "en-us", "-s", "150", "-p", "45",
             "-w", tmp, "Warning! Cat on keyboard!"],
            check=True,
        )
        with wave.open(tmp, "rb") as w:
            ch, sw, rate, frames = w.getnchannels(), w.getsampwidth(), w.getframerate(), w.readframes(w.getnframes())
    assert sw == 2 and ch == 1, (ch, sw)
    x = list(struct.unpack("<%dh" % (len(frames) // 2), frames))
    if rate != SR:
        # Linear resample; the voice is band-limited well below Nyquist.
        m = int(len(x) * SR / rate)
        x = [_interp(x, i * rate / SR) for i in range(m)]
    # Trim leading/trailing silence so the cue is punchy.
    thr = 0.02 * max(abs(s) for s in x)
    lo = next(i for i, s in enumerate(x) if abs(s) > thr)
    hi = len(x) - next(i for i, s in enumerate(reversed(x)) if abs(s) > thr)
    write_wav("warning.wav", [float(s) for s in x[max(lo - 200, 0):hi + 400]])


def _interp(x, pos):
    i = int(pos)
    j = min(i + 1, len(x) - 1)
    return x[i] + (x[j] - x[i]) * (pos - i)


def make_alarm():
    n = n_samples(0.8)
    seg = n_samples(0.1)
    sig, phase = [], 0.0
    for i in range(n):
        f = 880.0 if (i // seg) % 2 == 0 else 660.0
        phase += TAU * f / SR
        # Odd harmonics make it square-ish; the low-pass below softens the edge.
        sig.append(math.sin(phase) + math.sin(3 * phase) / 3 + math.sin(5 * phase) / 5)
    sig = onepole_lp(sig, 3500)
    # Per-segment dip avoids clicks at the tone switch.
    env = [min(1.0, min(i % seg, seg - 1 - i % seg) / (0.01 * SR)) for i in range(n)]
    write_wav("alarm.wav", apply_env(sig, env))


def make_meow():
    n = n_samples(0.7)
    rng = random.Random(1)
    # Pitch: rise then fall, with vibrato fading in late like a real meow.
    base = lerp_curve([(0, 500), (0.4, 900), (1, 450)], n)
    f0 = [b * (1 + 0.025 * math.sin(TAU * 6.5 * i / SR) * min(1, i / (0.3 * SR))) for i, b in enumerate(base)]
    src = harmonic_osc(f0, 40, rolloff=1.0)
    src = [s + 0.15 * rng.uniform(-1, 1) for s in src]  # breathiness
    # Vowel path m -> ee -> a -> u as (position, F1, F2, F3).
    path = [(0.00, 300, 1100, 2400), (0.14, 330, 1300, 2500), (0.30, 350, 2500, 3300),
            (0.55, 850, 1500, 2800), (0.80, 600, 1100, 2600), (1.00, 420, 900, 2500)]
    f1 = lerp_curve([(p, a) for p, a, _, _ in path], n)
    f2 = lerp_curve([(p, b) for p, _, b, _ in path], n)
    f3 = lerp_curve([(p, c) for p, _, _, c in path], n)
    voiced = mix(
        biquad(src, "bp", f1, 6),
        [0.8 * s for s in biquad(src, "bp", f2, 8)],
        [0.4 * s for s in biquad(src, "bp", f3, 10)],
    )
    # Nasal "m" start is quieter, then swell, long decay on the "ow".
    env = lerp_curve([(0, 0), (0.05, 0.35), (0.25, 1.0), (0.6, 0.85), (0.9, 0.35), (1, 0)], n)
    write_wav("meow.wav", apply_env(voiced, env))


def make_bark():
    rng = random.Random(2)

    def one(dur, f_start, f_end):
        n = n_samples(dur)
        f0 = lerp_curve([(0, f_start), (0.25, f_start * 0.7), (1, f_end)], n)
        buzz = harmonic_osc(f0, 30, rolloff=0.8)
        hiss = biquad(noise(n, rng), "bp", 1800, 0.8)
        src = mix(buzz, [0.9 * s for s in hiss])
        # Throat-like formants, F1 near 700 and F2 near 1400.
        voiced = mix(biquad(src, "bp", 700, 3), [0.7 * s for s in biquad(src, "bp", 1400, 3)])
        env = [min(1.0, i / (0.004 * SR)) * math.exp(-5.5 * i / n) for i in range(n)]
        return apply_env(voiced, env)

    a, b = one(0.3, 400, 200), one(0.3, 380, 190)
    gap = [0.0] * n_samples(0.1)
    write_wav("bark.wav", a + gap + b)


def make_vacuum():
    n = n_samples(1.2)
    rng = random.Random(3)
    wob = [150 * (1 + 0.04 * math.sin(TAU * 3.0 * i / SR)) for i in range(n)]
    motor = harmonic_osc(wob, 8, rolloff=1.2)
    whine = biquad(noise(n, rng), "bp", 2800, 1.5)
    whine = [s * (1 + 0.2 * math.sin(TAU * 7.0 * i / SR)) for i, s in enumerate(whine)]
    sig = mix([0.5 * s for s in motor], [1.4 * s for s in whine])
    env = lerp_curve([(0, 0), (0.2, 1), (0.8, 1), (1, 0)], n)
    write_wav("vacuum.wav", apply_env(sig, env))


def make_bubbles():
    n = n_samples(0.8)
    rng = random.Random(4)
    out = [0.0] * n
    for _ in range(5):
        start = rng.randint(0, n - n_samples(0.15))
        dur = n_samples(rng.uniform(0.05, 0.1))
        f = rng.uniform(400, 900)
        phase = 0.0
        for i in range(dur):
            t = i / dur
            # Rising sweep mimics the shrinking bubble cavity.
            phase += TAU * f * (1 + 2.5 * t) / SR
            out[start + i] += math.sin(phase) * math.exp(-4 * t) * min(1.0, i / 40)
    write_wav("bubbles.wav", out)


def make_purr():
    n = n_samples(1.5)
    rng = random.Random(5)
    rumble = onepole_lp(noise(n, rng), 250)
    buzz = onepole_lp(harmonic_osc([50.0] * n, 10, rolloff=1.0), 600)
    sig = mix([4 * s for s in rumble], [0.3 * s for s in buzz])
    # Sharpened 25 Hz pulses sound like the purr's rattle rather than a tremolo.
    am = [(0.5 + 0.5 * math.sin(TAU * 25 * i / SR)) ** 1.5 for i in range(n)]
    # Two breaths: swell on inhale, swell on exhale.
    breath = lerp_curve([(0, 0.2), (0.25, 1), (0.5, 0.45), (0.75, 1), (1, 0.2)], n)
    write_wav("purr.wav", apply_env(apply_env(sig, am), breath))


def make_lofi():
    n = n_samples(1.5)
    rng = random.Random(6)
    hz = lambda semi: 261.63 * 2 ** (semi / 12)  # noqa: E731
    chords = [(0.0, [0, 4, 7, 11]), (0.7, [2, 5, 9, 12])]  # Cmaj7 then Dm7
    out = [0.0] * n
    for t0, notes in chords:
        for k, semi in enumerate(notes):
            f = hz(semi)
            start = n_samples(t0 + k * 0.012)  # slight strum
            for i in range(n - start):
                t = i / SR
                # Fundamental + a quick bell partial give a Rhodes-style tine.
                tone = (math.sin(TAU * f * t) * math.exp(-2.5 * t)
                        + 0.3 * math.sin(TAU * f * 2 * t) * math.exp(-4 * t)
                        + 0.15 * math.sin(TAU * f * 7 * t) * math.exp(-18 * t))
                out[start + i] += tone * min(1.0, i / (0.02 * SR))
    out = onepole_lp(out, 2500)
    # Sparse random clicks, high-passed by subtracting a low-passed copy.
    clicks = [rng.uniform(-1, 1) * 0.25 if rng.random() < 0.0015 else 0.0 for _ in range(n)]
    clicks = [a - b for a, b in zip(clicks, onepole_lp(clicks, 1500))]
    dust = [0.01 * rng.uniform(-1, 1) for _ in range(n)]
    sig = mix(out, [m * 0.5 for m in clicks], dust)
    env = lerp_curve([(0, 1), (0.85, 1), (1, 0)], n)
    write_wav("lofi.wav", apply_env(sig, env))


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for fn in (make_warning, make_alarm, make_meow, make_bark, make_vacuum,
               make_bubbles, make_purr, make_lofi):
        fn()
    total = 0
    print("%-12s %5s %3s %3s %8s %8s" % ("file", "rate", "ch", "sw", "dur(s)", "bytes"))
    for p in sorted(OUT.glob("*.wav")):
        with wave.open(str(p), "rb") as w:
            dur = w.getnframes() / w.getframerate()
            print("%-12s %5d %3d %3d %8.3f %8d" % (p.name, w.getframerate(), w.getnchannels(),
                                                   w.getsampwidth(), dur, p.stat().st_size))
        total += p.stat().st_size
    print("total bytes:", total)


if __name__ == "__main__":
    main()
