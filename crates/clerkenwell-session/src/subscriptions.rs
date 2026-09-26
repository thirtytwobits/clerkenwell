use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::transport::ProjectionSubscribeResume;

/// How many patches a connection retains for resuming subscriptions, unless
/// the application chooses otherwise.
pub const DEFAULT_RETAINED_PATCH_WINDOW: usize = 64;

/// One projection subscription and the revision its client holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSubscription {
    pub projection: String,
    pub params: Value,
    pub revision: u64,
    /// Where the last delivery left the client, for a projection that sends
    /// only what follows it, such as a collaborative document's accepted
    /// frontier. `None` until such a delivery.
    pub delivered: Option<String>,
}

/// A patch the connection sent, retained so a resuming subscription can
/// replay it instead of taking a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct RetainedProjectionPatch<P> {
    pub projection: String,
    pub params: Value,
    pub from_revision: u64,
    pub to_revision: u64,
    pub patch: P,
}

/// A subscription's cursor is ahead of the projection's current revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorAhead {
    pub cursor_revision: u64,
    pub current_revision: u64,
}

/// An accepted subscription and how its client catches up.
#[derive(Debug, Clone, PartialEq)]
pub struct SubscribeOutcome<P> {
    pub subscription_id: u64,
    /// Absent when the client subscribed without a cursor.
    pub resume: Option<ProjectionSubscribeResume>,
    /// The retained patches to replay, in order. `None` means the client
    /// needs a snapshot; an empty list means it is up to date.
    pub replay: Option<Vec<RetainedProjectionPatch<P>>>,
}

/// Counters describing one connection's subscriptions. Parameters are
/// reported only as a hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRuntimeDiagnostics {
    pub subscriptions: Vec<ProjectionSubscriptionDiagnostics>,
    pub current_revision: u64,
    pub retained_window_floor: u64,
    pub retained_window_ceiling: u64,
    pub retained_patch_count: u64,
    pub dropped_update_count: u64,
    pub gapped_update_count: u64,
    pub resync_count: u64,
    pub last_resync_reason: Option<String>,
    pub resume_up_to_date_count: u64,
    pub resume_patch_count: u64,
    pub resume_snapshot_count: u64,
    pub mutation_count: u64,
    pub mutation_rejection_count: u64,
    pub mutation_latency_micros_total: u64,
    pub mutation_latency_micros_max: u64,
    /// Rejection counts by category.
    pub rejections: BTreeMap<String, u64>,
}

/// One subscription as diagnostics report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSubscriptionDiagnostics {
    pub projection: String,
    pub params_sha256: String,
    pub client_revision: u64,
}

#[derive(Debug, Default)]
struct Counters {
    dropped_update_count: u64,
    gapped_update_count: u64,
    resync_count: u64,
    last_resync_reason: Option<String>,
    resume_up_to_date_count: u64,
    resume_patch_count: u64,
    resume_snapshot_count: u64,
    mutation_count: u64,
    mutation_rejection_count: u64,
    mutation_latency_micros_total: u64,
    mutation_latency_micros_max: u64,
    rejections: BTreeMap<String, u64>,
}

/// One connection's projection subscriptions and the patches retained for
/// resuming them. `P` is the application's patch payload.
#[derive(Debug)]
pub struct ProjectionSubscriptions<P> {
    next_subscription_id: u64,
    subscriptions: HashMap<u64, ProjectionSubscription>,
    retained_patch_window: usize,
    retained_patches: VecDeque<RetainedProjectionPatch<P>>,
    counters: Counters,
}

impl<P> Default for ProjectionSubscriptions<P> {
    fn default() -> Self {
        Self::new(DEFAULT_RETAINED_PATCH_WINDOW)
    }
}

impl<P> ProjectionSubscriptions<P> {
    /// Subscriptions that retain up to `retained_patch_window` patches.
    pub fn new(retained_patch_window: usize) -> Self {
        Self {
            next_subscription_id: 1,
            subscriptions: HashMap::new(),
            retained_patch_window,
            retained_patches: VecDeque::new(),
            counters: Counters::default(),
        }
    }

    /// Registers a subscription at `revision` without a resume decision.
    pub fn insert(&mut self, projection: String, params: Value, revision: u64) -> u64 {
        let subscription_id = self.next_subscription_id;
        self.next_subscription_id += 1;
        self.subscriptions.insert(
            subscription_id,
            ProjectionSubscription {
                projection,
                params,
                revision,
                delivered: None,
            },
        );
        subscription_id
    }

