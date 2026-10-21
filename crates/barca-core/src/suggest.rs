//! "Did you mean" for a mistyped name: a flag, a subcommand, a target, a `--refresh` name, a
//! `barca docs` topic or a decorator argument. One rule everywhere (`barca docs agents`): the
//! closest names within a small edit distance, the best one only, or two when they tie. A
//! suggestion is advice in the message and the remediation; barca never acts on it, and the
//! error and its exit code are the same with or without one.

/// The names in `candidates` the user most likely meant by `unknown`, closest first: those at
/// the smallest edit distance, when that distance is at most one for a name of up to four
/// characters and two for a longer one (`amount` -> `amounts`, `statsu` -> `stats`, `status`).
/// One name, or two that tie; more than two at the same distance is no guess. Case is ignored.
pub fn closest<'a>(unknown: &str, candidates: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let typed = unknown;
    let unknown = unknown.to_lowercase();
    let limit = if unknown.chars().count() > 4 { 2 } else { 1 };
    let mut best: Vec<&'a str> = Vec::new();
    let mut best_distance = limit + 1;
    for candidate in candidates {
        if best.contains(&candidate) {
            continue;
        }
        let distance = edit_distance(&unknown, &candidate.to_lowercase());
        if candidate == typed || distance > limit {
            continue;
        }
        if distance < best_distance {
            best_distance = distance;
            best.clear();
        }
        if distance == best_distance {
            best.push(candidate);
        }
    }
    if best.len() > 2 { Vec::new() } else { best }
}

/// `Did you mean 'a'?` or `Did you mean 'a' or 'b'?` for [`closest`]'s result, each name
/// between `quote`s; `None` when there is no guess.
pub fn did_you_mean(names: &[&str], quote: char) -> Option<String> {
    let q = |n: &str| format!("{quote}{n}{quote}");
    match names {
        [] => None,
        [one] => Some(format!("Did you mean {}?", q(one))),
        [a, b, ..] => Some(format!("Did you mean {} or {}?", q(a), q(b))),
    }
}

/// [`did_you_mean`] of [`closest`], with names in single quotes.
pub fn suggestion<'a>(
    unknown: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    did_you_mean(&closest(unknown, candidates), '\'')
}

/// Optimal string alignment distance: insertions, deletions, substitutions and swaps of two
/// neighbouring characters each count one.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    d[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_best_match_only() {
        assert_eq!(closest("amount", ["raw", "amounts", "report"]), ["amounts"]);
        // `stats` and `status` are one edit away (a swap), `state` two: the best only.
        assert_eq!(
            closest("statsu", ["list", "state", "stats", "status"]),
            ["stats", "status"]
        );
        assert_eq!(closest("refesh", ["refresh", "json"]), ["refresh"]);
        assert_eq!(closest("REPORT", ["report"]), ["report"]);
    }

    #[test]
    fn no_guess_when_nothing_is_close_or_too_many_tie() {
        assert!(closest("orders", ["amounts", "report"]).is_empty());
        // Short names allow one edit only.
        assert!(closest("ab", ["xy", "abcd"]).is_empty());
        assert!(closest("cat", ["bat", "hat", "mat"]).is_empty());
        // An exact match is not a typo.
        assert!(closest("report", ["report"]).is_empty());
    }

    #[test]
    fn wording() {
        assert_eq!(
            suggestion("amount", ["amounts"]).as_deref(),
            Some("Did you mean 'amounts'?")
        );
        assert_eq!(
            did_you_mean(&["stats", "status"], '`').as_deref(),
            Some("Did you mean `stats` or `status`?")
        );
        assert_eq!(suggestion("zzz", ["amounts"]), None);
    }
}
