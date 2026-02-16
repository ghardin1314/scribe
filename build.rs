fn main() {
    // Weak-link ScreenCaptureKit so macOS 26+ symbols don't crash on older OS
    println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");

    // Add Xcode toolchain's Swift runtime to rpath
    let swift_bin = String::from_utf8(
        std::process::Command::new("xcrun")
            .args(["--toolchain", "default", "--find", "swift"])
            .output()
            .expect("xcrun failed")
            .stdout,
    )
    .expect("invalid utf8")
    .trim()
    .to_string();

    // swift binary is at .../usr/bin/swift, we need .../usr/lib/swift/macosx
    if let Some(usr_dir) = std::path::Path::new(&swift_bin)
        .parent() // bin
        .and_then(|p| p.parent()) // usr
    {
        // Prefer swift-5.5 backcompat path (contains libswift_Concurrency.dylib on disk)
        let swift55_lib = usr_dir.join("lib").join("swift-5.5").join("macosx");
        if swift55_lib.exists() {
            println!(
                "cargo:rustc-link-arg=-Wl,-rpath,{}",
                swift55_lib.display()
            );
        }
    }
}
