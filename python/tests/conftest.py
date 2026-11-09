import pytest


@pytest.fixture(autouse=True)
def isolated_cache(tmp_path, monkeypatch):
    """Chaque test a son propre cache et ses propres réglages."""
    monkeypatch.setenv("EUHPEN_CACHE_DIR", str(tmp_path / "cache"))
    monkeypatch.setenv("EUHPEN_CONFIG_DIR", str(tmp_path / "config"))
