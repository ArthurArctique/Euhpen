"""Corpus annotés → étiquettes par trame de 20 ms.

Rhapsodie (https://rhapsodie.modyco.fr, CC BY-NC-SA 3.0 FR) est aligné à la main au phonème et
marque par ``H`` les syllabes disfluentes : « euh », allongements excessifs, répétitions,
amorces. Ce marquage ne dit pas lequel : on le déduit ainsi.

- ``hesitation`` : les mots « euh », « heu », « hum »…
- ``lengthening`` : dans une syllabe ``H``, l'excès du phonème le plus long quand il dure au moins
  deux fois la médiane du fichier pour cette classe de sons (voyelle ou continue) et au moins
  180 ms. Seul l'excès est étiqueté : le début du son reste de la parole, car c'est ce qu'il faut
  garder au montage. Ces valeurs sont fixées a priori.
- ``silence`` et ``speech`` : d'après le phonème.
- ignoré (``IGNORE``) : hors annotation, bruits (``%``), syllabes douteuses (``?``), « mh » de
  l'interlocuteur et tours où deux locuteurs se chevauchent.
"""

from __future__ import annotations

import hashlib
import re
import statistics
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np

from ..intervals import Interval, union

HOP = 0.02  # une trame de l'encodeur de Whisper toutes les 20 ms
LABELS = ("speech", "hesitation", "lengthening", "silence")
SPEECH, HESITATION, LENGTHENING, SILENCE = range(4)
IGNORE = -1

VOWELS = {"a", "e", "E", "i", "o", "O", "u", "y", "2", "9", "@", "a~", "e~", "o~", "9~"}
CONTINUANTS = {"m", "n", "l", "R", "s", "z", "f", "v", "S", "Z", "j", "w", "H", "N"}
FILLERS = {"euh", "heu", "euhm", "hum", "hm"}
BACKCHANNELS = {"mh", "mhm", "mm"}

LENGTHENING_RATIO = 2.0
LENGTHENING_MIN = 0.18
LENGTHENING_KEEP_MIN = 0.08
LENGTHENING_MIN_EXCESS = 0.06
_NON_LETTER = re.compile(r"[^\w]")


def read_textgrid(path: str | Path) -> dict[str, list[tuple[float, float, str]]]:
    """Lit un TextGrid au format long : {nom de la tire: [(début, fin, texte)]}."""
    raw = Path(path).read_bytes()
    encodings = ("utf-16", "utf-8", "latin-1") if raw[:2] in (b"\xff\xfe", b"\xfe\xff") else ("utf-8", "latin-1")
    text = ""
    for encoding in encodings:
        try:
            text = raw.decode(encoding)
            break
        except UnicodeDecodeError:
            continue
    tiers: dict[str, list[tuple[float, float, str]]] = {}
    for block in re.split(r"item \[\d+\]:", text)[1:]:
        if 'class = "IntervalTier"' not in block:
            continue
        name = re.search(r'name = "(.*?)"', block).group(1)
        pattern = r'xmin = ([\d.e-]+)\s*xmax = ([\d.e-]+)\s*text = "(.*?)"\s*(?=intervals|$)'
        tiers[name] = [(float(a), float(b), t) for a, b, t in re.findall(pattern, block, re.S)]
    return tiers


@dataclass
class LabelledSample:
    id: str
    audio: str
    labels: np.ndarray  # une étiquette par trame, IGNORE compris
    events: dict[str, list[Interval]] = field(default_factory=dict)  # référence pour l'évaluation

    def counts(self) -> dict[str, int]:
        return {name: int((self.labels == k).sum()) for k, name in enumerate(LABELS)}


def _frame(t: float) -> int:
    return int(round(t / HOP))


def _bare(word: str) -> str:
    return _NON_LETTER.sub("", word.lower())


def _medians(phones, syllables, hes) -> tuple[float, float]:
    """Durées médianes des voyelles et des continues hors syllabes disfluentes (rythme du fichier)."""
    flagged = [(a, b) for (a, b, _), (_, _, h) in zip(syllables, hes) if h.strip() in ("H", "?")]
    vowels, continuants = [], []
    k = 0
    for a, b, p in phones:
        while k < len(flagged) and flagged[k][1] <= a:
            k += 1
        if k < len(flagged) and flagged[k][0] <= a < flagged[k][1]:
            continue
        if p in VOWELS:
            vowels.append(b - a)
        elif p in CONTINUANTS:
            continuants.append(b - a)
    return (statistics.median(vowels) if vowels else 0.09, statistics.median(continuants) if continuants else 0.07)


def lengthening_excess(phones_in_syllable, vowel_median: float, continuant_median: float) -> Interval | None:
    """Partie excédentaire du phonème le plus long de la syllabe, s'il est anormalement long."""
    best = None
    for a, b, p in phones_in_syllable:
        if p not in VOWELS and p not in CONTINUANTS:
            continue
        if best is None or b - a > best[1] - best[0]:
            best = (a, b, p)
    if best is None:
        return None
    a, b, p = best
    median = vowel_median if p in VOWELS else continuant_median
    if b - a < max(LENGTHENING_MIN, LENGTHENING_RATIO * median):
        return None
    start = a + max(LENGTHENING_KEEP_MIN, median)
    return (start, b) if b - start >= LENGTHENING_MIN_EXCESS else None