    pub fn get(&self, subscription_id: u64) -> Option<&ProjectionSubscription> {
        self.subscriptions.get(&subscription_id)
    }

    pub fn all(&self) -> &HashMap<u64, ProjectionSubscription> {
        &self.subscriptions
    }

    pub fn update_revision(&mut self, subscription_id: u64, revision: u64) {
        if let Some(subscription) = self.subscriptions.get_mut(&subscription_id) {
            subscription.revision = revision;
        }
    }

    /// Records a delivery that took the client to `revision` and left it at
    /// `delivered`.
    pub fn record_delivery(&mut self, subscription_id: u64, revision: u64, delivered: String) {
        if let Some(subscription) = self.subscriptions.get_mut(&subscription_id) {
            subscription.revision = revision;
            subscription.delivered = Some(delivered);
        }
    }

    /// Records a delivery that follows the one the client held at
    /// `from_revision`, unless another delivery reached it since. Only a
    /// recorded delivery may be sent: one built on a superseded delivery
    /// would not follow what the client holds.
    pub fn record_following_delivery(
        &mut self,
        subscription_id: u64,
        from_revision: u64,
        to_revision: u64,
        delivered: String,
    ) -> bool {
        match self.subscriptions.get_mut(&subscription_id) {
            Some(subscription) if subscription.revision == from_revision => {
                subscription.revision = to_revision;
                subscription.delivered = Some(delivered);
                true
            }
            _ => false,
        }
    }

    pub fn remove(&mut self, subscription_id: u64) -> bool {
        self.subscriptions.remove(&subscription_id).is_some()
    }

    /// Retains a sent patch, evicting the oldest beyond the window.
    pub fn retain_patch(
        &mut self,
        projection: String,
        params: Value,
        from_revision: u64,
        to_revision: u64,
        patch: P,
    ) {
        if self.retained_patch_window == 0 {
            self.retained_patches.clear();
            return;
        }
        self.retained_patches.push_back(RetainedProjectionPatch {
            projection,
            params,
            from_revision,
            to_revision,
            patch,
        });
        while self.retained_patches.len() > self.retained_patch_window {
            self.retained_patches.pop_front();
        }
    }

    /// The highest revision any subscription or retained patch has reached.
    pub fn current_revision(&self) -> u64 {
        self.subscriptions
            .values()
            .map(|subscription| subscription.revision)
            .chain(self.retained_patches.iter().map(|patch| patch.to_revision))
            .max()
            .unwrap_or(0)
    }

    pub fn record_resync(&mut self, reason: &str) {
        self.counters.resync_count = self.counters.resync_count.saturating_add(1);
        self.counters.last_resync_reason = Some(reason.to_string());
    }

    /// Records updates lost to a lagging broadcast, which forces a resync.
    pub fn record_dropped_updates(&mut self, count: u64) {
        self.counters.dropped_update_count =
            self.counters.dropped_update_count.saturating_add(count);
        self.record_resync("broadcastLag");
    }

    pub fn record_mutation(&mut self, latency: Duration, rejection: Option<&str>) {
        let micros = u64::try_from(latency.as_micros()).unwrap_or(u64::MAX);
        let counters = &mut self.counters;
        counters.mutation_count = counters.mutation_count.saturating_add(1);
        counters.mutation_latency_micros_total = counters
            .mutation_latency_micros_total
            .saturating_add(micros);
        counters.mutation_latency_micros_max = counters.mutation_latency_micros_max.max(micros);
        if let Some(category) = rejection {
            counters.mutation_rejection_count = counters.mutation_rejection_count.saturating_add(1);
            let count = counters.rejections.entry(category.to_string()).or_default();
            *count = count.saturating_add(1);
        }
    }

