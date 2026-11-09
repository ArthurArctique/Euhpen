"""Prototype de détection des hésitations : uniquement les fonctions pures, sans modèle."""

import os
from pathlib import Path

import numpy as np
import pytest

from euhpen.finetune import head as head_module
from euhpen.finetune import pipeline
from euhpen.finetune.corpus import (
    HESITATION,
    IGNORE,
    LENGTHENING,
    SILENCE,
    SPEAKER_GROUPS,
    SPEECH,
    label_rhapsodie,
    lengthening_excess,
    read_textgrid,
    split_by_hash,
)
from euhpen.finetune.corrections import CUT, KEEP, UNDO, Correction, is_label, log_correction, read_corrections
from euhpen.finetune.head import head_dir, load_head, trained_dir
from euhpen.finetune.holdout import HOLDOUT_NAMES, is_holdout, refuse_holdout
from euhpen.finetune.spans import DetectionParams, Score, Span, frames_to_spans, insert_spans, match_events
from euhpen.transcribe import HESITATION as H_KIND
from euhpen.transcribe import (
    WHISPER_HEAD_MODEL,
    WHISPER_MODEL,
    WORD,
    AudioJob,
    Transcript,
    Word,
    cached_transcript,
    transcribe,
)


def tier(name, intervals):
    body = "".join(
        f'\n        intervals [{i + 1}]:\n            xmin = {a}\n            xmax = {b}\n            text = "{t}"'
        for i, (a, b, t) in enumerate(intervals)
    )
    return f'\n    item [1]:\n        class = "IntervalTier"\n        name = "{name}"{body}\n'


def write_grid(path, tiers):
    body = "".join(tier(n, v) for n, v in tiers.items())
    path.write_text('File type = "ooTextFile"\nitem []:' + body, encoding="utf-8")


def grid(tmp_path, hes="H"):
    path = tmp_path / "x-Pro.TextGrid"
    write_grid(
        path,
        {
            "phone": [
                (0.0, 0.2, "_"), (0.2, 0.3, "l"), (0.3, 0.9, "@"), (0.9, 1.0, "b"), (1.0, 1.1, "a"), (1.1, 1.5, "2"),
            ],
            "syllabe": [(0.0, 0.2, "_"), (0.2, 0.9, "l@"), (0.9, 1.1, "ba"), (1.1, 1.5, "2")],
            "hes": [(0.0, 0.2, ""), (0.2, 0.9, hes), (0.9, 1.1, ""), (1.1, 1.5, "")],
            "word": [(0.0, 0.2, "_"), (0.2, 0.9, "le"), (0.9, 1.1, "ba"), (1.1, 1.5, "euh")],
        },
    )
    return path


def test_textgrid_tiers_are_read(tmp_path):
    tiers = read_textgrid(grid(tmp_path))
    assert tiers["phone"][2] == (0.3, 0.9, "@")
    assert [t[2] for t in tiers["word"]] == ["_", "le", "ba", "euh"]


def test_labels_cover_filler_and_only_the_excess_of_the_lengthened_vowel(tmp_path):
    sample = label_rhapsodie(grid(tmp_path), tmp_path / "x.wav", "x")
    labels = sample.labels
    assert labels[5] == SILENCE
    assert labels[12] == SPEECH  # /l/
    assert labels[20] == SPEECH  # début de la voyelle : la durée normale reste de la parole
    assert labels[35] == LENGTHENING
    assert labels[60] == HESITATION
    assert sample.events["hesitation"] == [(1.1, 1.5)]
    assert len(sample.events["lengthening"]) == 1


def test_doubtful_syllables_are_ignored(tmp_path):
    labels = label_rhapsodie(grid(tmp_path, hes="?"), tmp_path / "x.wav", "x").labels
    assert labels[20] == IGNORE


def test_short_vowel_is_not_a_lengthening():
    assert lengthening_excess([(0.0, 0.1, "@")], 0.09, 0.07) is None


def test_split_never_separates_recordings_sharing_a_speaker():
    ids = sorted({i for g in SPEAKER_GROUPS for i in g} | {f"Rhap-Z{n:04d}" for n in range(60)})
    split = split_by_hash(ids)
    part_of = {i: name for name, members in split.items() for i in members}
    assert sorted(part_of) == ids
    for group in SPEAKER_GROUPS:
        assert len({part_of[i] for i in group}) == 1


