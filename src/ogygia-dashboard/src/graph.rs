//! Layout of the commit timeline as a branching graph in the style of
//! `jj log`: one commit per row, with edges to its parents kept in lanes.

use std::collections::HashMap;

use crate::nixos::CommitInfo;

/// Order commits so each one comes before its parents, following each branch
/// down to where it forks before moving on to the next. Heads keep their
/// relative input order.
pub fn topo_order(commits: Vec<CommitInfo>) -> Vec<CommitInfo> {
    let index: HashMap<&str, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.hash(), i))
        .collect();
    let parent_indices = |i: usize| {
        commits[i]
            .parents()
            .iter()
            .filter_map(|p| index.get(p.hash.as_str()).copied())
    };

    let mut unplaced_children = vec![0; commits.len()];
    for i in 0..commits.len() {
        for p in parent_indices(i) {
            unplaced_children[p] += 1;
        }
    }

    let mut stack: Vec<usize> = (0..commits.len())
        .rev()
        .filter(|&i| unplaced_children[i] == 0)
        .collect();
    let mut order = Vec::with_capacity(commits.len());
    while let Some(i) = stack.pop() {
        order.push(i);
        // Reversed so the first parent is visited next.
        for p in parent_indices(i).collect::<Vec<_>>().into_iter().rev() {
            unplaced_children[p] -= 1;
            if unplaced_children[p] == 0 {
                stack.push(p);
            }
        }
    }
    drop(index);

    let mut commits: Vec<Option<CommitInfo>> = commits.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|i| commits[i].take())
        .collect()
}

/// Vertical position within a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Y {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Vertical {
        lane: usize,
        from: Y,
        to: Y,
        elided: bool,
    },
    /// Runs across the middle of the row, drawn in the colour of `lane`.
    Horizontal {
        from: usize,
        to: usize,
        lane: usize,
        elided: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Lane holding the commit's node.
    pub column: usize,
    pub segments: Vec<Segment>,
}

impl Row {
    /// Number of lanes this row draws in.
    pub fn width(&self) -> usize {
        self.segments
            .iter()
            .map(|s| match *s {
                Segment::Vertical { lane, .. } => lane,
                Segment::Horizontal { from, to, .. } => from.max(to),
            })
            .fold(self.column, usize::max)
            + 1
    }
}

/// An edge travelling down a lane towards `target`.
struct Edge<'a> {
    target: &'a str,
    elided: bool,
}

/// Assign every commit, given in [`topo_order`], a lane and the line segments
/// connecting it to the rows above and below.
pub fn layout(commits: &[CommitInfo]) -> Vec<Row> {
    let mut lanes: Vec<Option<Edge>> = Vec::new();

    commits
        .iter()
        .map(|commit| {
            let hash = commit.hash();
            let mut segments = Vec::new();

            let incoming: Vec<usize> = lanes
                .iter()
                .enumerate()
                .filter(|(_, l)| l.as_ref().is_some_and(|l| l.target == hash))
                .map(|(i, _)| i)
                .collect();
            let column = match incoming.first() {
                Some(&i) => i,
                None => free_lane(&mut lanes, &[]),
            };

            for (lane, edge) in lanes.iter().enumerate() {
                let Some(edge) = edge else { continue };
                let elided = edge.elided;
                if edge.target != hash {
                    segments.push(Segment::Vertical {
                        lane,
                        from: Y::Top,
                        to: Y::Bottom,
                        elided,
                    });
                    continue;
                }
                segments.push(Segment::Vertical {
                    lane,
                    from: Y::Top,
                    to: Y::Middle,
                    elided,
                });
                if lane != column {
                    segments.push(Segment::Horizontal {
                        from: lane,
                        to: column,
                        lane,
                        elided,
                    });
                }
            }
            for &i in &incoming {
                lanes[i] = None;
            }

            for (n, parent) in commit.parents().iter().enumerate() {
                let elided = parent.elided;
                let shared = lanes.iter().position(|l| {
                    l.as_ref()
                        .is_some_and(|l| l.target == parent.hash && l.elided == elided)
                });
                let lane = match shared {
                    // Already passing straight through this row.
                    Some(lane) => lane,
                    None => {
                        let lane = if n == 0 {
                            column
                        } else {
                            // Avoid lanes that just merged into this commit so
                            // an edge doesn't turn straight back on itself.
                            free_lane(&mut lanes, &incoming)
                        };
                        lanes[lane] = Some(Edge {
                            target: &parent.hash,
                            elided,
                        });
                        segments.push(Segment::Vertical {
                            lane,
                            from: Y::Middle,
                            to: Y::Bottom,
                            elided,
                        });
                        lane
                    }
                };
                if lane != column {
                    segments.push(Segment::Horizontal {
                        from: column,
                        to: lane,
                        lane,
                        elided,
                    });
                }
            }

            while lanes.last().is_some_and(Option::is_none) {
                lanes.pop();
            }

            Row { column, segments }
        })
        .collect()
}

