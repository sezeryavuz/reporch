//! Ranking discovered repositories (§5.3).
//!
//! | Signal | Weight |
//! | :--- | ---: |
//! | Directory name starts with the project name | 100 |
//! | Directory name fuzzy-matches the project name | 0–60 |
//! | Git remote shares an org with an already-selected repo | 80 |
//! | Sibling of an already-selected repo in the filesystem | 40 |
//! | Recent activity (HEAD commit within 90 days) | 0–20 |
//!
//! **Ranking is live.** Selecting `modulex` immediately re-ranks using its remote org and its
//! parent directory, which pulls the rest of the family up before the user has typed anything.
//! That is the behaviour that makes the screen feel like it understands the project, so the
//! selection-dependent signals are recomputed on every change rather than once.

use std::collections::BTreeSet;
use std::path::PathBuf;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use crate::discover::FoundRepo;

pub const W_NAME_PREFIX: i32 = 100;
pub const W_FUZZY_MAX: i32 = 60;
pub const W_SHARED_ORG: i32 = 80;
pub const W_SIBLING: i32 = 40;
pub const W_RECENT_MAX: i32 = 20;

/// 90 days, after which a checkout is "abandoned" for ranking purposes.
const RECENT_WINDOW_SECS: u64 = 90 * 24 * 60 * 60;

/// A scored candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub repo: FoundRepo,
    pub score: i32,
    /// Why it scored what it did — shown in the UI so the ordering is never mysterious.
    pub reasons: Vec<&'static str>,
}

/// What the user has already selected. The selection-dependent signals read from this.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub orgs: BTreeSet<String>,
    pub parents: BTreeSet<PathBuf>,
    pub names: BTreeSet<String>,
}

