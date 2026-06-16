fn main() {
    // CI pins ORBIT_VERSION so the binary's --version matches the release tag
    // exactly. For local/dev builds, derive it from Cargo.toml's major.minor
    // (the single source of truth) plus the build date.
    println!("cargo:rerun-if-env-changed=ORBIT_VERSION");
    let version = std::env::var("ORBIT_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            let base = std::env::var("CARGO_PKG_VERSION").unwrap();
            let major_minor = base.split('.').take(2).collect::<Vec<_>>().join(".");
            let date = chrono::Local::now().format("%y%m%d");
            format!("{major_minor}.{date}-dev")
        });
    println!("cargo:rustc-env=ORBIT_VERSION={version}");
}
