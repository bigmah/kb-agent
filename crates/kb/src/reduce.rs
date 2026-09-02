//! Reducing the long list: every pair compared, the pairs that match merged.
//!
//! Every source that was read answers with its own points, and every book on
//! a subject makes the same three points, so the list that comes back from the
//! reading pool says most things several times. The reduction compares every
//! pair of points — each comparison in a fresh context, seeing only those two
//! — and then merges each group of points that were judged the same into one
//! point carrying everything the group did.
//!
//! # Groups, not pairs
//!
//! "Same" is judged pairwise but applied transitively: if A is the same as B
//! and B the same as C, all three become one point, whatever the verdict on
//! A and C alone. That is what "the same information" means once it is
//! written down — a list where A and C survive as separate points because
//! their pair was judged on a strict day is not a refined list. The merge sees
//! the whole group, so nothing one of them said is lost.
//!
//! # Cost
//!
//! Comparisons are the square of the list: `n` points is `n(n-1)/2`
//! requests, each tiny. Two hundred points is twenty thousand requests; it is
//! the caller's job to know that before starting — see
//! [`pairs_for`] — and [`QueryOptions::reduce`] turns the stage off.

use std::time::Instant;

use agent::Usage;

use crate::query::{Point, QueryOptions, Reduction};
use crate::report::{Progress, Stage};
use crate::{Error, fanout};

/// How many comparisons reducing `points` points takes.
pub fn pairs_for(points: usize) -> usize {
    points * points.saturating_sub(1) / 2
}

/// Compare every pair, merge the groups, and return the refined list.
pub async fn reduce(points: Vec<Point>, options: &QueryOptions) -> Result<Reduction, Error> {
    let before = points.len();

    // Every pair once, in a fresh context each.
    let started = Instant::now();
    let pairs: Vec<(usize, usize)> = (0..points.len())
        .flat_map(|i| (i + 1..points.len()).map(move |j| (i, j)))
        .collect();
    let total = pairs.len();
    let verdicts = fanout::fan_out(
        pairs.clone(),
        options.concurrency,
        |done, total| options.emit(Progress::Comparing { done, total }),
        |(i, j)| {
            let (a, b) = (&points[i].text, &points[j].text);
            async move {
                options
                    .agent
                    .same_point(a, b)
                    .await
                    .map_err(Error::agent(format!("comparing points {} and {}", i + 1, j + 1)))
            }
        },
    )
    .await?;
    let compare_usage = verdicts.iter().fold(Usage::default(), |sum, r| sum + r.usage);
    let compare = Stage::from(started, compare_usage);

    let same = pairs
        .iter()
        .zip(&verdicts)
        .filter(|(_, verdict)| verdict.value)
        .map(|(pair, _)| *pair);
    let groups = clusters(points.len(), same);

    // One merge per group that has more than one point in it.
    let started = Instant::now();
    let to_merge: Vec<(usize, Vec<String>)> = groups
        .iter()
        .enumerate()
        .filter(|(_, group)| group.len() > 1)
        .map(|(g, group)| (g, group.iter().map(|&i| points[i].text.clone()).collect()))
        .collect();
    let merged_count = to_merge.len();
    let merged = fanout::fan_out(
        to_merge,
        options.concurrency,
        |done, total| options.emit(Progress::Merging { done, total }),
        |(g, texts)| async move {
            options
                .agent
                .merge_points(&texts)
                .await
                .map(|reply| (g, reply))
                .map_err(Error::agent(format!("merging group {}", g + 1)))
        },
    )
    .await?;
    let merge_usage = merged.iter().fold(Usage::default(), |sum, (_, r)| sum + r.usage);
    let merge = Stage::from(started, merge_usage);

    let mut merged_text: Vec<Option<String>> = (0..groups.len()).map(|_| None).collect();
    for (g, reply) in merged {
        merged_text[g] = Some(reply.value);
    }
    let reduced = groups
        .iter()
        .enumerate()
        .map(|(g, group)| {
            let mut sources = Vec::new();
            for &i in group {
                for source in &points[i].sources {
                    if !sources.contains(source) {
                        sources.push(source.clone());
                    }
                }
            }
            let text = merged_text[g]
                .take()
                .unwrap_or_else(|| points[group[0]].text.clone());
            Point { text, sources }
        })
        .collect();

    Ok(Reduction {
        points: reduced,
        before,
        pairs: total,
        merged: merged_count,
        compare,
        merge,
    })
}

/// Group `n` items by the pairs judged the same, transitively. Groups come
/// back ordered by their earliest member, members in order, so the reduced
/// list reads in the order the reading produced it.
pub(crate) fn clusters(n: usize, same: impl IntoIterator<Item = (usize, usize)>) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut root = i;
        while parent[root] != root {
            root = parent[root];
        }
        let mut i = i;
        while parent[i] != root {
            let next = parent[i];
            parent[i] = root;
            i = next;
        }
        root
    }
    for (a, b) in same {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            // Attach the later root under the earlier one, so a group's root
            // is its earliest member and the order below falls out for free.
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            parent[hi] = lo;
        }
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut slot_of_root: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let root = find(&mut parent, i);
        match slot_of_root[root] {
            Some(slot) => groups[slot].push(i),
            None => {
                slot_of_root[root] = Some(groups.len());
                groups.push(vec![i]);
            }
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_are_the_triangle() {
        assert_eq!(pairs_for(0), 0);
        assert_eq!(pairs_for(1), 0);
        assert_eq!(pairs_for(2), 1);
        assert_eq!(pairs_for(10), 45);
        assert_eq!(pairs_for(200), 19_900);
    }

    #[test]
    fn same_is_applied_transitively_and_groups_keep_reading_order() {
        // 0~1, 1~4 makes {0,1,4}; 2~3 makes {2,3}; 5 stands alone.
        let groups = clusters(6, [(1, 4), (0, 1), (2, 3)]);
        assert_eq!(groups, [vec![0, 1, 4], vec![2, 3], vec![5]]);
    }

    #[test]
    fn nothing_the_same_leaves_every_point_its_own_group() {
        assert_eq!(clusters(3, []), [vec![0], vec![1], vec![2]]);
        assert!(clusters(0, []).is_empty());
    }

    #[test]
    fn a_group_s_order_does_not_depend_on_the_order_pairs_arrived() {
        let a = clusters(5, [(3, 4), (0, 4), (1, 2)]);
        let b = clusters(5, [(0, 4), (1, 2), (3, 4)]);
        assert_eq!(a, b);
        assert_eq!(a, [vec![0, 3, 4], vec![1, 2]]);
    }
}