def test_holdout_is_refused_by_name_and_by_content(tmp_path):
    named = tmp_path / "Test audio euhh.mp3"
    named.write_bytes(b"x")
    assert named.stem.lower() in HOLDOUT_NAMES
    assert is_holdout(named)
    with pytest.raises(ValueError):
        refuse_holdout([named])
    assert not is_holdout(tmp_path / "autre.wav")


def test_holdout_is_never_journaled(tmp_path):
    named = tmp_path / "Test audio euhh.mp3"
    named.write_bytes(b"x")
    journal = tmp_path / "j.jsonl"
    assert log_correction(Correction("f", str(named), KEEP, 1.0, 2.0, "hesitation"), journal) is False
    assert not journal.exists()


def test_undo_removes_the_correction_covering_that_time(tmp_path):
    media = tmp_path / "a.wav"
    media.write_bytes(b"x")
    journal = tmp_path / "j.jsonl"
    log_correction(Correction("f", str(media), KEEP, 1.0, 2.0, "hesitation"), journal)
    log_correction(Correction("f", str(media), CUT, 3.0, 3.4, "word"), journal)
    log_correction(Correction("f", str(media), UNDO, 1.5, 1.5), journal)
    assert [e.action for e in read_corrections(journal)] == [CUT]


def test_only_unambiguous_corrections_become_labels():
    assert is_label(Correction("f", "a", KEEP, 1, 2, "hesitation"))
    assert not is_label(Correction("f", "a", KEEP, 1, 2, "silence"))
    assert is_label(Correction("f", "a", CUT, 1, 1.4, "word"))
    assert not is_label(Correction("f", "a", CUT, 1, 1.4, "passage"))
    assert not is_label(Correction("f", "a", CUT, 1, 4, "word"))


def probabilities(frames, spans):
    p = np.zeros((frames, 4))
    p[:, SPEECH] = 1.0
    for a, b, k in spans:
        p[a:b] = 0
        p[a:b, k] = 1.0
    return p


def test_frames_become_spans_of_the_right_kind():
    p = probabilities(100, [(10, 30, HESITATION), (50, 60, LENGTHENING)])
    spans = frames_to_spans(p, DetectionParams())
    found = [(s.kind, round(s.start, 2), round(s.end, 2)) for s in spans]
    assert found == [("hesitation", 0.2, 0.6), ("lengthening", 1.0, 1.2)]


def test_too_short_detections_are_dropped_and_close_ones_merged():
    params = DetectionParams(median_frames=1)
    assert frames_to_spans(probabilities(100, [(10, 13, HESITATION)]), params) == []
    merged = frames_to_spans(probabilities(100, [(10, 20, HESITATION), (22, 30, HESITATION)]), params)
    assert len(merged) == 1 and merged[0].end == pytest.approx(0.6)


def transcript(words):
    return Transcript(WHISPER_MODEL, "r", "fr", words)


def test_span_inside_a_word_splits_it():
    t = transcript([Word("donc", 1.0, 2.0, 0.9, WORD)])
    out = insert_spans(t, [Span(1.4, 1.8, "lengthening", 0.8)]).words
    assert [(w.text, w.kind) for w in out] == [("donc", WORD), ("[étirement]", H_KIND), ("…", WORD)]
    assert (out[0].end, out[1].start, out[1].end, out[2].start) == (1.4, 1.4, 1.8, 1.8)


def test_span_at_the_end_of_a_word_keeps_no_tail():
    out = insert_spans(transcript([Word("le", 1.0, 2.0, 0.9, WORD)]), [Span(1.3, 1.99, "lengthening", 0.8)]).words
    assert [w.text for w in out] == ["le", "[étirement]"]


@pytest.mark.parametrize(("kind", "marker"), [("hesitation", "[euh]"), ("lengthening", "[étirement]")])
def test_each_detection_writes_its_own_marker(kind, marker):
    word = insert_spans(transcript([]), [Span(1.0, 1.4, kind, 0.8)]).words[0]
    assert word.text == marker and word.kind == H_KIND
    assert Word.from_list(word.to_list()).kind == H_KIND


def test_span_already_covered_by_a_model_token_is_skipped():
    t = transcript([Word("[UH]", 1.0, 1.5, None, H_KIND)])
    assert len(insert_spans(t, [Span(1.1, 1.4, "hesitation", 0.9)]).words) == 1


