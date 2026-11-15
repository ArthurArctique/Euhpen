"""Entrée du réseau Respiro-en, en numpy seul : même calcul que ``feature_extractor`` des auteurs.

Une colonne toutes les 10 ms (trame de 25 ms centrée sur l'instant), trois canaux de 128 lignes :

1. le spectrogramme mel en décibels (``librosa.feature.melspectrogram`` puis
   ``librosa.power_to_db(ref=np.max)``) : 0 dB au plus fort du passage, plancher à −80 dB ;
2. la variance de ces 128 valeurs (non biaisée, comme ``torch.var``), recopiée sur les 128 lignes ;
3. le taux de passages par zéro de la trame, recopié de même.

librosa n'est pas une dépendance d'Euhpen : la transformée de Fourier à court terme (fenêtre de
Hann, bords complétés par des zéros) et le banc de filtres mel (échelle et normalisation
« slaney ») sont refaits ici, avec les mêmes formules.
"""

from __future__ import annotations

import numpy as np

SAMPLE_RATE = 16_000
N_FFT = 400  # 25 ms
HOP = 160  # 10 ms
N_MELS = 128
AMIN = 1e-10
TOP_DB = 80.0
FRAME_SECONDS = HOP / SAMPLE_RATE


def frame_count(samples: int) -> int:
    """Nombre de trames de librosa (``center=True``) : une toutes les 10 ms, la première à 0 s."""
    return 1 + samples // HOP


# --- banc de filtres mel (librosa.filters.mel, htk=False, norm="slaney") -----------------

_F_SP = 200.0 / 3  # Hz par mel sous 1 kHz
_MIN_LOG_HZ = 1000.0
_MIN_LOG_MEL = _MIN_LOG_HZ / _F_SP
_LOGSTEP = np.log(6.4) / 27.0


def hz_to_mel(hz: np.ndarray) -> np.ndarray:
    hz = np.asanyarray(hz, dtype=np.float64)
    mel = hz / _F_SP
    high = hz >= _MIN_LOG_HZ
    mel[high] = _MIN_LOG_MEL + np.log(hz[high] / _MIN_LOG_HZ) / _LOGSTEP
    return mel


def mel_to_hz(mel: np.ndarray) -> np.ndarray:
    mel = np.asanyarray(mel, dtype=np.float64)
    hz = _F_SP * mel
    high = mel >= _MIN_LOG_MEL
    hz[high] = _MIN_LOG_HZ * np.exp(_LOGSTEP * (mel[high] - _MIN_LOG_MEL))
    return hz


def mel_filters(sr: int = SAMPLE_RATE, n_fft: int = N_FFT, n_mels: int = N_MELS) -> np.ndarray:
    """Filtres triangulaires de 0 Hz à sr/2, d'aire constante : (n_mels, 1 + n_fft // 2), float32."""
    weights = np.zeros((n_mels, 1 + n_fft // 2), dtype=np.float32)
    fft_freqs = np.fft.rfftfreq(n=n_fft, d=1.0 / sr)
    mel_f = mel_to_hz(np.linspace(hz_to_mel(np.array([0.0]))[0], hz_to_mel(np.array([sr / 2]))[0], n_mels + 2))
    fdiff = np.diff(mel_f)
    ramps = np.subtract.outer(mel_f, fft_freqs)
    for i in range(n_mels):
        lower = -ramps[i] / fdiff[i]
        upper = ramps[i + 2] / fdiff[i + 1]
        weights[i] = np.maximum(0, np.minimum(lower, upper))
    weights *= (2.0 / (mel_f[2 : n_mels + 2] - mel_f[:n_mels]))[:, np.newaxis]
    return weights


_FILTERS = mel_filters()
# Fenêtre de Hann périodique, celle de scipy.signal.get_window("hann", N_FFT) qu'utilise librosa.
_WINDOW = 0.5 - 0.5 * np.cos(2.0 * np.pi * np.arange(N_FFT) / N_FFT)


# --- trames ------------------------------------------------------------------------------


def frames(samples: np.ndarray, start: int, count: int) -> np.ndarray:
    """Trames ``start`` à ``start + count`` (exclue), de N_FFT échantillons chacune.

    La trame k est centrée sur l'échantillon ``k * HOP`` ; hors du fichier, des zéros (le
    ``pad_mode="constant"`` de librosa 0.10).
    """
    first = start * HOP - N_FFT // 2
    size = (count - 1) * HOP + N_FFT
    segment = np.zeros(size, dtype=np.float32)
    a, b = max(first, 0), min(first + size, samples.size)
    if b > a:
        segment[a - first : b - first] = samples[a:b]
    return np.lib.stride_tricks.sliding_window_view(segment, N_FFT)[::HOP]


def mel_power(framed: np.ndarray) -> np.ndarray:
    """Spectrogramme mel en puissance, (trames, N_MELS), float32 comme librosa."""
    spectrum = np.fft.rfft(framed * _WINDOW, axis=-1).astype(np.complex64)
    return (np.abs(spectrum) ** 2) @ _FILTERS.T


def power_to_db(power: np.ndarray, ref: float) -> np.ndarray:
    """``librosa.power_to_db(power, ref=ref)`` quand ``ref`` est le maximum de la zone normalisée."""
    db = np.float32(10.0) * np.log10(np.maximum(np.float32(AMIN), power))
    db -= np.float32(10.0) * np.log10(np.maximum(np.float32(AMIN), np.float32(ref)))
    return np.maximum(db, db.max() - np.float32(TOP_DB))


def zero_crossing_rate(framed: np.ndarray) -> np.ndarray:
    """Passages par zéro de chaque trame, divisés par sa longueur (``zcr_extractor`` des auteurs)."""
    signs = np.sign(framed)
    return (np.abs(np.diff(signs, axis=-1)).sum(axis=-1) * np.float32(0.5 / N_FFT)).astype(np.float32)


def features(samples: np.ndarray, start: int, count: int) -> np.ndarray:
    """Entrée du réseau pour les trames ``start`` à ``start + count`` : (3, N_MELS, count), float32.

    Le passage est normalisé comme un enregistrement entier chez les auteurs : 0 dB à son
    maximum. Le réseau a appris sur des phrases de quelques secondes, chacune normalisée ainsi.
    """
    framed = frames(np.asarray(samples, dtype=np.float32), start, count)
    power = mel_power(framed)
    db = power_to_db(power, float(power.max()))
    out = np.empty((3, N_MELS, count), dtype=np.float32)
    out[0] = db.T
    out[1] = db.var(axis=-1, ddof=1)[np.newaxis, :]
    out[2] = zero_crossing_rate(framed)[np.newaxis, :]
    return out
