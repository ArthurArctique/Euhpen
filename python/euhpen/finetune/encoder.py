"""États de l'encodeur de Whisper : une trame de 1280 dimensions toutes les 20 ms.

L'encodeur est gelé : il sert seulement d'extracteur. Le fichier est lu par fenêtres de 30 s qui
se recouvrent de 6 s ; on ne garde que le centre de chaque fenêtre, car les bords disposent de
moins de contexte.
"""

from __future__ import annotations

import os
from collections.abc import Callable
from dataclasses import dataclass

import numpy as np

os.environ.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")

SAMPLE_RATE = 16_000
WINDOW = 30.0
MARGIN = 3.0
HOP = 0.02

WHISPER_MODEL = "whisper-turbo"
# Dépôt Hugging Face et révision figée (licence MIT).
OPENAI_ENCODERS = {
    WHISPER_MODEL: ("openai/whisper-large-v3-turbo", "41f01f3fe87f28c78e2fbf8b568835947dd65ed9"),
}


@dataclass
class Encoder:
    model: str
    processor: object
    network: object
    device: str
    dtype: object


def download_snapshot(model: str, **options) -> str:
    """Télécharge le modèle à sa révision figée et renvoie son dossier local.

    Un seul fil de téléchargement : sans droit de créer des liens symboliques (Windows sans le
    mode développeur), huggingface_hub les croit disponibles le temps de son test, et un second
    fil qui en crée un pendant ce test fait échouer tout le téléchargement (WinError 1314).
    """
    from huggingface_hub import snapshot_download

    repo, revision = OPENAI_ENCODERS[model]
    return snapshot_download(repo, revision=revision, max_workers=1, **options)


def model_path(model: str) -> str:
    """Dossier local du modèle."""
    return download_snapshot(model)


def load_encoder(model: str = WHISPER_MODEL) -> Encoder:
    """L'encodeur du Whisper chargé pour la transcription : un seul exemplaire en mémoire graphique."""
    from ..transcribe import detect_hardware, ensure_model, load_pipeline

    pipe = load_pipeline(ensure_model(model), detect_hardware())
    network = pipe.model.model.encoder.eval()
    for parameter in network.parameters():
        parameter.requires_grad_(False)
    first = next(network.parameters())
    return Encoder(model, pipe.feature_extractor, network, str(first.device), first.dtype)


def window_starts(samples: int) -> list[int]:
    """Débuts (en échantillons) des fenêtres qui couvrent le fichier."""
    length = int(WINDOW * SAMPLE_RATE)
    step = int((WINDOW - 2 * MARGIN) * SAMPLE_RATE)
    if samples <= length:
        return [0]
    starts = list(range(0, samples - length, step))
    starts.append(samples - length)
    return starts


def encode_audio(
    encoder: Encoder,
    audio: np.ndarray,
    batch_size: int = 4,
    progress: Callable[[float], None] | None = None,
) -> np.ndarray:
    """Audio mono à 16 kHz → tableau float16 de forme (trames, 1280)."""
    import torch

    frames = int(np.ceil(audio.size / SAMPLE_RATE / HOP))
    starts = window_starts(audio.size)
    frames_per_window = int(WINDOW / HOP)
    margin = int(MARGIN / HOP)
    output = np.zeros((frames, encoder.network.config.d_model), dtype=np.float16)

    for first in range(0, len(starts), batch_size):
        batch = starts[first : first + batch_size]
        chunks = [audio[s : s + int(WINDOW * SAMPLE_RATE)] for s in batch]
        inputs = encoder.processor(chunks, sampling_rate=SAMPLE_RATE, return_tensors="pt").input_features
        with torch.inference_mode():
            states = encoder.network(inputs.to(encoder.device, encoder.dtype)).last_hidden_state
        states = states.float().cpu().numpy().astype(np.float16)
        for k, start in enumerate(batch):
            offset = int(round(start / SAMPLE_RATE / HOP))
            is_first, is_last = start == starts[0], start == starts[-1]
            low = 0 if is_first else margin
            high = frames_per_window if is_last else frames_per_window - margin
            high = min(high, frames - offset)
            if high > low:
                output[offset + low : offset + high] = states[k, low:high]
        if progress:
            progress(min(1.0, (first + len(batch)) / len(starts)))
    return output
