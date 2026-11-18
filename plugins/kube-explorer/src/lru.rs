//! A bounded map that drops its oldest entry by insertion (FIFO; a read does not refresh it), never the whole cache.
use std::borrow::Borrow;
use std::collections::{BTreeMap, VecDeque};

pub(crate) struct Lru<K, V> {
    entries: BTreeMap<K, V>,
    order: VecDeque<K>,
}

impl<K, V> Default for Lru<K, V> {
    fn default() -> Self {
        Self { entries: BTreeMap::new(), order: VecDeque::new() }
    }
}

impl<K: Ord + Clone, V> Lru<K, V> {
    pub(crate) fn insert(&mut self, key: K, value: V, capacity: usize) {
        self.remove(&key);
        while self.entries.len() >= capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
    }

    pub(crate) fn remove<Q: Ord + ?Sized>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
    {
        if self.entries.remove(key).is_some() {
            self.order.retain(|queued| queued.borrow() != key);
        }
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        let dropped: Vec<K> = self.entries.iter().filter(|(key, value)| !keep(key, value)).map(|(key, _)| key.clone()).collect();
        for key in dropped {
            self.remove(&key);
        }
    }

    pub(crate) fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.entries.get(key)
    }

    pub(crate) fn pop_oldest(&mut self) -> Option<(K, V)> {
        let key = self.order.pop_front()?;
        let value = self.entries.remove(&key)?;
        Some((key, value))
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn iter(&self) -> std::collections::btree_map::Iter<'_, K, V> {
        self.entries.iter()
    }

    pub(crate) fn keys(&self) -> std::collections::btree_map::Keys<'_, K, V> {
        self.entries.keys()
    }

    #[cfg(test)]
    pub(crate) fn values(&self) -> std::collections::btree_map::Values<'_, K, V> {
        self.entries.values()
    }
}

impl<'a, K: Ord + Clone, V> IntoIterator for &'a Lru<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::btree_map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_cache_drops_only_the_oldest_entry() {
        let mut cache = Lru::default();
        for key in 0..4 {
            cache.insert(key, key, 3);
        }
        assert_eq!(cache.keys().copied().collect::<Vec<_>>(), [1, 2, 3]);
        cache.insert(1, 10, 3);
        cache.insert(9, 9, 3);
        assert_eq!(cache.keys().copied().collect::<Vec<_>>(), [1, 3, 9]);
        assert_eq!(cache.get(&1), Some(&10));
    }
}
