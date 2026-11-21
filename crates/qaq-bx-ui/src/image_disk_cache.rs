//! 封面/头像磁盘缓存：url → 原始编码字节（有界 FIFO-by-mtime，重启免重下）。
//!
//! 与 `image_cache`（内存 LRU）分层：内存层命中最快；磁盘层命中免网络往返，
//! 回填内存层；网络下载成功后双层写入。所有 IO 均 best-effort——缓存目录
//! 不可写/文件损坏时静默降级为纯网络路径，绝不 panic、绝不阻塞 UI 线程
//! （调用方在 `spawn_background` + 共享 runtime 内使用）。
//!
//! 落盘布局：`<data>/cache/images/{fnv1a64(url):016x}.bin`，temp+rename 原子写。
//! 容量上限按字节数计（默认 200MB），超限按文件 mtime 从最旧逐出；
//! `get` 命中会刷新 mtime（近似 LRU）。进程内维护字节总数（启动扫描一次），
//! 避免每次写入都全目录 stat。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// 磁盘缓存字节上限（200MB，适配 8G 内存小盘机；缩略图 ~25KB/张 ≈ 8000 张）。
pub const DISK_CACHE_MAX_BYTES: u64 = 200 * 1024 * 1024;

/// FNV-1a 64 位散列：url → 文件名（无加密需求，只求稳定 + 低碰撞 + 零依赖）。
fn url_hash(url: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub struct ImageDiskCache {
    dir: PathBuf,
    max_bytes: u64,
    /// 进程内记账的当前总字节（启动扫描初始化；写入/逐出/清空时增减）。
    total_bytes: Mutex<u64>,
}

impl ImageDiskCache {
    /// 打开（必要时创建）缓存目录并扫描一次现有条目记账。
    pub fn open(dir: PathBuf) -> Self {
        Self::open_with_capacity(dir, DISK_CACHE_MAX_BYTES)
    }

    pub fn open_with_capacity(dir: PathBuf, max_bytes: u64) -> Self {
        let _ = fs::create_dir_all(&dir);
        let total = scan_total(&dir);
        Self {
            dir,
            max_bytes: max_bytes.max(1),
            total_bytes: Mutex::new(total),
        }
    }

    fn path_for(&self, url: &str) -> PathBuf {
        self.dir.join(format!("{:016x}.bin", url_hash(url)))
    }

    /// 命中返回原始字节并刷新 mtime（近似 LRU 新鲜度）。
    pub fn get(&self, url: &str) -> Option<Vec<u8>> {
        if url.is_empty() {
            return None;
        }
        let path = self.path_for(url);
        let bytes = fs::read(&path).ok()?;
        // 刷新访问时间：mtime 即逐出依据（失败不影响命中）。
        if let Ok(file) = fs::OpenOptions::new().write(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        Some(bytes)
    }

    /// 写入（temp+rename 原子落盘）；超限时按 mtime 从最旧逐出。
    pub fn put(&self, url: &str, bytes: &[u8]) {
        if url.is_empty() || bytes.is_empty() {
            return;
        }
        let len = bytes.len() as u64;
        if len > self.max_bytes {
            return; // 单条超上限：不缓存,走纯网络。
        }
        let path = self.path_for(url);
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, bytes).is_err() {
            let _ = fs::remove_file(&tmp);
            return;
        }
        let replaced = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if fs::rename(&tmp, &path).is_err() {
            let _ = fs::remove_file(&tmp);
            return;
        }
        {
            let mut total = self.total_bytes.lock().expect("磁盘缓存记账锁中毒");
            *total = total.saturating_sub(replaced).saturating_add(len);
        }
        self.evict_if_over_budget();
    }

    /// 当前记账总字节（设置页展示用）。
    pub fn size_bytes(&self) -> u64 {
        *self.total_bytes.lock().expect("磁盘缓存记账锁中毒")
    }

    /// 整目录清空（一键清空缓存）；返回实际释放的字节数。
    /// 只动本目录内的 `.bin`/`.tmp` 文件——绝不触碰目录外的任何文件。
    pub fn clear(&self) -> u64 {
        let mut freed = 0u64;
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !is_cache_file(&path) {
                    continue;
                }
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if fs::remove_file(&path).is_ok() {
                    freed += len;
                }
            }
        }
        *self.total_bytes.lock().expect("磁盘缓存记账锁中毒") = 0;
        freed
    }

    fn evict_if_over_budget(&self) {
        let over = {
            let total = self.total_bytes.lock().expect("磁盘缓存记账锁中毒");
            *total > self.max_bytes
        };
        if !over {
            return;
        }
        // 收集 (mtime, len, path)，按 mtime 升序逐出到预算内。
        let mut entries: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        if let Ok(dir) = fs::read_dir(&self.dir) {
            for entry in dir.flatten() {
                let path = entry.path();
                if !is_cache_file(&path) {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    entries.push((mtime, meta.len(), path));
                }
            }
        }
        entries.sort_by_key(|(mtime, _, _)| *mtime);
        let mut total = self.total_bytes.lock().expect("磁盘缓存记账锁中毒");
        for (_, len, path) in entries {
            if *total <= self.max_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                *total = total.saturating_sub(len);
            }
        }
    }
}

