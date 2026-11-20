//! Bounded storage for complete extended grapheme clusters.
//! The ordinary scalar path keeps Copy cells and does not allocate.
use crate::screen::Cell;
use alloc::{collections::BTreeMap, sync::Arc, vec, vec::Vec};

/// An individual retained cluster never grows without bound.
pub const MAX_GRAPHEME_BYTES: usize = 256;
const MAX_ENTRIES: usize = 16_384;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const ARC_HEADER: usize = 2 * core::mem::size_of::<usize>();

// A scalar occupies at most 21 bits. The full ordered pair remains in each
// entry: collisions can evict a result, never substitute another pair's rule.
const SCALAR_PAIR_KEY: u64 = (1 << 42) - 1;
const SCALAR_PAIR_BOUNDARY: u64 = 1 << 62;
const SCALAR_PAIR_VALID: u64 = 1 << 63;

struct ScalarBoundaryCache([u64; 64]);

impl Default for ScalarBoundaryCache {
    fn default() -> Self {
        Self([0; 64])
    }
}

impl ScalarBoundaryCache {
    fn key(before: char, after: char) -> u64 {
        ((before as u64) << 21) | after as u64
    }

    fn slot(key: u64) -> usize {
        (key.wrapping_mul(0x9e3779b97f4a7c15) >> 58) as usize
    }

    fn get(&self, before: char, after: char) -> Option<bool> {
        let key = Self::key(before, after);
        let entry = self.0[Self::slot(key)];
        if entry & SCALAR_PAIR_VALID != 0 && entry & SCALAR_PAIR_KEY == key {
            Some(entry & SCALAR_PAIR_BOUNDARY != 0)
        } else {
            None
        }
    }

    fn insert(&mut self, before: char, after: char, boundary: bool) {
        let key = Self::key(before, after);
        self.0[Self::slot(key)] =
            key | SCALAR_PAIR_VALID | if boundary { SCALAR_PAIR_BOUNDARY } else { 0 };
    }
}

#[derive(Default)]
pub(crate) struct Graphemes {
    slots: Vec<Option<Arc<str>>>,
    free: Vec<u32>,
    index: BTreeMap<Arc<str>, u32>,
    bytes: usize,
    scalar_boundaries: ScalarBoundaryCache,
}

impl Graphemes {
    pub(crate) fn cached_scalar_boundary(&self, before: char, after: char) -> Option<bool> {
        self.scalar_boundaries.get(before, after)
    }

    pub(crate) fn cache_scalar_boundary(&mut self, before: char, after: char, boundary: bool) {
        self.scalar_boundaries.insert(before, after, boundary);
    }

    pub(crate) fn checkpoint_parts(&self) -> (&[Option<Arc<str>>], &[u32]) {
        (&self.slots, &self.free)
    }

    pub(crate) fn restore_checkpoint_parts(
        slots: Vec<Option<Arc<str>>>,
        free: Vec<u32>,
    ) -> Option<Self> {
        if slots.len() > MAX_ENTRIES {
            return None;
        }
        let mut index = BTreeMap::new();
        let mut bytes = 0usize;
        let mut vacant = vec![false; slots.len()];
        for (position, text) in slots.iter().enumerate() {
            if let Some(text) = text {
                if text.is_empty() || text.len() > MAX_GRAPHEME_BYTES {
                    return None;
                }
                bytes = bytes.checked_add(text.len() + ARC_HEADER)?;
                if bytes > MAX_TEXT_BYTES
                    || index.insert(text.clone(), position as u32 + 1).is_some()
                {
                    return None;
                }
            } else {
                vacant[position] = true;
            }
        }
        for &position in &free {
            let entry = vacant.get_mut(position as usize)?;
            if !*entry {
                return None;
            }
            *entry = false;
        }
        if vacant.iter().any(|value| *value) {
            return None;
        }
        Some(Self {
            slots,
            free,
            index,
            bytes,
            scalar_boundaries: ScalarBoundaryCache::default(),
        })
    }

    pub(crate) fn get(&self, id: u32) -> Option<&str> {
        id.checked_sub(1)
            .and_then(|index| self.slots.get(index as usize))
            .and_then(Option::as_deref)
    }

