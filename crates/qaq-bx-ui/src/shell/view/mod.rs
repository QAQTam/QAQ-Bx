//! view 区域构建(P5-lite):视图构建方法自 mod.rs 逐字迁移,`view()` 根装配留守
//! (窗口视觉/尺寸泵/回调装配/页面分发/导航壳,待后续阶段进一步分拆)。

mod chrome_nav;
mod peek_overlay;
mod player_views;
mod shell_root;

pub(in crate::shell) use chrome_nav::nav_item;
