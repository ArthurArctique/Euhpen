"""Dossier de cache et lecture/écriture JSON."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any


def cache_dir() -> Path:
    """Dossier de cache : EUHPEN_CACHE_DIR, sinon l'emplacement standard du système."""
    override = os.environ.get("EUHPEN_CACHE_DIR")
    if override:
        return Path(override)
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches" / "euhpen"
    if os.name == "nt":
        base = os.environ.get("LOCALAPPDATA") or str(Path.home() / "AppData" / "Local")
        return Path(base) / "euhpen" / "Cache"
    base = os.environ.get("XDG_CACHE_HOME") or str(Path.home() / ".cache")
    return Path(base) / "euhpen"


def read_json(path: Path) -> dict[str, Any] | None:
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def write_json(path: Path, data: dict[str, Any]) -> None:
    """Écriture atomique (fichier temporaire puis renommage)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, separators=(",", ":"))
    os.replace(tmp, path)
