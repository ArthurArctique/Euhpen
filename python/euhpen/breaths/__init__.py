"""Respirations, détectées par Respiro-en (Dong Yang et al., Interspeech 2024, licence MIT).

Le modèle donne toutes les 10 ms la probabilité d'entendre une respiration. Chaque passage
détecté devient un jeton ``[respiration]`` dans la transcription, avec sa plus forte probabilité :
la décision (en Rust) le coupe comme les autres bruits, sans marge mais avec les mêmes bords
recalés, et ne garde que ceux au-dessus du seuil réglé dans l'application.

Deux modèles pour l'application :

- ``respiro-en`` : les respirations seules, sans Whisper ;
- ``<modèle>+respiro-en`` : la transcription de ce modèle (cache compris), respirations ajoutées.

Chacun est mis en cache sous son nom. La révision contient celle de la détection
(:data:`BREATH_REVISION`) : une détection modifiée ne réutilise pas d'anciens résultats.

Les poids (35 Mo) sont téléchargés depuis Hugging Face au premier usage, à une révision figée,
et vérifiés par leur empreinte SHA-256. Le réseau, entraîné sur des livres audio anglais
(LibriTTS-R), est repris tel quel dans :mod:`euhpen.breaths.network`.
"""

from __future__ import annotations

import time
from collections.abc import Callable
from dataclasses import replace
from datetime import datetime, timezone

from ..cache import write_json
from ..transcribe import (
    BREATH_MODEL,
    BREATH_SUFFIX,
    AudioJob,
    Transcript,
    Word,
    _transcript_path,
    cached_transcript,
    classify,
    detect_hardware,
    transcribe,
)
from .detect import BREATH_REVISION, detect_breaths, ensure_weights

__all__ = ["BREATH_REVISION", "TOKEN", "merge_words", "transcribe_breaths"]

TOKEN = "[respiration]"
LABEL = "détection des respirations"


def merge_words(words: list[Word], breaths: list[Word]) -> list[Word]:
    """Les deux listes, dans l'ordre du temps. Une respiration peut chevaucher un mot de Whisper,
    dont les bornes débordent souvent sur les pauses : elle est gardée telle quelle."""
    return sorted([*words, *breaths], key=lambda w: (w.start, w.end))


def _device() -> str:
    return detect_hardware().device


def _detect_only(job: AudioJob, language: str, progress: Callable[[float, str], None] | None) -> Transcript:
    def report(fraction: float, label: str) -> None:
        if progress:
            progress(fraction, label)

    samples = job.samples()
    ensure_weights(report)
    report(0.0, LABEL)
    started = time.perf_counter()
    found, device = detect_breaths(samples, _device(), report, LABEL)
    words = [Word(TOKEN, b.start, b.end, b.probability, classify(TOKEN)) for b in found]
    transcript = Transcript(
        model=BREATH_MODEL,
        revision=f"r{BREATH_REVISION}",
        language=language,
        words=words,
        device=device,
        processing_time=time.perf_counter() - started,
        created=datetime.now(timezone.utc).isoformat(timespec="seconds"),
    )
    write_json(_transcript_path(job.cache_dir, BREATH_MODEL, transcript.revision, language), transcript.to_dict())
    return transcript


def transcribe_breaths(
    job: AudioJob,
    language: str,
    model: str,
    *,
    use_cache: bool = True,
    progress: Callable[[float, str], None] | None = None,
) -> tuple[Transcript, bool]:
    """Comme :func:`euhpen.transcribe.transcribe` pour ``respiro-en`` et ``<modèle>+respiro-en``."""
    if use_cache:
        cached = cached_transcript(job.cache_dir, model, language)
        if cached is not None:
            return cached, True
    if model == BREATH_MODEL:
        result = _detect_only(job, language, progress)
    else:
        # Téléchargé avant Whisper, qui passe ensuite le processus hors ligne.
        ensure_weights(progress)
        base, _ = transcribe(job, language, model.removesuffix(BREATH_SUFFIX), use_cache=use_cache, progress=progress)
        breaths, _ = transcribe_breaths(job, language, BREATH_MODEL, use_cache=use_cache, progress=progress)
        result = replace(
            base,
            model=model,
            revision=f"{base.revision}r{BREATH_REVISION}",
            words=merge_words(base.words, breaths.words),
            processing_time=base.processing_time + breaths.processing_time,
            created=datetime.now(timezone.utc).isoformat(timespec="seconds"),
        )
        write_json(_transcript_path(job.cache_dir, model, result.revision, language), result.to_dict())
    if progress:
        progress(1.0, "terminé")
    return result, False
