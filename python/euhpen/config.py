"""Dossier de configuration (distinct du cache)."""

from __future__ import annotations

import os
import sys
from pathlib import Path


def config_dir() -> Path:
    """Dossier de configuration : EUHPEN_CONFIG_DIR, sinon l'emplacement standard du système."""
    override = os.environ.get("EUHPEN_CONFIG_DIR")
    if override:
        return Path(override)
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "euhpen"
    if os.name == "nt":
        return Path(os.environ.get("APPDATA") or Path.home() / "AppData" / "Roaming") / "euhpen"
    return Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "euhpen"