impl Selection {
    /// Build from the currently selected repos.
    pub fn from_repos<'a>(selected: impl IntoIterator<Item = &'a FoundRepo>) -> Self {
        let mut s = Self::default();
        for r in selected {
            if let Some(o) = &r.remote_org {
                s.orgs.insert(o.clone());
            }
            if let Some(p) = r.parent() {
                s.parents.insert(p.to_path_buf());
            }
            s.names.insert(r.name.clone());
        }
        s
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Score one repository.
pub fn score(repo: &FoundRepo, project: &str, sel: &Selection) -> Ranked {
    let mut score = 0;
    let mut reasons = Vec::new();
    let name_lower = repo.name.to_ascii_lowercase();
    let project_lower = project.trim().to_ascii_lowercase();

    // Strongest single signal: `modulex-ui` for project `modulex`.
    if !project_lower.is_empty() && name_lower.starts_with(&project_lower) {
        score += W_NAME_PREFIX;
        reasons.push("name");
    } else if !project_lower.is_empty() {
        // Similarity, for `mdlx-web` against `modulex`. Scored on the leading token, so a repo's
        // suffix (`-web`, `-ui`) does not dilute the comparison.
        let head = name_lower
            .split(['-', '_', '.'])
            .next()
            .unwrap_or(&name_lower);
        let sim = jaro_winkler(head, &project_lower).max(jaro_winkler(&name_lower, &project_lower));
        // Below this, similarity is noise: unrelated short names score surprisingly well on any
        // string metric, and a false promotion is more annoying than a missed one.
        const FLOOR: f64 = 0.70;
        if sim > FLOOR {
            let scaled = (sim - FLOOR) / (1.0 - FLOOR);
            let pts = (scaled * W_FUZZY_MAX as f64).round() as i32;
            if pts > 0 {
                score += pts;
                reasons.push("similar name");
            }
        }
    }

    // A shared remote org catches repos named nothing like the project.
    if let Some(org) = &repo.remote_org {
        if sel.orgs.contains(org) {
            score += W_SHARED_ORG;
            reasons.push("same org");
        }
    }

    // Multi-repo projects are almost always checked out side by side.
    if let Some(parent) = repo.parent() {
        if sel.parents.contains(parent) {
            score += W_SIBLING;
            reasons.push("sibling");
        }
    }

    // Demote abandoned checkouts. Linear decay across the window.
    if let Some(age) = repo.head_age_secs {
        if age < RECENT_WINDOW_SECS {
            let frac = 1.0 - (age as f32 / RECENT_WINDOW_SECS as f32);
            let pts = (frac * W_RECENT_MAX as f32).round() as i32;
            if pts > 0 {
                score += pts;
                reasons.push("recent");
            }
        }
    }

    Ranked {
        repo: repo.clone(),
        score,
        reasons,
    }
}

/// Jaro-Winkler similarity, 0.0–1.0.
///
/// > **Deviation from §5.3.** The spec says "Jaro-Winkler via `nucleo-matcher`". `nucleo-matcher`
/// > does not implement Jaro-Winkler — it is a *subsequence* matcher, which asks "do the
/// > pattern's characters appear in order?", not "how alike are these two strings?". The spec's
/// > own worked example proves the difference: `modulex` is not a subsequence of `mdlx-web`
/// > (no `o`, no `u`), so nucleo scores it as **no match at all**, exactly like a completely
/// > unrelated repo.
/// >
/// > So the two jobs are split, each using the right tool. Name *similarity* — the 0–60 ranking
/// > signal — uses Jaro-Winkler, implemented here in a few lines. The interactive `/` *filter*,
/// > where the user is typing a prefix or an abbreviation and subsequence matching is precisely
/// > what they expect, keeps `nucleo-matcher`.
///
/// Jaro-Winkler is the right metric for this signal specifically because of its prefix bonus:
/// repositories in one project overwhelmingly share a leading string.
pub fn jaro_winkler(a: &str, b: &str) -> f64 {
    let j = jaro(a, b);
    if j < 0.7 {
        return j;
    }
    // Standard Winkler prefix bonus: up to 4 leading characters, scaling factor 0.1.
    let prefix = a
        .chars()
        .zip(b.chars())
        .take(4)
        .take_while(|(x, y)| x == y)
        .count() as f64;
    j + prefix * 0.1 * (1.0 - j)
}

fn jaro(a: &str, b: &str) -> f64 {
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    if x.is_empty() && y.is_empty() {
        return 1.0;
    }
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    // Characters further apart than this cannot be considered matching.
    let window = (x.len().max(y.len()) / 2).saturating_sub(1);
    let mut x_hit = vec![false; x.len()];
    let mut y_hit = vec![false; y.len()];
    let mut matches = 0usize;

    for (i, xc) in x.iter().enumerate() {
        let lo = i.saturating_sub(window);
        let hi = (i + window + 1).min(y.len());
        for k in lo..hi {
            if !y_hit[k] && y[k] == *xc {
                x_hit[i] = true;
                y_hit[k] = true;
                matches += 1;
                break;
            }
        }
    }
    if matches == 0 {
        return 0.0;
    }

    // Transpositions: matched characters that appear in a different order.
    let mut transpositions = 0usize;
    let mut k = 0usize;
    for i in 0..x.len() {
        if !x_hit[i] {
            continue;
        }
        while !y_hit[k] {
            k += 1;
        }
        if x[i] != y[k] {
            transpositions += 1;
        }
        k += 1;
    }
    let m = matches as f64;
    (m / x.len() as f64 + m / y.len() as f64 + (m - transpositions as f64 / 2.0) / m) / 3.0
}

/// Rank every candidate, best first.
///
/// Ties break by name so the list is stable: a list that reorders under the cursor while the user
/// is reading it is worse than one that is slightly wrong.
pub fn rank(repos: &[FoundRepo], project: &str, sel: &Selection) -> Vec<Ranked> {
    let mut out: Vec<Ranked> = repos.iter().map(|r| score(r, project, sel)).collect();
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.repo.name.cmp(&b.repo.name))
    });
    out
}

