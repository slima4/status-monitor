use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

use crate::domain::{OrgId, Target};
use crate::error::Result;
use crate::storage::admin::EnabledTargetSource;

/// A scheduled target plus its owning tenant. The `org_id` rides with the
/// target through the scheduler→worker→alert path so channel resolution is
/// tenant-scoped.
#[derive(Debug, Clone)]
pub struct ScheduledTarget {
    pub org_id: OrgId,
    pub target: Arc<Target>,
}

pub struct TargetRegistry {
    source: Arc<dyn EnabledTargetSource>,
    targets: DashMap<Uuid, ScheduledTarget>,
}

#[derive(Debug, Default, Clone)]
pub struct RegistryDiff {
    pub added: Vec<ScheduledTarget>,
    pub updated: Vec<ScheduledTarget>,
    pub removed: Vec<Uuid>,
}

impl RegistryDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.updated.is_empty() && self.removed.is_empty()
    }
}

impl TargetRegistry {
    pub fn new(source: Arc<dyn EnabledTargetSource>) -> Self {
        Self {
            source,
            targets: DashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.targets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    pub async fn refresh(&self) -> Result<RegistryDiff> {
        let fresh = self.source.list_all_enabled_targets().await?;
        let mut diff = RegistryDiff::default();
        let mut seen = std::collections::HashSet::with_capacity(fresh.len());

        for (org_id, target) in fresh {
            seen.insert(target.id);
            let id = target.id;
            let st = ScheduledTarget {
                org_id,
                target: Arc::new(target),
            };
            match self.targets.get(&id) {
                Some(existing) => {
                    if existing.target.updated_at != st.target.updated_at {
                        drop(existing);
                        self.targets.insert(id, st.clone());
                        diff.updated.push(st);
                    }
                }
                None => {
                    self.targets.insert(id, st.clone());
                    diff.added.push(st);
                }
            }
        }

        let removed: Vec<Uuid> = self
            .targets
            .iter()
            .filter_map(|e| (!seen.contains(e.key())).then_some(*e.key()))
            .collect();
        for id in &removed {
            self.targets.remove(id);
        }
        diff.removed = removed;

        Ok(diff)
    }
}