    pub fn diagnostics(&self) -> ProjectionRuntimeDiagnostics {
        let mut subscriptions = self
            .subscriptions
            .values()
            .map(|subscription| ProjectionSubscriptionDiagnostics {
                projection: subscription.projection.clone(),
                params_sha256: format!(
                    "sha256:{}",
                    sha256_hex(
                        &serde_json::to_vec(&subscription.params)
                            .unwrap_or_else(|_| b"unserialisable".to_vec())
                    )
                ),
                client_revision: subscription.revision,
            })
            .collect::<Vec<_>>();
        subscriptions.sort_by(|left, right| {
            (&left.projection, &left.params_sha256).cmp(&(&right.projection, &right.params_sha256))
        });
        let current_revision = self.current_revision();
        let counters = &self.counters;
        ProjectionRuntimeDiagnostics {
            subscriptions,
            current_revision,
            retained_window_floor: self
                .retained_patches
                .front()
                .map_or(current_revision, |patch| patch.from_revision),
            retained_window_ceiling: self
                .retained_patches
                .back()
                .map_or(current_revision, |patch| patch.to_revision),
            retained_patch_count: self.retained_patches.len() as u64,
            dropped_update_count: counters.dropped_update_count,
            gapped_update_count: counters.gapped_update_count,
            resync_count: counters.resync_count,
            last_resync_reason: counters.last_resync_reason.clone(),
            resume_up_to_date_count: counters.resume_up_to_date_count,
            resume_patch_count: counters.resume_patch_count,
            resume_snapshot_count: counters.resume_snapshot_count,
            mutation_count: counters.mutation_count,
            mutation_rejection_count: counters.mutation_rejection_count,
            mutation_latency_micros_total: counters.mutation_latency_micros_total,
            mutation_latency_micros_max: counters.mutation_latency_micros_max,
            rejections: counters.rejections.clone(),
        }
    }
}

impl<P: Clone + PartialEq> ProjectionSubscriptions<P> {
    /// Accepts a subscription at the projection's current `revision` and
    /// decides how its client catches up from `cursor`: nothing to send, the
    /// retained patches it missed, or a snapshot when those are no longer
    /// retained. A client without a cursor takes a snapshot.
    pub fn subscribe(
        &mut self,
        projection: String,
        params: Value,
        revision: u64,
        cursor: Option<u64>,
    ) -> Result<SubscribeOutcome<P>, CursorAhead> {
        let Some(cursor) = cursor else {
            let subscription_id = self.insert(projection, params, revision);
            return Ok(SubscribeOutcome {
                subscription_id,
                resume: None,
                replay: None,
            });
        };
        if cursor > revision {
            return Err(CursorAhead {
                cursor_revision: cursor,
                current_revision: revision,
            });
        }
        let replay = self.retained_patches(&projection, &params, cursor, revision);
        let resume = match &replay {
            Some(patches) if patches.is_empty() => {
                self.counters.resume_up_to_date_count =
                    self.counters.resume_up_to_date_count.saturating_add(1);
                ProjectionSubscribeResume::UpToDate { revision }
            }
            Some(patches) => {
                self.counters.resume_patch_count =
                    self.counters.resume_patch_count.saturating_add(1);
                ProjectionSubscribeResume::Patches {
                    from_revision: cursor,
                    revision,
                    patch_count: patches.len(),
                }
            }
            None => {
                self.counters.resume_snapshot_count =
                    self.counters.resume_snapshot_count.saturating_add(1);
                self.counters.gapped_update_count =
                    self.counters.gapped_update_count.saturating_add(1);
                self.record_resync("retainedWindowExpired");
                ProjectionSubscribeResume::Snapshot {
                    from_revision: cursor,
                    revision,
                }
            }
        };
        let subscription_id = self.insert(projection, params, revision);
        Ok(SubscribeOutcome {
            subscription_id,
            resume: Some(resume),
            replay,
        })
    }

    /// Whether the most recent patch retained for this projection and these
    /// parameters is `patch`.
    pub fn last_retained_patch_matches(&self, projection: &str, params: &Value, patch: &P) -> bool {
        self.retained_patches
            .iter()
            .rev()
            .find(|retained| retained.projection == projection && &retained.params == params)
            .is_some_and(|retained| &retained.patch == patch)
    }

    /// The retained patches that carry this projection and these parameters
    /// from `from_revision` to exactly `to_revision`, or `None` when the
    /// retained window no longer covers that span.
    pub fn retained_patches(
        &self,
        projection: &str,
        params: &Value,
        from_revision: u64,
        to_revision: u64,
    ) -> Option<Vec<RetainedProjectionPatch<P>>> {
        if from_revision == to_revision {
            return Some(Vec::new());
        }
        let mut cursor = from_revision;
        let mut patches = Vec::new();
        for patch in &self.retained_patches {
            if patch.projection != projection
                || &patch.params != params
                || patch.from_revision != cursor
                || patch.to_revision > to_revision
            {
                continue;
            }
            cursor = patch.to_revision;
            patches.push(patch.clone());
            if cursor == to_revision {
                return Some(patches);
            }
        }
        None
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
