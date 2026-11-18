//! 凭据类型、持久化存储与对外提供的凭据契约。
//!
//! 依赖方向约定：解析类插件只依赖 [`CredentialProvider`] 这个抽象，
//! 不感知登录插件的具体实现（扫码 / 密码 / 导入 Cookie）。

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::RwLock,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;

use crate::error::{BiliError, Result};
use crate::storage::{atomic_write_json, load_json_opt, remove_file_if_exists};

/// 登录成功后的一组凭据。字段与 CDP 实测一致（见 docs/bilibili-login-research.md）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Credentials {
    /// 会话令牌（HttpOnly Cookie）
    pub sessdata: String,
    /// CSRF 令牌
    pub bili_jct: String,
    /// 用户 mid
    pub dede_user_id: u64,
    #[serde(default)]
    pub dede_user_id_ck_md5: Option<String>,
    /// 设备指纹，部分接口风控需要
    #[serde(default)]
    pub buvid3: Option<String>,
    #[serde(default)]
    pub buvid4: Option<String>,
    /// 刷新令牌（localStorage `ac_time_value`），用于 180 天后的 Cookie 续期
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub created_at: i64,
    /// 预计过期时间（unix 秒）
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// 其余 Cookie 原样保留（兜底）
    #[serde(default)]
    pub cookies: BTreeMap<String, String>,
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Credentials {
    /// 拼出可直接放入 `Cookie` 头的字符串（所有已知 cookie）。
    pub fn cookie_header(&self) -> String {
        let mut pairs: Vec<(String, String)> = vec![
            ("SESSDATA".into(), self.sessdata.clone()),
            ("bili_jct".into(), self.bili_jct.clone()),
            ("DedeUserID".into(), self.dede_user_id.to_string()),
        ];
        if let Some(v) = &self.dede_user_id_ck_md5 {
            pairs.push(("DedeUserID__ckMd5".into(), v.clone()));
        }
        if let Some(v) = &self.buvid3 {
            pairs.push(("buvid3".into(), v.clone()));
        }
        if let Some(v) = &self.buvid4 {
            pairs.push(("buvid4".into(), v.clone()));
        }
        for (k, v) in &self.cookies {
            if !pairs.iter().any(|(name, _)| name == k) {
                pairs.push((k.clone(), v.clone()));
            }
        }
        pairs
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// 是否已过期（预留 1 天缓冲，避免临界点请求失败）。
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(t) => now_unix() >= t - 86_400,
            None => false,
        }
    }
}

/// 凭据持久化。实现可以是文件、系统钥匙串、数据库……
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn load(&self) -> Result<Option<Credentials>>;
    async fn save(&self, creds: &Credentials) -> Result<()>;
    async fn clear(&self) -> Result<()>;
}

/// 默认文件实现（JSON）。
pub struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<Credentials>> {
        load_json_opt(&self.path, "凭据文件")
    }

    async fn save(&self, creds: &Credentials) -> Result<()> {
        atomic_write_json(&self.path, creds, "凭据")
    }

    async fn clear(&self) -> Result<()> {
        remove_file_if_exists(&self.path)
    }
}

/// ★ 解析类插件依赖此抽象获取凭据。
#[async_trait]
pub trait CredentialProvider: Send + Sync {
    /// 返回当前可用凭据。未登录 → [`BiliError::NotLoggedIn`]，过期 → [`BiliError::Expired`]。
    async fn get(&self) -> Result<Credentials>;

    /// 仅读内存缓存，不发 IO、不报错。
    fn cached(&self) -> Option<Credentials>;
}

/// [`CredentialProvider`] 的默认实现：内存缓存 + 底层 store。
/// 登录插件在登录成功/续期后调用 [`DefaultCredentialProvider::update`] 写穿。
pub struct DefaultCredentialProvider {
    store: std::sync::Arc<dyn CredentialStore>,
    cache: RwLock<Option<Credentials>>,
}

impl DefaultCredentialProvider {
    pub fn new(store: std::sync::Arc<dyn CredentialStore>) -> Self {
        Self {
            store,
            cache: RwLock::new(None),
        }
    }

    /// 登录成功 / 续期后写入（内存 + 落盘）。
    pub async fn update(&self, creds: &Credentials) -> Result<()> {
        self.store.save(creds).await?;
        *self.cache.write().expect("credential cache poisoned") = Some(creds.clone());
        Ok(())
    }

    /// 登出 / 凭据作废。
    pub async fn clear(&self) -> Result<()> {
        self.store.clear().await?;
        *self.cache.write().expect("credential cache poisoned") = None;
        Ok(())
    }
}

#[async_trait]
impl CredentialProvider for DefaultCredentialProvider {
    async fn get(&self) -> Result<Credentials> {
        if let Some(c) = self.cached() {
            if !c.is_expired() {
                return Ok(c);
            }
        }
        let creds = self.store.load().await?.ok_or(BiliError::NotLoggedIn)?;
        if creds.is_expired() {
            return Err(BiliError::Expired);
        }
        *self.cache.write().expect("credential cache poisoned") = Some(creds.clone());
        Ok(creds)
    }

    fn cached(&self) -> Option<Credentials> {
        self.cache
            .read()
            .expect("credential cache poisoned")
            .clone()
    }
}
