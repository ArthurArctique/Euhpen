"""Respiro-en sur un fichier entier : probabilité de respiration toutes les 10 ms, puis passages.

Le réseau a appris sur des phrases de quelques secondes. Un long fichier est donc lu par
fenêtres de 20 s qui se recouvrent, chacune normalisée à part comme une phrase des auteurs ;
chaque trame est prise dans la fenêtre où elle est le plus loin des bords.
"""

from __future__ import annotations

import hashlib
import logging
import os
import threading
from collections.abc import Callable
from dataclasses import dataclass

import numpy as np

from .features import FRAME_SECONDS, SAMPLE_RATE, features, frame_count

# Poids publiés par les auteurs, figés à une révision et vérifiés à l'octet près.
WEIGHTS_REPO = "ydqmkkx/respiro-en"
WEIGHTS_REVISION = "ea478afd15a27ec4f24598cb77dc8199a9bcf35e"
WEIGHTS_FILE = "respiro-en.pt"
WEIGHTS_SHA256 = "1f4a9b96f96645c480bf0e07b1e18cd68878ac0b4bb5dc920ad93f9b17df858a"
WEIGHTS_MB = 35

# À augmenter dès que la détection change (fenêtres, seuils…) : les anciens résultats en cache
# ne servent plus. Recopié dans src/transcript.rs (BREATH_REVISION).
DETECTION_VERSION = 1
BREATH_REVISION = f"{WEIGHTS_SHA256[:8]}v{DETECTION_VERSION}"

THRESHOLD = 0.064  # seuil des auteurs, choisi sur leur jeu de validation
MIN_FRAMES = 21  # plus de 20 trames (200 ms), comme le détecteur des auteurs
WINDOW = 2000  # trames par passage dans le réseau (20 s)
MARGIN = 300  # au moins 3 s de contexte de chaque côté d'une trame gardée
BATCH = 8

DOWNLOAD_LABEL = "téléchargement du modèle des respirations"


@dataclass
class Breath:
    start: float  # secondes, dans le temps de la piste audio
    end: float
    probability: float  # la plus forte du passage


# --- poids -------------------------------------------------------------------------------

_verified: set[str] = set()


def _check(path: str) -> str:
    if path not in _verified:
        digest = hashlib.sha256()
        with open(path, "rb") as f:
            for block in iter(lambda: f.read(1 << 20), b""):
                digest.update(block)
        if digest.hexdigest() != WEIGHTS_SHA256:
            raise RuntimeError(
                f"Le modèle des respirations ({path}) n'est pas celui attendu : supprimez ce fichier, "
                "il sera téléchargé de nouveau."
            )
        _verified.add(path)
    return path


def local_weights() -> str | None:
    """Fichier des poids s'il est déjà téléchargé (aucun accès réseau)."""
    try:
        from huggingface_hub import hf_hub_download

        path = hf_hub_download(WEIGHTS_REPO, WEIGHTS_FILE, revision=WEIGHTS_REVISION, local_files_only=True)
    except Exception:  # noqa: BLE001 - absent du cache ou huggingface_hub manquant
        return None
    return path if os.path.isfile(path) else None


def ensure_weights(progress: Callable[[float, str], None] | None = None) -> str:
    """Télécharge les poids (35 Mo) au premier usage et renvoie leur chemin, vérifié."""
    path = local_weights()
    if path:
        return _check(path)
    from huggingface_hub import snapshot_download
    from tqdm.auto import tqdm as base_tqdm

    class _Tqdm(base_tqdm):  # type: ignore[misc, valid-type]
        def update(self, n=1):
            out = super().update(n)
            if progress and self.total and self.unit == "B":
                progress(min(1.0, self.n / self.total), DOWNLOAD_LABEL)
            return out

    if progress:
        progress(0.0, DOWNLOAD_LABEL)
    try:
        folder = snapshot_download(
            WEIGHTS_REPO, revision=WEIGHTS_REVISION, allow_patterns=[WEIGHTS_FILE], max_workers=1, tqdm_class=_Tqdm
        )
    except Exception as exc:  # noqa: BLE001 - réseau, hors ligne, dépôt inaccessible
        raise RuntimeError(
            f"Le modèle des respirations ({WEIGHTS_MB} Mo) n'a pas pu être téléchargé depuis Hugging Face "
            f"({exc.__class__.__name__}). Vérifiez la connexion à Internet, puis relancez."
        ) from exc
    return _check(os.path.join(folder, WEIGHTS_FILE))


