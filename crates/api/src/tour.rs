//! Stop order from a drive-time matrix.
//!
//! Matrix index 0 is the start and 1..=n are the stops; a fixed end, when it is not the
//! start, is index n + 1. Times are asymmetric: on one-way and divided roads, reaching
//! the next lot "backwards" can mean a long loop, so a stop placed one position too
//! early shows up as backtracking. Routes of up to [`EXACT_LIMIT`] stops are solved
//! exactly; larger ones start from VROOM's order and are improved by [`search`] for a
//! set time.

use std::time::{Duration, Instant};

/// Routes with at most this many stops are solved exactly (Held–Karp). Time and memory
/// double with each stop: 20 stops take about 0.3 s and 105 MB on a desktop CPU, about
/// three times as long on the server.
pub const EXACT_LIMIT: usize = 20;

/// Where the route must finish.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Finish {
    /// Wherever is shortest.
    Open,
    /// At this matrix index (0 for a round trip).
    At(usize),
}

/// Drive time of start → stops in `order` (stop numbers 0..n) → finish.
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
///
/// The table holds, for every set of stops and every last stop in it, the fastest time
/// from the start through that set: 2ⁿ · n entries of 5 bytes (a 4-byte time and a
/// 1-byte back-pointer).
pub fn exact(m: &[Vec<f64>], n: usize, finish: Finish) -> Vec<usize> {
    assert!(n <= 24, "exact ordering is only for small routes");
    if n == 0 {
        return Vec::new();
    }
    // Stop-to-stop times as a flat f32 table: faster to scan than the f64 rows.
    let d: Vec<f32> = (0..n)
        .flat_map(|a| (0..n).map(move |b| m[a + 1][b + 1] as f32))
        .collect();
    let full = (1usize << n) - 1;
    let mut best = vec![f32::INFINITY; (full + 1) * n];
    let mut prev = vec![u8::MAX; (full + 1) * n];
    for s in 0..n {
        best[(1 << s) * n + s] = m[0][s + 1] as f32;
    }
    for mask in 1..=full {
        let row = mask * n;
        for last in 0..n {
            let here = best[row + last];
            if mask & (1 << last) == 0 || !here.is_finite() {
                continue;
            }
            let from = &d[last * n..last * n + n];
            let mut free = full & !mask;
            while free != 0 {
                let next = free.trailing_zeros() as usize;
                free &= free - 1;
                let t = here + from[next];
                let slot = (mask | (1 << next)) * n + next;
                if t < best[slot] {
                    best[slot] = t;
                    prev[slot] = last as u8;
                }
            }
        }
    }
    let tail = |last: usize| match finish {
        Finish::Open => 0.0,
        Finish::At(end) => m[last + 1][end] as f32,
    };
    let total = |last: usize| best[full * n + last] + tail(last);
    let mut last = (0..n)
        .min_by(|&a, &b| total(a).total_cmp(&total(b)))
        .unwrap();
    let mut order = Vec::with_capacity(n);
    let mut mask = full;
    loop {
        order.push(last);
        let p = prev[mask * n + last];
        mask &= !(1 << last);
        if mask == 0 {
            break;
        }
        last = p as usize;
    }
    order.reverse();
    order
}

