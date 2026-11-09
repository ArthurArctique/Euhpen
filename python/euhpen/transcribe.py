"""Transcription mot à mot avec Whisper, puis détection des hésitations.

Whisper donne les mots et leurs horodatages. La tête entraînée sur du français (voir
``euhpen/finetune``) y ajoute les hésitations, sous forme de jetons entre crochets
(``[euh]``, ``[étirement]``).

Le résultat est mis en cache, indexé par l'empreinte du fichier, le modèle (avec sa
révision exacte) et la langue. Whisper n'est pas fourni avec Euhpen : il est téléchargé
depuis Hugging Face au premier usage, puis tout fonctionne hors ligne.

L'application (en Rust) décode l'audio, décide des coupes et relit le cache : ce module ne
fait que tourner le modèle et écrire ``words-*.json``.
"""

from __future__ import annotations

import logging
import os
import platform
import re
import shutil
import threading
import time
import warnings
from collections import Counter
from collections.abc import Callable
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace

import numpy as np

from . import __version__
from .cache import read_json, write_json
from .finetune.encoder import OPENAI_ENCODERS, SAMPLE_RATE, WHISPER_MODEL, WINDOW, download_snapshot, window_starts

# Sur Mac, une opération que Metal (MPS) ne sait pas faire repasse sur le processeur
# au lieu d'échouer. À définir avant le premier import de torch.
os.environ.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")

TRANSCRIPT_VERSION = 1
# Whisper pour les mots, plus une tête entraînée sur le français (voir euhpen/finetune).
HEAD_SUFFIX = "+head"
WHISPER_HEAD_MODEL = WHISPER_MODEL + HEAD_SUFFIX
# Fichiers sans lesquels transformers ne peut pas charger le modèle.
MODEL_FILES = (
    "config.json",
    "generation_config.json",
    "preprocessor_config.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "model.safetensors",
)


@dataclass
class AudioJob:
    """Ce que l'application prépare pour une transcription."""

    audio_path: str  # mono 16 kHz, flottants 32 bits
    cache_dir: Path  # dossier de cache du fichier

    def samples(self) -> np.ndarray:
        return np.fromfile(self.audio_path, dtype=np.float32)


# --- classement des mots ------------------------------------------------------------

WORD, HESITATION, COUGH, LAUGH, BREATH, EVENT = "word", "hesitation", "cough", "laugh", "breath", "event"
EVENT_KINDS = (HESITATION, COUGH, LAUGH, BREATH, EVENT)
WORD_TAIL = "…"  # fin d'un mot coupé en deux par la tête

_BRACKET = re.compile(r"^\W*\[([^\]]+)\]\W*$")
_HESITATION_TOKENS = {
    "uh", "um", "uhm", "umm", "uhh", "hm", "hmm", "hmmm", "mm", "mmm", "mhm", "er", "erm",
    "ah", "eh", "ehm", "euh", "heu", "euhm", "hum", "äh", "ähm", "öh", "öhm", "em", "eeh",
    "filler", "hesitation", "étirement",
}
_LAUGH_TOKENS = {"laugh", "laughs", "laughter", "laughing", "chuckle", "chuckles", "giggle", "giggles", "rire", "rires"}
_COUGH_TOKENS = {
    "cough", "coughs", "coughing", "throat clearing", "clears throat", "throat clear",
    "clearing throat", "sneeze", "sneezes", "sneezing", "toux",
}
_BREATH_TOKENS = {
    "breath", "breathing", "breathes", "inhale", "inhales", "inhaling", "exhale", "exhales",
    "sigh", "sighs", "sniff", "sniffs", "sniffing", "lip smack", "smack", "click", "tsk",
}
# Hésitations écrites comme des mots ordinaires (sans crochets).
_PLAIN_FILLERS = {"euh", "heu", "euhm", "hum", "hmm", "hm", "mmh", "uh", "um", "uhm", "erm", "äh", "ähm"}


def classify(text: str) -> str:
    """Nature d'un mot : mot réel, hésitation, toux, rire, souffle ou autre événement."""
    m = _BRACKET.match(text.strip())
    if m:
        token = m.group(1).strip().lower().replace("_", " ")
        if token in _HESITATION_TOKENS:
            return HESITATION
        if token in _LAUGH_TOKENS or token.startswith("laugh"):
            return LAUGH
        if token in _COUGH_TOKENS or token.startswith("cough"):
            return COUGH
        if token in _BREATH_TOKENS or token.startswith("breath"):
            return BREATH
        return EVENT
    bare = re.sub(r"[^\w]", "", text.lower())
    if bare in _PLAIN_FILLERS:
        return HESITATION
    return WORD


