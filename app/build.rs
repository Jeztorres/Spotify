use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads/main");
    println!("cargo:rerun-if-changed=../.git/packed-refs");
    println!("cargo:rerun-if-env-changed=GITHUB_RUN_NUMBER");

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into());
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "sin-git".into());

    let build = match std::env::var("GITHUB_RUN_NUMBER") {
        Ok(num) if !num.trim().is_empty() => format!("build #{}", num.trim()),
        _ => "compilación local".into(),
    };

    let info = format!("v{version} · {commit} · {build}");
    println!("cargo:rustc-env=APP_BUILD_INFO={info}");
}
