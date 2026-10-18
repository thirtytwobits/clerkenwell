//! The counter a host numbers its modifications from.

use clerkenwell_session::ModificationCounter;

#[test]
fn each_number_taken_follows_the_last_and_last_names_it() {
    let counter = ModificationCounter::default();
    let before = counter.last();

    let first = counter.take();
    let second = counter.take();

    assert!(first > before);
    assert!(second > first);
    assert_eq!(counter.last(), second);
}

#[test]
fn numbers_taken_at_once_on_several_threads_are_each_taken_once() {
    let counter = ModificationCounter::default();
    let (threads, each) = (4, 500);

    let mut taken: Vec<u64> = std::thread::scope(|scope| {
        let takers: Vec<_> = (0..threads)
            .map(|_| scope.spawn(|| (0..each).map(|_| counter.take()).collect::<Vec<_>>()))
            .collect();
        takers
            .into_iter()
            .flat_map(|taker| taker.join().expect("a taker"))
            .collect()
    });
    taken.sort_unstable();
    taken.dedup();

    assert_eq!(taken.len(), threads * each);
    assert_eq!(counter.last(), *taken.last().expect("numbers taken"));
}