/// Filter by the user's typed query, preserving rank order (§5.3: `/` filter).
pub fn filter<'a>(ranked: &'a [Ranked], query: &str) -> Vec<&'a Ranked> {
    let q = query.trim();
    if q.is_empty() {
        return ranked.iter().collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, &Ranked)> = ranked
        .iter()
        .filter_map(|r| {
            let mut buf = Vec::new();
            let hay = format!("{} {}", r.repo.name, r.repo.path.display());
            pattern
                .score(nucleo_matcher::Utf32Str::new(&hay, &mut buf), &mut matcher)
                .map(|s| (s, r))
        })
        .collect();
    // Within a filter, the query's own match quality leads; ranking breaks ties.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.score.cmp(&a.1.score)));
    scored.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn repo(name: &str, parent: &str, org: Option<&str>) -> FoundRepo {
        FoundRepo {
            name: name.to_string(),
            path: Path::new(parent).join(name),
            remote_url: org.map(|o| format!("git@github.com:{o}/{name}.git")),
            remote_org: org.map(str::to_string),
            head_age_secs: None,
        }
    }

    fn names(r: &[Ranked]) -> Vec<&str> {
        r.iter().map(|x| x.repo.name.as_str()).collect()
    }

    #[test]
    fn a_name_prefix_dominates() {
        let repos = vec![
            repo("unrelated", "/r", None),
            repo("modulex-ui", "/r", None),
            repo("modulex", "/r", None),
        ];
        let ranked = rank(&repos, "modulex", &Selection::default());
        assert_eq!(names(&ranked)[..2], ["modulex", "modulex-ui"]);
        assert_eq!(ranked[2].repo.name, "unrelated");
    }

    #[test]
    fn ties_break_by_name_for_a_stable_list() {
        let repos = vec![
            repo("modulex-zz", "/r", None),
            repo("modulex-aa", "/r", None),
        ];
        let ranked = rank(&repos, "modulex", &Selection::default());
        assert_eq!(names(&ranked), vec!["modulex-aa", "modulex-zz"]);
    }

    /// The §5.3 example: `mdlx-web` for project `modulex` should score above an unrelated repo,
    /// but below a true prefix match.
    #[test]
    fn a_fuzzy_name_scores_between_unrelated_and_prefix() {
        let repos = vec![
            repo("modulex", "/r", None),
            repo("mdlx-web", "/r", None),
            repo("zzzz", "/r", None),
        ];
        let ranked = rank(&repos, "modulex", &Selection::default());
        let by = |n: &str| ranked.iter().find(|r| r.repo.name == n).unwrap().score;
        assert!(by("modulex") > by("mdlx-web"), "prefix beats fuzzy");
        assert!(by("mdlx-web") > by("zzzz"), "fuzzy beats unrelated");
        assert!(by("mdlx-web") <= W_FUZZY_MAX);
    }

    /// **The behaviour that makes §5.3 feel intelligent.** Selecting one repo must immediately
    /// pull its family up, including repos named nothing like the project.
    #[test]
    fn selecting_one_repo_pulls_its_org_siblings_up() {
        let repos = vec![
            repo("modulex", "/repos", Some("example-org")),
            // Sorts before `keymaster`, so a tie on score would leave keymaster behind it.
            repo("aaa-unrelated", "/elsewhere", None),
            // Named nothing like the project — only the shared org connects it.
            repo("keymaster", "/repos", Some("example-org")),
        ];

        let before = rank(&repos, "modulex", &Selection::default());
        let keymaster_before = before
            .iter()
            .position(|r| r.repo.name == "keymaster")
            .unwrap();

        let selected = [repo("modulex", "/repos", Some("example-org"))];
        let sel = Selection::from_repos(&selected);
        let after = rank(&repos, "modulex", &sel);
        let keymaster_after = after
            .iter()
            .position(|r| r.repo.name == "keymaster")
            .unwrap();

        assert!(
            keymaster_after < keymaster_before,
            "shared org must promote it: before {keymaster_before}, after {keymaster_after}"
        );
        assert!(after
            .iter()
            .find(|r| r.repo.name == "keymaster")
            .unwrap()
            .reasons
            .contains(&"same org"));
    }

    #[test]
    fn filesystem_siblings_are_promoted() {
        let repos = vec![
            repo("sibling", "/repos", None),
            repo("faraway", "/elsewhere", None),
        ];
        let selected = [repo("modulex", "/repos", None)];
        let sel = Selection::from_repos(&selected);
        let ranked = rank(&repos, "modulex", &sel);
        assert_eq!(ranked[0].repo.name, "sibling");
        assert!(ranked[0].reasons.contains(&"sibling"));
    }

    #[test]
    fn org_outweighs_sibling() {
        // A repo in the same org elsewhere on disk should beat an unrelated neighbour, because
        // the org is the stronger statement about what belongs together.
        let repos = vec![
            repo("same-org-far", "/elsewhere", Some("example-org")),
            repo("no-org-near", "/repos", None),
        ];
        let selected = [repo("modulex", "/repos", Some("example-org"))];
        let ranked = rank(&repos, "x", &Selection::from_repos(&selected));
        assert_eq!(ranked[0].repo.name, "same-org-far");
    }

    #[test]
    fn recency_demotes_abandoned_checkouts() {
        let fresh = FoundRepo {
            head_age_secs: Some(60),
            ..repo("aaa-fresh", "/r", None)
        };
        let stale = FoundRepo {
            head_age_secs: Some(400 * 24 * 3600),
            ..repo("aaa-stale", "/r", None)
        };
        let ranked = rank(&[stale, fresh], "zzz", &Selection::default());
        assert_eq!(ranked[0].repo.name, "aaa-fresh");
        assert!(ranked[0].reasons.contains(&"recent"));
        assert!(!ranked[1].reasons.contains(&"recent"));
    }

    #[test]
    fn recency_alone_never_outweighs_a_name_match() {
        let fresh_unrelated = FoundRepo {
            head_age_secs: Some(0),
            ..repo("zzz", "/r", None)
        };
        let stale_match = FoundRepo {
            head_age_secs: Some(400 * 24 * 3600),
            ..repo("modulex-docs", "/r", None)
        };
        let ranked = rank(
            &[fresh_unrelated, stale_match],
            "modulex",
            &Selection::default(),
        );
        assert_eq!(ranked[0].repo.name, "modulex-docs");
    }

    #[test]
    fn an_unknown_project_name_still_produces_a_stable_order() {
        let repos = vec![repo("b", "/r", None), repo("a", "/r", None)];
        let ranked = rank(&repos, "", &Selection::default());
        assert_eq!(names(&ranked), vec!["a", "b"]);
    }

    #[test]
    fn filtering_narrows_and_keeps_the_best_match_first() {
        let repos = vec![
            repo("modulex", "/r", None),
            repo("modulex-ui", "/r", None),
            repo("modulex-docs", "/r", None),
        ];
        let ranked = rank(&repos, "modulex", &Selection::default());
        let filtered = filter(&ranked, "docs");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].repo.name, "modulex-docs");
    }

    #[test]
    fn an_empty_filter_returns_everything_in_rank_order() {
        let repos = vec![repo("modulex-ui", "/r", None), repo("modulex", "/r", None)];
        let ranked = rank(&repos, "modulex", &Selection::default());
        let filtered = filter(&ranked, "   ");
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].repo.name, "modulex");
    }

    #[test]
    fn a_filter_matching_nothing_returns_nothing() {
        let repos = vec![repo("modulex", "/r", None)];
        let ranked = rank(&repos, "modulex", &Selection::default());
        assert!(filter(&ranked, "qqqqzzzz").is_empty());
    }

    #[test]
    fn filtering_matches_on_path_as_well_as_name() {
        let repos = vec![
            repo("alpha", "/home/u/work", None),
            repo("beta", "/home/u/play", None),
        ];
        let ranked = rank(&repos, "", &Selection::default());
        let filtered = filter(&ranked, "work");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].repo.name, "alpha");
    }

    // ---- the hand-written similarity metric ------------------------------

    #[test]
    fn jaro_winkler_matches_known_reference_values() {
        // Classic reference pairs, to 3 decimal places.
        let cases = [
            ("martha", "marhta", 0.961),
            ("dixon", "dicksonx", 0.813),
            ("dwayne", "duane", 0.840),
            ("", "", 1.0),
        ];
        for (a, b, want) in cases {
            let got = jaro_winkler(a, b);
            assert!(
                (got - want).abs() < 0.001,
                "jaro_winkler({a:?}, {b:?}) = {got:.4}, expected {want}"
            );
        }
    }

    #[test]
    fn jaro_winkler_is_symmetric_and_bounded() {
        let words = ["modulex", "mdlx", "ui", "", "a", "modulex-integrations"];
        for a in words {
            for b in words {
                let ab = jaro_winkler(a, b);
                let ba = jaro_winkler(b, a);
                assert!((0.0..=1.0).contains(&ab), "{a}/{b} out of range: {ab}");
                assert!((ab - ba).abs() < 1e-9, "asymmetric for {a}/{b}");
            }
            assert!((jaro_winkler(a, a) - 1.0).abs() < 1e-9, "{a} vs itself");
        }
    }

    #[test]
    fn jaro_winkler_handles_no_overlap_and_empty_inputs() {
        assert_eq!(jaro_winkler("abc", "xyz"), 0.0);
        assert_eq!(jaro_winkler("abc", ""), 0.0);
        assert_eq!(jaro_winkler("", "abc"), 0.0);
    }

    /// Multi-byte input must not panic — repo names contain anything the filesystem allows.
    #[test]
    fn jaro_winkler_is_char_safe() {
        let _ = jaro_winkler("日本語", "日本");
        let _ = jaro_winkler("café", "cafe");
        let _ = jaro_winkler("🎉", "🎉🎉");
    }

    /// The prefix bonus is the reason this metric was chosen: repos in one project share a
    /// leading string far more often than they share an interior one.
    #[test]
    fn the_prefix_bonus_favours_a_shared_leading_string() {
        assert!(jaro_winkler("modulex-ui", "modulex") > jaro_winkler("ui-modulex", "modulex"));
    }

    #[test]
    fn scoring_is_deterministic() {
        let repos: Vec<FoundRepo> = (0..20)
            .map(|i| repo(&format!("modulex-{i}"), "/r", Some("example-org")))
            .collect();
        let sel = Selection::from_repos(&repos[..3]);
        let a = rank(&repos, "modulex", &sel);
        let b = rank(&repos, "modulex", &sel);
        assert_eq!(a, b);
    }
}
