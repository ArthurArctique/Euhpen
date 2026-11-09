"""Entraînement de la tête sur des états gelés, seuils calés sur la validation uniquement."""

from __future__ import annotations

import random
from dataclasses import dataclass, replace
from pathlib import Path

import numpy as np

from .corpus import HESITATION, HOP, IGNORE, LABELS, LENGTHENING, SPEECH, LabelledSample
from .corrections import KEEP, Correction, is_label, read_corrections
from .encoder import WHISPER_MODEL
from .head import build_network, predict
from .holdout import refuse_holdout
from .spans import DetectionParams, Score, frames_to_spans, match_events

CROP = 500  # 10 s
THRESHOLDS = tuple(round(x, 2) for x in np.arange(0.2, 0.91, 0.05))


@dataclass
class Example:
    id: str
    features: np.ndarray  # projeté en mémoire
    labels: np.ndarray
    events: dict


def load_examples(samples: list[LabelledSample], feature_paths: dict[str, Path]) -> list[Example]:
    """Exemples d'entraînement ou de validation. Refuse tout audio réservé à l'évaluation."""
    refuse_holdout(s.audio for s in samples)
    examples = []
    for s in samples:
        features = np.load(feature_paths[s.id], mmap_mode="r")
        n = min(features.shape[0], s.labels.size)
        examples.append(Example(s.id, features[:n], s.labels[:n], s.events))
    return examples


def _device() -> str:
    import torch

    if torch.cuda.is_available():
        return "cuda"
    return "mps" if torch.backends.mps.is_available() else "cpu"


def _class_weights(examples: list[Example]) -> np.ndarray:
    counts = np.zeros(len(LABELS))
    for e in examples:
        for k in range(len(LABELS)):
            counts[k] += int((e.labels == k).sum())
    weights = 1.0 / np.sqrt(np.maximum(counts, 1.0))
    return weights / weights.mean()


def _batch(examples: list[Example], size: int, rng: random.Random, lengths: list[int]):
    xs, ys = [], []
    for e in rng.choices(examples, weights=lengths, k=size):
        start = rng.randint(0, max(0, e.labels.size - CROP))
        x = np.asarray(e.features[start : start + CROP], dtype=np.float32)
        y = e.labels[start : start + CROP]
        if y.size < CROP:
            x = np.pad(x, ((0, CROP - y.size), (0, 0)))
            y = np.pad(y, (0, CROP - y.size), constant_values=IGNORE)
        xs.append(x)
        ys.append(y)
    return np.stack(xs), np.stack(ys).astype(np.int64)


def event_score(examples: list[Example], outputs: list[np.ndarray], params: DetectionParams, kind: str) -> Score:
    total = Score(0, 0, 0)
    for e, probabilities in zip(examples, outputs):
        predicted = [(s.start, s.end) for s in frames_to_spans(probabilities, params) if s.kind == kind]
        total += match_events(predicted, e.events.get(kind, []))
    return total


def calibrate(examples: list[Example], outputs: list[np.ndarray]) -> tuple[DetectionParams, dict]:
    """Seuils maximisant le F1 par événement. À n'appeler que sur la validation."""
    params = DetectionParams()
    report = {}
    for kind, field_name in (("hesitation", "hesitation_threshold"), ("lengthening", "lengthening_threshold")):
        best = None
        for threshold in THRESHOLDS:
            trial = replace(params, **{field_name: float(threshold)})
            score = event_score(examples, outputs, trial, kind)
            key = (round(score.f1, 4), float(threshold))
            if best is None or key > best[0]:
                best = (key, float(threshold), score)
        params = replace(params, **{field_name: best[1]})
        report[kind] = {"threshold": best[1], "validation": best[2].row()}
    return params, report


def _frame_f1(examples: list[Example], outputs: list[np.ndarray]) -> float:
    """Moyenne des F1 par trame pour les deux classes utiles."""
    values = []
    for k in (HESITATION, LENGTHENING):
        tp = fp = fn = 0
        for e, probabilities in zip(examples, outputs):
            mask = e.labels != IGNORE
            predicted = probabilities.argmax(axis=1)[mask]
            truth = e.labels[mask]
            tp += int(((predicted == k) & (truth == k)).sum())
            fp += int(((predicted == k) & (truth != k)).sum())
            fn += int(((predicted != k) & (truth == k)).sum())
        values.append(2 * tp / (2 * tp + fp + fn) if tp else 0.0)
    return float(np.mean(values))


