//! Many requests in flight, a bounded number at a time.
//!
//! This is the whole of the reading pool's machinery. Every stage of a query
//! is a list of independent requests — one per document, one per pair of
//! points — so every stage is this one function with a different closure.
//! Results come back in the order the items went in, whatever order they
//! finished in, and the first failure stops everything still in flight: a
//! question is answered from the whole library or not at all, so there is no
//! point finishing the rest once one read is lost.

use futures::StreamExt;

use crate::Error;

/// Run `work` over every item with at most `concurrency` in flight, calling
/// `on_done(done, total)` as each lands, and return the results in item order.
pub(crate) async fn fan_out<T, R, Fut>(
    items: Vec<T>,
    concurrency: usize,
    mut on_done: impl FnMut(usize, usize),
    work: impl Fn(T) -> Fut,
) -> Result<Vec<R>, Error>
where
    Fut: Future<Output = Result<R, Error>>,
{
    let total = items.len();
    let mut slots: Vec<Option<R>> = (0..total).map(|_| None).collect();
    let mut in_flight = futures::stream::iter(
        items
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let future = work(item);
                async move { (index, future.await) }
            }),
    )
    .buffer_unordered(concurrency.max(1));

    let mut done = 0;
    while let Some((index, outcome)) = in_flight.next().await {
        // Dropping `in_flight` on the way out cancels whatever is still
        // running; a request that has not returned has cost nothing yet.
        slots[index] = Some(outcome?);
        done += 1;
        on_done(done, total);
    }
    drop(in_flight);

    Ok(slots
        .into_iter()
        .map(|slot| slot.expect("every index landed exactly once"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn results_come_back_in_item_order_whatever_order_they_finish() {
        // Later items finish first; the output must not care.
        let results = fan_out(vec![30u64, 20, 10, 0], 4, |_, _| {}, |ms| async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            Ok::<u64, Error>(ms * 2)
        })
        .await
        .unwrap();
        assert_eq!(results, [60, 40, 20, 0]);
    }

    #[tokio::test]
    async fn no_more_than_the_limit_run_at_once() {
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let results = fan_out(vec![(); 20], 3, |_, _| {}, |()| {
            let (running, peak) = (running.clone(), peak.clone());
            async move {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                running.fetch_sub(1, Ordering::SeqCst);
                Ok::<(), Error>(())
            }
        })
        .await
        .unwrap();
        assert_eq!(results.len(), 20);
        assert!(peak.load(Ordering::SeqCst) <= 3, "{}", peak.load(Ordering::SeqCst));
        assert!(peak.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn the_first_failure_stops_the_run_and_progress_counts_what_landed() {
        let mut seen = Vec::new();
        let outcome = fan_out(
            vec![1, 2, 3, 4],
            1,
            |done, total| seen.push((done, total)),
            |n| async move {
                if n == 3 {
                    Err(Error::Options("three".to_string()))
                } else {
                    Ok(n)
                }
            },
        )
        .await;
        assert!(matches!(outcome, Err(Error::Options(_))));
        assert_eq!(seen, [(1, 4), (2, 4)]);
    }

    #[tokio::test]
    async fn an_empty_list_is_an_empty_result() {
        let results: Vec<()> = fan_out(Vec::<()>::new(), 8, |_, _| {}, |()| async { Ok(()) })
            .await
            .unwrap();
        assert!(results.is_empty());
    }
}
