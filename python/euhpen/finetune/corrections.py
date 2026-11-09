"""Journal local des corrections faites dans l'interface, réutilisables à l'entraînement.

Rien ne sort de la machine. Une ligne JSON par correction, dans le dossier de configuration.
Les fichiers réservés à l'évaluation ne sont jamais journalisés, et sont filtrés une seconde fois
à la relecture.

Seules deux corrections disent clairement quelque chose :
- garder un passage coupé comme hésitation : c'est de la parole normale ;
- couper un mot précis : c'est une hésitation probable.

Couper tout un passage (sans mot désigné) ne dit pas ce qu'il contient : ces lignes sont
conservées mais ne servent pas d'étiquettes.
"""

from __future__ import annotations

import json
import os
from dataclasses import asdict, dataclass
from pathlib import Path

from ..config import config_dir
from .holdout import is_holdout

KEEP, CUT, UNDO = "keep", "cut", "undo"
MAX_WORD_CUT = 1.5  # au-delà, ce n'est plus un mot isolé


@dataclass
class Correction:
    fingerprint: str
    source: str
    action: str
    start: float
    end: float
    reason: str = ""  # motif de la coupe annulée, ou "word" / "passage" pour une coupe manuelle


def journal_path() -> Path:
    return config_dir() / "finetune" / "corrections.jsonl"


def log_correction(entry: Correction, path: Path | None = None) -> bool:
    """Ajoute une ligne au journal. Renvoie False si le fichier est réservé à l'évaluation."""
    if is_holdout(entry.source):
        return False
    target = path or journal_path()
    target.parent.mkdir(parents=True, exist_ok=True)
    with open(target, "a", encoding="utf-8") as f:
        f.write(json.dumps(asdict(entry), ensure_ascii=False) + "\n")
    return True


def read_corrections(path: Path | None = None) -> list[Correction]:
    """Relit le journal : annulations appliquées, fichiers réservés ou disparus écartés."""
    source = path or journal_path()
    if not source.exists():
        return []
    entries: list[Correction] = []
    for line in source.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            item = Correction(**json.loads(line))
        except (TypeError, ValueError):
            continue
        if item.action == UNDO:
            entries = [
                e for e in entries
                if not (e.fingerprint == item.fingerprint and e.start <= item.start < e.end)
            ]
        else:
            entries.append(item)
    return [e for e in entries if os.path.exists(e.source) and not is_holdout(e.source)]


def is_label(entry: Correction) -> bool:
    """La correction indique-t-elle sans ambiguïté le contenu du passage ?"""
    if entry.action == KEEP:
        return entry.reason == "hesitation"
    return entry.action == CUT and entry.reason == "word" and entry.end - entry.start <= MAX_WORD_CUT
