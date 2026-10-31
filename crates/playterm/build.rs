//! Embeds the application icon and version information into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/playterm.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/playterm.ico")
        .set("ProductName", "playterm")
        .set("FileDescription", "playterm - Telnet/SSH/Serial terminal")
        .set("OriginalFilename", "playterm.exe")
        .set("LegalCopyright", "MIT License");
    if let Err(e) = res.compile() {
        // A missing resource compiler only costs the exe icon; do not fail the build.
        println!("cargo:warning=Windows resources not embedded: {e}");
    }
}