fn is_cache_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext == "bin" || ext == "tmp")
}

fn scan_total(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_cache_file(&path) {
                continue;
            }
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "qaq-bx-disk-cache-test-{}-{}-{}",
            std::process::id(),
            tag,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn put_get_roundtrip_and_size_accounting() {
        let dir = temp_dir("roundtrip");
        let cache = ImageDiskCache::open(dir.clone());
        assert_eq!(cache.size_bytes(), 0);
        cache.put("https://a/1.jpg", &[1, 2, 3]);
        assert_eq!(cache.get("https://a/1.jpg").as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(cache.size_bytes(), 3);
        assert!(cache.get("https://a/missing.jpg").is_none());
        // 同 url 覆盖写：记账替换而非叠加。
        cache.put("https://a/1.jpg", &[9, 9]);
        assert_eq!(cache.size_bytes(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopen_scans_existing_entries() {
        let dir = temp_dir("reopen");
        {
            let cache = ImageDiskCache::open(dir.clone());
            cache.put("https://a/1.jpg", &[1, 2, 3, 4]);
        }
        // 模拟重启：新实例无内存记账,扫描恢复;命中免网络。
        let reopened = ImageDiskCache::open(dir.clone());
        assert_eq!(reopened.size_bytes(), 4);
        assert_eq!(reopened.get("https://a/1.jpg").as_deref(), Some(&[1, 2, 3, 4][..]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn evicts_oldest_when_over_budget() {
        let dir = temp_dir("evict");
        let cache = ImageDiskCache::open_with_capacity(dir.clone(), 10);
        cache.put("https://a/old.jpg", &[0; 6]);
        // 人为做旧 mtime,确保逐出顺序确定(同秒写入的 mtime 分辨率不足)。
        let old_path = cache.path_for("https://a/old.jpg");
        let file = fs::OpenOptions::new().write(true).open(&old_path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH).unwrap();
        cache.put("https://a/new.jpg", &[1; 6]);
        assert!(cache.get("https://a/old.jpg").is_none(), "超限须逐出最旧条目");
        assert!(cache.get("https://a/new.jpg").is_some());
        assert!(cache.size_bytes() <= 10);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_frees_bytes_and_never_touches_foreign_files() {
        let root = temp_dir("clear");
        let dir = root.join("cache").join("images");
        let cache = ImageDiskCache::open(dir.clone());
        cache.put("https://a/1.jpg", &[1, 2, 3]);
        cache.put("https://a/2.jpg", &[4, 5]);
        // 白名单纪律:数据目录里的凭据/设置/无关文件绝不可被清空波及。
        let credentials = root.join("credentials.json");
        let settings = root.join("settings.json");
        fs::write(&credentials, "secret").unwrap();
        fs::write(&settings, "{}").unwrap();
        let foreign = dir.join("keep.txt");
        fs::write(&foreign, "not a cache entry").unwrap();

        let freed = cache.clear();
        assert_eq!(freed, 5);
        assert_eq!(cache.size_bytes(), 0);
        assert!(cache.get("https://a/1.jpg").is_none());
        assert!(credentials.exists(), "credentials.json 绝不能被清缓存删除");
        assert!(settings.exists(), "settings.json 绝不能被清缓存删除");
        assert!(foreign.exists(), "缓存目录内非 .bin/.tmp 文件也不动");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn hash_is_stable_and_url_sensitive() {
        assert_eq!(url_hash("https://a/1.jpg"), url_hash("https://a/1.jpg"));
        assert_ne!(url_hash("https://a/1.jpg"), url_hash("https://a/2.jpg"));
    }

    #[test]
    fn empty_url_and_oversize_entry_are_noops() {
        let dir = temp_dir("noop");
        let cache = ImageDiskCache::open_with_capacity(dir.clone(), 4);
        cache.put("", &[1]);
        cache.put("https://a/big.jpg", &[0; 8]);
        assert_eq!(cache.size_bytes(), 0);
        assert!(cache.get("").is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