def bracket_token(text: str) -> str | None:
    """Le jeton entre crochets tel que le modèle l'a écrit (``[UH]``), sinon None."""
    m = _BRACKET.match(text.strip())
    return f"[{m.group(1).strip()}]" if m else None


@dataclass
class Word:
    text: str
    start: float  # secondes, dans le temps de la piste audio
    end: float
    probability: float | None
    kind: str

    def to_list(self) -> list:
        prob = None if self.probability is None else round(self.probability, 3)
        return [self.text, round(self.start, 3), round(self.end, 3), prob]

    @classmethod
    def from_list(cls, data: list) -> Word:
        text, start, end, prob = data
        return cls(text, float(start), float(end), None if prob is None else float(prob), classify(text))


@dataclass
class Transcript:
    model: str  # taille ou identifiant du modèle
    revision: str  # révision exacte du modèle (début du hash Hugging Face)
    language: str
    words: list[Word]
    device: str = ""
    processing_time: float = 0.0
    created: str = ""

    @property
    def text(self) -> str:
        return " ".join(w.text for w in self.words)

    def bracket_tokens(self) -> dict[str, int]:
        """Tous les jetons entre crochets rencontrés, avec leur nombre (à vérifier par langue)."""
        counts = Counter(t for t in (bracket_token(w.text) for w in self.words) if t)
        return dict(counts.most_common())

    def counts(self) -> dict[str, int]:
        return dict(Counter(w.kind for w in self.words))

    def to_dict(self) -> dict:
        return {
            "version": TRANSCRIPT_VERSION,
            "euhpen": __version__,
            "model": self.model,
            "revision": self.revision,
            "language": self.language,
            "device": self.device,
            "processing_time": round(self.processing_time, 2),
            "created": self.created,
            "words": [w.to_list() for w in self.words],
        }

    @classmethod
    def from_dict(cls, data: dict) -> Transcript:
        return cls(
            model=data["model"],
            revision=data.get("revision", ""),
            language=data["language"],
            words=[Word.from_list(w) for w in data["words"]],
            device=data.get("device", ""),
            processing_time=float(data.get("processing_time", 0.0)),
            created=data.get("created", ""),
        )


def _transcript_path(folder: Path, model: str, revision: str, language: str) -> Path:
    safe_model = re.sub(r"[^\w.-]", "_", model)
    return folder / f"words-{safe_model}-{revision or 'local'}-{language}-v{TRANSCRIPT_VERSION}.json"


def cached_transcript(folder: Path, model: str, language: str) -> Transcript | None:
    """Transcription en cache dans ce dossier pour ce modèle et cette langue (toute révision)."""
    safe_model = re.sub(r"[^\w.-]", "_", model)
    if not folder.exists():
        return None
    revision = r"\w+"
    if model.endswith(HEAD_SUFFIX):
        from .finetune.pipeline import head_revision

        head = head_revision(model)
        if head is None:
            return None
        revision = rf"\w+h{head}"
    name = rf"words-{re.escape(safe_model)}-{revision}-{re.escape(language)}-v{TRANSCRIPT_VERSION}"
    pattern = re.compile(rf"^{name}\.json$")
    candidates = sorted((p for p in folder.iterdir() if pattern.match(p.name)), key=lambda p: p.stat().st_mtime)
    for path in reversed(candidates):
        data = read_json(path)
        if data and data.get("version") == TRANSCRIPT_VERSION:
            try:
                return Transcript.from_dict(data)
            except (KeyError, TypeError, ValueError):
                continue
    return None


# --- matériel et disponibilité ---------------------------------------------------------


@dataclass
class Hardware:
    device: str  # "cuda" | "mps" | "cpu"
    name: str
    memory_gb: float | None
    compute_type: str
    torch_version: str | None = None
    warnings: list[str] = field(default_factory=list)

    def to_dict(self) -> dict:
        return asdict(self)


_hardware: Hardware | None = None


