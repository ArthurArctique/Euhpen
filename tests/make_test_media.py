"""Génère des médias de test (« mire de synchro ») avec ffmpeg.

Le son alterne des passages de « parole » (une tonalité modulée) et des silences de
durées connues, répétés toutes les 12 secondes. L'image passe au blanc pendant une
image exactement quand un passage sonore démarre : après import dans le logiciel de
montage, le flash et le début de la forme d'onde doivent tomber sur la même image.

    python tests/make_test_media.py DOSSIER            # jeu complet, 36 s par fichier
    python tests/make_test_media.py DOSSIER --long     # ajoute une version d'1 h (29,97)
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from fractions import Fraction

PERIOD = 12.0
# Passages sonores (début, fin) dans chaque période de 12 s. Silences entre eux :
# 0–1 s (1,0 s), 3,0–3,3 (0,3 s, trop court : gardé), 5,0–6,0 (1,0 s), 6,5–8,5 (2,0 s),
# 11,0–13,0 (2,0 s, à cheval sur la période suivante).
BURSTS: tuple[tuple[float, float], ...] = ((1.0, 3.0), (3.3, 5.0), (6.0, 6.5), (8.5, 11.0))


def bursts_until(duration: float) -> list[tuple[float, float]]:
    """Tous les passages sonores d'un fichier de cette durée."""
    result = []
    k = 0
    while k * PERIOD < duration:
        for a, b in BURSTS:
            start, end = k * PERIOD + a, k * PERIOD + b
            if start < duration:
                result.append((start, min(end, duration)))
        k += 1
    return result


def _sound_expr() -> str:
    gate = "+".join(f"between(mod(t,{PERIOD}),{a},{b})" for a, b in BURSTS)
    voice = "0.5*sin(2*PI*220*t)*(0.65+0.35*sin(2*PI*3*t))"
    noise = "0.002*(random(0)-0.5)"
    return f"({gate})*{voice}+{noise}"


def _flash_expr(fps: Fraction) -> str:
    frame = float(1 / fps)
    eps = 1e-6
    terms = [
        f"between(mod(t,{PERIOD}),{a - eps},{a - eps + frame * 0.999})" for a, _ in BURSTS
    ]
    return "+".join(terms)


def make_media(
    path: str,
    *,
    fps: str = "25",
    duration: float = 36.0,
    video: bool = True,
    audio: str = "mono",  # mono | stereo | quad | two-streams
    timecode: str | None = None,
    vfr: bool = False,
    size: str = "320x180",
    bwf_seconds: float | None = None,
    ffmpeg: str | None = None,
) -> str:
    ffmpeg = ffmpeg or shutil.which("ffmpeg") or "ffmpeg"
    rate = Fraction(fps) if "/" not in fps else Fraction(*map(int, fps.split("/")))
    ext = os.path.splitext(path)[1].lower()
    cmd = [ffmpeg, "-nostdin", "-hide_banner", "-v", "error", "-y"]
    maps: list[str] = []
    inputs = 0
    if video and ext != ".wav":
        chain = (
            f"testsrc2=size={size}:rate={fps}:duration={duration},"
            f"drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='{_flash_expr(rate)}'"
        )
        if vfr:
            chain += ",select='not(eq(mod(n,7),3))'"
        cmd += ["-f", "lavfi", "-i", chain]
        maps += ["-map", f"{inputs}:v"]
        inputs += 1
    cmd += ["-f", "lavfi", "-i", f"aevalsrc=exprs='{_sound_expr()}':s=48000:d={duration}"]
    maps += ["-map", f"{inputs}:a"]
    inputs += 1
    if audio == "two-streams":
        cmd += ["-f", "lavfi", "-i", f"anoisesrc=a=0.001:r=48000:d={duration}"]
        maps += ["-map", f"{inputs}:a"]
        inputs += 1
    cmd += maps
    if audio == "stereo":
        cmd += ["-filter:a:0", "pan=stereo|c0=c0|c1=c0"]
    elif audio == "quad":
        cmd += ["-filter:a:0", "pan=4.0|c0=c0|c1=0*c0|c2=0*c0|c3=0*c0"]

    if ext == ".wav":
        cmd += ["-c:a", "pcm_s16le"]
        if bwf_seconds is not None:
            cmd += ["-write_bext", "1", "-metadata", f"time_reference={int(bwf_seconds * 48000)}"]
    else:
        cmd += ["-c:v", "libx264", "-preset", "ultrafast", "-crf", "28", "-pix_fmt", "yuv420p"]
        if vfr:
            cmd += ["-fps_mode", "passthrough"]
        cmd += ["-c:a", "aac" if ext == ".mp4" else "pcm_s16le"]
        if timecode:
            cmd += ["-timecode", timecode]
    cmd.append(path)
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    subprocess.run(cmd, check=True)
    return path


SUITE = [
    # nom, options
    ("23976 mono.mov", dict(fps="24000/1001")),
    ("25 mono.mov", dict(fps="25")),
    ("2997 stéréo.mov", dict(fps="30000/1001", audio="stereo")),
    ("50 quatre canaux.mov", dict(fps="50", audio="quad")),
    ("5994 mono.mov", dict(fps="60000/1001")),
    ("25 timecode 10h.mov", dict(fps="25", timecode="10:00:00:00")),
    ("2997 timecode DF.mov", dict(fps="30000/1001", timecode="01:00:00;00")),
    ("Prise été n°1 — accents & espaces.mov", dict(fps="25")),
    ("obs deux pistes.mov", dict(fps="30", audio="two-streams")),
    ("téléphone VFR.mp4", dict(fps="30", vfr=True)),
    ("micro BWF 10h.wav", dict(video=False, bwf_seconds=36000.0)),
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("folder")
    parser.add_argument("--long", action="store_true", help="ajoute un fichier d'1 h à 29,97 i/s")
    args = parser.parse_args()
    os.makedirs(args.folder, exist_ok=True)
    for name, options in SUITE:
        print(f"→ {name}")
        make_media(os.path.join(args.folder, name), **options)
    if args.long:
        print("→ 2997 une heure.mov (quelques minutes)")
        make_media(os.path.join(args.folder, "2997 une heure.mov"), fps="30000/1001", duration=3600.0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
