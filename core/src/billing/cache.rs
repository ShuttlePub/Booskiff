//! Process-local billing layers; zero TTL bypasses storage entirely.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::model::Owner;

type Rules = Vec<(String, serde_json::Value)>;

pub struct BillingCache {
    ttl: Duration,
    global: Mutex<Option<Cached<Rules>>>,
    owners: Mutex<HashMap<(String, String), Cached<OwnerEntry>>>,
    generation: Mutex<std::sync::Arc<()>>,
}

struct Cached<T> {
    value: T,
    fetched_at: Instant,
}

#[derive(Clone)]
pub struct OwnerEntry {
    pub assignment: Option<crate::model::Plan>,
    pub rules: Rules,
}

/// At 10k owners, inserting an untracked key clears the map. Reloading
/// billing layers is cheap compared with unbounded process-local growth.
const MAX_CACHED_OWNERS: usize = 10_000;

impl BillingCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            ttl: Duration::from_secs(ttl_secs),
            global: Mutex::new(None),
            owners: Mutex::new(HashMap::new()),
            generation: Mutex::new(std::sync::Arc::new(())),
        }
    }

    pub fn get_global(&self) -> Option<Rules> {
        let global = self.global.lock().unwrap_or_else(|err| err.into_inner());
        global
            .as_ref()
            .filter(|entry| entry.fetched_at.elapsed() < self.ttl)
            .map(|entry| entry.value.clone())
    }

    pub fn get_owner(&self, owner: &Owner) -> Option<OwnerEntry> {
        let owners = self.owners.lock().unwrap_or_else(|err| err.into_inner());
        owners
            .get(&(owner.owner_type.clone(), owner.owner_id.clone()))
            .filter(|entry| entry.fetched_at.elapsed() < self.ttl)
            .map(|entry| entry.value.clone())
    }

    pub fn store_global(&self, value: Rules) {
        self.store_global_if_current(value, &self.generation());
    }

    pub(crate) fn generation(&self) -> std::sync::Arc<()> {
        self.generation
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    pub(crate) fn store_global_if_current(&self, value: Rules, generation: &std::sync::Arc<()>) {
        let current = self
            .generation
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if !std::sync::Arc::ptr_eq(&current, generation) {
            return;
        }
        if self.ttl.is_zero() {
            return;
        }
        *self.global.lock().unwrap_or_else(|err| err.into_inner()) = Some(Cached {
            value,
            fetched_at: Instant::now(),
        });
    }

    pub fn store_owner(&self, owner: &Owner, value: OwnerEntry) {
        self.store_owner_if_current(owner, value, &self.generation());
    }

    pub(crate) fn store_owner_if_current(
        &self,
        owner: &Owner,
        value: OwnerEntry,
        generation: &std::sync::Arc<()>,
    ) {
        let current = self
            .generation
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if !std::sync::Arc::ptr_eq(&current, generation) {
            return;
        }
        if self.ttl.is_zero() {
            return;
        }
        let mut owners = self.owners.lock().unwrap_or_else(|err| err.into_inner());
        let key = (owner.owner_type.clone(), owner.owner_id.clone());
        if owners.len() >= MAX_CACHED_OWNERS && !owners.contains_key(&key) {
            owners.clear();
        }
        owners.insert(
            key,
            Cached {
                value,
                fetched_at: Instant::now(),
            },
        );
    }

    pub fn invalidate_owner(&self, owner: &Owner) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        *generation = std::sync::Arc::new(());
        self.owners
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&(owner.owner_type.clone(), owner.owner_id.clone()));
    }

    pub fn invalidate_all(&self) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        *generation = std::sync::Arc::new(());
        self.global
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take();
        self.owners
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> OwnerEntry {
        OwnerEntry {
            assignment: Some(crate::model::Plan::Premium),
            rules: vec![],
        }
    }

    fn owner(owner_type: &str, owner_id: &str) -> Owner {
        Owner::new(owner_type, owner_id)
    }

    #[test]
    fn fresh_entries_expire() {
        let cache = BillingCache::new(60);
        let owner = owner("account", "a");
        cache.store_global(vec![]);
        cache.store_owner(&owner, entry());
        assert!(cache.get_global().is_some());
        assert!(cache.get_owner(&owner).is_some());
        cache.global.lock().unwrap().as_mut().unwrap().fetched_at -= Duration::from_secs(61);
        cache
            .owners
            .lock()
            .unwrap()
            .get_mut(&(owner.owner_type.clone(), owner.owner_id.clone()))
            .unwrap()
            .fetched_at -= Duration::from_secs(61);
        assert!(cache.get_global().is_none());
        assert!(cache.get_owner(&owner).is_none());
    }

    #[test]
    fn invalidate_owner_preserves_other_layers() {
        let cache = BillingCache::new(60);
        let first = owner("account", "a");
        let second = owner("account", "b");
        cache.store_global(vec![]);
        cache.store_owner(&first, entry());
        cache.store_owner(&second, entry());
        cache.invalidate_owner(&first);
        assert!(cache.get_owner(&first).is_none());
        assert!(cache.get_owner(&second).is_some());
        assert!(cache.get_global().is_some());
    }

    #[test]
    fn owner_keys_are_structured_and_collision_free() {
        let cache = BillingCache::new(60);
        let first = owner("a:b", "c");
        let second = owner("a", "b:c");
        let first_entry = OwnerEntry {
            assignment: Some(crate::model::Plan::Default),
            rules: vec![],
        };
        let second_entry = entry();

        cache.store_owner(&first, first_entry.clone());
        cache.store_owner(&second, second_entry.clone());

        assert_eq!(
            cache.get_owner(&first).unwrap().assignment,
            first_entry.assignment
        );
        assert_eq!(
            cache.get_owner(&second).unwrap().assignment,
            second_entry.assignment
        );

        cache.invalidate_owner(&first);
        assert!(cache.get_owner(&first).is_none());
        assert_eq!(
            cache.get_owner(&second).unwrap().assignment,
            second_entry.assignment
        );
    }

    #[test]
    fn invalidate_all_clears_both_layers() {
        let cache = BillingCache::new(60);
        let owner = owner("account", "a");
        cache.store_global(vec![]);
        cache.store_owner(&owner, entry());
        cache.invalidate_all();
        assert!(cache.get_global().is_none());
        assert!(cache.get_owner(&owner).is_none());
    }

    #[test]
    fn zero_ttl_disables_caching() {
        let cache = BillingCache::new(0);
        let owner = owner("account", "a");
        cache.store_global(vec![]);
        cache.store_owner(&owner, entry());
        assert!(cache.get_global().is_none());
        assert!(cache.get_owner(&owner).is_none());
        assert!(cache.owners.lock().unwrap().is_empty());
    }

    #[test]
    fn invalidation_rejects_in_flight_fills() {
        let cache = BillingCache::new(60);
        let owner = owner("account", "a");
        let generation = cache.generation();
        cache.invalidate_owner(&owner);
        cache.store_owner_if_current(&owner, entry(), &generation);
        assert!(cache.get_owner(&owner).is_none());
        let generation = cache.generation();
        cache.invalidate_all();
        cache.store_global_if_current(vec![], &generation);
        cache.store_owner_if_current(&owner, entry(), &generation);
        assert!(cache.get_global().is_none());
        assert!(cache.get_owner(&owner).is_none());
    }

    #[test]
    fn overflow_clears_owners_but_not_global() {
        let cache = BillingCache::new(60);
        let owner_for_zero = |value: String| Owner::new("owner", value);
        cache.store_global(vec![]);
        for i in 0..MAX_CACHED_OWNERS {
            let owner = owner_for_zero(i.to_string());
            cache.store_owner(&owner, entry());
        }
        let zero = owner_for_zero("0".to_owned());
        cache.store_owner(&zero, entry());
        assert_eq!(cache.owners.lock().unwrap().len(), MAX_CACHED_OWNERS);
        let new_owner = owner_for_zero("new".to_owned());
        cache.store_owner(&new_owner, entry());
        assert_eq!(cache.owners.lock().unwrap().len(), 1);
        assert!(cache.get_owner(&zero).is_none());
        assert!(cache.get_owner(&new_owner).is_some());
        assert!(cache.get_global().is_some());
    }
}
