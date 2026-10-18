//! The feed an application announces its own changes on.

use std::sync::{Arc, Mutex};

use clerkenwell_session::{Announced, AnnouncementFeed};

/// Each announcement a listener heard, with the listener that heard it.
type Heard = Arc<Mutex<Vec<(usize, Announced<u32>)>>>;

#[test]
fn every_listener_hears_every_announcement_in_the_order_it_was_made() {
    let feed = AnnouncementFeed::<u32>::default();
    let heard: Heard = Arc::default();
    for listener in 0..2 {
        let heard = Arc::clone(&heard);
        feed.listen(move |announced| {
            heard
                .lock()
                .expect("heard")
                .push((listener, Announced::clone(announced)));
        });
    }

    let announcements = [
        (&["Note"][..], 1, 10),
        (&["Note", "Board"][..], 2, 20),
        (&[][..], 3, 30),
    ];
    for (touches, modification, announcement) in announcements {
        feed.announce(touches, modification, announcement);
    }

    let heard = heard.lock().expect("heard");
    for listener in 0..2 {
        let by_listener: Vec<_> = heard
            .iter()
            .filter(|(by, _)| *by == listener)
            .map(|(_, announced)| {
                (
                    announced.touches,
                    announced.modification,
                    announced.announcement,
                )
            })
            .collect();
        assert_eq!(by_listener, announcements);
    }
}

#[test]
fn an_announcement_made_before_a_listener_listens_is_not_heard_by_it() {
    let feed = AnnouncementFeed::<u32>::default();
    feed.announce(&["Note"], 1, 10);
    let heard: Arc<Mutex<Vec<u32>>> = Arc::default();
    let recorder = Arc::clone(&heard);
    feed.listen(move |announced| recorder.lock().expect("heard").push(announced.announcement));

    feed.announce(&["Note"], 2, 20);

    assert_eq!(*heard.lock().expect("heard"), [20]);
}
