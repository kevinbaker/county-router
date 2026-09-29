//! Stop order from a drive-time matrix.
//!
//! Matrix index 0 is the start and 1..=n are the stops; a fixed end, when it is not the
//! start, is index n + 1. Times are asymmetric: on one-way and divided roads, reaching
//! the next lot "backwards" can mean a long loop, so a stop placed one position too
//! early shows up as backtracking. Small routes are solved exactly; for larger ones,
//! VROOM's order is polished by moving stops and short runs of stops to better spots.

/// Routes with at most this many stops are solved exactly (Held–Karp, about n² · 2ⁿ
/// steps and 2ⁿ · n table entries: 7 million steps and about 8 MB for 15 stops).
pub const EXACT_LIMIT: usize = 15;

/// Where the route must finish.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Finish {
    /// Wherever is shortest.
    Open,
    /// At this matrix index (0 for a round trip).
    At(usize),
}

/// Drive time of start → stops in `order` (stop numbers 0..n) → finish.
#[cfg(test)]
pub fn cost(m: &[Vec<f64>], order: &[usize], finish: Finish) -> f64 {
    let mut total = 0.0;
    let mut at = 0;
    for &s in order {
        total += m[at][s + 1];
        at = s + 1;
    }
    if let Finish::At(end) = finish {
        total += m[at][end];
    }
    total
}

/// The fastest order of all `n` stops, exactly. Only for small `n` (see [`EXACT_LIMIT`]).
pub fn exact(m: &[Vec<f64>], n: usize, finish: Finish) -> Vec<usize> {
    assert!(n <= 20, "exact ordering is only for small routes");
    if n == 0 {
        return Vec::new();
    }
    let full = (1usize << n) - 1;
    // best[mask][last]: fastest time from the start through the stops in `mask`,
    // ending at stop `last` (which is in `mask`).
    let mut best = vec![vec![f64::INFINITY; n]; 1 << n];
    let mut prev = vec![vec![usize::MAX; n]; 1 << n];
    for s in 0..n {
        best[1 << s][s] = m[0][s + 1];
    }
    for mask in 1..=full {
        for last in 0..n {
            let here = best[mask][last];
            if mask & (1 << last) == 0 || !here.is_finite() {
                continue;
            }
            for next in 0..n {
                if mask & (1 << next) != 0 {
                    continue;
                }
                let t = here + m[last + 1][next + 1];
                let to = mask | (1 << next);
                if t < best[to][next] {
                    best[to][next] = t;
                    prev[to][next] = last;
                }
            }
        }
    }
    let tail = |last: usize| match finish {
        Finish::Open => 0.0,
        Finish::At(end) => m[last + 1][end],
    };
    let mut last = (0..n)
        .min_by(|&a, &b| (best[full][a] + tail(a)).total_cmp(&(best[full][b] + tail(b))))
        .unwrap();
    let mut order = Vec::with_capacity(n);
    let mut mask = full;
    loop {
        order.push(last);
        let p = prev[mask][last];
        mask &= !(1 << last);
        if mask == 0 {
            break;
        }
        last = p;
    }
    order.reverse();
    order
}

