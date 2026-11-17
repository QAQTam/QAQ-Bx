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
            "Microsoft.UI.Xaml.Application.Current",
            "Microsoft.UI.Xaml.Application.Resources",
            "Microsoft.UI.Xaml.ResourceDictionary",
            "Microsoft.UI.Xaml.Media.FontFamily",
            "Microsoft.UI.Xaml.Media.IFontFamilyFactory",
            "Windows.Foundation.PropertyValue.CreateString",
            "Microsoft.UI.Xaml.Controls.ScrollViewer.ChangeView",
            "Microsoft.UI.Xaml.Controls.ScrollViewer.VerticalOffset",
            "Microsoft.UI.Xaml.Controls.ScrollViewer.ViewportHeight",
            "Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent",
            "Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement",
            "Microsoft.UI.Xaml.UIElement.XamlRoot",
            "Microsoft.UI.Xaml.DependencyObject",
            "Microsoft.Windows.AppNotifications.AppNotificationManager.Default",
            "Microsoft.Windows.AppNotifications.AppNotificationManager.Register",
            "Microsoft.Windows.AppNotifications.AppNotificationManager.Unregister",
            "Microsoft.Windows.AppNotifications.AppNotificationManager.NotificationInvoked",
            "Microsoft.Windows.AppNotifications.AppNotificationActivatedEventArgs",
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
    std::fs::write(&rc_path, format!("1 ICON \"{ico_clean}\"\n"))
        .expect("write qaq_bx_icon.rc");

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
        std::env::var_os("ProgramFiles(x86)")
            .unwrap_or_else(|| r"C:\Program Files (x86)".into()),
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
