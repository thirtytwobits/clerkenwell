use std::collections::{BTreeMap, HashMap};
use std::marker::PhantomData;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::transport::{
    ProjectionResyncAccepted, ProjectionSubscribeAccepted, ProjectionTransportEvent,
};

/// One projection subscription and the revision its client holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSubscription<D> {
    pub projection: String,
    pub params: Value,
    pub revision: u64,
    /// What the last update left the client holding, for a projection whose
    /// host names it. `None` until such an update, or after one the host
    /// could not name.
    pub delivered: Option<D>,
}

/// Why a delivery that follows the one its client held is not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowingDelivery {
    /// Another delivery reached the client since this one was built, so it
    /// would not follow what the client holds.
    Superseded,
    /// It would leave the client where the last delivery did.
    AlreadyHeld,
}

/// Counters describing one connection's subscriptions. Parameters are
/// reported only as a hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRuntimeDiagnostics {
    pub subscriptions: Vec<ProjectionSubscriptionDiagnostics>,
    pub current_revision: u64,
    pub dropped_update_count: u64,
    pub resync_count: u64,
    pub last_resync_reason: Option<String>,
    /// Subscriptions whose client already held what they would have sent.
    pub resume_up_to_date_count: u64,
    /// Subscriptions whose client held something, and was sent what it
    /// lacked.
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
    resync_count: u64,
    last_resync_reason: Option<String>,
    resume_up_to_date_count: u64,
    resume_snapshot_count: u64,
    mutation_count: u64,
    mutation_rejection_count: u64,
    mutation_latency_micros_total: u64,
    mutation_latency_micros_max: u64,
    rejections: BTreeMap<String, u64>,
}

/// One connection's projection subscriptions. `P` is the application's patch
/// payload, and `D` what an update leaves a client holding.
///
/// A subscription's revisions order the updates it sends its client. What a
/// client holds is named by `D`, which the client sends back when it
/// subscribes again.
#[derive(Debug)]
pub struct ProjectionSubscriptions<P, D> {
    next_subscription_id: u64,
    subscriptions: HashMap<u64, ProjectionSubscription<D>>,
    counters: Counters,
    patches: PhantomData<P>,
}

impl<P, D> Default for ProjectionSubscriptions<P, D> {
    fn default() -> Self {
        Self {
            next_subscription_id: 1,
            subscriptions: HashMap::new(),
            counters: Counters::default(),
            patches: PhantomData,
        }
    }
}

impl<P, D> ProjectionSubscriptions<P, D> {
    /// Registers a subscription at `revision`.
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

    pub fn get(&self, subscription_id: u64) -> Option<&ProjectionSubscription<D>> {
        self.subscriptions.get(&subscription_id)
    }

    pub fn all(&self) -> &HashMap<u64, ProjectionSubscription<D>> {
        &self.subscriptions
    }

    /// Records an update that took the client to `revision` and left it
    /// holding `delivered`.
    pub fn record_delivery(&mut self, subscription_id: u64, revision: u64, delivered: Option<D>) {
        if let Some(subscription) = self.subscriptions.get_mut(&subscription_id) {
            subscription.revision = revision;
            subscription.delivered = delivered;
        }
    }

    pub fn remove(&mut self, subscription_id: u64) -> bool {
        self.subscriptions.remove(&subscription_id).is_some()
    }

    /// The highest revision any subscription has reached.
    pub fn current_revision(&self) -> u64 {
        self.subscriptions
            .values()
            .map(|subscription| subscription.revision)
            .max()
            .unwrap_or(0)
    }

    /// The revision a mutation's effects reach: past every revision this
    /// connection has sent when it publishes a change, and where they stand
    /// when it changes nothing.
    pub fn mutation_revision(&self, publishes: bool) -> u64 {
        let current = self.current_revision().max(1);
        if publishes {
            current + 1
        } else {
            current
        }
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
        let counters = &self.counters;
        ProjectionRuntimeDiagnostics {
            subscriptions,
            current_revision: self.current_revision(),
            dropped_update_count: counters.dropped_update_count,
            resync_count: counters.resync_count,
            last_resync_reason: counters.last_resync_reason.clone(),
            resume_up_to_date_count: counters.resume_up_to_date_count,
            resume_snapshot_count: counters.resume_snapshot_count,
            mutation_count: counters.mutation_count,
            mutation_rejection_count: counters.mutation_rejection_count,
            mutation_latency_micros_total: counters.mutation_latency_micros_total,
            mutation_latency_micros_max: counters.mutation_latency_micros_max,
            rejections: counters.rejections.clone(),
        }
    }
}

