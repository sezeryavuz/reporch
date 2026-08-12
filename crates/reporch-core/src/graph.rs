//! The producer/consumer dependency graph (§5.4 Addition).
//!
//! Built from every repo's `consumes` / `produces_contracts_for`, and then reused three times:
//! electing the master (§5.7), ordering the merge (§11.1), and telling the master which repos a
//! contract change is likely to ripple into.
//!
//! Because three features depend on it, this module is deliberately defensive: the input comes
//! from a language model and will contain repos that do not exist, edges asserted from only one
//! side, and cycles.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::model::GraphNode;

/// A normalised dependency graph over a project's repos.
///
/// Edges point **downstream**: `producer -> consumer`. `example-app -> example-app-ui` means the backend
/// produces a contract the frontend consumes, so the backend is upstream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepGraph {
    /// Every known repo, in a stable order.
    nodes: Vec<String>,
    /// `producer -> {consumers}`.
    downstream: BTreeMap<String, BTreeSet<String>>,
    /// `consumer -> {producers}`.
    upstream: BTreeMap<String, BTreeSet<String>>,
    /// Edges the model asserted that name a repo outside the project. Kept for reporting rather
    /// than silently dropped — a graph that quietly loses half its edges is worse than one that
    /// says so.
    dangling: Vec<(String, String)>,
}

impl DepGraph {
    /// Build from the per-repo declarations, keeping only edges where **both** ends are repos in
    /// this project.
    ///
    /// The two directions are merged: `a.produces_contracts_for = [b]` and `b.consumes = [a]` are
    /// the same edge, and either one alone is enough to create it. Models routinely fill in one
    /// side and forget the other.
    pub fn build(members: &[String], declared: &BTreeMap<String, GraphNode>) -> Self {
        let known: BTreeSet<&str> = members.iter().map(String::as_str).collect();
        let mut downstream: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut upstream: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut dangling = Vec::new();

        for name in members {
            downstream.entry(name.clone()).or_default();
            upstream.entry(name.clone()).or_default();
        }

        let mut add = |producer: &str, consumer: &str, dangling: &mut Vec<(String, String)>| {
            if producer == consumer {
                return; // a repo consuming itself is noise, not a cycle
            }
            if !known.contains(producer) || !known.contains(consumer) {
                dangling.push((producer.to_string(), consumer.to_string()));
                return;
            }
            downstream
                .entry(producer.to_string())
                .or_default()
                .insert(consumer.to_string());
            upstream
                .entry(consumer.to_string())
                .or_default()
                .insert(producer.to_string());
        };

        for (name, node) in declared {
            for consumer in &node.produces_contracts_for {
                add(name, consumer, &mut dangling);
            }
            for producer in &node.consumes {
                add(producer, name, &mut dangling);
            }
        }

        dangling.sort();
        dangling.dedup();

        Self {
            nodes: members.to_vec(),
            downstream,
            upstream,
            dangling,
        }
    }

    pub fn nodes(&self) -> &[String] {
        &self.nodes
    }

    pub fn contains(&self, repo: &str) -> bool {
        self.nodes.iter().any(|n| n == repo)
    }

    /// Repos that consume `repo`'s contracts — where a change here is likely to ripple.
    pub fn consumers_of(&self, repo: &str) -> Vec<String> {
        self.downstream
            .get(repo)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Repos whose contracts `repo` depends on.
    pub fn producers_for(&self, repo: &str) -> Vec<String> {
        self.upstream
            .get(repo)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Edges naming a repo outside the project.
    pub fn dangling_edges(&self) -> &[(String, String)] {
        &self.dangling
    }

    /// How far upstream a repo sits: the number of repos reachable downstream of it.
    ///
    /// This is the master-election bias of §5.7. Contracts flow downstream, so a master that has
    /// to wait on a repo it delegated to before it can define its own interface will stall.
    /// Electing the repo with the widest downstream reach avoids that.
    pub fn downstream_reach(&self, repo: &str) -> usize {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(repo.to_string());
        while let Some(cur) = queue.pop_front() {
            for next in self.downstream.get(&cur).into_iter().flatten() {
                if seen.insert(next.clone()) {
                    queue.push_back(next.clone());
                }
            }
        }
        seen.remove(repo);
        seen.len()
    }

    /// Rank candidates by how far upstream they sit, most-upstream first.
    ///
    /// Ties break by fewer direct producers (more genuinely upstream), then by name for
    /// determinism — an election that changes between runs on the same input is a bug.
    pub fn rank_upstream(&self, candidates: &[String]) -> Vec<String> {
        let mut v: Vec<String> = candidates
            .iter()
            .filter(|c| self.contains(c))
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            self.downstream_reach(b)
                .cmp(&self.downstream_reach(a))
                .then_with(|| {
                    self.producers_for(a)
                        .len()
                        .cmp(&self.producers_for(b).len())
                })
                .then_with(|| a.cmp(b))
        });
        v
    }

