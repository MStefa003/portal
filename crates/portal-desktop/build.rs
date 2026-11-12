//! Builds the program icon (and name) into `portal-desktop.exe` on Windows,
//! where Explorer, the Start menu and the taskbar show it.

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/icon/app.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("../../packaging/icon/app.ico")
            .set("ProductName", "Portal")
            .set("FileDescription", "Portal");
        // A missing resource compiler only costs the icon, not the build.
        if let Err(e) = resource.compile() {
            println!("cargo:warning=could not add the icon to portal-desktop.exe: {e}");
        }
    }
}