    pub(crate) fn intern(&mut self, text: &str) -> Option<u32> {
        if let Some(id) = self.index.get(text) {
            return Some(*id);
        }
        let bytes = text.len().checked_add(ARC_HEADER)?;
        if text.len() > MAX_GRAPHEME_BYTES
            || self.index.len() == MAX_ENTRIES
            || self.bytes.checked_add(bytes)? > MAX_TEXT_BYTES
        {
            return None;
        }
        let text: Arc<str> = Arc::from(text);
        let index = if let Some(index) = self.free.pop() {
            self.slots[index as usize] = Some(text.clone());
            index
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Some(text.clone()));
            index
        };
        self.bytes += bytes;
        let id = index + 1;
        self.index.insert(text, id);
        Some(id)
    }

    /// Only the owning Terminal may reclaim IDs, after examining all retained
    /// primary, alternate and history cells. Live IDs are never renumbered.
    pub(crate) fn collect(&mut self, ids: impl Iterator<Item = u32>) {
        let mut live = vec![false; self.slots.len()];
        for id in ids {
            if let Some(index) = id.checked_sub(1)
                && let Some(slot) = live.get_mut(index as usize)
            {
                *slot = true;
            }
        }
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if !live[index]
                && let Some(text) = slot.take()
            {
                self.index.remove(text.as_ref());
                self.bytes -= text.len() + ARC_HEADER;
                self.free.push(index as u32);
            }
        }
    }

    pub(crate) fn chars<'a>(&'a self, cell: &Cell) -> CellChars<'a> {
        CellChars {
            text: self.get(cell.grapheme_id).map(str::chars),
            inline: if cell.width == 0 {
                ['\0'; 3]
            } else {
                [cell.ch, cell.combining[0], cell.combining[1]]
            }
            .into_iter(),
        }
    }
}

/// The complete original scalar sequence of one leading cell, with no
/// normalization and no allocation. Trailing wide cells produce no characters.
#[derive(Clone)]
pub struct CellChars<'a> {
    text: Option<core::str::Chars<'a>>,
    inline: core::array::IntoIter<char, 3>,
}

impl Iterator for CellChars<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        if let Some(text) = &mut self.text {
            text.next()
        } else {
            self.inline.find(|ch| *ch != '\0')
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_interns_clusters_and_never_reuses_a_live_id() {
        let mut pool = Graphemes::default();
        let family = "👩🏽‍💻";
        let a = pool.intern(family).unwrap();
        let b = pool.intern("🇺🇳").unwrap();
        assert_eq!(pool.intern(family), Some(a));
        pool.collect(core::iter::once(a));
        assert_eq!(pool.get(a), Some(family));
        assert_eq!(pool.get(b), None);
        let c = pool.intern("1️⃣").unwrap();
        assert_eq!(b, c);
        assert_eq!(pool.get(a), Some(family));
        assert_eq!(pool.get(c), Some("1️⃣"));
    }

    #[test]
    fn pool_has_fixed_text_and_entry_limits_including_arc_headers() {
        let mut pool = Graphemes::default();
        assert!(pool.intern(&"x".repeat(MAX_GRAPHEME_BYTES + 1)).is_none());
        for i in 0..MAX_ENTRIES + 1 {
            let text = alloc::format!("{i:05}{}", "x".repeat(MAX_GRAPHEME_BYTES - 5));
            if pool.intern(&text).is_none() {
                break;
            }
        }
        assert!(pool.bytes <= MAX_TEXT_BYTES);
        assert!(pool.slots.len() <= MAX_ENTRIES);
        assert!(pool.index.len() < MAX_ENTRIES);
        pool.collect(core::iter::empty());
        assert_eq!(pool.bytes, 0);
        assert!(pool.index.is_empty());
        assert!(pool.intern("👨‍👩‍👧‍👦").is_some());
    }

    #[test]
    fn scalar_cache_stores_zero_keys_and_both_results_exactly() {
        let mut cache = ScalarBoundaryCache::default();
        assert_eq!(core::mem::size_of::<ScalarBoundaryCache>(), 512);
        assert_eq!(cache.get('\0', '\0'), None);
        cache.insert('\0', '\0', false);
        assert_eq!(cache.get('\0', '\0'), Some(false));
        cache.insert('\0', '\0', true);
        assert_eq!(cache.get('\0', '\0'), Some(true));
        cache.insert('\u{10ffff}', '\u{10ffff}', false);
        assert_eq!(cache.get('\u{10ffff}', '\u{10ffff}'), Some(false));
        assert_eq!(cache.get('\u{10fffe}', '\u{10ffff}'), None);
    }

    #[test]
    fn scalar_cache_all_misses_and_collisions_never_reuse_another_pair() {
        let before = '界';
        let target = ScalarBoundaryCache::slot(ScalarBoundaryCache::key(before, '\0'));
        let colliding: Vec<char> = (0..=0x10ffff)
            .filter_map(char::from_u32)
            .filter(|after| {
                ScalarBoundaryCache::slot(ScalarBoundaryCache::key(before, *after)) == target
            })
            .take(128)
            .collect();
        assert_eq!(colliding.len(), 128);
        let mut cache = ScalarBoundaryCache::default();
        for (index, &after) in colliding.iter().enumerate() {
            assert_eq!(cache.get(before, after), None);
            let boundary = index % 2 == 0;
            cache.insert(before, after, boundary);
            assert_eq!(cache.get(before, after), Some(boundary));
            for &previous in &colliding[..index] {
                assert_eq!(cache.get(before, previous), None);
            }
        }
    }
}
