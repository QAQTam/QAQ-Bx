# biliplay AppX / MSIX 打包设计（v0.0.0.0）

> 版本约定：Cargo workspace 版本 `0.0.0`（`Cargo.toml` `[workspace.package].version`），对应 `AppxManifest.xml` 四段式版本号 `0.0.0.0`。

## 1. `windows-reactor-setup` 与打包模式选型

`bili-ui` 基于 `windows-reactor`（WinUI 3 / Windows App SDK 2.x）。参考 `windows-reactor-setup` 的实现，AppX/MSIX 打包有两种模式：

### 模式 A：Self-Contained（自包含，推荐用于独立分发 / 侧载）
使用 `windows-reactor-setup` 在编译期将 Windows App SDK Runtime 与 WebView2 投影 DLL 部署到可执行文件同级目录，用户无需预先安装 Windows App Runtime 框架包。

1. **构建配置**（`crates/bili-ui/Cargo.toml` + `crates/bili-ui/build.rs`）：
   - `[build-dependencies]` 引入 `windows-reactor-setup = { path = "D:/Projects/qaq-winui-reactor/crates/libs/reactor-setup" }`。
   - `build.rs` 调用 `windows_reactor_setup::as_self_contained()`：
     - 从 NuGet 下载并缓存 `Microsoft.WindowsAppSDK.Runtime`（`2.6.4`）与 `Microsoft.Web.WebView2`（`1.0.4078.44`）到 `%LOCALAPPDATA%\windows-reactor-setup\temp\v1`。
     - 解压 `MSIX/win10-{arch}/Microsoft.WindowsAppRuntime.2.msix`，将 `assets/runtime.txt` 列出的运行时 DLL/子目录及 `Microsoft.Web.WebView2.Core.dll` 复制到 Cargo 输出目录（`target/release/`）。
     - 通过 `/MANIFEST:EMBED` + `/MANIFESTINPUT` 将 `assets/app.manifest`（含 `<description>windows-reactor-self-contained</description>` 与 WinRT SxS 注册表项）嵌入 `bili-ui.exe`。
2. **Manifest 差异**：自包含模式下，`AppxManifest.xml` **不需要**声明 `<PackageDependency Name="Microsoft.WindowsAppRuntime.2" ... />`。
3. **打包内容**：除 `bili-ui.exe` 外，必须完整保留 `target/release/` 下由 `windows-reactor-setup` 部署的所有运行时 DLL 与子目录（如 `Microsoft.WindowsAppRuntime.dll`、`Microsoft.ui.xaml.dll`、`Microsoft.Web.WebView2.Core.dll` 等）。

### 模式 B：Framework-Dependent（框架依赖，包体最小）
不引入 `windows-reactor-setup`，改为在 `AppxManifest.xml` 中声明系统级框架包依赖（适用于 Store 分发或目标机已安装 Windows App Runtime 2.x 的环境）：

```xml
<PackageDependency
  Name="Microsoft.WindowsAppRuntime.2"
  MinVersion="2.0.1.0"
  Publisher="CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US" />
```

---

## 2. `AppxManifest.xml` 模板（v0.0.0.0）

以**模式 A（自包含）**为例，放置在打包暂存目录根目录（若采用模式 B，则取消注释 `PackageDependency`）：

```xml
<?xml version="1.0" encoding="utf-8"?>
<Package
  xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
  xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
  xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
  IgnorableNamespaces="uap rescap">
  <Identity
    Name="Biliplay.Desktop"
    Publisher="CN=BiliplayDev"
    Version="0.0.0.0"
    ProcessorArchitecture="x64" />
  <Properties>
    <DisplayName>biliplay</DisplayName>
    <PublisherDisplayName>BiliplayDev</PublisherDisplayName>
    <Logo>Assets\StoreLogo.png</Logo>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily
      Name="Windows.Desktop"
      MinVersion="10.0.17763.0"
      MaxVersionTested="10.0.26100.0" />
    <!-- 模式 B（非自包含）时启用：
    <PackageDependency
      Name="Microsoft.WindowsAppRuntime.2"
      MinVersion="2.0.1.0"
      Publisher="CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US" />
    -->
  </Dependencies>
  <Resources>
    <Resource Language="zh-cn" />
  </Resources>
  <Applications>
    <Application
      Id="App"
      Executable="bili-ui.exe"
      EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements
        DisplayName="biliplay"
        Description="biliplay Desktop Client"
        BackgroundColor="transparent"
        Square150x150Logo="Assets\Square150x150Logo.png"
        Square44x44Logo="Assets\Square44x44Logo.png" />
    </Application>
  </Applications>
  <Capabilities>
    <Capability Name="internetClient" />
    <rescap:Capability Name="runFullTrust" />
  </Capabilities>
</Package>
```

---

## 3. biliplay 专项适配要点

1. **`libmpv-2.dll` 随包分发**：
   - `bili-video` 通过 `LoadLibraryW("libmpv-2.dll")` 动态加载播放器引擎（优先搜索 exe 同级目录）。
   - 打包暂存时需将 `libmpv-2.dll` 复制到 `bili-ui.exe` 同级目录。
2. **可写数据目录重定向（关键）**：
   - 当前 `bili_core::resolve_data_dir()` 默认回退到 `<cwd>/data`（用于存放登录凭据、ticket 与设置）。
   - 在 MSIX/AppX 容器内，安装目录（`WindowsApps\...`）为**只读**。正式出包前需在 `resolve_data_dir()`（或 `bili-ui` 启动入口）检测打包运行态（如 `GetCurrentPackageFullName`），将默认数据目录切至 `%LOCALAPPDATA%\biliplay\data`（或 `ApplicationData.Current.LocalFolder`）。
3. **图标资源（`Assets\`）**：
   - 需准备 `StoreLogo.png`（50×50）、`Square44x44Logo.png`（44×44）、`Square150x150Logo.png`（150×150）。

---

## 4. 打包与签名流程

```powershell
# 1. Release 编译（启用 windows-reactor-setup 时会自动部署 WinAppSDK 运行时）
cargo build --release -p bili-ui

# 2. 组装暂存目录 target\appx-staging\
#    - target\release\bili-ui.exe
#    - target\release\*.dll 及运行时子目录（由 windows-reactor-setup 生成）
#    - libmpv-2.dll
#    - AppxManifest.xml（Version="0.0.0.0"）
#    - Assets\*.png

# 3. 使用 Windows SDK MakeAppx 打包
makeappx.exe pack /o /d target\appx-staging /p target\biliplay_0.0.0.0_x64.msix

# 4. 自签名测试证书签名（Publisher 需与 AppxManifest.xml <Identity Publisher="..." /> 一致）
signtool.exe sign /fd SHA256 /a /f biliplay-dev.pfx /p <password> target\biliplay_0.0.0.0_x64.msix
```
