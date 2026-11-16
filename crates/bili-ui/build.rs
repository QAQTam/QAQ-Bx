fn main() {
    windows_reactor_setup::as_self_contained();

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let bindings = format!("{out_dir}/winrt_bindings.rs");
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
        ])
        .write();
}
