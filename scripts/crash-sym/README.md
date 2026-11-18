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

## 配套:崩溃转储捕获(已配置,2026-11-18)

WER LocalDumps(HKCU,用户级,无需管理员)已为 `qaq-bx-ui.exe` 开启:

```powershell
reg query "HKCU\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\qaq-bx-ui.exe"
# DumpFolder = D:\QAQ-Bx\data\crash-dumps   DumpType = 2(full)   DumpCount = 10
```

下一次任何原生崩溃都会在此目录留下完整 .dmp;时间戳与 `data/ui-trace.log`
(页面切换轨迹)配对即可还原现场。拿到 dump 后用 WinDbg/cdb 打开:

```
!sym noisy; .reload; !analyze -v   (符号: srv*D:\qaqh-sym*https://msdl.microsoft.com/download/symbols)
```

## 已知案例

- `Microsoft.UI.Xaml.Controls.dll`(WinAppSDK 2.6)RVA `0x359595`
  → `?XamlRoot@?$consume_Microsoft_UI_Xaml_IUIElement@VTitleBar@@@impl@winrt@@QEBA@XZ`
  (ICF 折叠,同址含 ScrollView/SwipeControl/PipsPager 等所有 UIElement 派生控件
  的 `consume_IUIElement<D>::XamlRoot`)。详见 `docs/dev-notes/crash-0x359595-xamlroot.md`。