fn free_lane(lanes: &mut Vec<Option<Edge>>, avoid: &[usize]) -> usize {
    match (0..lanes.len()).find(|i| lanes[*i].is_none() && !avoid.contains(i)) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::nixos::GraphParent;

    fn commit(hash: &str, parents: &[(&str, bool)]) -> CommitInfo {
        CommitInfo::Complete {
            hash: hash.to_string(),
            message: String::new(),
            author: String::new(),
            timestamp: Utc::now(),
            branch: String::new(),
            hosts_using: Vec::new(),
            parents: parents
                .iter()
                .map(|&(hash, elided)| GraphParent {
                    hash: hash.to_string(),
                    elided,
                })
                .collect(),
        }
    }

    fn hashes(commits: &[CommitInfo]) -> Vec<&str> {
        commits.iter().map(CommitInfo::hash).collect()
    }

    #[test]
    fn topo_order_keeps_branches_together() {
        // main: m2 -> m1 -> base; branch: b2 -> b1 -> base. Newest first by
        // time interleaves the two.
        let commits = vec![
            commit("m2", &[("m1", false)]),
            commit("b2", &[("b1", false)]),
            commit("m1", &[("base", false)]),
            commit("b1", &[("base", false)]),
            commit("base", &[]),
        ];
        assert_eq!(
            hashes(&topo_order(commits)),
            ["m2", "m1", "b2", "b1", "base"]
        );
    }

    #[test]
    fn topo_order_puts_children_before_parents() {
        // Clock skew made the parent look newer than its child.
        let commits = vec![commit("parent", &[]), commit("child", &[("parent", false)])];
        assert_eq!(hashes(&topo_order(commits)), ["child", "parent"]);
    }

    #[test]
    fn layout_fork() {
        let commits = vec![
            commit("main", &[("base", false)]),
            commit("branch", &[("base", true)]),
            commit("base", &[]),
        ];
        let rows = layout(&commits);

        assert_eq!(rows[0].column, 0);
        assert_eq!(
            rows[0].segments,
            [Segment::Vertical {
                lane: 0,
                from: Y::Middle,
                to: Y::Bottom,
                elided: false
            }]
        );

        assert_eq!(rows[1].column, 1);
        assert!(rows[1].segments.contains(&Segment::Vertical {
            lane: 0,
            from: Y::Top,
            to: Y::Bottom,
            elided: false
        }));
        assert!(rows[1].segments.contains(&Segment::Vertical {
            lane: 1,
            from: Y::Middle,
            to: Y::Bottom,
            elided: true
        }));

        assert_eq!(rows[2].column, 0);
        assert!(rows[2].segments.contains(&Segment::Horizontal {
            from: 1,
            to: 0,
            lane: 1,
            elided: true
        }));
        assert_eq!(rows[2].width(), 2);
    }

    #[test]
    fn layout_merge_shares_passing_lane() {
        // m's second parent b is reached before b's own row via another
        // lane, so the merge edge joins that lane instead of opening one.
        let commits = vec![
            commit("x", &[("b", false)]),
            commit("m", &[("a", false), ("b", false)]),
            commit("a", &[]),
            commit("b", &[]),
        ];
        let rows = layout(&commits);
        assert_eq!(rows[1].column, 1);
        assert!(rows[1].segments.contains(&Segment::Horizontal {
            from: 1,
            to: 0,
            lane: 0,
            elided: false
        }));
        assert!(rows.iter().all(|r| r.width() <= 2));
    }

    #[test]
    fn layout_unconnected_commits_reuse_lane() {
        let commits = vec![commit("missing", &[]), commit("lone", &[])];
        let rows = layout(&commits);
        assert_eq!(rows[0].column, 0);
        assert_eq!(rows[1].column, 0);
        assert!(rows.iter().all(|r| r.segments.is_empty()));
    }
}
