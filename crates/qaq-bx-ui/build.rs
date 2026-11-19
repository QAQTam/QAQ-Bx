use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    windows_reactor_setup::as_self_contained();

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    embed_app_icon(&out_dir);

    let bindings = format!("{}/winrt_bindings.rs", out_dir.display());
    windows_bindgen::builder()
        .input("D:/Projects/qaq-winui-reactor/crates/tools/reactor-metadata/winmd")
        .input_default()
        .output(bindings)
        .flat()
        .minimal()
        .filters([
            "Microsoft.UI.WindowId",
            "Microsoft.UI.Windowing.AppWindow.GetFromWindowId",
            "Microsoft.UI.Windowing.AppWindow.Presenter",
            "Microsoft.UI.Windowing.AppWindow.SetPresenterByKind",
            // 竖屏模式(实验):贴边最大化需要窗口几何手术与工作区查询。
            "Microsoft.UI.Windowing.AppWindow.MoveAndResize",
            "Microsoft.UI.Windowing.AppWindow.Position",
            "Microsoft.UI.Windowing.AppWindow.Size",
            "Microsoft.UI.Windowing.IAppWindow2.ClientSize",
            "Microsoft.UI.Windowing.DisplayArea.GetFromWindowId",
            "Microsoft.UI.Windowing.DisplayArea.WorkArea",
            "Microsoft.UI.Windowing.DisplayAreaFallback",
            "Microsoft.UI.Windowing.OverlappedPresenter.IsMaximizable",
            "Windows.Graphics.RectInt32",
            "Windows.Graphics.SizeInt32",
            "Windows.Graphics.PointInt32",
            "Microsoft.UI.Windowing.AppWindowPresenter.Kind",
            "Microsoft.UI.Windowing.AppWindowPresenterKind",
            "Microsoft.UI.Xaml.Application.Current",
            "Microsoft.UI.Xaml.Application.Resources",
            "Microsoft.UI.Xaml.ResourceDictionary",
            "Microsoft.UI.Xaml.Media.FontFamily",
            "Microsoft.UI.Xaml.Media.IFontFamilyFactory",
            "Windows.Foundation.PropertyValue.CreateString",
            "Microsoft.UI.Xaml.Controls.ScrollViewer.ChangeView",
            "Microsoft.UI.Xaml.Controls.ScrollViewer.VerticalOffset",
            "Microsoft.UI.Xaml.Controls.ScrollViewer.ViewportHeight",
            "Microsoft.UI.Xaml.Controls.TextBox",
            "Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent",
            "Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement",
            "Microsoft.UI.Xaml.UIElement.XamlRoot",
            "Microsoft.UI.Xaml.DependencyObject",
            // xamlroot-probe(诊断构建,--probe-xamlroot):可视树遍历 + 标题栏属性驱动
            "Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount",
            "Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild",
            "Microsoft.UI.Xaml.Controls.TitleBar.Title",
            "Microsoft.UI.Xaml.Controls.Panel.Children",
            "Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread",
            "Microsoft.UI.Dispatching.DispatcherQueue.CreateTimer",
            "Microsoft.UI.Dispatching.DispatcherQueueTimer",
            "Microsoft.UI.Dispatching.DispatcherQueueTimer.Interval",
            "Microsoft.UI.Dispatching.DispatcherQueueTimer.Tick",
            "Microsoft.UI.Dispatching.DispatcherQueueTimer.Start",
            "Microsoft.Windows.AppNotifications.AppNotificationManager.Default",
            "Microsoft.Windows.AppNotifications.AppNotificationManager.Register",
            "Microsoft.Windows.AppNotifications.AppNotificationManager.Unregister",
            "Microsoft.Windows.AppNotifications.AppNotificationManager.NotificationInvoked",
            "Microsoft.Windows.AppNotifications.AppNotificationActivatedEventArgs",
            // 液态玻璃(iOS 26 风格)路线 0:stock composition effect graph
            // (Compositor 效果刷 + 圆角 ShapeVisual;经 fork imperative
            // SetCompositionChildVisual 垫到底栏内容网格之下)。
            "Microsoft.UI.Composition.Compositor.CreateBackdropBrush",
            "Microsoft.UI.Composition.Compositor.CreateColorBrush",
            "Microsoft.UI.Composition.Compositor.CreateSpriteVisual",
            "Microsoft.UI.Composition.Compositor.CreateContainerVisual",
            "Microsoft.UI.Composition.ContainerVisual.Children",
            "Microsoft.UI.Composition.IVisualCollection.InsertAtBottom",
            "Microsoft.UI.Composition.Compositor.CreateGeometricClip",
            "Microsoft.UI.Composition.CompositionGeometricClip.Geometry",
            "Microsoft.UI.Composition.Visual.Clip",
            "Microsoft.UI.Composition.CompositionColorBrush",
            "Microsoft.UI.Composition.CompositionColorBrush.Color",
            "Microsoft.UI.Composition.SpriteVisual.Brush",
            "Microsoft.UI.Composition.CompositionEffectSourceParameter",
            "Microsoft.UI.Composition.ICompositionEffectSourceParameterFactory",
            "Windows.Foundation.PropertyValue.CreateSingle",
            "Windows.Foundation.PropertyValue.CreateUInt32",
            "Microsoft.UI.Composition.Compositor.CreateEffectFactory",
            "Microsoft.UI.Composition.Compositor.CreateShapeVisual",
            "Microsoft.UI.Composition.Compositor.CreateSpriteShape",
            "Microsoft.UI.Composition.Compositor.CreateRoundedRectangleGeometry",
            "Microsoft.UI.Composition.CompositionEffectFactory.CreateBrush",
            "Microsoft.UI.Composition.CompositionEffectBrush.SetSourceParameter",
            "Microsoft.UI.Composition.CompositionPropertySet.InsertScalar",
            "Microsoft.UI.Composition.CompositionPropertySet.InsertColor",
            "Microsoft.UI.Composition.Visual.Size",
            "Microsoft.UI.Composition.ShapeVisual.Shapes",
            "Microsoft.UI.Composition.CompositionSpriteShape.Geometry",
            "Microsoft.UI.Composition.CompositionSpriteShape.FillBrush",
            "Microsoft.UI.Composition.CompositionRoundedRectangleGeometry.Size",
            "Microsoft.UI.Composition.CompositionRoundedRectangleGeometry.CornerRadius",
            // 注:Microsoft.UI.Composition.Effects 效果结构体(GaussianBlur 等)
            // 不在 fork curated winmd 中(阶段 2 模糊需手工实现 IGraphicsEffect,
            // 见 widgets/glass.rs 文档)。
        ])
        .write();
}

