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

    embed_engine();

    let info = format!("v{version} · {commit} · {build}");
    println!("cargo:rustc-env=APP_BUILD_INFO={info}");
    println!("cargo:rustc-env=APP_COMMIT_HASH={commit}");
}

/// Incrusta el motor (`spotifymac-engine`) dentro del ejecutable de la app, para que funcione
/// aunque se mueva solo el `.exe` o se abra desde un acceso directo en otra carpeta.
/// Origen: `SPOTIFYMAC_ENGINE_EMBED` o `../Engine/target/release` (lo que compila el CI antes que la app).
/// Si no hay motor compilado, la app sigue buscándolo junto al ejecutable como antes.
fn embed_engine() {
    use std::path::PathBuf;
    println!("cargo::rustc-check-cfg=cfg(embedded_engine)");
    println!("cargo:rerun-if-env-changed=SPOTIFYMAC_ENGINE_EMBED");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").map(|t| t == "windows").unwrap_or(false);
    let name = if windows { "spotifymac-engine.exe" } else { "spotifymac-engine" };
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let default_path = manifest.join("../Engine/target/release").join(name);
    let explicit = std::env::var_os("SPOTIFYMAC_ENGINE_EMBED").filter(|v| !v.is_empty());
    let required = explicit.is_some();
    let source = explicit.map(PathBuf::from).unwrap_or(default_path);
    println!("cargo:rerun-if-changed={}", source.display());

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("engine.bin");
    let bytes = std::fs::read(&source).ok().filter(|b| !b.is_empty());
    let wanted = bytes.clone().unwrap_or_default();
    // Solo se reescribe si cambia: así no se recompila la app en cada `cargo build`.
    if std::fs::read(&out).ok().as_deref() != Some(wanted.as_slice()) {
        std::fs::write(&out, &wanted).expect("no se pudo escribir engine.bin");
    }
    if bytes.is_some() {
        println!("cargo:rustc-cfg=embedded_engine");
    } else if required {
        // En el CI se pide explícitamente: mejor fallar que publicar un .exe que no arranca solo.
        panic!("SPOTIFYMAC_ENGINE_EMBED apunta a {} pero no existe o está vacío", source.display());
    } else {
        println!("cargo:warning=Motor no incrustado: compila Engine (cargo build --release) antes que la app para un .exe autónomo.");
    }
}
