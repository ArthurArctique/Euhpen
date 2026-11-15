import os
import re
from pathlib import Path

import numpy as np
import pytest

import euhpen.breaths as breaths
from euhpen.breaths import detect as _  # noqa: F401 - le sous-module, pas la fonction
from euhpen.cache import write_json
from euhpen.transcribe import (
    BREATH,
    BREATH_MODEL,
    BREATH_SUFFIX,
    WHISPER_HEAD_MODEL,
    WHISPER_MODEL,
    WORD,
    AudioJob,
    Transcript,
    Word,
    _revision_pattern,
    _transcript_path,
    cached_transcript,
    classify,
    transcribe,
)

detect = __import__("sys").modules["euhpen.breaths.detect"]
features = __import__("sys").modules["euhpen.breaths.features"]
ROOT = Path(__file__).resolve().parents[2]


# --- caractéristiques ----------------------------------------------------------------------


def zcr_reference(wav, win_length=400, hop_length=160):
    """``zcr_extractor`` des auteurs, tel quel."""
    pad_length = win_length // 2
    wav = np.pad(wav, (pad_length, pad_length), "constant")
    num_frames = 1 + (wav.shape[0] - win_length) // hop_length
    zcrs = np.zeros(num_frames)
    for i in range(num_frames):
        start = i * hop_length
        end = start + win_length
        zcr = np.abs(np.sign(wav[start + 1 : end]) - np.sign(wav[start : end - 1]))
        zcrs[i] = np.sum(zcr) * 0.5 / win_length
    return zcrs.astype(np.float32)


def noisy(seconds=3.0, seed=0):
    rng = np.random.default_rng(seed)
    t = np.arange(int(seconds * 16000)) / 16000
    wav = 0.3 * np.sin(2 * np.pi * 220 * t) * (t > 1) + 0.01 * rng.standard_normal(t.size)
    wav[100:500] = 0.0  # zéros exacts : le signe vaut 0
    return wav.astype(np.float32)


def test_mel_filters_are_those_of_librosa():
    # librosa.filters.mel(sr=16000, n_fft=400, n_mels=128), librosa 0.10.0
    mel = features.mel_filters()
    assert mel.shape == (128, 201) and mel.dtype == np.float32
    assert mel.sum() == pytest.approx(3.1909854412078857, rel=1e-6)
    assert mel[0].sum() == pytest.approx(0.012373986653983593, rel=1e-6)
    assert mel[127].sum() == pytest.approx(0.024936042726039886, rel=1e-6)


def test_zero_crossing_rate_matches_the_authors_loop():
    wav = noisy()
    total = features.frame_count(wav.size)
    out = features.features(wav, 0, total)
    np.testing.assert_allclose(out[2, 0], zcr_reference(wav), atol=1e-7)
    assert (out[2] == out[2, 0]).all()