fn embed_app_icon(out_dir: &Path) {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest_dir.join("../../packaging/appx/Assets/QAQ-Bx.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    let Ok(ico_abs) = std::fs::canonicalize(&ico) else {
        return;
    };
    let Some(rc_exe) = find_rc_exe() else {
        println!("cargo:warning=未找到 Windows SDK rc.exe，跳过内嵌应用图标资源");
        return;
    };

    // Strip Win32 verbatim `\\?\` prefix so rc.exe parses the path cleanly.
    let ico_clean = ico_abs
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    let rc_path = out_dir.join("qaq_bx_icon.rc");
    let res_path = out_dir.join("qaq_bx_icon.res");
    std::fs::write(&rc_path, format!("1 ICON \"{ico_clean}\"\n")).expect("write qaq_bx_icon.rc");

    let status = Command::new(rc_exe)
        .args([
            "/nologo",
            "/fo",
            &res_path.to_string_lossy(),
            &rc_path.to_string_lossy(),
        ])
        .status()
        .expect("invoke rc.exe");
    assert!(status.success(), "rc.exe failed with {status}");
    println!("cargo:rustc-link-arg-bins={}", res_path.display());
}

fn find_rc_exe() -> Option<PathBuf> {
    if let Ok(output) = Command::new("where.exe").arg("rc.exe").output()
        && output.status.success()
        && let Some(first) = String::from_utf8_lossy(&output.stdout).lines().next()
    {
        let p = PathBuf::from(first.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    let arch_dir = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86") => "x86",
        _ => "x64",
    };
    let kits = PathBuf::from(
        std::env::var_os("ProgramFiles(x86)").unwrap_or_else(|| r"C:\Program Files (x86)".into()),
    )
    .join(r"Windows Kits\10\bin");
    let entries = std::fs::read_dir(kits).ok()?;
    let mut versions: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .map(|p| p.join(arch_dir).join("rc.exe"))
        .filter(|p| p.is_file())
        .collect();
    versions.sort();
    versions.pop()
}