/// Improves an order by moving single stops and runs of up to three stops to any other
/// position, keeping every move that saves at least a second, until none does.
///
/// Moving a run leaves its inside untouched, so each move is priced from the few legs
/// around the run's old and new places: O(1) per move, O(n²) per pass, fine for the
/// largest routes (250 stops).
pub fn polish(m: &[Vec<f64>], order: &[usize], finish: Finish) -> Vec<usize> {
    let mut order = order.to_vec();
    let n = order.len();
    // Drive time between two places, where a place is `Some(matrix index)` or `None`
    // for an open finish (free to reach).
    let t = |a: usize, b: Option<usize>| b.map_or(0.0, |b| m[a][b]);
    let end = match finish {
        Finish::Open => None,
        Finish::At(e) => Some(e),
    };
    // Matrix index of the place before position `i` (the start before position 0) and
    // after position `i` (the finish after the last stop).
    let before = |o: &[usize], i: usize| if i == 0 { 0 } else { o[i - 1] + 1 };
    let after = |o: &[usize], i: usize| {
        if i + 1 == o.len() {
            end
        } else {
            Some(o[i + 1] + 1)
        }
    };

    // Each accepted move saves at least a second, so this ends; the cap is a safeguard.
    for _ in 0..10_000 {
        let mut best: Option<(f64, usize, usize, usize)> = None;
        for len in 1..=3.min(n) {
            for from in 0..=n - len {
                let first = order[from] + 1;
                let last = order[from + len - 1] + 1;
                let a = before(&order, from);
                let b = after(&order, from + len - 1);
                // Taking the run out joins a to b directly.
                let removed = m[a][first] + t(last, b) - t(a, b);
                // Put it back between two places of the remaining order.
                let rest: Vec<usize> = order[..from]
                    .iter()
                    .chain(&order[from + len..])
                    .copied()
                    .collect();
                for to in 0..=rest.len() {
                    if to == from {
                        continue;
                    }
                    let c = before(&rest, to);
                    let d = if to == rest.len() {
                        end
                    } else {
                        Some(rest[to] + 1)
                    };
                    let added = m[c][first] + t(last, d) - t(c, d);
                    let saving = removed - added;
                    if saving > 1.0 && best.is_none_or(|(s, ..)| saving > s) {
                        best = Some((saving, len, from, to));
                    }
                }
            }
        }
        let Some((_, len, from, to)) = best else {
            break;
        };
        let run: Vec<usize> = order.drain(from..from + len).collect();
        order.splice(to..to, run);
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Start at 0; stops 1..=4 lie along a one-way street heading away from the start,
    /// 10 s apart. Going with the street is quick; going against it means a 300 s loop.
    fn one_way_street() -> Vec<Vec<f64>> {
        let n = 5;
        let mut m = vec![vec![0.0; n]; n];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, t) in row.iter_mut().enumerate() {
                if i != j {
                    *t = if j > i { 10.0 * (j - i) as f64 } else { 300.0 };
                }
            }
        }
        m
    }

    #[test]
    fn exact_follows_the_one_way_street() {
        let m = one_way_street();
        assert_eq!(exact(&m, 4, Finish::Open), vec![0, 1, 2, 3]);
        assert_eq!(cost(&m, &[0, 1, 2, 3], Finish::Open), 40.0);
    }

    #[test]
    fn exact_respects_a_round_trip() {
        let m = one_way_street();
        let order = exact(&m, 4, Finish::At(0));
        assert_eq!(order, vec![0, 1, 2, 3]);
        assert_eq!(cost(&m, &order, Finish::At(0)), 340.0);
    }

    #[test]
    fn polish_fixes_two_neighbours_in_the_wrong_order() {
        let m = one_way_street();
        // Stops 2 and 3 swapped: a loop back to reach stop 2.
        let bad = vec![0, 1, 3, 2];
        assert!(cost(&m, &bad, Finish::Open) > 300.0);
        let fixed = polish(&m, &bad, Finish::Open);
        assert_eq!(fixed, vec![0, 1, 2, 3]);
    }

    #[test]
    fn polish_never_makes_it_worse() {
        let m = one_way_street();
        let good = vec![0, 1, 2, 3];
        assert_eq!(polish(&m, &good, Finish::At(0)), good);
    }

    #[test]
    fn polish_only_ever_lowers_the_cost() {
        // Pseudo-random asymmetric matrices (a fixed LCG keeps the test repeatable).
        let mut seed: u64 = 42;
        let mut rnd = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) % 600) as f64
        };
        for n in [2usize, 5, 9, 30] {
            let size = n + 2;
            let m: Vec<Vec<f64>> = (0..size)
                .map(|i| {
                    (0..size)
                        .map(|j| if i == j { 0.0 } else { rnd() + 1.0 })
                        .collect()
                })
                .collect();
            let start: Vec<usize> = (0..n).collect();
            for finish in [Finish::Open, Finish::At(0), Finish::At(n + 1)] {
                let polished = polish(&m, &start, finish);
                let mut sorted = polished.clone();
                sorted.sort_unstable();
                assert_eq!(sorted, start, "polish must keep every stop exactly once");
                assert!(cost(&m, &polished, finish) <= cost(&m, &start, finish));
                if n <= 9 {
                    // Never better than the true optimum.
                    let best = cost(&m, &exact(&m, n, finish), finish);
                    assert!(cost(&m, &polished, finish) >= best - 1e-9);
                }
            }
        }
    }

    #[test]
    fn empty_and_single_stop() {
        let m = one_way_street();
        assert!(exact(&m, 0, Finish::Open).is_empty());
        assert_eq!(exact(&m, 1, Finish::Open), vec![0]);
        assert_eq!(polish(&m, &[0], Finish::Open), vec![0]);
    }
}