def detect_hardware() -> Hardware:
    """Appareil de calcul : carte NVIDIA, puce graphique d'un Mac (MPS), sinon processeur en float32."""
    global _hardware
    if _hardware is not None:
        return _hardware
    try:
        import torch
    except ImportError:
        _hardware = Hardware("cpu", platform.processor() or "processeur", None, "float32")
        return _hardware
    hw_warnings: list[str] = []
    if torch.cuda.is_available():
        props = torch.cuda.get_device_properties(0)
        memory = props.total_memory / 2**30
        major, minor = torch.cuda.get_device_capability(0)
        arch = f"sm_{major}{minor}"
        if arch not in torch.cuda.get_arch_list():
            hw_warnings.append(
                f"Cette version de PyTorch ne gère pas votre carte ({props.name}, {arch}). "
                "Pour une RTX série 50, il faut PyTorch compilé pour CUDA 12.8 ou plus : "
                "relancez le lanceur d'Euhpen."
            )
        _hardware = Hardware("cuda", props.name, round(memory, 1), "float16", torch.__version__, hw_warnings)
    elif getattr(torch.backends, "mps", None) is not None and torch.backends.mps.is_available():
        _hardware = Hardware("mps", f"Apple {platform.machine()} (Metal)", None, "float16", torch.__version__)
    else:
        if shutil.which("nvidia-smi"):
            hw_warnings.append(
                f"Une carte NVIDIA est présente, mais PyTorch ({torch.__version__}) ne peut pas s'en servir : "
                "le calcul se fera sur le processeur, bien plus lentement. Relancez le lanceur d'Euhpen."
            )
        name = platform.processor() or "processeur"
        _hardware = Hardware("cpu", name, None, "float32", torch.__version__, hw_warnings)
    return _hardware


def hesitations_available() -> tuple[bool, str]:
    """La détection des hésitations est-elle installée ? (oui/non, explication)."""
    import importlib.util

    missing = [m for m in ("torch", "transformers", "accelerate") if importlib.util.find_spec(m) is None]
    if missing:
        return False, (
            "La détection des hésitations n'est pas installée (manque : "
            + ", ".join(missing)
            + "). Relancez le lanceur d'Euhpen et acceptez son installation."
        )
    return True, "prête"


def _snapshot_target(model: str) -> dict[str, str]:
    """Dépôt Hugging Face du modèle et sa révision figée."""
    repo, revision = OPENAI_ENCODERS[model]
    return {"repo_id": repo, "revision": revision}


def local_model_path(model: str = WHISPER_MODEL) -> str | None:
    """Dossier local du modèle s'il est entièrement téléchargé (aucun accès réseau)."""
    try:
        from huggingface_hub import snapshot_download

        path = snapshot_download(**_snapshot_target(model), local_files_only=True)
    except Exception:  # noqa: BLE001 - absent du cache ou huggingface_hub manquant
        return None
    # Après un téléchargement interrompu, le dossier existe mais il y manque des fichiers.
    if all(os.path.isfile(os.path.join(path, name)) for name in MODEL_FILES):
        return path
    return None


def ensure_model(model: str = WHISPER_MODEL, progress: Callable[[float, str], None] | None = None) -> str:
    """Télécharge le modèle si besoin et renvoie son dossier local."""
    path = local_model_path(model)
    if path:
        return path
    from tqdm.auto import tqdm as base_tqdm

    class _Tqdm(base_tqdm):  # type: ignore[misc, valid-type]
        def update(self, n=1):
            out = super().update(n)
            if progress and self.total and self.unit == "B":
                progress(min(1.0, self.n / self.total), "téléchargement du modèle")
            return out

    if progress:
        progress(0.0, "téléchargement du modèle")
    return download_snapshot(model, tqdm_class=_Tqdm)


def _revision(path: str) -> str:
    """Révision du modèle : nom du dossier « snapshots/<hash> » du cache Hugging Face."""
    parts = os.path.normpath(path).split(os.sep)
    if len(parts) >= 2 and parts[-2] == "snapshots":
        return parts[-1][:10]
    return "local"


# --- chargement du modèle (une instance gardée en mémoire) ----------------------------

_model_lock = threading.RLock()
_loaded: dict[str, object] = {}


def _build_whisper(path: str, hw: Hardware):
    import torch
    from transformers import pipeline

    return pipeline(
        "automatic-speech-recognition",
        model=path,
        torch_dtype=torch.float16 if hw.compute_type == "float16" else torch.float32,
        device=hw.device,
        # Les horodatages des mots viennent de l'attention croisée, que cette implémentation expose.
        model_kwargs={"attn_implementation": "eager"},
    )


def load_pipeline(path: str, hw: Hardware):
    """Whisper chargé sur ``hw``. Une seule instance : l'encodeur de la tête est le sien."""
    key = f"{path}|{hw.device}|{hw.compute_type}"
    if key in _loaded:
        return _loaded[key]
    os.environ["HF_HUB_OFFLINE"] = "1"  # le modèle est local : aucun accès réseau
    _quiet_libraries()
    _loaded.clear()  # une seule instance à la fois (mémoire graphique)
    model = _build_whisper(path, hw)
    _loaded[key] = model
    return model


def loaded_device() -> str | None:
    """Appareil sur lequel se trouve réellement le modèle chargé (``cuda:0``, ``cpu``…)."""
    for pipe in _loaded.values():
        return str(next(pipe.model.parameters()).device)
    return None