def label_rhapsodie(textgrid: str | Path, audio: str | Path, sample_id: str | None = None) -> LabelledSample:
    tiers = read_textgrid(textgrid)
    phones = tiers["phone"]
    syllables, hes = tiers["syllabe"], tiers["hes"]
    words = tiers.get("word") or tiers.get("words") or []
    end = max(b for _, b, _ in phones)
    labels = np.full(_frame(end) + 1, IGNORE, dtype=np.int8)

    def paint(a: float, b: float, value: int) -> None:
        labels[max(0, _frame(a)) : max(0, _frame(b))] = value

    for a, b, p in phones:
        p = p.strip()
        if p == "_":
            paint(a, b, SILENCE)
        elif p and p != "%":
            paint(a, b, SPEECH)
    for a, b, p in phones:
        if p.strip() == "%":
            paint(a, b, IGNORE)
    for (a, b, _), (_, _, h) in zip(syllables, hes):
        if h.strip() == "?":
            paint(a, b, IGNORE)

    fillers: list[Interval] = []
    for a, b, w in words:
        bare = _bare(w)
        if bare in FILLERS:
            fillers.append((a, b))
            paint(a, b, HESITATION)
        elif bare in BACKCHANNELS:
            paint(a, b, IGNORE)

    vowel_median, continuant_median = _medians(phones, syllables, hes)
    excess: list[Interval] = []
    k = 0
    for (a, b, _), (_, _, h) in zip(syllables, hes):
        while k < len(phones) and phones[k][1] <= a + 1e-6:
            k += 1
        if h.strip() != "H" or any(x < b and a < y for x, y in fillers):
            continue
        inside = []
        j = k
        while j < len(phones) and phones[j][0] < b - 1e-6:
            inside.append(phones[j])
            j += 1
        span = lengthening_excess(inside, vowel_median, continuant_median)
        if span:
            excess.append(span)
            paint(span[0], span[1], LENGTHENING)

    # Deux locuteurs à la fois : ni la parole ni l'hésitation ne sont fiables.
    for a, b, who in tiers.get("locuteur", []):
        if "-" in who:
            paint(a, b, IGNORE)

    def usable(spans: list[Interval]) -> list[Interval]:
        kept = []
        for a, b in union(spans):
            frames = labels[_frame(a) : _frame(b)]
            if frames.size and (frames != IGNORE).mean() >= 0.5:
                kept.append((a, b))
        return kept

    return LabelledSample(
        id=sample_id or Path(audio).stem,
        audio=str(audio),
        labels=labels,
        events={"hesitation": usable(fillers), "lengthening": usable(excess)},
    )


def rhapsodie_samples(root: str | Path) -> list[LabelledSample]:
    """Associe chaque TextGrid à son fichier audio, sous ``root`` (cherche récursivement)."""
    root = Path(root)
    audio = {p.stem: p for p in root.rglob("*.wav")}
    samples = []
    for textgrid in sorted(root.rglob("*-Pro.TextGrid")):
        stem = textgrid.name.removesuffix("-Pro.TextGrid")
        if stem in audio:
            samples.append(label_rhapsodie(textgrid, audio[stem], stem))
    return samples


# Enregistrements partageant un locuteur d'après les métadonnées IMDI du corpus (acteurs nommés).
SPEAKER_GROUPS = (
    {"Rhap-D0001", "Rhap-D0002", "Rhap-D0004", "Rhap-D0006"},  # Sonia Branca, §LF1
    {"Rhap-D0007", "Rhap-D0008", "Rhap-D0020", "Rhap-M0010"},  # §LF9
    {"Rhap-D2001", "Rhap-D2009"},  # J. Chancel
    {"Rhap-D2012", "Rhap-M0002"},  # §LF23
)


def split_by_hash(ids: list[str], validation: int = 8, test: int = 8) -> dict[str, list[str]]:
    """Découpage déterministe, sans locuteur commun connu entre les trois parties."""
    group_of = {i: min(group) for group in SPEAKER_GROUPS for i in group}
    groups: dict[str, list[str]] = {}
    for i in sorted(ids):
        groups.setdefault(group_of.get(i, i), []).append(i)
    ordered = sorted(groups, key=lambda g: hashlib.sha256(f"euhpen-finetune:{g}".encode()).hexdigest())
    split: dict[str, list[str]] = {"test": [], "validation": [], "train": []}
    for key in ordered:
        if len(split["test"]) < test:
            split["test"] += groups[key]
        elif len(split["validation"]) < validation:
            split["validation"] += groups[key]
        else:
            split["train"] += groups[key]
    return split
