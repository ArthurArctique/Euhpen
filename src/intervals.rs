//! Opérations sur des listes d'intervalles [début, fin) en secondes.

pub type Interval = (f64, f64);

/// Fusionne les intervalles qui se chevauchent ou se touchent ; écarte les vides.
pub fn union(intervals: &[Interval]) -> Vec<Interval> {
    let mut sorted: Vec<Interval> = intervals.iter().copied().filter(|i| i.1 > i.0).collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut result: Vec<Interval> = Vec::with_capacity(sorted.len());
    for (start, end) in sorted {
        match result.last_mut() {
            Some(last) if start <= last.1 => {
                if end > last.1 {
                    last.1 = end;
                }
            }
            _ => result.push((start, end)),
        }
    }
    result
}

/// Ce qui, dans [start, end), n'est couvert par aucun intervalle.
pub fn complement(intervals: &[Interval], start: f64, end: f64) -> Vec<Interval> {
    let mut result = Vec::new();
    let mut cursor = start;
    for (a, b) in union(intervals) {
        if b <= start || a >= end {
            continue;
        }
        if a > cursor {
            result.push((cursor, a));
        }
        cursor = cursor.max(b);
    }
    if cursor < end {
        result.push((cursor, end));
    }
    result
}

/// intervals − removed.
pub fn subtract(intervals: &[Interval], removed: &[Interval]) -> Vec<Interval> {
    let removed = union(removed);
    let mut result = Vec::new();
    for (a, b) in union(intervals) {
        let mut pieces = vec![(a, b)];
        for &(ra, rb) in &removed {
            if rb <= a {
                continue;
            }
            if ra >= b {
                break;
            }
            let mut next = Vec::with_capacity(pieces.len() + 1);
            for (pa, pb) in pieces {
                if rb <= pa || ra >= pb {
                    next.push((pa, pb));
                    continue;
                }
                if ra > pa {
                    next.push((pa, ra));
                }
                if rb < pb {
                    next.push((rb, pb));
                }
            }
            pieces = next;
        }
        result.extend(pieces);
    }
    result
}

pub fn clip(intervals: &[Interval], start: f64, end: f64) -> Vec<Interval> {
    intervals
        .iter()
        .filter(|(a, b)| b.min(end) > a.max(start))
        .map(|(a, b)| (a.max(start), b.min(end)))
        .collect()
}

pub fn total(intervals: &[Interval]) -> f64 {
    intervals.iter().map(|(a, b)| b - a).sum()
}

/// L'intervalle qui contient `t` dans une liste triée et disjointe.
pub fn find(intervals: &[Interval], t: f64) -> Option<Interval> {
    let k = intervals.partition_point(|i| i.1 <= t);
    intervals.get(k).copied().filter(|i| i.0 <= t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fusionne() {
        assert_eq!(union(&[(3.0, 4.0), (0.0, 1.0), (1.0, 2.0), (5.0, 5.0)]), vec![(0.0, 2.0), (3.0, 4.0)]);
    }

    #[test]
    fn complete() {
        assert_eq!(complement(&[(1.0, 2.0)], 0.0, 3.0), vec![(0.0, 1.0), (2.0, 3.0)]);
        assert_eq!(complement(&[], 0.0, 3.0), vec![(0.0, 3.0)]);
        assert_eq!(complement(&[(0.0, 3.0)], 0.0, 3.0), vec![]);
        assert_eq!(complement(&[(-1.0, 1.0), (2.0, 9.0)], 0.0, 3.0), vec![(1.0, 2.0)]);
    }

    #[test]
    fn soustrait() {
        assert_eq!(subtract(&[(0.0, 10.0)], &[(2.0, 3.0), (5.0, 6.0)]), vec![(0.0, 2.0), (3.0, 5.0), (6.0, 10.0)]);
        assert_eq!(subtract(&[(1.0, 2.0)], &[(0.0, 5.0)]), vec![]);
    }

    #[test]
    fn borne() {
        assert_eq!(clip(&[(-1.0, 1.0), (2.0, 5.0), (9.0, 10.0)], 0.0, 4.0), vec![(0.0, 1.0), (2.0, 4.0)]);
        assert_eq!(total(&[(0.0, 1.0), (2.0, 4.0)]), 3.0);
    }

    #[test]
    fn trouve() {
        let list = [(1.0, 2.0), (3.0, 4.0)];
        assert_eq!(find(&list, 1.5), Some((1.0, 2.0)));
        assert_eq!(find(&list, 2.0), None);
        assert_eq!(find(&list, 3.0), Some((3.0, 4.0)));
        assert_eq!(find(&list, 0.5), None);
        assert_eq!(find(&list, 9.0), None);
    }
}