# --- réseau ------------------------------------------------------------------------------

_lock = threading.Lock()
_models: dict[str, object] = {}


def load_model(path: str, device: str):
    """Réseau chargé sur ``device`` (gardé en mémoire : 35 Mo)."""
    import torch

    from .network import DetectionNet

    key = f"{path}|{device}"
    with _lock:
        if key not in _models:
            checkpoint = torch.load(path, map_location="cpu", weights_only=True)
            net = DetectionNet()
            net.load_state_dict(checkpoint["model"])
            _models.clear()
            _models[key] = net.eval().to(device)
        return _models[key]


def window_starts(total: int, window: int = WINDOW, margin: int = MARGIN) -> list[int]:
    """Début de chaque fenêtre (en trames) ; la dernière finit avec le fichier."""
    if total <= window:
        return [0]
    starts = list(range(0, total - window, window - 2 * margin))
    starts.append(total - window)
    return starts


def frame_probabilities(
    samples: np.ndarray,
    model,
    device: str,
    progress: Callable[[float], None] | None = None,
) -> np.ndarray:
    """Probabilité de respiration de chaque trame de 10 ms (``frame_count(samples.size)`` valeurs)."""
    import torch

    total = frame_count(samples.size)
    length = min(WINDOW, total)
    starts = window_starts(total, length, MARGIN)
    limits = [0, *((a + b + length) // 2 for a, b in zip(starts, starts[1:])), total]
    probabilities = np.zeros(total, dtype=np.float32)
    for first in range(0, len(starts), BATCH):
        batch = starts[first : first + BATCH]
        x = torch.from_numpy(np.stack([features(samples, s, length) for s in batch])).to(device)
        lengths = torch.full((len(batch),), length, dtype=torch.long, device=device)
        with torch.inference_mode():
            out = model(x, lengths).float().cpu().numpy()
        for k, start in enumerate(batch, start=first):
            lo, hi = limits[k], limits[k + 1]
            probabilities[lo:hi] = out[k - first, lo - start : hi - start]
        if progress:
            progress(min(len(starts), first + BATCH) / len(starts))
    return probabilities


def runs(mask: np.ndarray) -> list[tuple[int, int]]:
    """Plages [début, fin) des valeurs vraies."""
    edges = np.diff(np.concatenate(([0], mask.astype(np.int8), [0])))
    return list(zip(np.flatnonzero(edges == 1).tolist(), np.flatnonzero(edges == -1).tolist()))


def find_breaths(
    probabilities: np.ndarray, duration: float, threshold: float = THRESHOLD, min_frames: int = MIN_FRAMES
) -> list[Breath]:
    """Passages au-dessus du seuil assez longs. La trame k couvre [k − ½, k + ½] × 10 ms."""
    found = []
    for a, b in runs(probabilities > threshold):
        if b - a < min_frames:
            continue
        start = max(0.0, (a - 0.5) * FRAME_SECONDS)
        end = min(duration, (b - 0.5) * FRAME_SECONDS)
        if end > start:
            found.append(Breath(start, end, float(probabilities[a:b].max())))
    return found


def detect_breaths(
    samples: np.ndarray, device: str, progress: Callable[[float, str], None] | None = None, label: str = ""
) -> tuple[list[Breath], str]:
    """Respirations d'un fichier mono 16 kHz, et l'appareil qui a réellement servi."""
    path = ensure_weights(progress)
    samples = np.asarray(samples, dtype=np.float32)
    duration = samples.size / SAMPLE_RATE
    if frame_count(samples.size) < MIN_FRAMES:
        return [], device

    def step(fraction: float) -> None:
        if progress:
            progress(fraction, label)

    try:
        probabilities = frame_probabilities(samples, load_model(path, device), device, step)
    except Exception as exc:  # noqa: BLE001
        if device == "cpu":
            raise
        logging.getLogger(__name__).warning("Échec sur %s (%s) : repli sur le processeur.", device, exc)
        device = "cpu"
        probabilities = frame_probabilities(samples, load_model(path, device), device, step)
    return find_breaths(probabilities, duration), device
