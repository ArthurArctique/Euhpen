"""``python -m euhpen train`` : entraînement de la tête (lancé par ``euhpen train``)."""

from __future__ import annotations

import argparse
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(prog="euhpen")
    sub = parser.add_subparsers(dest="command", required=True)
    train = sub.add_parser("train", help="entraîne la tête de détection des hésitations (prototype)")
    train.add_argument("corpus", help="dossier du corpus Rhapsodie (TextGrid et WAV, cherchés récursivement)")
    train.add_argument("--epochs", type=int, default=40, help="nombre maximal d'époques (défaut 40)")
    train.add_argument("--output", help="dossier des poids (défaut : dossier de configuration d'Euhpen)")
    train.add_argument("--corrections", action=argparse.BooleanOptionalAction, default=True,
                       help="ajoute les corrections faites dans l'interface (défaut oui)")
    args = parser.parse_args()

    from .finetune.encoder import WHISPER_MODEL
    from .finetune.train import train_from_rhapsodie

    folder = train_from_rhapsodie(
        args.corpus,
        WHISPER_MODEL,
        Path(args.output) if args.output else None,
        epochs=args.epochs,
        use_corrections=args.corrections,
    )
    print(f"Tête enregistrée dans {folder}. Entraînée sur Rhapsodie (CC BY-NC-SA) : usage non commercial, à créditer.")
    return 0


raise SystemExit(main())