def test_matching_requires_half_of_the_shorter_interval():
    assert match_events([(0.0, 1.0)], [(0.4, 1.4)]).true_positive == 1
    assert match_events([(0.0, 1.0)], [(0.9, 1.9)]).true_positive == 0


def test_matching_is_one_to_one():
    score = match_events([(0.0, 1.0), (0.1, 0.9)], [(0.0, 1.0)])
    assert (score.true_positive, score.false_positive, score.false_negative) == (1, 1, 0)
    assert Score(1, 1, 1).f1 == pytest.approx(0.5)


# --- Whisper + tête dans l'application ---------------------------------------------------


def install_head(content=b"poids", model=WHISPER_MODEL):
    """Comme une tête réentraînée : jamais dans les poids livrés avec le paquet."""
    folder = trained_dir(model)
    folder.mkdir(parents=True, exist_ok=True)
    (folder / "head.pt").write_bytes(content)
    (folder / "head.json").write_text("{}", encoding="utf-8")
    return folder


@pytest.fixture
def fake_engine(monkeypatch):
    """La transcription de base et la tête sont remplacées : ni modèle, ni PyTorch."""
    calls = []
    words = [Word("donc", 1.0, 2.0, 0.9, WORD)]
    base = Transcript(WHISPER_MODEL, "abc123", "fr", words, device="mps", processing_time=2.0)

    def fake_transcribe(job, language, model, *, use_cache=True, progress=None):
        calls.append(model)
        return base, False

    monkeypatch.setattr(pipeline, "transcribe", fake_transcribe)
    spans = [Span(1.5, 2.0, "lengthening", 0.9)]
    monkeypatch.setattr(pipeline, "detect_audio", lambda audio, model, progress=None: spans)
    return calls


def job_for_cache(seconds=1):
    folder = Path(os.environ["EUHPEN_CACHE_DIR"]) / "job"
    folder.mkdir(parents=True, exist_ok=True)
    audio = folder / "audio.f32"
    np.zeros(16000 * seconds, np.float32).tofile(audio)
    return AudioJob(str(audio), folder)


def test_head_model_adds_detections_and_is_cached(fake_engine):
    install_head()
    analysis = job_for_cache()
    result, cached = transcribe(analysis, "fr", WHISPER_HEAD_MODEL)
    assert not cached and result.model == WHISPER_HEAD_MODEL and fake_engine == [WHISPER_MODEL]
    assert [w.text for w in result.words] == ["donc", "[étirement]"]

    again, cached = transcribe(analysis, "fr", WHISPER_HEAD_MODEL)
    assert cached and again.revision == result.revision and fake_engine == [WHISPER_MODEL]


def test_new_markers_do_not_reuse_old_transcripts(fake_engine, monkeypatch):
    install_head()
    analysis = job_for_cache()
    transcribe(analysis, "fr", WHISPER_HEAD_MODEL)
    monkeypatch.setitem(pipeline.TOKENS, "lengthening", "[allongement]")
    assert not transcribe(analysis, "fr", WHISPER_HEAD_MODEL)[1]


def test_retrained_head_does_not_reuse_old_detections(fake_engine):
    install_head(b"poids 1")
    analysis = job_for_cache()
    transcribe(analysis, "fr", WHISPER_HEAD_MODEL)
    install_head(b"poids 2")
    assert cached_transcript(analysis.cache_dir, WHISPER_HEAD_MODEL, "fr") is None
    assert not transcribe(analysis, "fr", WHISPER_HEAD_MODEL)[1]


def test_head_model_without_weights_is_a_clear_error(fake_engine, monkeypatch, tmp_path):
    monkeypatch.setattr(head_module, "BUNDLED", tmp_path)
    assert cached_transcript(job_for_cache().cache_dir, WHISPER_HEAD_MODEL, "fr") is None
    with pytest.raises(RuntimeError, match="introuvable"):
        transcribe(job_for_cache(), "fr", WHISPER_HEAD_MODEL)


def test_bundled_head_needs_no_training():
    pytest.importorskip("torch")
    assert head_dir() == head_module.BUNDLED / WHISPER_MODEL
    assert pipeline.head_revision(WHISPER_HEAD_MODEL) is not None
    _, params, meta = load_head()
    assert meta["model"] == WHISPER_MODEL
    assert (params.hesitation_threshold, params.lengthening_threshold) == (0.70, 0.45)


