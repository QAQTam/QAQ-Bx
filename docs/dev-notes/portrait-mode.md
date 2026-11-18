# 竖屏模式(实验)设计与行为隔离矩阵

> 状态:2026-11-19 首版落地(设置开关 + 宽度钳制 + 贴边最大化),真机验证全绿。
> 本文是竖屏模式的唯一设计权威;未来「隔离更新」(两模式功能节奏分叉)按本文
> 的双轴模型归属,不再另起文档。

## 0. 定位与边界(拍板口径)

- **竖屏模式 ≠ 第二客户端**:同一二进制,模式 = 「窗口策略开关 + 既有移动布局」。
  窗口客户区钳制在 **[390, 860) DIP**——上限取底栏(5-Tab 导航)出现边界
  `MOBILE_BREAKPOINT`(layout.rs:16,严格 `<`)+ 1 的余量,常量
  `PORTRAIT_MAX_WIDTH_DIPS = MOBILE_BREAKPOINT - 1.0`(shell.rs),阈值挪动自动跟随。
- 移动布局(底栏 5-Tab、移动播放页吸顶+Tab、Compact/Medium 内容档)全部**复用既有
  档位系统**,竖屏模式零新增布局分支。
- 开关即时生效(约束翻转即钳制),重启后保持(settings 持久化,旧配置缺字段缺省关)。

## 1. 双轴隔离模型(核心决策)

未来两模式的功能差异,**按两条正交轴归属,不横向混杂**:

### 轴 A:布局档位轴(自动隔离,零新代码)
`ViewportLayout`(chrome: Mobile/Desktop × content: Compact/Medium/Expanded)驱动的一切
分支天然隔离——竖屏模式只是「被钳制在 Mobile/Medium(或 Compact)档」的一般模式。
**落新特性时先问:它是不是档位分支?是 → 写进既有 `is_mobile()`/tier 分支,完事。**
既有先例:底栏(chrome.rs compact_bottom_bar)、移动播放页(player.rs PlayerTab)、
半屏抽屉(桌面限定)、内敛搜索框(Expanded 限定)。

### 轴 B:窗口策略轴(显式 `settings.portrait_mode`,集中一处)
只涉及「窗口几何与系统 chrome 策略」的差异,全部收在:
- `shell.rs` visuals 构造处(constraints/client_size/placement 分支)+ `portrait_*` 字段
- `winrt_ext.rs` presenter 手术三件套(set_maximizable / dock_window_right / restore)
- `chrome.rs` build_title_bar 的 right_items(贴边钮)
**禁止把轴 B 标志散进业务代码**(页面/组件不读 portrait_mode;它们只感知布局档位)。

### 共有区(明确不隔离)
引擎(mpv/native 与布局正交)、弹幕、SMTC、登录/通知/私信、评论/虚拟化、设置落盘、
搜索/热门/直播数据链。

## 2. 行为隔离清单(本轮已实现 + 前瞻)

| # | 行为 | 一般模式 | 竖屏模式 | 轴 |
|---|---|---|---|---|
| 1 | 宽度约束 | min 390,无上限 | min 390,**max 859**(presenter 硬钳制,拖不动) | B |
| 2 | 初始几何 | 恢复上次 placement | 忽略 placement(可能超带),client_size 859×默认高 | B |
| 3 | 系统最大化钮 | 可用 | **没收**(presenter `SetIsMaximizable(false)`,控件渲染置灰、真实点击无效——已真机验证 WS_MAXIMIZEBOX=CLEAR) | B |
| 4 | 最大化语义 | 系统最大化 | **贴边最大化**:右缘贴合工作区 × 全高 × 宽 859,再点还原(dock_window_right,DIP→px 用 inner/当前 dips 推 scale,零 DPI API) | B |
| 5 | 贴边钮 | 无 | TitleBar right_items 最右(紧邻 caption 钮;与搜索/通知钮同容器,避免单值 relation 覆盖) | B |
| 6 | **全屏播放** | 保留 | **保留**(用户钦定)——走 FullScreen presenter 独立通道,与最大化没收正交;进出全屏后在 `sync_window_fullscreen` 重按一次没收(防 presenter kind 切换复位) | 共有(特殊处理) |
| 7 | 上次窗口位置记忆 | settings.window 恢复 | 竖屏期不写不读(贴边/钳制几何由模式自管);未来可拆两套记忆 | B(前瞻) |
| 8 | 右栏相关推荐/双栏 | 有 | 移动布局自动(相关推荐进 Tab) | A(自动) |
| 9 | 半屏抽屉 | 桌面限定 | 移动布局自动不触发 | A(自动) |
| 10 | 播放页 PIP 下滚小窗 | 桌面限定 | 移动布局自动不触发 | A(自动) |

