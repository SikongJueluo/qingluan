//! The file graph: dedup edges, count fans, find strongly connected
//! components.
//!
//! Nodes are indices into the sorted file list; edges are a sorted set, so
//! repeated runs over unchanged code produce byte-identical reports (the
//! same contract as the complexity tables).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Directed graph over indexed nodes.
pub(crate) struct FileGraph {
    /// Adjacency: `edges[from]` is the sorted, deduplicated `to` list.
    edges: Vec<Vec<usize>>,
}

impl FileGraph {
    /// Build from `(from, to)` index pairs. Self-edges are dropped: they are
    /// within-file references, not coupling between files.
    pub(crate) fn new(
        node_count: usize,
        raw_edges: impl IntoIterator<Item = (usize, usize)>,
    ) -> Self {
        let mut sets: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); node_count];
        for (from, to) in raw_edges {
            if from != to {
                sets[from].insert(to);
            }
        }
        FileGraph {
            edges: sets
                .into_iter()
                .map(|set| set.into_iter().collect())
                .collect(),
        }
    }

    pub(crate) fn fan_out(&self, node: usize) -> u32 {
        self.edges[node].len() as u32
    }

    /// All fan-ins in one pass (a per-node scan would be quadratic).
    pub(crate) fn fan_ins(&self) -> Vec<u32> {
        let mut ins = vec![0u32; self.edges.len()];
        for adj in &self.edges {
            for to in adj {
                ins[*to] += 1;
            }
        }
        ins
    }

    /// Strongly connected components of size ≥ 2 (size-1 components cannot
    /// be cycles here — self-edges are already dropped).
    ///
    /// Iterative Tarjan: a recursive formulation would put the stack depth
    /// at the mercy of the longest dependency chain in the repo.
    pub(crate) fn cycles(&self) -> Vec<Vec<usize>> {
        let n = self.edges.len();
        let mut index = vec![usize::MAX; n];
        let mut low = vec![0usize; n];
        let mut on_stack = vec![false; n];
        let mut stack: Vec<usize> = Vec::new();
        let mut next_index = 0usize;
        let mut out: Vec<Vec<usize>> = Vec::new();

        // Frame: (node, next child position).
        for root in 0..n {
            if index[root] != usize::MAX {
                continue;
            }
            let mut frames: Vec<(usize, usize)> = vec![(root, 0)];
            while let Some(&mut (node, ref mut pos)) = frames.last_mut() {
                if *pos == 0 {
                    index[node] = next_index;
                    low[node] = next_index;
                    next_index += 1;
                    stack.push(node);
                    on_stack[node] = true;
                }
                let adj = &self.edges[node];
                let mut advanced = false;
                while *pos < adj.len() {
                    let child = adj[*pos];
                    *pos += 1;
                    if index[child] == usize::MAX {
                        frames.push((child, 0));
                        advanced = true;
                        break;
                    } else if on_stack[child] {
                        low[node] = low[node].min(index[child]);
                    }
                }
                if advanced {
                    continue;
                }
                // Node fully explored: pop the frame, propagate the lowlink,
                // and emit an SCC when this node is its root.
                let (done, _) = frames.pop().expect("frame exists");
                if let Some(&mut (parent, _)) = frames.last_mut() {
                    low[parent] = low[parent].min(low[done]);
                }
                if low[done] == index[done] {
                    let mut component = Vec::new();
                    while let Some(top) = stack.pop() {
                        on_stack[top] = false;
                        component.push(top);
                        if top == done {
                            break;
                        }
                    }
                    if component.len() >= 2 {
                        component.sort_unstable();
                        out.push(component);
                    }
                }
            }
        }
        out
    }
}

/// One dependency cycle: an SCC that is not a single node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cycle {
    /// Member files, lexicographically sorted.
    pub files: Vec<PathBuf>,
    /// Whether the members span more than one directory — the interesting
    /// cycles cross package boundaries (research §8: "光数环没意义").
    pub cross_directory: bool,
}

