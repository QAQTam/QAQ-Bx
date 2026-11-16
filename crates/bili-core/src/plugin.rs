//! 插件基座：元信息、生命周期、注册表。

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
}

/// 插件共享上下文：HTTP 客户端（统一 UA/超时）与数据目录。
pub struct PluginContext {
    pub http: reqwest::Client,
    pub data_dir: PathBuf,
}

/// 插件基座。实现方用内部可变性（OnceLock/RwLock）保存 init 注入的资源。
#[async_trait]
pub trait Plugin: Send + Sync {
    fn meta(&self) -> &PluginMeta;

    /// 注册后由 [`PluginRegistry::init_all`] 调用一次。
    async fn init(&self, ctx: &PluginContext) -> Result<()>;
}

/// 极简注册表：只负责生命周期编排。
/// 服务获取不做泛型服务定位——需要某能力的组件（如解析插件）
/// 直接在构造函数里注入 `Arc<dyn CredentialProvider>` 等具体抽象。
#[derive(Default)]
pub struct PluginRegistry {
    plugins: Vec<Arc<dyn Plugin>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, plugin: Arc<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    pub fn plugins(&self) -> &[Arc<dyn Plugin>] {
        &self.plugins
    }

    pub async fn init_all(&self, ctx: &PluginContext) -> Result<()> {
        for p in &self.plugins {
            p.init(ctx).await?;
        }
        Ok(())
    }
}