    /// Merge order: producers before consumers (§11.1).
    ///
    /// A Kahn topological sort. Ties break by name so the order is deterministic. Repos in a
    /// cycle cannot be ordered by the graph and are appended at the end, in name order, with the
    /// cycle reported separately rather than silently resolved.
    pub fn merge_order(&self) -> MergeOrder {
        let mut indegree: BTreeMap<&str, usize> = self
            .nodes
            .iter()
            .map(|n| (n.as_str(), self.upstream.get(n).map_or(0, |s| s.len())))
            .collect();

        // BTreeSet as the ready-set gives deterministic tie-breaking by name for free.
        let mut ready: BTreeSet<&str> = indegree
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(&n, _)| n)
            .collect();

        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(&next) = ready.iter().next() {
            ready.remove(next);
            order.push(next.to_string());
            for consumer in self.downstream.get(next).into_iter().flatten() {
                if let Some(d) = indegree.get_mut(consumer.as_str()) {
                    *d -= 1;
                    if *d == 0 {
                        ready.insert(consumer.as_str());
                    }
                }
            }
        }

        let cyclic: Vec<String> = self
            .nodes
            .iter()
            .filter(|n| !order.contains(n))
            .cloned()
            .collect();
        order.extend(cyclic.iter().cloned());
        MergeOrder { order, cyclic }
    }

    /// Check a master-proposed merge order against the graph (§11.1).
    ///
    /// A proposed order that contradicts the graph is a **warning shown to the user, not a silent
    /// override** — the master may know something the graph does not.
    pub fn validate_merge_order(&self, proposed: &[String]) -> Vec<OrderViolation> {
        let position: BTreeMap<&str, usize> = proposed
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        let mut out = Vec::new();
        for (producer, consumers) in &self.downstream {
            let Some(&pi) = position.get(producer.as_str()) else {
                continue;
            };
            for consumer in consumers {
                if let Some(&ci) = position.get(consumer.as_str()) {
                    if ci < pi {
                        out.push(OrderViolation {
                            producer: producer.clone(),
                            consumer: consumer.clone(),
                        });
                    }
                }
            }
        }
        out.sort();
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOrder {
    /// Every repo, producers before consumers where the graph allows it.
    pub order: Vec<String>,
    /// Repos that could not be ordered because they sit in a cycle. Empty in a healthy project.
    pub cyclic: Vec<String>,
}

/// A consumer scheduled to merge before one of its producers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrderViolation {
    pub producer: String,
    pub consumer: String,
}