def test_features_are_normalised_like_one_utterance():
    wav = noisy()
    total = features.frame_count(wav.size)
    out = features.features(wav, 0, total)
    assert out.shape == (3, 128, total) == (3, 128, 1 + wav.size // 160)
    assert out[0].max() == 0.0 and out[0].min() >= -80.0
    np.testing.assert_allclose(out[1, 0], out[0].var(axis=0, ddof=1), rtol=1e-5)


def test_a_window_sees_the_same_frames_as_the_whole_file():
    wav = noisy(seconds=4.0)
    total = features.frame_count(wav.size)
    whole = features.mel_power(features.frames(wav, 0, total))
    part = features.mel_power(features.frames(wav, 150, 100))
    np.testing.assert_allclose(part, whole[150:250], rtol=1e-5, atol=1e-9)
    edge = features.frames(wav, total - 3, 3)  # au-delà de la fin : des zéros
    assert edge.shape == (3, 400) and not edge[-1, 200:].any()


# --- fenêtres et passages ------------------------------------------------------------------


@pytest.mark.parametrize("total", [1, 999, 2000, 2001, 3399, 3400, 3401, 10_000, 123_457])
def test_windows_cover_the_file_and_keep_their_middle(total):
    starts = detect.window_starts(total)
    length = min(detect.WINDOW, total)
    assert starts[0] == 0 and starts[-1] + length == total
    limits = [0, *((a + b + length) // 2 for a, b in zip(starts, starts[1:])), total]
    for k, start in enumerate(starts):
        lo, hi = limits[k], limits[k + 1]
        assert start <= lo < hi <= start + length
        if k > 0:
            assert lo - start >= detect.MARGIN
        if k < len(starts) - 1:
            assert start + length - hi >= detect.MARGIN


def test_breaths_need_21_frames_and_keep_their_peak():
    probabilities = np.zeros(300, dtype=np.float32)
    probabilities[10:30] = 0.9  # 20 trames : trop court
    probabilities[100:121] = 0.07  # 21 trames juste au-dessus du seuil
    probabilities[110] = 0.8
    probabilities[200:260] = 1.0
    found = detect.find_breaths(probabilities, duration=2.55)
    assert [(round(b.start, 3), round(b.end, 3), round(b.probability, 2)) for b in found] == [
        (0.995, 1.205, 0.8),
        (1.995, 2.55, 1.0),
    ]


def test_long_files_are_stitched_from_the_middle_of_each_window(monkeypatch):
    torch = pytest.importorskip("torch")
    total = 7_777

    def position(samples, start, count):  # chaque trame porte son numéro
        return np.broadcast_to(np.arange(start, start + count, dtype=np.float32), (3, 128, count)).copy()

    def model(x, lengths):
        assert (lengths == x.shape[-1]).all()
        index = x[:, 0, 0, :]
        inside = torch.arange(x.shape[-1], dtype=torch.float32)
        edge = torch.minimum(inside, x.shape[-1] - 1 - inside)  # distance au bord de la fenêtre
        return index * 1000 + edge

    monkeypatch.setattr(detect, "features", position)
    samples = np.zeros((total - 1) * 160, dtype=np.float32)
    out = detect.frame_probabilities(samples, model, "cpu")
    index, edge = np.divmod(out, 1000)
    assert (index == np.arange(total)).all()
    t = np.arange(total)
    assert (edge >= np.minimum(detect.MARGIN, np.minimum(t, total - 1 - t))).all()


# --- transcription -------------------------------------------------------------------------


def test_the_token_is_a_breath():
    assert classify(breaths.TOKEN) == BREATH
    assert classify("[Respiration]") == BREATH


def test_revision_is_the_same_in_rust():
    source = (ROOT / "src" / "transcript.rs").read_text(encoding="utf-8")
    match = re.search(r'pub const BREATH_REVISION: &str = "([^"]+)";', source)
    assert match and match.group(1) == breaths.BREATH_REVISION
    assert re.search(rf'pub const BREATH_MODEL: &str = "{BREATH_MODEL}";', source)


def test_cache_patterns():
    rev = breaths.BREATH_REVISION
    assert _revision_pattern(BREATH_MODEL) == f"r{rev}"
    assert _revision_pattern(WHISPER_MODEL + BREATH_SUFFIX) == rf"\w+r{rev}"
    head = _revision_pattern(WHISPER_HEAD_MODEL)
    assert head is not None and _revision_pattern(WHISPER_HEAD_MODEL + BREATH_SUFFIX) == f"{head}r{rev}"


@pytest.fixture
def fake_detector(monkeypatch):
    """Ni Whisper, ni réseau : une transcription et des respirations toutes faites."""
    calls = []
    base = Transcript(WHISPER_MODEL, "abc123", "fr", [Word("donc", 1.0, 2.0, None, WORD)], device="mps")

    def fake_transcribe(job, language, model, *, use_cache=True, progress=None):
        calls.append(model)
        return base, False

    def fake_detect(samples, device, progress=None, label=""):
        calls.append(BREATH_MODEL)
        return [detect.Breath(0.2, 0.6, 0.97), detect.Breath(1.9, 2.4, 0.4)], device

    monkeypatch.setattr(breaths, "transcribe", fake_transcribe)
    monkeypatch.setattr(breaths, "detect_breaths", fake_detect)
    monkeypatch.setattr(breaths, "ensure_weights", lambda progress=None: "respiro-en.pt")
    monkeypatch.setattr(breaths, "_device", lambda: "cpu")
    return calls


def job_for_cache(seconds=3):
    folder = Path(os.environ["EUHPEN_CACHE_DIR"]) / "job"
    folder.mkdir(parents=True, exist_ok=True)
    audio = folder / "audio.f32"
    np.zeros(16000 * seconds, np.float32).tofile(audio)
    return AudioJob(str(audio), folder)


def test_breaths_alone(fake_detector):
    job = job_for_cache()
    result, cached = transcribe(job, "fr", BREATH_MODEL)
    assert not cached and fake_detector == [BREATH_MODEL]
    assert [(w.text, w.kind, w.probability) for w in result.words] == [
        ("[respiration]", BREATH, 0.97),
        ("[respiration]", BREATH, 0.4),
    ]
    again, cached = transcribe(job, "fr", BREATH_MODEL)
    assert cached and again.words == result.words and fake_detector == [BREATH_MODEL]


def test_breaths_are_added_to_the_words_and_each_part_is_cached(fake_detector):
    job = job_for_cache()
    model = WHISPER_MODEL + BREATH_SUFFIX
    result, cached = transcribe(job, "fr", model)
    assert not cached and fake_detector == [WHISPER_MODEL, BREATH_MODEL]
    assert result.model == model and result.revision == f"abc123r{breaths.BREATH_REVISION}"
    assert [w.text for w in result.words] == ["[respiration]", "donc", "[respiration]"]
    assert cached_transcript(job.cache_dir, model, "fr").words == result.words
    assert transcribe(job, "fr", model)[1]
    assert transcribe(job, "fr", BREATH_MODEL)[1]  # les respirations seules aussi
    assert fake_detector == [WHISPER_MODEL, BREATH_MODEL]


def test_a_new_detection_does_not_reuse_old_breaths(fake_detector, monkeypatch):
    job = job_for_cache()
    old = Transcript(BREATH_MODEL, "r0000000v0", "fr", [Word("[respiration]", 0.1, 0.5, 1.0, BREATH)])
    write_json(_transcript_path(job.cache_dir, BREATH_MODEL, old.revision, "fr"), old.to_dict())
    assert cached_transcript(job.cache_dir, BREATH_MODEL, "fr") is None
    assert not transcribe(job, "fr", BREATH_MODEL)[1]