def train_head(
    train: list[Example],
    validation: list[Example],
    *,
    epochs: int = 40,
    patience: int = 8,
    batch_size: int = 32,
    seed: int = 7,
    log=print,
):
    """Renvoie (réseau sur le processeur, seuils, résumé)."""
    import torch
    from torch import nn

    random.seed(seed)
    np.random.seed(seed)
    torch.manual_seed(seed)
    rng = random.Random(seed)
    device = _device()
    network = build_network().to(device)
    weights = torch.tensor(_class_weights(train), dtype=torch.float32, device=device)
    criterion = nn.CrossEntropyLoss(weight=weights, ignore_index=IGNORE)
    optimizer = torch.optim.AdamW(network.parameters(), lr=1e-3, weight_decay=1e-2)
    lengths = [e.labels.size for e in train]
    steps = max(1, sum(lengths) // (CROP * batch_size))
    scheduler = torch.optim.lr_scheduler.OneCycleLR(optimizer, max_lr=2e-3, total_steps=epochs * steps)

    best_score, best_state, stale = -1.0, None, 0
    history = []
    for epoch in range(1, epochs + 1):
        network.train()
        running = 0.0
        for _ in range(steps):
            x, y = _batch(train, batch_size, rng, lengths)
            logits = network(torch.from_numpy(x).to(device))
            loss = criterion(logits.transpose(1, 2), torch.from_numpy(y).to(device))
            optimizer.zero_grad()
            loss.backward()
            optimizer.step()
            scheduler.step()
            running += loss.item()
        network.eval()
        outputs = [predict(network, e.features, device) for e in validation]
        score = _frame_f1(validation, outputs)
        history.append({"epoch": epoch, "loss": round(running / steps, 4), "validation_frame_f1": round(score, 4)})
        log(f"époque {epoch:02d}  perte {running / steps:.4f}  F1 validation par trame {score:.3f}")
        if score > best_score:
            best_score, stale = score, 0
            best_state = {k: v.detach().cpu().clone() for k, v in network.state_dict().items()}
        else:
            stale += 1
            if stale >= patience:
                log("arrêt anticipé")
                break

    network.load_state_dict(best_state)
    network = network.to("cpu").eval()
    outputs = [predict(network, e.features) for e in validation]
    params, thresholds = calibrate(validation, outputs)
    summary = {
        "epochs_run": len(history),
        "best_validation_frame_f1": round(best_score, 4),
        "thresholds": thresholds,
        "train_ids": [e.id for e in train],
        "validation_ids": [e.id for e in validation],
        "history": history,
    }
    return network, params, summary


def correction_samples(entries: list[Correction], feature_lengths: dict[str, int]) -> list[LabelledSample]:
    """Corrections → exemples partiels : seules les trames corrigées portent une étiquette."""
    grouped: dict[str, list[Correction]] = {}
    for entry in entries:
        if is_label(entry) and entry.fingerprint in feature_lengths:
            grouped.setdefault(entry.fingerprint, []).append(entry)
    samples = []
    for key, items in grouped.items():
        labels = np.full(feature_lengths[key], IGNORE, dtype=np.int8)
        for e in items:
            value = SPEECH if e.action == KEEP else HESITATION
            labels[int(round(e.start / HOP)) : int(round(e.end / HOP))] = value
        samples.append(LabelledSample(key, items[0].source, labels))
    return samples


def train_from_rhapsodie(
    corpus: str,
    model: str = WHISPER_MODEL,
    folder: Path | None = None,
    *,
    epochs: int = 40,
    use_corrections: bool = True,
    log=print,
) -> Path:
    """Du corpus à la tête enregistrée. Le test interne n'est ni lu ni utilisé ici."""
    from .corpus import rhapsodie_samples, split_by_hash
    from .features import ensure_features
    from .head import save_head

    samples = rhapsodie_samples(corpus)
    refuse_holdout(s.audio for s in samples)
    split = split_by_hash([s.id for s in samples])
    used = [s for s in samples if s.id not in split["test"]]
    paths = ensure_features(((s.id, s.audio) for s in used), model, log=log)
    by_id = {s.id: s for s in used}
    train_set = load_examples([by_id[i] for i in split["train"]], paths)
    validation_set = load_examples([by_id[i] for i in split["validation"]], paths)
    if use_corrections:
        entries = [e for e in read_corrections() if is_label(e)]
        sources = {e.fingerprint: e.source for e in entries}
        extra_paths = ensure_features(sources.items(), model, log=log)
        lengths = {k: np.load(p, mmap_mode="r").shape[0] for k, p in extra_paths.items()}
        extra = correction_samples(entries, lengths)
        train_set += load_examples(extra, extra_paths)
        log(f"{len(extra)} fichier(s) corrigé(s) dans l'interface ajouté(s) à l'entraînement")
    log(f"{len(train_set)} enregistrements d'entraînement, {len(validation_set)} de validation")
    network, params, summary = train_head(train_set, validation_set, epochs=epochs, log=log)
    return save_head(network, params, model, summary, folder)
