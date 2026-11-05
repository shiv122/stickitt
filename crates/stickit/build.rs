//! Windows: embeds the app icon and version details in the executable.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/stickitt.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/stickitt.ico")
        .set("ProductName", "Stick-itt")
        .set("FileDescription", "Stick-itt")
        .set("LegalCopyright", "MIT License");
    if let Err(e) = res.compile() {
        panic!("embedding the Windows icon failed: {e}");
    }
}
