//! Embed the app icon and version info in valolights.exe (Windows).
fn main() {
    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(&icon.to_string_lossy());
        res.set("ProductName", "ValoLights");
        res.set("FileDescription", "ValoLights - Valorant highlight editor");
        res.set("CompanyName", "ValoLights");
        if let Err(e) = res.compile() {
            // no Windows SDK resource compiler: build without an icon rather than failing
            println!("cargo:warning=exe icon not embedded: {e}");
        }
    }
}
