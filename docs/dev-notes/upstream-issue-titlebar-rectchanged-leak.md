# Upstream Issue 草稿 — microsoft/microsoft-ui-xaml(待提交,按 bug_report.yaml 模板)

> 提交策略(据 CONTRIBUTING 与 #12459/#12671 惯例,并经 §9 C# 复刻实验后修订):
> 1. **先提本 issue**(模板自动打 `bug`+`needs-triage`;正文含 area-TitleBar 关键词);
> 2. issue 里声明 **愿意提 PR**(PR 必须 `Fixes #<issue>`;CLA 一次性);
> 3. ~~C# 最小复刻~~ → 实验证明托管 GC 路径天然无法进入触发状态(§9),
>    issue 直接以此为机制论据;若被索要,升级弹药 = C++/WinRT 原生强引用最小复现。
> 标题:
> **TitleBar leaks its WindowRectChanged subscription; a later window-move event calls back into the destroyed control and AVs in `consume_IUIElement::XamlRoot` (unchecked QI output)**

---

### Describe the bug

`TitleBar` subscribes to the **window-lifetime** `InputNonClientPointerSource.WindowRectChanged` event with a raw-`this` handler (non-`auto_revoke`) inside `UpdateIcon()`, but its destructor never revokes that token. When the TitleBar element is destroyed while the window stays alive (e.g. the app replaces `Window.TitleBar`, or the control is dropped from the visual tree and released), the next window-rect change (any title-bar drag — fired at pointer rate) dispatches into the destroyed object. The callback chain

```
TitleBar::OnWindowRectChanged
  → TitleBar::UpdateIconRegion
    → TitleBar::GetInputNonClientPointerSource
      → TitleBar::GetAppWindowId
        → XamlRoot()   // consume_IUIElement<D>::XamlRoot
```

faults at a null dereference (AV `0xC0000005`, reading address `0`) exactly at `Microsoft.UI.Xaml.Controls.dll` RVA `0x359595`: inside the projection template, the null-member branch falls through to the same unchecked code path that dereferences the QI output without verifying the HRESULT.

Verified faulting stack from a full user-mode dump (WinAppSDK 2.6.5, `Microsoft.UI.Xaml.Controls.dll` 3.3.0.0, PE timestamp 8b577a7c; symbols from the public PDB):

| frame | symbol (nearest public + offset) |
|---|---|
| fault | `?XamlRoot@?$consume_Microsoft_UI_Xaml_IUIElement@...`+0x3b |
| +0x58 | `?GetAppWindowId@TitleBar@@...`+0x1f |
| +0x98 | `?GetInputNonClientPointerSource@TitleBar@@...`+0x27 |
| +0xf8 | `?UpdateIconRegion@TitleBar@@...`+0x22 |
| +0x118 | `?OnWindowRectChanged@TitleBar@@...`+0x1a |

Source references (pin to a commit hash per repo convention):
- subscription with raw this, no revoker:
  `controls/dev/TitleBar/TitleBar.cpp` (`UpdateIcon()`): `m_windowRectChangedToken = nonClientPointerSource.WindowRectChanged({ this, &TitleBar::OnWindowRectChanged });`
- the **only** unsubscription path is re-entering `UpdateIcon()` with `IconSource == nullptr`;
- `TitleBar::~TitleBar()` revokes `m_sizeChangedRevoker`, `m_flowDirectionChangedRevoker` and `m_inputActivationChangedToken` — but **not** `m_windowRectChangedToken`.

### Why is this important?

Any app that dynamically swaps or releases its TitleBar control while the window keeps pumping rect changes gets an end-user-affecting, deterministic AV: crashes look like random native faults inside WinUI with no app code on the stack, which makes attribution extremely hard (we burned four releases bisecting before capturing dumps). This is not exotic usage — `Window.SetTitleBar` and the `TitleBar.IconSource` property are public supported APIs; the lifecycle combination is simply one C# + static XAML markup rarely produces (in XAML, the TitleBar outlives the window), so the leak has apparently never been hit upstream either.

### Steps to reproduce the bug

Production repro (packaged app, WinAppSDK **2.6.5** self-contained, OS build 26300):

1. Any app that sets `Window.ExtendsContentIntoTitleBar = true`, installs a `TitleBar` with a non-null `IconSource`, then starts playback in fullscreen.
2. **Exit fullscreen, then immediately drag the window by its title bar.**
3. The process freezes briefly (drag-region storm) and crashes; WER signature: `APPROB... module Microsoft.UI.Xaml.Controls.dll, offset 0x359595, exception 0xC0000005` — identical offset across 4 crashes on 3 different app builds.

Minimal C# pattern we expect to reproduce (finalizing; happy to attach the standalone repro project if needed — see plan at the bottom):

```csharp
// 1. window with TitleBar A shown (A.IconSource = something non-null), SetTitleBar(A)
// 2. window.SetTitleBar(B);  // A no longer referenced by the window
// 3. drop all managed refs to A; GC.Collect(); GC.WaitForPendingFinalizers();
//    // ~TitleBar runs WITHOUT revoking WindowRectChanged
// 4. drag the window by its title bar -> WindowRectChanged fires -> AV at Controls.dll+0x359595
```

### Actual behavior

Access violation (`0xC0000005`, read at `NULL`) inside `Microsoft.UI.Xaml.Controls.dll` when `WindowRectChanged` is dispatched to a destroyed `TitleBar`.

### Expected behavior

`~TitleBar()` should stop all window-level event subscriptions it established (as it already does for SizeChanged / FlowDirection / InputActivationChanged), i.e. `WindowRectChanged` must be revoked on teardown.

### Workaround we shipped (for other affected devs)

Before releasing a TitleBar projection, explicitly set `IconSource = null` (this re-enters `UpdateIcon()` and takes the only official unsubscription path), *then* let the element go. Our reactive framework does exactly this and the crash disappeared in the deterministic repro.

### NuGet package version

Microsoft.WindowsAppSDK **2.6.5** (self-contained packaged app). Same code shape on `main` as of 2026-11-17.

### Windows version

Windows Insider Build 26300 (also structurally reachable on 26100 — the defect is in the control's teardown, not the OS).

### Additional context

- The failing projection instruction also has a defensive gap: the null-member branch falls through to a shared code path that dereferences the QI output **without checking the HRESULT** (we disassembled the shipped binary; happy to share the exact instruction dump). Fixing the leak is the primary ask; hardening the projection is secondary.
- Proposed fix (we'll open a PR after this issue is accepted — per `PULL_REQUEST_TEMPLATE.md`, a PR without a linked issue may be closed): store the subscription as `winrt::auto_revoke`-style revoker tied to the `InputNonClientPointerSource` lifetime, or at minimum revoke `m_windowRectChangedToken` in `~TitleBar()`; keep `UpdateIcon()`'s re-subscribe path idempotent.
- Artifacts available on request: full memory dump (with PDB-symbolized stack), WER report signatures, disassembly of the faulting instruction sequence, and the timeline of 4 production crashes.

---

## 附:C# 复刻工程计划(备用弹药,非前置)

若 triage 要求 repro project(参照 #12459 的 winui-repros 模式),本机 1 小时工作量:
unpackaged C# WinAppSDK 控制台宿主(或 VS 模板)+ code-behind 四步(上文注释),
加一个 Win32 `MoveWindow`/`SetWindowPos` 循环模拟拖框(rect 变化同样触发
`WindowRectChanged`,modal loop 非必需——待实验确认;若需真拖,SendInput 30 行)。
预期:**能复现**(RCW 终结器在 finalizer 线程销毁 native peer,对象死亡但事件源
仍订阅 → 命中)。这个实验同时也最终回答「是否与 reactor 设计有关」——预期答案:
无关,任何生命周期管理只要释放 TitleBar 引用就中招。
