"""Fichiers réservés à l'évaluation : jamais d'entraînement ni de réglage dessus.

La règle est appliquée par le code : :func:`refuse_holdout` arrête l'entraînement si un audio
réservé s'y trouve, quel que soit son nom ou son emplacement (l'empreinte porte sur le contenu).
"""

from __future__ import annotations

import hashlib
from pathlib import Path

# « Test audio euhh.mp3 »
HOLDOUT_SHA256 = {"bc6d8d0d71abbabdbc4bdca1374a86fe2763dbbdf5d32a43afd24bab33a275bf"}
HOLDOUT_NAMES = {"test audio euhh"}


def content_hash(path: str | Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def is_holdout(path: str | Path) -> bool:
    path = Path(path)
    if path.stem.lower() in HOLDOUT_NAMES:
        return True
    return path.exists() and content_hash(path) in HOLDOUT_SHA256


def refuse_holdout(paths) -> None:
    """Lève une erreur si l'un des fichiers est réservé à l'évaluation."""
    banned = [str(p) for p in paths if is_holdout(p)]
    if banned:
        raise ValueError("fichier réservé à l'évaluation, interdit à l'entraînement : " + ", ".join(banned))