### 前瞻(未实现,防遗忘)
- 设置页按模式过滤无意义项(如半屏宽滑杆在竖屏下无意义)——轴 B 的小分支,exp 后再说。
- 「贴边」方向(左/右)与贴边宽度个性化——dock 参数已参数化,加设置项即可。
- 若未来竖屏特有功能需要**自己的数据/缓存代次**(如独立的历史栈),那是轴 C
  (会话状态轴)——届时开新文档,不要塞进 A/B。

## 3. 机制要点(实现契约)

1. **边界语义**:底栏出现在 `width < 860`;859 上限保证任意 DPI 取整后仍在带内
   (真机 96dpi 实测客户区恰 859.0)。
2. **没收的完整性**:presenter `SetIsMaximizable(false)` → WS_MAXIMIZEBOX 清位
   (真机 GetWindowLong 验证)→ 控件渲染置灰 + 真实鼠标点击无效(UIA Invoke 可
   旁路触发,但宽度被 presenter max 钳死,无法逃逸到横屏)。
3. **right_header 是单值 relation**(reactor generated.rs:5603 → SetRightHeader,
   后写覆盖):贴边钮必须并入 right_items 统一构建,不能独立调用(首版踩过)。
4. **caption 钮是 TitleBar 控件 XAML 层的**(UIA 树可见 Minimize/Maximize/Close),
   presenter 开关只影响其可用态,不能隐藏单钮——想彻底隐藏需 reactor 级能力
   (TitleBar 模板/接口扩展),留待与并行会话协调。
5. dock 的 DIP→像素换算不依赖任何 DPI API:`AppWindow.ClientSize / 当前客户区 DIP`
   即 scale,`Size − ClientSize` 即非客户区补偿。
6. settings 序列化:`#[serde(default)]` 容器级,新字段旧配置自动缺省(有测试钉死)。

## 4. 验证记录(2026-11-19 真机,96dpi,QAQ_BX_DATA_DIR 隔离)

- 竖屏启动:客户区 **859.0×800.0**(= 带顶),移动布局+底栏+移动播放页渲染正常
  (截图 frames-edge/portrait-0-launch.png)。
- 最大化没收:`WS_MAXIMIZEBOX` CLEAR;真实鼠标点击置灰钮几何零变化;
  UIA Invoke 旁路触发后宽度仍被钳在 875px(外框)——无横屏逃逸。
- 贴边:点击后 (1045,0) 875×1032 = 右缘 1920 精确贴合 + 工作区全高;再点还原
  (208,208) 875×808 与原几何逐像素一致。
- 回归:cargo test --workspace 全绿(core 61 / ui 75);selftest boot/stress PASS。

## 5. 诚实清单(未验证/已知妥协)

- 最大化钮是**置灰**而非隐藏(presenter 开关的控件表现);彻底隐藏需 reactor 能力。
- UIA Invoke 可旁路置灰钮触发「钳制最大化」(859 宽 × 全高、居中)——自动化工具
  行为,真实鼠标已验证无效;键盘 Win+Up 受样式位保护(未单独实测)。
- 贴边态下用户手动拖走窗口:内部 docked 标记不清除,再点「还原」回到旧几何——
  不完美,exp 接受;要精确需区分应用 Move 与用户拖拽(信号可做,先不花)。
- 未实测:150% DPI 下的像素换算与钳制(公式上自洽,真机待用户复核)、多显示器
  跨屏拖动后的 DisplayArea 归属。
- E13(需登录态)未回归;全屏播放与贴边态的组合(全屏进出后贴边几何还原)依赖
  presenter 的 bounds 记忆,未单独构造验证。
