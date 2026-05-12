use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

use crate::domain::Target;
use crate::error::Result;
use crate::storage::TargetStore;

pub struct TargetRegistry {
    store: Arc<dyn TargetStore>,
    targets: DashMap<Uuid, Arc<Target>>,
}

#[derive(Debug, Default, Clone)]
pub struct RegistryDiff {
    pub added: Vec<Arc<Target>>,
    pub updated: Vec<Arc<Target>>,
    pub removed: Vec<Uuid>,
}

impl RegistryDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.updated.is_empty() && self.removed.is_empty()
    }
}

impl TargetRegistry {
    pub fn new(store: Arc<dyn TargetStore>) -> Self {
        Self {
            store,
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
        let fresh = self.store.list_enabled().await?;
        let mut diff = RegistryDiff::default();
        let mut seen = std::collections::HashSet::with_capacity(fresh.len());

        for target in fresh {
            seen.insert(target.id);
            let id = target.id;
            let arc = Arc::new(target);
            match self.targets.get(&id) {
                Some(existing) => {
                    if existing.updated_at != arc.updated_at {
                        drop(existing);
                        self.targets.insert(id, arc.clone());
                        diff.updated.push(arc);
                    }
                }
                None => {
                    self.targets.insert(id, arc.clone());
                    diff.added.push(arc);
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
