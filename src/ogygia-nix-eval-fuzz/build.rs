//! libFuzzer links the C++ standard library dynamically. Where it lives
//! outside the default library path, as in Nix, record its directory as a
//! runpath so the fuzzer starts without `LD_LIBRARY_PATH`.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CXX");
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_owned());
    let Ok(out) = Command::new(cxx)
        .arg("-print-file-name=libstdc++.so")
        .output()
    else {
        return;
    };
    let file = String::from_utf8_lossy(&out.stdout);
    // The compiler echoes the bare name back when it does not know the path.
    if let Some(dir) = Path::new(file.trim()).parent().filter(|d| d.is_absolute()) {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{}", dir.display());
    }
}
