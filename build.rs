fn main() {
    let now = chrono::Local::now();
    let date = now.format("%y%m%d").to_string();
    println!("cargo:rustc-env=ORBIT_VERSION=0.1.{}", date);
    println!("cargo:rustc-env=ORBIT_BUILD_DATE={}", date);
}
