"""Transcription suivie de la tête : ce que lance l'application.

La transcription de base est réutilisée telle quelle (cache compris). Seules les détections de la
tête s'y ajoutent, et le résultat est mis en cache sous son propre nom de modèle. La révision
contient une empreinte des poids : une tête réentraînée ne réutilise pas d'anciennes détections.
"""

from __future__ import annotations

import hashlib
import time
from collections.abc import Callable
from dataclasses import replace

from ..cache import write_json
from ..transcribe import HEAD_SUFFIX, AudioJob, Transcript, _transcript_path, cached_transcript, transcribe
from .head import FILES, detect_audio, head_dir
from .spans import TOKENS, insert_spans

LABEL = "détection des syllabes étirées"


def base_model(model: str) -> str:
    return model.removesuffix(HEAD_SUFFIX)


def head_installed(model: str) -> bool:
    folder = head_dir(base_model(model))
    return all((folder / name).is_file() for name in FILES)


def head_revision(model: str) -> str | None:
    """Empreinte des poids installés et des jetons écrits, ou None s'il n'y a pas de poids."""
    if not head_installed(model):
        return None
    digest = hashlib.sha1(repr(sorted(TOKENS.items())).encode())
    for name in FILES:
        digest.update((head_dir(base_model(model)) / name).read_bytes())
    return digest.hexdigest()[:10]


def transcribe_with_head(
    job: AudioJob,
    language: str,
    model: str,
    *,
    use_cache: bool = True,
    progress: Callable[[float, str], None] | None = None,
) -> tuple[Transcript, bool]:
    """Comme :func:`euhpen.transcribe.transcribe`, avec les détections de la tête en plus."""
    revision = head_revision(model)
    if revision is None:
        raise RuntimeError(f"Tête entraînée introuvable dans {head_dir(base_model(model))}.")
    if use_cache:
        cached = cached_transcript(job.cache_dir, model, language)
        if cached is not None:
            return cached, True

    def report(fraction: float, label: str) -> None:
        if progress:
            progress(fraction, label)

    base, _ = transcribe(job, language, base_model(model), use_cache=use_cache, progress=progress)

    report(0.0, LABEL)
    started = time.perf_counter()
    # Les mêmes échantillons que la transcription : canal choisi et temps de la piste audio.
    spans = detect_audio(job.samples(), base_model(model), progress=lambda f: report(f, LABEL))
    result = replace(
        insert_spans(base, spans),
        model=model,
        revision=f"{base.revision}h{revision}",
        processing_time=base.processing_time + time.perf_counter() - started,
    )
    write_json(_transcript_path(job.cache_dir, model, result.revision, language), result.to_dict())
    report(1.0, "terminé")
    return result, False
