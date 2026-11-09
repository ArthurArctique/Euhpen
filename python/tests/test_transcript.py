import pytest

from euhpen import transcribe as tr
from euhpen.transcribe import Transcript, Word, classify


def words(*items):
    """(texte, début, fin[, probabilité]) → transcription."""
    out = []
    for item in items:
        text, start, end, *prob = item
        out.append(Word(text, start, end, prob[0] if prob else 0.99, classify(text)))
    return Transcript("small", "abc", "fr", out)


@pytest.mark.parametrize(
    "text, kind",
    [
        ("[UH]", "hesitation"), ("[UM]", "hesitation"), ("[um]", "hesitation"), ("[UH],", "hesitation"),
        ("euh", "hesitation"), ("Euh,", "hesitation"), ("heu...", "hesitation"), ("hum", "hesitation"),
        ("[laughter]", "laugh"), ("[Laughs]", "laugh"), ("[cough]", "cough"), ("[throat_clearing]", "cough"),
        ("[breath]", "breath"), ("[music]", "event"), ("heureux", "word"), ("humour", "word"),
        ("Bonjour", "word"), ("tous.", "word"), ("[", "word"),
    ],
)
def test_classify(text, kind):
    assert classify(text) == kind


def test_transcript_roundtrip_and_tokens():
    t = words(("Alors", 0, 0.5), ("[UH]", 0.6, 0.8, 0.97), ("[UH]", 1.0, 1.2), ("[cough]", 2, 2.3, None))
    back = Transcript.from_dict(t.to_dict())
    assert [w.kind for w in back.words] == ["word", "hesitation", "hesitation", "cough"]
    assert back.words[3].probability is None
    assert back.bracket_tokens() == {"[UH]": 2, "[cough]": 1}
    assert back.counts() == {"word": 1, "hesitation": 2, "cough": 1}


def test_transcript_cache_lookup(tmp_path, monkeypatch):
    monkeypatch.setenv("EUHPEN_CACHE_DIR", str(tmp_path))
    t = words(("Bonjour", 0, 0.5))
    from euhpen.cache import write_json

    write_json(tr._transcript_path(tmp_path, "small", "abc", "fr"), t.to_dict())
    assert tr.cached_transcript(tmp_path, "small", "fr").words[0].text == "Bonjour"
    assert tr.cached_transcript(tmp_path, "small", "en") is None
    assert tr.cached_transcript(tmp_path, "turbo", "fr") is None
    assert tr.cached_transcript(tmp_path / "autre", "small", "fr") is None
