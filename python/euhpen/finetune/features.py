"""Cache disque des états de l'encodeur, par modèle et par contenu audio."""

from __future__ import annotations

import hashlib
import shutil
import subprocess
from collections.abc import Iterable
from pathlib import Path

import numpy as np

from ..cache import cache_dir
from .encoder import SAMPLE_RATE, Encoder, encode_audio, load_encoder


def features_dir(model: str) -> Path:
    path = cache_dir() / "finetune" / "features" / model
    path.mkdir(parents=True, exist_ok=True)
    return path


def decode_16k(path: str | Path) -> np.ndarray:
    """Décode en mono 16 kHz : les mêmes échantillons que voit l'encodeur."""
    command = [
        shutil.which("ffmpeg") or "ffmpeg", "-nostdin", "-v", "error", "-i", str(path),
        "-ac", "1", "-ar", str(SAMPLE_RATE), "-f", "f32le", "-",
    ]
    data = subprocess.run(command, check=True, capture_output=True).stdout
    return np.frombuffer(data, dtype=np.float32).copy()


def file_job(path: str | Path):
    """Pour les scripts d'évaluation : un fichier décodé en simple mélange mono.

    L'application choisit le canal utile avant de transcrire ; ici tous les canaux sont mélangés.
    """
    from ..transcribe import AudioJob

    key = hashlib.sha1(str(Path(path).resolve()).encode()).hexdigest()[:16]
    folder = cache_dir() / "finetune" / "files" / key
    folder.mkdir(parents=True, exist_ok=True)
    audio = folder / "audio.f32"
    if not audio.exists():
        decode_16k(path).tofile(audio)
    return AudioJob(str(audio), folder)


def ensure_features(items: Iterable[tuple[str, str]], model: str = "whisper-turbo", log=print) -> dict[str, Path]:
    """Calcule ce qui manque. ``items`` : (identifiant, chemin audio)."""
    items = list(items)
    folder = features_dir(model)
    paths = {sample_id: folder / f"{sample_id}.npy" for sample_id, _ in items}
    missing = [(i, a) for i, a in items if not paths[i].exists()]
    encoder: Encoder | None = None
    for index, (sample_id, audio_path) in enumerate(missing):
        encoder = encoder or load_encoder(model)
        states = encode_audio(encoder, decode_16k(audio_path))
        temporary = paths[sample_id].with_suffix(".tmp.npy")
        np.save(temporary, states)
        temporary.replace(paths[sample_id])
        log(f"[{index + 1}/{len(missing)}] {sample_id}: {states.shape[0]} trames")
    return paths
