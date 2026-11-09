"""Des probabilités par trame aux intervalles de coupe, et leur insertion dans la transcription.

Ce module n'utilise que numpy. La sortie passe par la transcription : :func:`insert_spans` ajoute
des jetons ``[euh]`` et ``[étirement]`` que :func:`euhpen.decision.decide` traite comme les ``[UH]`` du
modèle, avec les mêmes marges et les mêmes bords recalés. Les coupes ressortent donc en segments
retirés de type ``hesitation``, sans changer la décision.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass

import numpy as np

from ..intervals import Interval
from ..transcribe import HESITATION, WORD, WORD_TAIL, Transcript, Word
from .corpus import HESITATION as H_CLASS
from .corpus import HOP
from .corpus import LENGTHENING as L_CLASS

TOKENS = {"hesitation": "[euh]", "lengthening": "[étirement]"}  # jeton écrit, selon la classe détectée
TAIL = WORD_TAIL
MIN_PIECE = 0.03  # un morceau de mot plus court ne vaut pas d'être gardé


@dataclass
class Span:
    start: float
    end: float
    kind: str  # "hesitation" | "lengthening"
    probability: float


@dataclass
class DetectionParams:
    hesitation_threshold: float = 0.5
    lengthening_threshold: float = 0.5
    median_frames: int = 5
    merge_gap: float = 0.06
    min_hesitation: float = 0.12
    min_lengthening: float = 0.08

    def to_dict(self) -> dict:
        return asdict(self)


def median_filter(values: np.ndarray, width: int) -> np.ndarray:
    """Médiane glissante, de même longueur que l'entrée."""
    if width <= 1 or values.size == 0:
        return values
    half = width // 2
    padded = np.pad(values, half, mode="edge")
    return np.median(np.lib.stride_tricks.sliding_window_view(padded, width), axis=1)


def runs(mask: np.ndarray) -> list[tuple[int, int]]:
    edges = np.diff(np.concatenate(([0], mask.astype(np.int8), [0])))
    return list(zip(np.flatnonzero(edges == 1).tolist(), np.flatnonzero(edges == -1).tolist()))


def _spans_of(mask, score, kind, minimum, merge_gap, hop) -> list[Span]:
    merged: list[list[int]] = []
    for a, b in runs(mask):
        if merged and (a - merged[-1][1]) * hop <= merge_gap:
            merged[-1][1] = b
        else:
            merged.append([a, b])
    return [
        Span(a * hop, b * hop, kind, float(score[a:b].mean()))
        for a, b in merged
        if (b - a) * hop >= minimum - 1e-9
    ]


def frames_to_spans(probabilities: np.ndarray, params: DetectionParams, hop: float = HOP) -> list[Span]:
    """Probabilités de forme (trames, 4) → intervalles, triés par début."""
    hesitation = median_filter(probabilities[:, H_CLASS], params.median_frames)
    lengthening = median_filter(probabilities[:, L_CLASS], params.median_frames)
    is_hesitation = (hesitation >= params.hesitation_threshold) & (hesitation >= lengthening)
    is_lengthening = (lengthening >= params.lengthening_threshold) & (lengthening > hesitation)
    spans = _spans_of(is_hesitation, hesitation, "hesitation", params.min_hesitation, params.merge_gap, hop)
    spans += _spans_of(is_lengthening, lengthening, "lengthening", params.min_lengthening, params.merge_gap, hop)
    return sorted(spans, key=lambda s: s.start)


def overlap(a: Interval, b: Interval) -> float:
    return max(0.0, min(a[1], b[1]) - max(a[0], b[0]))


def insert_spans(transcript: Transcript, spans: list[Span]) -> Transcript:
    """Ajoute les détections à la transcription, en coupant les mots où elles tombent.

    Une détection déjà couverte à moitié par un jeton du modèle est ignorée. Un mot entièrement
    recouvert disparaît au profit du jeton : le modèle l'a souvent inventé à la place d'un « euh ».
    """
    events = [(w.start, w.end) for w in transcript.words if w.kind != WORD]
    accepted = [
        s for s in spans
        if sum(overlap((s.start, s.end), e) for e in events) < 0.5 * (s.end - s.start)
    ]
    words = list(transcript.words)
    tokens: list[Word] = []
    for span in accepted:
        pieces: list[Word] = []
        for w in words:
            if w.kind != WORD or w.end <= span.start or w.start >= span.end:
                pieces.append(w)
                continue
            head = span.start - w.start
            tail = w.end - span.end
            if head >= MIN_PIECE:
                pieces.append(Word(w.text, w.start, span.start, w.probability, WORD))
            if tail >= MIN_PIECE:
                text = TAIL if head >= MIN_PIECE else w.text
                pieces.append(Word(text, span.end, w.end, w.probability, WORD))
        words = pieces
        # Pas de probabilité : le seuil de la tête est déjà appliqué, celui de Whisper n'a pas ce sens.
        tokens.append(Word(TOKENS[span.kind], span.start, span.end, None, HESITATION))
    merged = sorted(words + tokens, key=lambda w: (w.start, w.end))
    return Transcript(
        model=f"{transcript.model}+head",
        revision=transcript.revision,
        language=transcript.language,
        words=merged,
        device=transcript.device,
        processing_time=transcript.processing_time,
        created=transcript.created,
    )


# --- évaluation ----------------------------------------------------------------------------


@dataclass
class Score:
    true_positive: int
    false_positive: int
    false_negative: int

    @property
    def precision(self) -> float:
        total = self.true_positive + self.false_positive
        return self.true_positive / total if total else 0.0

    @property
    def recall(self) -> float:
        total = self.true_positive + self.false_negative
        return self.true_positive / total if total else 0.0

    @property
    def f1(self) -> float:
        p, r = self.precision, self.recall
        return 2 * p * r / (p + r) if p + r else 0.0

    def __add__(self, other: Score) -> Score:
        return Score(
            self.true_positive + other.true_positive,
            self.false_positive + other.false_positive,
            self.false_negative + other.false_negative,
        )

    def row(self) -> str:
        return (
            f"P={self.precision:.2f} R={self.recall:.2f} F1={self.f1:.2f} "
            f"(VP {self.true_positive}, FP {self.false_positive}, FN {self.false_negative})"
        )


def match_events(predicted: list[Interval], reference: list[Interval], min_overlap: float = 0.5) -> Score:
    """Appariement un à un : le recouvrement doit couvrir la moitié du plus court des deux."""
    pairs = []
    for i, p in enumerate(predicted):
        for j, r in enumerate(reference):
            shared = overlap(p, r)
            if shared >= min_overlap * min(p[1] - p[0], r[1] - r[0]) and shared > 0:
                pairs.append((shared, i, j))
    used_p: set[int] = set()
    used_r: set[int] = set()
    for _, i, j in sorted(pairs, reverse=True):
        if i not in used_p and j not in used_r:
            used_p.add(i)
            used_r.add(j)
    tp = len(used_p)
    return Score(tp, len(predicted) - tp, len(reference) - tp)
