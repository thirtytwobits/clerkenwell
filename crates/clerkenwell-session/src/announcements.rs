//! Changes an application makes outside any request.

use std::fmt;
use std::sync::{Arc, RwLock};

/// A change the application made outside any request: the entities it
/// touches, and the application's account of it, from which its host builds
/// each subscription's patch.
#[derive(Debug, Clone, PartialEq)]
pub struct Announced<N> {
    pub touches: &'static [&'static str],
    pub announcement: N,
}

type Listener<N> = Arc<dyn Fn(&Arc<Announced<N>>) + Send + Sync>;

/// The in-process feed an application announces the changes it makes outside
/// any request on. Every listener hears every announcement; the announcements
/// one thread makes are heard in the order it made them.
pub struct AnnouncementFeed<N> {
    listeners: RwLock<Vec<Listener<N>>>,
}

impl<N> Default for AnnouncementFeed<N> {
    fn default() -> Self {
        Self {
            listeners: RwLock::new(Vec::new()),
        }
    }
}

impl<N> AnnouncementFeed<N> {
    /// Adds `listener`, which hears every announcement made from now on. A
    /// listener runs on the announcing thread, so it hands work off rather
    /// than doing it.
    pub fn listen(&self, listener: impl Fn(&Arc<Announced<N>>) + Send + Sync + 'static) {
        self.listeners
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Arc::new(listener));
    }

    /// Tells every listener that the application changed the entities
    /// `touches` names, as `announcement` describes.
    pub fn announce(&self, touches: &'static [&'static str], announcement: N) {
        let announced = Arc::new(Announced {
            touches,
            announcement,
        });
        let listeners = self
            .listeners
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        for listener in listeners {
            listener(&announced);
        }
    }
}

impl<N> fmt::Debug for AnnouncementFeed<N> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnnouncementFeed")
            .field(
                "listeners",
                &self
                    .listeners
                    .read()
                    .map(|listeners| listeners.len())
                    .unwrap_or_default(),
            )
            .finish()
    }
}