impl Cycle {
    pub fn new(files: Vec<PathBuf>) -> Self {
        let cross_directory = files
            .iter()
            .map(|f| f.parent().unwrap_or(Path::new("")))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1;
        Cycle {
            files,
            cross_directory,
        }
    }
}

/// Order cycles for the report: cross-directory first, then larger, then by
/// first member — deterministic and puts the architecturally meaningful
/// tangles on top.
pub(crate) fn sort_cycles(cycles: &mut [Cycle]) {
    cycles.sort_by(|a, b| {
        b.cross_directory
            .cmp(&a.cross_directory)
            .then_with(|| b.files.len().cmp(&a.files.len()))
            .then_with(|| a.files.cmp(&b.files))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn two_node_cycle_is_detected() {
        let graph = FileGraph::new(2, [(0, 1), (1, 0)]);
        assert_eq!(graph.cycles(), vec![vec![0, 1]]);
    }

    #[test]
    fn dag_has_no_cycles() {
        let graph = FileGraph::new(3, [(0, 1), (1, 2), (0, 2)]);
        assert!(graph.cycles().is_empty());
    }

    #[test]
    fn two_independent_cycles_both_reported() {
        let graph = FileGraph::new(5, [(0, 1), (1, 0), (3, 4), (4, 3), (2, 0)]);
        let cycles = graph.cycles();
        assert_eq!(cycles.len(), 2);
        assert!(cycles.contains(&vec![0, 1]));
        assert!(cycles.contains(&vec![3, 4]));
    }

    #[test]
    fn a_three_node_scc_is_one_cycle_not_three() {
        let graph = FileGraph::new(3, [(0, 1), (1, 2), (2, 0), (0, 2)]);
        assert_eq!(graph.cycles(), vec![vec![0, 1, 2]]);
    }

    #[test]
    fn self_edges_are_dropped() {
        let graph = FileGraph::new(1, [(0, 0)]);
        assert!(graph.cycles().is_empty());
        assert_eq!(graph.fan_ins()[0], 0);
        assert_eq!(graph.fan_out(0), 0);
    }

    #[test]
    fn duplicate_edges_count_once() {
        let graph = FileGraph::new(2, [(0, 1), (0, 1), (0, 1)]);
        assert_eq!(graph.fan_out(0), 1);
        assert_eq!(graph.fan_ins()[1], 1);
    }

    #[test]
    fn fans_and_instability_direction() {
        // 0 -> 1, 0 -> 2, 3 -> 0: file 0 is depended on once and depends on
        // twice, so its instability I = Ce/(Ca+Ce) = 2/3.
        let graph = FileGraph::new(4, [(0, 1), (0, 2), (3, 0)]);
        assert_eq!(graph.fan_ins(), vec![1, 1, 1, 0]);
        assert_eq!(graph.fan_out(0), 2);
    }

    #[test]
    fn long_chain_does_not_blow_the_stack() {
        // A 50_000-node path would kill a recursive Tarjan.
        let n = 50_000;
        let edges = (0..n - 1).map(|i| (i, i + 1));
        let graph = FileGraph::new(n, edges);
        assert!(graph.cycles().is_empty());
    }

    #[test]
    fn cycle_flags_cross_directory() {
        let cycle = Cycle::new(paths(&["a/x.rs", "a/y.rs"]));
        assert!(!cycle.cross_directory);
        let cycle = Cycle::new(paths(&["a/x.rs", "b/y.rs"]));
        assert!(cycle.cross_directory);
    }

    #[test]
    fn cycles_sort_cross_directory_first() {
        let mut cycles = vec![
            Cycle::new(paths(&["a/x.rs", "a/y.rs"])),
            Cycle::new(paths(&["a/x.rs", "b/y.rs"])),
        ];
        sort_cycles(&mut cycles);
        assert!(cycles[0].cross_directory);
    }
}