/// Improves an order until no single move saves at least a second. Moves: take a stop
/// or a run of up to three stops and put it anywhere else, or reverse a section.
///
/// Every move is priced from the few legs it changes, O(1) each (a reversal also uses
/// running totals of the section's times in both directions), so a pass is O(n²): fine
/// for the largest routes (250 stops).
pub fn polish(m: &[Vec<f64>], order: &[usize], finish: Finish) -> Vec<usize> {
    let mut order = order.to_vec();
    let n = order.len();
    let end = match finish {
        Finish::Open => None,
        Finish::At(e) => Some(e),
    };
    // Drive time to a place that is `Some(matrix index)`, or `None` for an open finish
    // (free to reach).
    let t = |a: usize, b: Option<usize>| b.map_or(0.0, |b| m[a][b]);
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

    enum Move {
        Relocate { len: usize, from: usize, to: usize },
        Reverse { i: usize, j: usize },
    }
    // Each accepted move saves at least a second, so this ends; the cap is a safeguard.
    for _ in 0..10_000 {
        let mut best: Option<(f64, Move)> = None;
        let mut consider = |saving: f64, mv: Move| {
            if saving > 1.0 && best.as_ref().is_none_or(|(s, _)| saving > *s) {
                best = Some((saving, mv));
            }
        };

        for len in 1..=3.min(n) {
            for from in 0..=n - len {
                let first = order[from] + 1;
                let last = order[from + len - 1] + 1;
                let a = before(&order, from);
                let b = after(&order, from + len - 1);
                // Taking the run out joins a to b directly.
                let removed = m[a][first] + t(last, b) - t(a, b);
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
                    consider(removed - added, Move::Relocate { len, from, to });
                }
            }
        }

        // Running totals along the order, forwards and backwards, so the inside of any
        // section costs O(1) in either direction.
        let mut fwd = vec![0.0; n];
        let mut bwd = vec![0.0; n];
        for k in 1..n {
            let (x, y) = (order[k - 1] + 1, order[k] + 1);
            fwd[k] = fwd[k - 1] + m[x][y];
            bwd[k] = bwd[k - 1] + m[y][x];
        }
        for i in 0..n {
            let p = before(&order, i);
            let first = order[i] + 1;
            for j in i + 1..n {
                let last = order[j] + 1;
                let q = after(&order, j);
                let old = m[p][first] + (fwd[j] - fwd[i]) + t(last, q);
                let new = m[p][last] + (bwd[j] - bwd[i]) + t(first, q);
                consider(old - new, Move::Reverse { i, j });
            }
        }

        match best {
            None => break,
            Some((_, Move::Relocate { len, from, to })) => {
                let run: Vec<usize> = order.drain(from..from + len).collect();
                order.splice(to..to, run);
            }
            Some((_, Move::Reverse { i, j })) => order[i..=j].reverse(),
        }
    }
    order
}

/// Improves a large route's order for up to `budget`: repeatedly shakes the best order
/// found (swapping two neighbouring sections, a "double bridge"), polishes it, and keeps
/// it when it is faster. Ends early after many shakes in a row without a gain.
pub fn search(m: &[Vec<f64>], order: &[usize], finish: Finish, budget: Duration) -> Vec<usize> {
    const PATIENCE: u32 = 20_000;
    const RESTART_AFTER: u32 = 200;
    let deadline = Instant::now() + budget;
    let mut best = polish(m, order, finish);
    let mut best_cost = cost(m, &best, finish);
    let n = best.len();
    if n < 8 {
        return best;
    }
    // A small fixed-seed generator: repeatable results, no extra dependency.
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ n as u64;
    let mut rand_below = |k: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % k as u64) as usize
    };
    // Shake the current order; move on to the result if it is at most 1% slower (so the
    // search can leave a dead end), and remember the best order seen.
    let mut current = best.clone();
    let mut current_cost = best_cost;
    let mut misses = 0;
    while misses < PATIENCE && Instant::now() < deadline {
        // Three cut points split the order into A B C D; try A C B D.
        let mut cuts = [
            1 + rand_below(n - 1),
            1 + rand_below(n - 1),
            1 + rand_below(n - 1),
        ];
        cuts.sort_unstable();
        let [a, b, c] = cuts;
        if a == b || b == c {
            continue;
        }
        let shaken: Vec<usize> = current[..a]
            .iter()
            .chain(&current[b..c])
            .chain(&current[a..b])
            .chain(&current[c..])
            .copied()
            .collect();
        let candidate = polish(m, &shaken, finish);
        let c = cost(m, &candidate, finish);
        if c < best_cost - 0.5 {
            best = candidate.clone();
            best_cost = c;
            misses = 0;
        } else {
            misses += 1;
        }
        if c <= current_cost * 1.01 {
            current = candidate;
            current_cost = c;
        }
        // Stuck around one order: start again from a random one (the best is kept).
        if misses > 0 && misses % RESTART_AFTER == 0 {
            let mut fresh: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                fresh.swap(i, rand_below(i + 1));
            }
            current = polish(m, &fresh, finish);
            current_cost = cost(m, &current, finish);
        }
    }
    best
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
    fn search_reaches_the_optimum_on_small_random_routes() {
        let mut seed: u64 = 99;
        let mut rnd = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) % 900) as f64 + 1.0
        };
        for n in [9usize, 11, 13] {
            let m: Vec<Vec<f64>> = (0..n + 2)
                .map(|i| {
                    (0..n + 2)
                        .map(|j| if i == j { 0.0 } else { rnd() })
                        .collect()
                })
                .collect();
            let start: Vec<usize> = (0..n).collect();
            for finish in [Finish::Open, Finish::At(0)] {
                let found = search(&m, &start, finish, Duration::from_secs(2));
                let optimum = cost(&m, &exact(&m, n, finish), finish);
                let ratio = cost(&m, &found, finish) / optimum;
                println!("n={n} {finish:?}: {ratio:.3} x optimal");
                assert!(ratio <= 1.05, "within 5% of optimal");
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

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    /// `cargo test --release -p county-api bench_exact -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_exact() {
        let mut seed: u64 = 7;
        let mut rnd = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) % 1200) as f64 + 1.0
        };
        for n in 12..=21 {
            let m: Vec<Vec<f64>> = (0..n + 2)
                .map(|i| {
                    (0..n + 2)
                        .map(|j| if i == j { 0.0 } else { rnd() })
                        .collect()
                })
                .collect();
            let t = Instant::now();
            let order = exact(&m, n, Finish::At(0));
            println!(
                "{n:2} stops: {:7.3} s, table {:6.1} MB, cost {:.0}",
                t.elapsed().as_secs_f64(),
                ((1usize << n) * n * 5) as f64 / 1e6,
                cost(&m, &order, Finish::At(0))
            );
        }
    }
}