impl std::fmt::Display for OrderViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` is scheduled before `{}`, which produces a contract it consumes",
            self.consumer, self.producer
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(consumes: &[&str], produces: &[&str]) -> GraphNode {
        GraphNode {
            consumes: consumes.iter().map(|s| s.to_string()).collect(),
            produces_contracts_for: produces.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn members(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The worked example from §8.1 / Appendix A.
    fn example_app() -> DepGraph {
        let m = members(&[
            "example-app",
            "example-app-ui",
            "example-app-ws",
            "example-app-js",
            "example-app-python",
            "example-app-docs",
            "example-app-www",
        ]);
        let mut d = BTreeMap::new();
        d.insert(
            "example-app".to_string(),
            node(
                &[],
                &[
                    "example-app-ui",
                    "example-app-js",
                    "example-app-python",
                    "example-app-docs",
                ],
            ),
        );
        d.insert(
            "example-app-ui".to_string(),
            node(&["example-app", "example-app-ws"], &[]),
        );
        d.insert("example-app-ws".to_string(), node(&[], &["example-app-ui"]));
        d.insert("example-app-js".to_string(), node(&["example-app"], &[]));
        d.insert(
            "example-app-python".to_string(),
            node(&["example-app"], &[]),
        );
        d.insert("example-app-docs".to_string(), node(&["example-app"], &[]));
        d.insert("example-app-www".to_string(), node(&[], &[]));
        DepGraph::build(&m, &d)
    }

    #[test]
    fn edges_merge_from_both_directions() {
        let m = members(&["a", "b"]);
        // Only the producer side is declared.
        let mut d1 = BTreeMap::new();
        d1.insert("a".to_string(), node(&[], &["b"]));
        let g1 = DepGraph::build(&m, &d1);

        // Only the consumer side is declared.
        let mut d2 = BTreeMap::new();
        d2.insert("b".to_string(), node(&["a"], &[]));
        let g2 = DepGraph::build(&m, &d2);

        assert_eq!(g1.consumers_of("a"), vec!["b".to_string()]);
        assert_eq!(g2.consumers_of("a"), vec!["b".to_string()]);
        assert_eq!(g1.producers_for("b"), vec!["a".to_string()]);
        assert_eq!(g2.producers_for("b"), vec!["a".to_string()]);
    }

    #[test]
    fn edges_to_unknown_repos_are_recorded_not_silently_dropped() {
        let m = members(&["a"]);
        let mut d = BTreeMap::new();
        d.insert("a".to_string(), node(&[], &["ghost"]));
        let g = DepGraph::build(&m, &d);
        assert!(g.consumers_of("a").is_empty());
        assert_eq!(
            g.dangling_edges(),
            &[("a".to_string(), "ghost".to_string())]
        );
    }

    #[test]
    fn self_edges_are_ignored() {
        let m = members(&["a"]);
        let mut d = BTreeMap::new();
        d.insert("a".to_string(), node(&["a"], &["a"]));
        let g = DepGraph::build(&m, &d);
        assert!(g.consumers_of("a").is_empty());
        assert!(g.dangling_edges().is_empty());
    }

    #[test]
    fn backend_is_furthest_upstream() {
        let g = example_app();
        assert_eq!(g.downstream_reach("example-app"), 4);
        assert_eq!(g.downstream_reach("example-app-ws"), 1);
        assert_eq!(g.downstream_reach("example-app-ui"), 0);
        assert_eq!(g.downstream_reach("example-app-www"), 0, "isolated repo");
    }

    /// §5.7: bias the election toward the repo furthest upstream among the likely-involved set.
    #[test]
    fn election_ranking_puts_the_backend_first() {
        let g = example_app();
        let ranked = g.rank_upstream(&members(&[
            "example-app-ui",
            "example-app-python",
            "example-app",
            "example-app-docs",
        ]));
        assert_eq!(ranked[0], "example-app");
    }

    #[test]
    fn ranking_ignores_repos_outside_the_project() {
        let g = example_app();
        let ranked = g.rank_upstream(&members(&["ghost", "example-app-ui"]));
        assert_eq!(ranked, vec!["example-app-ui".to_string()]);
    }

    #[test]
    fn merge_order_is_producers_before_consumers() {
        let g = example_app();
        let mo = g.merge_order();
        assert!(mo.cyclic.is_empty());
        let pos = |n: &str| mo.order.iter().position(|x| x == n).unwrap();
        assert!(pos("example-app") < pos("example-app-ui"));
        assert!(pos("example-app") < pos("example-app-js"));
        assert!(pos("example-app") < pos("example-app-python"));
        assert!(pos("example-app-ws") < pos("example-app-ui"));
        assert_eq!(mo.order.len(), 7, "every repo appears exactly once");
    }

    #[test]
    fn merge_order_is_deterministic() {
        let a = example_app().merge_order();
        let b = example_app().merge_order();
        assert_eq!(a, b);
    }

    /// A cycle must not hang the sort or drop repos — merge is a human-confirmed flow and it
    /// still has to present every repo.
    #[test]
    fn cycles_are_reported_not_silently_resolved() {
        let m = members(&["a", "b", "c"]);
        let mut d = BTreeMap::new();
        d.insert("a".to_string(), node(&[], &["b"]));
        d.insert("b".to_string(), node(&[], &["a"])); // a <-> b
        d.insert("c".to_string(), node(&[], &[]));
        let g = DepGraph::build(&m, &d);
        let mo = g.merge_order();
        assert_eq!(mo.cyclic, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(mo.order.len(), 3, "all repos still present");
        assert!(mo.order.contains(&"c".to_string()));
    }

    #[test]
    fn validate_flags_a_consumer_scheduled_first() {
        let g = example_app();
        let bad = members(&["example-app-ui", "example-app"]);
        let v = g.validate_merge_order(&bad);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].producer, "example-app");
        assert_eq!(v[0].consumer, "example-app-ui");
        assert!(v[0].to_string().contains("scheduled before"));
    }

    #[test]
    fn validate_accepts_the_graphs_own_order() {
        let g = example_app();
        assert!(g.validate_merge_order(&g.merge_order().order).is_empty());
    }

    #[test]
    fn validate_ignores_repos_absent_from_the_proposal() {
        // A master may legitimately omit unchanged repos from merge_order (§11.1).
        let g = example_app();
        assert!(g
            .validate_merge_order(&members(&["example-app", "example-app-ui"]))
            .is_empty());
        assert!(g
            .validate_merge_order(&members(&["example-app-www"]))
            .is_empty());
    }

    #[test]
    fn empty_graph_is_harmless() {
        let g = DepGraph::build(&[], &BTreeMap::new());
        assert!(g.merge_order().order.is_empty());
        assert!(g.rank_upstream(&members(&["x"])).is_empty());
        assert_eq!(g.downstream_reach("x"), 0);
    }
}
