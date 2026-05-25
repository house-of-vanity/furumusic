fn main() {
    println!(
        "cargo::rustc-env=FURU_TARGET={}",
        std::env::var("TARGET").unwrap()
    );

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let output = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .expect("failed to run rustc --version");
    let version = String::from_utf8_lossy(&output.stdout);
    println!("cargo::rustc-env=FURU_RUSTC_VERSION={}", version.trim());
}