impl<P, D: Serialize> ProjectionSubscriptions<P, D> {
    /// Sends `patch` to a subscription, taking its client from the revision it
    /// holds now to `to_revision`, or to the next revision when it already
    /// holds `to_revision` or later, and leaving it holding `delivered`. `None`
    /// when the subscription is gone.
    pub fn deliver_patch<S>(
        &mut self,
        subscription_id: u64,
        to_revision: u64,
        patch: P,
        delivered: Option<D>,
    ) -> Option<ProjectionTransportEvent<S, P>> {
        let subscription = self.subscriptions.get_mut(&subscription_id)?;
        let from_revision = subscription.revision;
        let to_revision = to_revision.max(from_revision + 1);
        let held = held_token(delivered.as_ref());
        subscription.revision = to_revision;
        subscription.delivered = delivered;
        Some(ProjectionTransportEvent::Patch {
            subscription_id,
            from_revision,
            to_revision,
            patch,
            held,
        })
    }

    /// Sends `snapshot` to a subscription at the revision after the one its
    /// client holds, recording `delivered` as what it leaves the client
    /// holding and `reason` as why. `None` when the subscription is gone.
    pub fn resync<S>(
        &mut self,
        subscription_id: u64,
        snapshot: S,
        delivered: Option<D>,
        reason: &str,
    ) -> Option<(ProjectionResyncAccepted, ProjectionTransportEvent<S, P>)> {
        let from_revision = self.subscriptions.get(&subscription_id)?.revision;
        let revision = from_revision + 1;
        let held = held_token(delivered.as_ref());
        self.record_delivery(subscription_id, revision, delivered);
        self.record_resync(reason);
        Some((
            ProjectionResyncAccepted {
                subscription_id,
                from_revision,
                revision,
            },
            ProjectionTransportEvent::Snapshot {
                subscription_id,
                revision,
                snapshot,
                held,
            },
        ))
    }
}

impl<P, D: PartialEq + Serialize> ProjectionSubscriptions<P, D> {
    /// Accepts a subscription and sends its client `snapshot`, which leaves it
    /// holding `delivered`, unless the client already holds that: `held` is
    /// what an earlier subscription's last update left it holding.
    pub fn accept_subscription<S>(
        &mut self,
        projection: String,
        params: Value,
        held: Option<D>,
        snapshot: S,
        delivered: Option<D>,
    ) -> (
        ProjectionSubscribeAccepted,
        Vec<ProjectionTransportEvent<S, P>>,
    ) {
        let revision = self.current_revision().max(1);
        let subscription_id = self.insert(projection, params, revision);
        let up_to_date = held.is_some() && held == delivered;
        if held.is_some() {
            let count = if up_to_date {
                &mut self.counters.resume_up_to_date_count
            } else {
                &mut self.counters.resume_snapshot_count
            };
            *count = count.saturating_add(1);
        }
        let events = if up_to_date {
            Vec::new()
        } else {
            vec![ProjectionTransportEvent::Snapshot {
                subscription_id,
                revision,
                snapshot,
                held: held_token(delivered.as_ref()),
            }]
        };
        self.record_delivery(subscription_id, revision, delivered);
        (
            ProjectionSubscribeAccepted {
                subscription_id,
                revision,
                up_to_date,
            },
            events,
        )
    }

    /// Records a delivery that follows the one the client held at
    /// `from_revision` and leaves it holding `delivered`, and returns the
    /// update that carries it as `patch`.
    pub fn deliver_following<S>(
        &mut self,
        subscription_id: u64,
        from_revision: u64,
        patch: P,
        delivered: D,
    ) -> Result<ProjectionTransportEvent<S, P>, FollowingDelivery> {
        match self.subscriptions.get_mut(&subscription_id) {
            Some(subscription) if subscription.revision == from_revision => {
                if subscription.delivered.as_ref() == Some(&delivered) {
                    return Err(FollowingDelivery::AlreadyHeld);
                }
                let to_revision = from_revision + 1;
                let held = held_token(Some(&delivered));
                subscription.revision = to_revision;
                subscription.delivered = Some(delivered);
                Ok(ProjectionTransportEvent::Patch {
                    subscription_id,
                    from_revision,
                    to_revision,
                    patch,
                    held,
                })
            }
            _ => Err(FollowingDelivery::Superseded),
        }
    }
}

/// What an update tells its client it holds, for it to send back when it
/// subscribes again.
fn held_token<D: Serialize>(delivered: Option<&D>) -> Option<Value> {
    delivered.and_then(|delivered| serde_json::to_value(delivered).ok())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