def test_retrained_head_replaces_the_bundled_one():
    folder = install_head()
    assert head_dir() == folder != head_module.BUNDLED / WHISPER_MODEL


def test_encoder_is_fetched_at_its_frozen_revision(monkeypatch):
    hub = pytest.importorskip("huggingface_hub")
    from euhpen.finetune.encoder import OPENAI_ENCODERS, model_path

    seen = []

    def fake_download(repo, revision, max_workers):
        seen.append((repo, revision))
        assert max_workers == 1  # plusieurs fils plantent sous Windows sans liens symboliques
        return "dossier"

    monkeypatch.setattr(hub, "snapshot_download", fake_download)
    assert model_path(WHISPER_MODEL) == "dossier"
    assert seen == [OPENAI_ENCODERS[WHISPER_MODEL]]


def test_interrupted_download_does_not_count_as_downloaded(monkeypatch, tmp_path):
    hub = pytest.importorskip("huggingface_hub")
    from euhpen.transcribe import MODEL_FILES, local_model_path

    monkeypatch.setattr(hub, "snapshot_download", lambda **kwargs: str(tmp_path))
    for name in MODEL_FILES:
        (tmp_path / name).touch()
    assert local_model_path() == str(tmp_path)

    (tmp_path / "config.json").unlink()
    assert local_model_path() is None


class FakePipeline:
    """Le pipeline de transformers : chaque fenêtre rend les mêmes temps, relatifs à son début."""

    def __init__(self):
        self.kwargs = None
        self.sizes = []

    def __call__(self, audio, **kwargs):
        self.kwargs = kwargs
        self.sizes.append(audio.size)
        return {"chunks": [
            {"text": " l", "timestamp": (5.0, 5.1)},
            {"text": "'idée,", "timestamp": (5.1, 6.0)},
            {"text": " euh", "timestamp": (15.0, 15.5)},
            {"text": " recouvrement", "timestamp": (25.0, 26.0)},
            {"text": " sans fin", "timestamp": (29.0, None)},
        ]}


def test_whisper_transcribes_word_by_word(monkeypatch):
    from euhpen import transcribe as tr

    pipe = FakePipeline()
    monkeypatch.setattr(tr, "_build_whisper", lambda path, hw: pipe)
    monkeypatch.setattr(tr, "hesitations_available", lambda: (True, ""))
    monkeypatch.setattr(tr, "ensure_model", lambda model, progress=None: "/hub/snapshots/41f01f3fe87f28c7")
    monkeypatch.setattr(tr, "detect_hardware", lambda: tr.Hardware("cpu", "test", None, "float32"))
    tr._loaded.clear()

    seen = []
    analysis = job_for_cache(40)
    result, cached = transcribe(analysis, "fr", WHISPER_MODEL, progress=lambda f, label: seen.append(f))
    tr._loaded.clear()

    assert not cached and result.model == WHISPER_MODEL and result.revision == "41f01f3fe8"
    # Fichier de 40 s : fenêtres 0-30 s et 10-40 s, limite à 20 s. Chaque instant ne compte qu'une fois.
    assert [(w.text, w.start, w.kind) for w in result.words] == [
        ("l'idée,", 5.0, WORD),
        ("euh", 15.0, H_KIND),
        ("euh", 25.0, H_KIND),
        ("recouvrement", 35.0, WORD),
    ]
    assert all(w.probability is None for w in result.words)
    assert pipe.sizes == [16000 * 30, 16000 * 30]
    assert pipe.kwargs["return_timestamps"] == "word"
    assert pipe.kwargs["generate_kwargs"] == {"language": "fr", "task": "transcribe"}
    assert seen[-3:] == [0.5, 1.0, 1.0]


def test_whisper_head_model_uses_whisper_for_words_and_head(fake_engine):
    install_head()
    result, _ = transcribe(job_for_cache(), "fr", WHISPER_HEAD_MODEL)
    assert result.model == WHISPER_HEAD_MODEL and fake_engine == [WHISPER_MODEL]
    assert [w.text for w in result.words] == ["donc", "[étirement]"]


def test_missing_dependency_is_reported(monkeypatch):
    from importlib import util

    from euhpen import transcribe as tr

    real = util.find_spec
    monkeypatch.setattr(util, "find_spec", lambda name: None if name == "accelerate" else real(name))
    ok, message = tr.hesitations_available()
    assert not ok and "accelerate" in message
