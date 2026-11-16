//! bili-core：biliplay 的契约层。
//!
//! 这里只定义"是什么"（trait 与类型），不定义"怎么做"（具体插件）。
//! 详见 docs/bilibili-login-research.md（端点逆向）与各插件 crate。

pub mod account;
pub mod auth;
pub mod credentials;
pub mod danmaku;
pub mod error;
pub mod feed;
pub mod httpx;
pub mod loose;
pub mod player;
pub mod plugin;
pub mod settings;
pub mod storage;
pub mod ticket;
pub mod wbi;

pub use crate::storage::resolve_data_dir;

pub mod prelude {
    pub use crate::account::{NavClient, NavSnapshot, UserProfile, VipStatus};
    pub use crate::auth::{AuthPlugin, LoginEvent, LoginFlow};
    pub use crate::credentials::{
        CredentialProvider, CredentialStore, Credentials, DefaultCredentialProvider,
        FileCredentialStore,
    };
    pub use crate::error::{BiliError, Result};
    pub use crate::feed::{ArchiveCard, FeedCard, FeedClient, OgvCard, RcmdQuery};
    pub use crate::plugin::{Plugin, PluginContext, PluginMeta, PluginRegistry};
    pub use crate::settings::{AppSettings, CodecPref, FileSettingsStore, QualityPref, SETTINGS_FILE};
    pub use crate::storage::resolve_data_dir;
    pub use crate::ticket::{FileTicketStore, TicketData, TicketManager, TICKET_FILE};
    pub use crate::wbi::WbiSigner;
}
