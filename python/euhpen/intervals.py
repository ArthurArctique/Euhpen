"""Opérations sur des listes d'intervalles [début, fin) en secondes."""

from __future__ import annotations

from collections.abc import Iterable

Interval = tuple[float, float]


def union(intervals: Iterable[Interval]) -> list[Interval]:
    """Fusionne les intervalles qui se chevauchent ou se touchent ; écarte les vides."""
    result: list[Interval] = []
    for start, end in sorted(i for i in intervals if i[1] > i[0]):
        if result and start <= result[-1][1]:
            if end > result[-1][1]:
                result[-1] = (result[-1][0], end)
        else:
            result.append((start, end))
    return result


def complement(intervals: Iterable[Interval], start: float, end: float) -> list[Interval]:
    """Ce qui, dans [start, end), n'est couvert par aucun intervalle."""
    result: list[Interval] = []
    cursor = start
    for a, b in union(intervals):
        if b <= start or a >= end:
            continue
        if a > cursor:
            result.append((cursor, a))
        cursor = max(cursor, b)
    if cursor < end:
        result.append((cursor, end))
    return result


def subtract(intervals: Iterable[Interval], removed: Iterable[Interval]) -> list[Interval]:
    """intervals − removed."""
    removed = union(removed)
    result: list[Interval] = []
    for a, b in union(intervals):
        pieces = [(a, b)]
        for ra, rb in removed:
            if rb <= a:
                continue
            if ra >= b:
                break
            next_pieces = []
            for pa, pb in pieces:
                if rb <= pa or ra >= pb:
                    next_pieces.append((pa, pb))
                    continue
                if ra > pa:
                    next_pieces.append((pa, ra))
                if rb < pb:
                    next_pieces.append((rb, pb))
            pieces = next_pieces
        result.extend(pieces)
    return result


def clip(intervals: Iterable[Interval], start: float, end: float) -> list[Interval]:
    return [(max(a, start), min(b, end)) for a, b in intervals if min(b, end) > max(a, start)]


def total(intervals: Iterable[Interval]) -> float:
    return sum(b - a for a, b in intervals)
