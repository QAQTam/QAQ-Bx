# crash-sym — 事件 ID 1000 崩溃偏移符号化(无 SDK 依赖)

Windows 事件查看器 `Application Error`(Event ID 1000)给出:

```
出错模块名称: <ModuleName>
错误偏移:     0x<RVA 十六进制>      ← 即模块内 RVA,无需 ASLR 换算
出错模块路径: <完整路径>
```

本目录的 `symbolize.py` 从该模块解析 CodeView 调试目录(PDB GUID+age),
必要时从微软符号服务器(msdl)下载对应 PDB(自断点续传),然后在 publics
符号流中定位包含该 RVA 的函数,并打印故障点前后的符号与可选反汇编。

## 用法

```powershell
python scripts/crash-sym/symbolize.py "C:\Program Files\WindowsApps\<包名>\<Module>.dll" 359595
python scripts/crash-sym/symbolize.py <dll> 359595 --disasm     # 反汇编故障点(需 capstone)
```

- PDB 缓存在 `%LOCALAPPDATA%\qaq-bx\crash-sym\`,可用 `--cache` 覆盖。
- 依赖:仅 Python 3 标准库 + `curl`(--disasm 需要 `pip install capstone`)。
- 原理:纯 Python 解析 MSF 7.00(PDB)流目录 → DBI → publics 记录链
  (S_PUB32,本仓库适配的布局为 flags(4)+off(4)+sect(2)+name),全程无 SDK。

## 配套:崩溃转储捕获(HKLM,已实测生效,2026-11-18 23:00)

**HKCU LocalDumps 对打包应用无效**(三次真实崩溃均 `EtwNonCollectReason=1`、
零 dump)。官方文档确认 LocalDumps 只读 HKLM 且需管理员。已配置:

```powershell
# HKLM\...\Windows Error Reporting\LocalDumps\qaq-bx-ui.exe
#   DumpFolder = D:\QAQ-Bx\data\crash-dumps  DumpType = 2(full)  DumpCount = 10
# 写入脚本(需 UAC):setup-hklm-localdumps.ps1
# 目录 ACL(打包进程 AppContainer 落盘必需,用户级即可):
#   icacls D:\QAQ-Bx\data\crash-dumps /grant *S-1-15-2-2:(OI)(CI)M
```

链路已用 `capture_gate.rs` 受控自崩实测打通(0.2.0.13):dump 命名
`qaq-bx-ui.exe.<pid>.dmp`;时间戳与包内 `ui-trace.log`
(`%LOCALAPPDATA%\Packages\QAQ-Bx-Dev_ptzv47e0cg1m0\LocalCache\Local\QAQ-Bx\data\`)
配对即可还原现场。拿到 dump 先粗读(无依赖):

```powershell
python scripts/crash-sym/minidump-info.py data/crash-dumps/qaq-bx-ui.exe.<pid>.dmp
```

打印异常码/ExceptionAddress/故障模块+RVA。**坑**:线程 CONTEXT 的 Rip 可能是
WerFault 交接点(实测在 ntdll),以 ExceptionStream 的 ExceptionAddress 为准。
故障 RVA 可直接喂 symbolize.py(本地 PDB 用 `--cache target/debug`)。再用 WinDbg/cdb 深挖:

```
!sym noisy; .reload; !analyze -v   (符号: srv*D:\qaqh-sym*https://msdl.microsoft.com/download/symbols)
```

## 已知案例

- `Microsoft.UI.Xaml.Controls.dll`(WinAppSDK 2.6)RVA `0x359595`
  → `?XamlRoot@?$consume_Microsoft_UI_Xaml_IUIElement@VTitleBar@@@impl@winrt@@QEBA@XZ`
  (ICF 折叠,同址含 ScrollView/SwipeControl/PipsPager 等所有 UIElement 派生控件
  的 `consume_IUIElement<D>::XamlRoot`)。详见 `docs/dev-notes/crash-0x359595-xamlroot.md`。
