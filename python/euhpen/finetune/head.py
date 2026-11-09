"""La tête de classification : réseau, chargement, inférence sur un fichier."""

from __future__ import annotations

import json
from collections.abc import Callable
from dataclasses import fields
from pathlib import Path

import numpy as np

from ..config import config_dir
from .corpus import LABELS
from .encoder import WHISPER_MODEL
from .spans import DetectionParams, Span, frames_to_spans

INPUT_DIM = 1280
HIDDEN = 96
DILATIONS = (1, 2, 4, 8)  # champ récepteur : ±0,6 s autour de la trame
FILES = ("head.pt", "head.json")
BUNDLED = Path(__file__).resolve().parent.parent / "weights"  # poids livrés avec Euhpen


def build_network(hidden: int = HIDDEN, dropout: float = 0.2):
    """Convolutions dilatées résiduelles : (lot, trames, 1280) → (lot, trames, 4)."""
    import torch
    from torch import nn

    class Block(nn.Module):
        def __init__(self, dilation: int):
            super().__init__()
            self.net = nn.Sequential(
                nn.Conv1d(hidden, hidden, 5, padding=2 * dilation, dilation=dilation),
                nn.GELU(),
                nn.Dropout(dropout),
                nn.Conv1d(hidden, hidden, 1),
            )
            self.norm = nn.GroupNorm(8, hidden)

        def forward(self, x: torch.Tensor) -> torch.Tensor:
            return self.norm(x + self.net(x))

    class FrameHead(nn.Module):
        def __init__(self):
            super().__init__()
            self.input = nn.Sequential(
                nn.LayerNorm(INPUT_DIM), nn.Linear(INPUT_DIM, hidden), nn.GELU(), nn.Dropout(dropout)
            )
            self.blocks = nn.Sequential(*[Block(d) for d in DILATIONS])
            self.output = nn.Linear(hidden, len(LABELS))

        def forward(self, x: torch.Tensor) -> torch.Tensor:
            x = self.input(x).transpose(1, 2)
            x = self.blocks(x).transpose(1, 2)
            return self.output(x)

    return FrameHead()


def trained_dir(model: str = WHISPER_MODEL) -> Path:
    """Où ``euhpen train`` enregistre une tête réentraînée."""
    return config_dir() / "finetune" / model


def head_dir(model: str = WHISPER_MODEL) -> Path:
    """Dossier des poids utilisés : la tête réentraînée s'il y en a une, sinon celle livrée."""
    trained = trained_dir(model)
    return trained if all((trained / name).is_file() for name in FILES) else BUNDLED / model


def save_head(network, params: DetectionParams, model: str, info: dict, folder: Path | None = None) -> Path:
    import torch

    folder = folder or trained_dir(model)
    folder.mkdir(parents=True, exist_ok=True)
    torch.save({k: v.cpu() for k, v in network.state_dict().items()}, folder / "head.pt")
    meta = {"model": model, "labels": LABELS, "params": params.to_dict(), "info": info}
    (folder / "head.json").write_text(json.dumps(meta, ensure_ascii=False, indent=1), encoding="utf-8")
    return folder


def load_head(model: str = WHISPER_MODEL, folder: Path | None = None):
    import torch

    folder = folder or head_dir(model)
    network = build_network()
    network.load_state_dict(torch.load(folder / "head.pt", map_location="cpu", weights_only=True))
    meta = json.loads((folder / "head.json").read_text(encoding="utf-8"))
    known = {f.name for f in fields(DetectionParams)}
    params = DetectionParams(**{k: v for k, v in meta["params"].items() if k in known})
    return network.eval(), params, meta


def predict(network, features: np.ndarray, device: str = "cpu") -> np.ndarray:
    """États de l'encodeur (trames, 1280) → probabilités (trames, 4)."""
    import torch

    with torch.inference_mode():
        x = torch.from_numpy(np.asarray(features, dtype=np.float32)).unsqueeze(0).to(device)
        return torch.softmax(network(x), dim=-1).squeeze(0).cpu().numpy()


def detect(audio_path: str, model: str = WHISPER_MODEL, folder: Path | None = None) -> list[Span]:
    """Détections pour un fichier audio, avec les seuils enregistrés à l'entraînement."""
    from .features import decode_16k

    return detect_audio(decode_16k(audio_path), model, folder)


def detect_audio(
    audio: np.ndarray,
    model: str = WHISPER_MODEL,
    folder: Path | None = None,
    progress: Callable[[float], None] | None = None,
) -> list[Span]:
    """Détections pour de l'audio mono à 16 kHz."""
    from .encoder import encode_audio, load_encoder

    encoder = load_encoder(model)
    network, params, _ = load_head(model, folder)
    # La tête tourne sur le même appareil que l'encodeur (carte graphique si elle est là).
    network = network.to(encoder.device)
    states = encode_audio(encoder, audio, progress=progress)
    return frames_to_spans(predict(network, states, encoder.device), params)
