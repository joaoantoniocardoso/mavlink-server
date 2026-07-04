use std::sync::{Arc, RwLock};

use indexmap::IndexMap;
use serde::Serialize;

use super::{AccumulatedStatsInner, AtomicAccumulatedStats};
use crate::{
    protocol::Protocol,
    stats::messages::{ComponentId, MessageId, SystemId},
};

#[derive(Default, Clone, Debug, Serialize)]
pub struct AccumulatedHubMessagesStats {
    pub systems_messages_stats: IndexMap<SystemId, AccumulatedSystemMessagesStats>,
}

#[derive(Default, Clone, Debug, Serialize)]
pub struct AccumulatedSystemMessagesStats {
    pub components_messages_stats: IndexMap<ComponentId, AccumulatedComponentMessageStats>,
}

#[derive(Default, Clone, Debug, Serialize)]
pub struct AccumulatedComponentMessageStats {
    pub messages_stats: IndexMap<MessageId, AccumulatedStatsInner>,
}

impl AccumulatedHubMessagesStats {
    pub fn update(&mut self, message: &Arc<Protocol>) {
        let (Some(system_id), Some(component_id), Some(message_id)) = (
            message.system_id(),
            message.component_id(),
            message.message_id(),
        ) else {
            return;
        };

        self.systems_messages_stats
            .entry(system_id)
            .or_default()
            .components_messages_stats
            .entry(component_id)
            .or_default()
            .messages_stats
            .entry(message_id)
            .and_modify(|accumulated_stats| accumulated_stats.update(message))
            .or_insert_with(|| AccumulatedStatsInner::new(message));
    }
}

/// Lock-free-on-the-hot-path counterpart of [`AccumulatedHubMessagesStats`].
///
/// Per-message updates take a shared read lock and increment the matching
/// entry's atomic counters concurrently; the write lock is only taken the first
/// time a `(system, component, message)` triple is seen. Readers take a
/// [`AtomicHubMessagesStats::snapshot`] once per stats period.
#[derive(Default)]
pub struct AtomicHubMessagesStats {
    entries: RwLock<IndexMap<(SystemId, ComponentId, MessageId), Arc<AtomicAccumulatedStats>>>,
}

impl AtomicHubMessagesStats {
    pub fn update(&self, message: &Arc<Protocol>) {
        let (Some(system_id), Some(component_id), Some(message_id)) = (
            message.system_id(),
            message.component_id(),
            message.message_id(),
        ) else {
            return;
        };

        let key = (system_id, component_id, message_id);

        if let Some(entry) = self.entries.read().unwrap().get(&key) {
            entry.update(message);
            return;
        }

        let entry = self
            .entries
            .write()
            .unwrap()
            .entry(key)
            .or_default()
            .clone();
        entry.update(message);
    }

    pub fn snapshot(&self) -> AccumulatedHubMessagesStats {
        let entries = self.entries.read().unwrap();

        let mut stats = AccumulatedHubMessagesStats::default();
        for ((system_id, component_id, message_id), entry) in entries.iter() {
            let Some(inner) = entry.snapshot() else {
                continue;
            };

            stats
                .systems_messages_stats
                .entry(*system_id)
                .or_default()
                .components_messages_stats
                .entry(*component_id)
                .or_default()
                .messages_stats
                .insert(*message_id, inner);
        }

        stats
    }

    pub fn reset(&self) {
        self.entries.write().unwrap().clear();
    }
}