def _quiet_libraries() -> None:
    warnings.filterwarnings("ignore", module="transformers")
    warnings.filterwarnings("ignore", category=UserWarning, module="torch")
    try:
        from transformers.utils import logging as hf_logging

        hf_logging.set_verbosity_error()
        hf_logging.disable_progress_bar()
    except Exception:  # noqa: BLE001
        pass


def _whisper_words(chunks: list[dict], offset: float) -> list[SimpleNamespace]:
    """Mots d'une fenêtre, en secondes depuis le début du fichier. Whisper coupe « l'idée » en deux."""
    words: list[SimpleNamespace] = []
    for chunk in chunks:
        text = chunk["text"].strip()
        start, end = chunk["timestamp"]
        if start is None or end is None:
            continue
        if words and text.startswith(("'", "’")):
            words[-1].word += text
            words[-1].end = offset + end
        else:
            words.append(SimpleNamespace(word=text, start=offset + start, end=offset + end, probability=None))
    return words


def _run_whisper(pipe, audio, language: str, on_chunk: Callable[[float], None] | None):
    """Whisper d'origine, mot à mot, sans probabilité par mot.

    Le fichier est lu par fenêtres qui se recouvrent, et chaque mot vient de la fenêtre où il est
    le plus loin des bords. Le découpage du pipeline de transformers (``chunk_length_s``) n'est pas
    utilisé : avec les horodatages par mot, il écrit deux fois les mots du recouvrement.
    """
    starts = window_starts(audio.size)
    length = int(WINDOW * SAMPLE_RATE)
    limits = [0.0, *((a + b + length) / 2 / SAMPLE_RATE for a, b in zip(starts, starts[1:])), float("inf")]
    words: list[SimpleNamespace] = []
    for k, start in enumerate(starts):
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            result = pipe(
                audio[start : start + length],
                return_timestamps="word",
                generate_kwargs={"language": language, "task": "transcribe"},
            )
        window = _whisper_words(result["chunks"], start / SAMPLE_RATE)
        words += [w for w in window if limits[k] <= (w.start + w.end) / 2 < limits[k + 1]]
        if on_chunk:
            on_chunk((k + 1) / len(starts))
    return SimpleNamespace(words=words)


def transcribe(
    job: AudioJob,
    language: str,
    model: str = WHISPER_HEAD_MODEL,
    *,
    use_cache: bool = True,
    progress: Callable[[float, str], None] | None = None,
) -> tuple[Transcript, bool]:
    """Transcrit un fichier. Renvoie (transcription, venait_du_cache).

    ``language`` est toujours passé au modèle : pas de détection automatique.
    """
    global _hardware
    if not language:
        raise ValueError("choisissez la langue parlée dans le fichier.")
    ok, message = hesitations_available()
    if not ok:
        raise RuntimeError(message)
    if model.endswith(HEAD_SUFFIX):
        from .finetune.pipeline import transcribe_with_head

        return transcribe_with_head(job, language, model, use_cache=use_cache, progress=progress)

    if use_cache:
        cached = cached_transcript(job.cache_dir, model, language)
        if cached is not None:
            return cached, True

    def report(fraction: float, label: str) -> None:
        if progress:
            progress(fraction, label)

    path = ensure_model(model, report)
    revision = _revision(path)
    hw = detect_hardware()
    audio = job.samples()

    with _model_lock:
        report(0.0, "chargement du modèle")
        started = time.perf_counter()
        device = hw.device
        try:
            m = load_pipeline(path, hw)
            report(0.0, "transcription")
            result = _run_whisper(m, audio, language, lambda f: report(f, "transcription"))
        except Exception as exc:  # noqa: BLE001
            if hw.device != "mps":
                raise
            # Repli sur le processeur si Metal échoue (opération non prise en charge…).
            logging.getLogger(__name__).warning("Échec sur MPS (%s) : repli sur le processeur.", exc)
            _hardware = Hardware("cpu", hw.name, None, "float32", hw.torch_version)
            device = "cpu"
            m = load_pipeline(path, _hardware)
            result = _run_whisper(m, audio, language, lambda f: report(f, "transcription (processeur)"))
        elapsed = time.perf_counter() - started

    words = [
        Word(w.word, float(w.start), float(w.end),
             None if w.probability is None else float(w.probability), classify(w.word))
        for w in (result.words or [])
        if w.start is not None and w.end is not None
    ]
    transcript = Transcript(
        model=model,
        revision=revision,
        language=language,
        words=words,
        device=device,
        processing_time=elapsed,
        created=datetime.now(timezone.utc).isoformat(timespec="seconds"),
    )
    write_json(_transcript_path(job.cache_dir, model, revision, language), transcript.to_dict())
    report(1.0, "terminé")
    return transcript, False
