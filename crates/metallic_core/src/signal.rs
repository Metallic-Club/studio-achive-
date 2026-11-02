//! A minimal single-threaded event: connect handlers, fire with arguments.
//!
//! Connection identities are shared across all signal instances and argument types, and are
//! never reused after disconnect/drop. Atomic identity allocation permits independent signals
//! to connect on different threads; each individual signal still requires exclusive access.
//!
//! [`SignalQueue`] is the other half of this file: what a class's declared signals (scripts'
//! `part.Touched` and friends, `docs/design/class-system.md` section 6) wait in between
//! [`World::fire`](crate::World::fire) and the next delivery by the script host.

use crate::classes::{ScriptValue, SignalKey};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(0);

fn allocate_connection(counter: &AtomicU64) -> ConnectionId {
    // Only uniqueness is shared; handler storage is local and needs no memory publication.
    // MAX is a terminal exhausted sentinel, never an issued ID. Failed attempts leave it set.
    ConnectionId(
        counter
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .expect("signal connection ID space exhausted"),
    )
}

/// Opaque identity for one handler across all signals, including different argument types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(u64);

impl ConnectionId {
    /// A new identity, never issued before. For a host that keeps connections of its own, so
    /// [`HostApi::disconnect`](crate::classes::HostApi::disconnect) needs nothing else.
    ///
    /// # Panics
    /// Panics if the shared u64 identity space is exhausted.
    pub fn allocate() -> ConnectionId {
        allocate_connection(&NEXT_CONNECTION)
    }
}

type Handler<Args> = Box<dyn FnMut(&Args) + Send>;

/// An event that calls its connected handlers in connection order.
pub struct Signal<Args> {
    handlers: Vec<(ConnectionId, Handler<Args>)>,
}

impl<Args> Default for Signal<Args> {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }
}

impl<Args> Signal<Args> {
    /// Adds a handler and returns the id to disconnect it with.
    ///
    /// # Panics
    /// Panics before registering the handler if the shared u64 identity space is exhausted.
    pub fn connect(&mut self, handler: impl FnMut(&Args) + Send + 'static) -> ConnectionId {
        let id = allocate_connection(&NEXT_CONNECTION);
        self.handlers.push((id, Box::new(handler)));
        id
    }
    /// Removes a handler; unknown ids, including IDs from other signals, are ignored.
    pub fn disconnect(&mut self, id: ConnectionId) {
        self.handlers.retain(|(c, _)| *c != id);
    }
    /// Calls every connected handler with `args`.
    pub fn fire(&mut self, args: &Args) {
        for (_, h) in &mut self.handlers {
            h(args);
        }
    }
    /// Number of connected handlers.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }
    /// True when no handler is connected.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

/// Most fires kept between two deliveries. Past it new fires are dropped and counted, so a world
/// nobody steps (a paused debugger, a host that stopped) cannot grow without bound.
pub const MAX_QUEUED_SIGNALS: usize = 65_536;

/// One fire of a declared signal, waiting for delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedSignal {
    /// Which signal of which instance.
    pub key: SignalKey,
    /// The payload, as the signal declares it.
    pub args: Vec<ScriptValue>,
}

/// Fires of declared signals waiting for the script host, and which signals anyone listens to.
///
/// A fire nobody listens to costs one lookup and queues nothing. A host publishes each key it has a
/// subscriber for with [`SignalQueue::watch`]; several hosts or VMs may watch one key.
#[derive(Default)]
pub struct SignalQueue {
    /// Subscribed keys, with how many hosts or VMs subscribe.
    watched: HashMap<SignalKey, u32>,
    queued: Vec<QueuedSignal>,
    dropped: u64,
}

impl SignalQueue {
    /// Someone started listening to `key`.
    pub fn watch(&mut self, key: SignalKey) {
        *self.watched.entry(key).or_insert(0) += 1;
    }

    /// One [`SignalQueue::watch`] of `key` is over. Unknown keys are ignored.
    pub fn unwatch(&mut self, key: SignalKey) {
        if let Some(count) = self.watched.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                self.watched.remove(&key);
            }
        }
    }

    /// True when someone listens to `key`.
    #[inline]
    pub fn is_watched(&self, key: &SignalKey) -> bool {
        self.watched.contains_key(key)
    }

    /// Queues a fire of `key`, unless nobody listens.
    pub fn fire(&mut self, key: SignalKey, args: Vec<ScriptValue>) {
        if !self.is_watched(&key) {
            return;
        }
        if self.queued.len() >= MAX_QUEUED_SIGNALS {
            self.dropped += 1;
            return;
        }
        self.queued.push(QueuedSignal { key, args });
    }

    /// Takes every waiting fire, oldest first. Allocates nothing when none waits.
    pub fn take(&mut self) -> Vec<QueuedSignal> {
        std::mem::take(&mut self.queued)
    }

    /// How many fires were dropped because the queue was full since the last call.
    pub fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustion_never_wraps_or_reissues_an_identity() {
        // An isolated counter exercises the real allocator boundary without exhausting or
        // resetting global state underneath independently running signal tests.
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_connection(&counter), ConnectionId(u64::MAX - 1));
        for _ in 0..2 {
            assert!(std::panic::catch_unwind(|| allocate_connection(&counter)).is_err());
            assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
        }
    }
}
