# QAQ-Bx AppX / MSIX 打包与商店上架准备

> 应用正式名：`QAQ-Bx`（CLI 可执行文件 `qaq-bx.exe`，桌面端可执行文件 `qaq-bx-ui.exe`）。
> 版本约定：Cargo workspace 版本默认 `0.0.0`，正式上架微软商店前须使用非零四段式版本号（例如 `0.1.0.0`）。

## 1. `windows-reactor-setup` 与打包模式选型

`qaq-bx-ui` 基于 `windows-reactor`（WinUI 3 / Windows App SDK 2.x）。参考 `windows-reactor-setup` 的实现，AppX/MSIX 打包有两种模式：

### 模式 A：Self-Contained（自包含，推荐用于独立分发 / 侧载）
使用 `windows-reactor-setup` 在编译期将 Windows App SDK Runtime 与 WebView2 投影 DLL 部署到可执行文件同级目录，用户无需预先安装 Windows App Runtime 框架包。

1. **构建配置**（`crates/qaq-bx-ui/Cargo.toml` + `crates/qaq-bx-ui/build.rs`）：
   - `[build-dependencies]` 引入 `windows-reactor-setup = { path = "D:/Projects/qaq-winui-reactor/crates/libs/reactor-setup" }`。
   - `build.rs` 调用 `windows_reactor_setup::as_self_contained()`：
     - 从 NuGet 下载并缓存 `Microsoft.WindowsAppSDK.Runtime`（`2.6.4`）与 `Microsoft.Web.WebView2`（`1.0.4078.44`）到 `%LOCALAPPDATA%\windows-reactor-setup\temp\v1`。
     - 解压 `MSIX/win10-{arch}/Microsoft.WindowsAppRuntime.2.msix`，将 `assets/runtime.txt` 列出的运行时 DLL/子目录及 `Microsoft.Web.WebView2.Core.dll` 复制到 Cargo 输出目录（`target/release/`）。
     - 通过 `/MANIFEST:EMBED` + `/MANIFESTINPUT` 将 `assets/app.manifest`（含 `<description>windows-reactor-self-contained</description>` 与 WinRT SxS 注册表项）嵌入 `qaq-bx-ui.exe`。
2. **Manifest 差异**：自包含模式下，`AppxManifest.xml` **不需要**声明 `<PackageDependency Name="Microsoft.WindowsAppRuntime.2" ... />`。
3. **打包内容**：除 `qaq-bx-ui.exe` 外，必须完整保留 `target/release/` 下由 `windows-reactor-setup` 部署的所有运行时 DLL 与子目录（如 `Microsoft.WindowsAppRuntime.dll`、`Microsoft.ui.xaml.dll`、`Microsoft.Web.WebView2.Core.dll` 等）。

### 模式 B：Framework-Dependent（框架依赖，包体最小）
不引入 `windows-reactor-setup`，改为在 `AppxManifest.xml` 中声明系统级框架包依赖（适用于 Store 分发或目标机已安装 Windows App Runtime 2.x 的环境）：

```xml
<PackageDependency
  Name="Microsoft.WindowsAppRuntime.2"
  MinVersion="2.0.1.0"
  Publisher="CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US" />
```

---

## 2. `AppxManifest` 模板与微软商店 / 通知标识

完整模板见 `packaging/appx/AppxManifest.template.xml`，核心占位符由 `scripts/package-appx.ps1` 注入：

1. **Microsoft Store 产品标识（Partner Center → 产品管理 → 产品标识，上架前填入）**：
   - `{{PACKAGE_NAME}}`：Store 分配的包系列名，如 `YourPartnerName.QAQ-Bx`（本地默认 `QAQ-Bx` / `QAQ-Bx-Dev`）
   - `{{PUBLISHER}}`：Store 分配的 Publisher ID，如 `CN=XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`（本地默认 `CN=QAQ`）
   - `{{PUBLISHER_DISPLAY_NAME}}`：Partner Center 账户发布者显示名（本地默认 `QAQ`）
   - `{{DISPLAY_NAME}}` / `{{SHORT_NAME}}` / `{{DESCRIPTION}}`：`QAQ-Bx`
2. **Windows Toast / App Notifications 标识**：
   - `ToastActivatorCLSID`：`133f6e22-a3f9-483c-9839-0be521534e66`（与 `crates/qaq-bx-ui/src/notifications.rs` 中的 `TOAST_ACTIVATOR_CLSID` 单源同步）。
   - 在清单中声明 `<desktop:Extension Category="windows.toastNotificationActivation">` 与 `<com:Extension Category="windows.comServer">`（`Arguments="----AppNotificationActivated:"`）；打包态 AUMID 由系统自动派生为 `<PackageFamilyName>!App`，非打包态通过 `SetCurrentProcessExplicitAppUserModelID("QAQ.QAQ-Bx")` 绑定。

---

## 3. QAQ-Bx 专项适配要点

1. **`libmpv-2.dll` 随包分发**：
   - `qaq-bx-video` 通过 `LoadLibraryW("libmpv-2.dll")` 动态加载播放器引擎（优先搜索 exe 同级目录）。
   - 打包暂存时需将 `libmpv-2.dll` 复制到 `qaq-bx-ui.exe` 同级目录。
2. **可写数据目录重定向**：
   - MSIX/AppX 容器内安装目录只读，`qaq_bx_core::resolve_data_dir()` 通过 `GetCurrentPackageFullName` 检测到打包运行态时，自动切至 `%LOCALAPPDATA%\QAQ-Bx\data`。
3. **图标资源（`Assets\` + `store\` + EXEs）**：
   - AppX 清单磁贴与图标：`packaging/appx/Assets/` 包含 `StoreLogo.png`（50×50）、`Square44x44Logo.png`（44×44）、`Square71x71Logo.png`（71×71）、`Square150x150Logo.png`（150×150）、`Wide310x150Logo.png`（310×150）及多分辨率 `QAQ-Bx.ico`（同时通过 `build.rs` 内嵌进 `qaq-bx-ui.exe` 资源 ID 1，标题栏另嵌入 48×48 PNG）。
   - 微软商店列表素材：`packaging/store/QAQ-Bx-1024.png`（1024×1024）与 `packaging/store/QAQ-Bx-300.png`（300×300）。

---

## 4. 打包与签名流程

```powershell
# 1. 本地调试包（debug + 自签名，自动递增 revision）
just dev

# 2. 微软商店上架包（无签名 Release .appx，填入 Partner Center 产品标识与非零版本）
just store pkg="YourPartnerName.QAQ-Bx" pub="CN=XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX" pub_name="YourPublisher" ver="0.1.0"

# 3. 本地 Release 自签名侧载包
just release-sign 0.1.0
```
