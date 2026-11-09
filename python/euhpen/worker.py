"""Processus qui exécute le modèle pour Euhpen (``python -m euhpen.worker``).

Le modèle reste chargé entre deux fichiers. L'application parle au processus en lignes JSON :
une demande par ligne sur l'entrée standard, des messages d'avancement sur la sortie standard.
L'arrêter (annulation) libère aussitôt la mémoire graphique, et un plantage de PyTorch ne fait
pas tomber l'interface.

Demande : ``{"job": "...", "audio": "x.f32", "cache_dir": "...", "language": "fr", "model": "...", "force": false}``
(``audio`` : mono 16 kHz en flottants 32 bits, décodé par l'application.)
Réponses : ``{"type": "ready", "hardware": {...}}`` au démarrage, puis
``{"job": "...", "type": "progress", "fraction": 0.4, "label": "..."}`` et enfin
``{"job": "...", "type": "done", ...}`` ou ``{"job": "...", "type": "error", "message": "..."}``.
"""

from __future__ import annotations

import json
import os
import sys
import traceback
from pathlib import Path


def _cuda_ready() -> bool:
    torch = sys.modules.get("torch")
    return torch is not None and torch.cuda.is_available()


def main() -> None:
    # Le canal de réponses est une copie de la sortie standard ; tout ce que les
    # bibliothèques écrivent ensuite sur la sortie standard part dans les erreurs.
    channel = os.fdopen(os.dup(sys.stdout.fileno()), "w", encoding="utf-8", buffering=1)
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())

    def send(message: dict) -> None:
        channel.write(json.dumps(message, ensure_ascii=False) + "\n")
        channel.flush()

    from .transcribe import AudioJob, detect_hardware, loaded_device, transcribe

    try:
        hardware = detect_hardware().to_dict()
    except Exception as exc:  # noqa: BLE001
        hardware = {"error": str(exc)}
    send({"type": "ready", "hardware": hardware, "pid": os.getpid()})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError:
            continue
        job = request.get("job")
        try:

            def progress(fraction: float, label: str, job=job) -> None:
                send({"job": job, "type": "progress", "fraction": round(fraction, 3), "label": label})

            if _cuda_ready():
                sys.modules["torch"].cuda.reset_peak_memory_stats()
            transcript, cached = transcribe(
                AudioJob(request["audio"], Path(request["cache_dir"])),
                request["language"],
                request["model"],
                use_cache=not request.get("force", False),
                progress=progress,
            )
            done = {
                "job": job,
                "type": "done",
                "cached": cached,
                "model": transcript.model,
                "device": transcript.device,
                "words": len(transcript.words),
            }
            if not cached:
                # Ce qui a réellement servi, lu sur le modèle chargé (pas ce qui était demandé).
                done["encoder_device"] = loaded_device()
                if _cuda_ready():
                    done["gpu_memory_mb"] = round(sys.modules["torch"].cuda.max_memory_allocated() / 2**20, 1)
            send(done)
        except Exception as exc:  # noqa: BLE001 - l'erreur est renvoyée à l'application
            traceback.print_exc()
            send({"job": job, "type": "error", "message": str(exc) or exc.__class__.__name__})


if __name__ == "__main__":
    main()