#[cfg(test)]
mod quality {
    use super::*;

    /// Road-like matrix: points on a plane, drive time = distance, plus a detour on some
    /// pairs in one direction (one-way and divided roads).
    fn road_like(n: usize, seed: &mut u64) -> Vec<Vec<f64>> {
        let mut rnd = || {
            *seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((*seed >> 33) % 10_000) as f64 / 10_000.0
        };
        let pts: Vec<(f64, f64)> = (0..n + 2)
            .map(|_| (rnd() * 3000.0, rnd() * 3000.0))
            .collect();
        let detours: Vec<f64> = (0..(n + 2) * (n + 2))
            .map(|_| {
                if rnd() < 0.15 {
                    60.0 + rnd() * 120.0
                } else {
                    0.0
                }
            })
            .collect();
        (0..n + 2)
            .map(|i| {
                (0..n + 2)
                    .map(|j| {
                        if i == j {
                            0.0
                        } else {
                            let (dx, dy) = (pts[i].0 - pts[j].0, pts[i].1 - pts[j].1);
                            (dx * dx + dy * dy).sqrt() / 15.0 + detours[i * (n + 2) + j]
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// `cargo test --release -p county-api search_quality -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn search_quality() {
        let mut seed = 11u64;
        let mut worst: f64 = 1.0;
        let mut total = 0.0;
        let mut count = 0.0;
        for n in [12usize, 14, 16, 18] {
            for _ in 0..5 {
                let m = road_like(n, &mut seed);
                let start: Vec<usize> = (0..n).collect();
                for finish in [Finish::Open, Finish::At(0)] {
                    let t = Instant::now();
                    let found = search(&m, &start, finish, Duration::from_millis(1500));
                    let secs = t.elapsed().as_secs_f64();
                    let r = cost(&m, &found, finish) / cost(&m, &exact(&m, n, finish), finish);
                    worst = worst.max(r);
                    total += r;
                    count += 1.0;
                    if r > 1.001 {
                        println!("n={n} {finish:?}: {r:.3} x optimal in {secs:.2}s");
                    }
                }
            }
        }
        println!(
            "road-like: {count} routes, mean {:.4} x optimal, worst {worst:.3}",
            total / count
        );
    }
}
