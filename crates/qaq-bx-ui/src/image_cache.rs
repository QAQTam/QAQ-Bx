//! 封面位图缓存：url → `EncodedImage`（有界 LRU）。
//!
//! 目的：往回滚、换一换后重复出现的卡（跨页重复实测存在）免重复下载；
//! 位图底座 `Arc<[u8]>`，缓存与卡片共享同一份字节，克隆廉价。
//!
//! 策略：容量上限 480 张（@480w_270h_1c.jpg 约 ~12MB），命中刷新新鲜度、超限逐出最久未访问条目，
//! 避免原先满 400 张整表清空导致的回滚全量重下抖动；点击「换一换」时整表清空。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use windows_reactor::EncodedImage;

/// 缓存上限（张）。适配 8GB 内存约束，超限逐出最久未用条。
const MAX_ENTRIES: usize = 480;

#[derive(Default)]
struct LruState {
    map: HashMap<String, EncodedImage>,
    order: VecDeque<String>,
}

impl LruState {
    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_string());
    }
}

pub struct ImageCache {
    capacity: usize,
    inner: Mutex<LruState>,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::with_capacity(MAX_ENTRIES)
    }
}

impl ImageCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            inner: Mutex::new(LruState::default()),
        }
    }

    /// 命中返回位图并刷新 LRU 新鲜度。
    pub fn get(&self, url: &str) -> Option<EncodedImage> {
        let mut guard = self.inner.lock().expect("封面缓存锁中毒");
        let img = guard.map.get(url).cloned()?;
        guard.touch(url);
        Some(img)
    }

    /// 写入；超限且为新键时逐出最久未访问的一条。
    pub fn put(&self, url: String, image: EncodedImage) {
        let mut guard = self.inner.lock().expect("封面缓存锁中毒");
        if guard.map.contains_key(&url) {
            guard.map.insert(url.clone(), image);
            guard.touch(&url);
            return;
        }
        while guard.map.len() >= self.capacity {
            let Some(oldest) = guard.order.pop_front() else {
                guard.map.clear();
                break;
            };
            guard.map.remove(&oldest);
        }
        guard.order.push_back(url.clone());
        guard.map.insert(url, image);
    }

    /// 整表清空（换一换时调用）。
    pub fn clear(&self) {
        let mut guard = self.inner.lock().expect("封面缓存锁中毒");
        guard.map.clear();
        guard.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_least_recently_used_and_touches_on_get() {
        let cache = ImageCache::with_capacity(2);
        let a = EncodedImage::new(vec![1]);
        let b = EncodedImage::new(vec![2]);
        let c = EncodedImage::new(vec![3]);

        cache.put("a".into(), a);
        cache.put("b".into(), b);
        assert!(cache.get("a").is_some()); // 命中 a → 最久未用变为 b
        cache.put("c".into(), c);

        assert!(cache.get("b").is_none());
        assert!(cache.get("a").is_some());
        assert!(cache.get("c").is_some());
    }
}
