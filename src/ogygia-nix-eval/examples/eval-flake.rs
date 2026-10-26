//! Evaluate an installable and print it like `nix eval --json
//! --no-update-lock-file`.
//!
//! Usage: `eval-flake [--apply <expr>] [--store <dir>] <installable>`

use std::path::PathBuf;

use ogygia_nix_eval::FlakeEnv;
use ogygia_nix_eval::Settings;

fn main() {
    let mut apply = "x: x".to_owned();
    let mut store = PathBuf::from("/nix/store");
    let mut installable = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = args.next().expect("--apply needs an argument"),
            "--store" => store = args.next().expect("--store needs an argument").into(),
            _ => installable = Some(arg),
        }
    }
    let installable =
        installable.expect("usage: eval-flake [--apply <expr>] [--store <dir>] <installable>");
    let env = FlakeEnv {
        cwd: std::env::current_dir().expect("current directory"),
        store,
    };
    match ogygia_nix_eval::eval_flake_json(&installable, &apply, &env, Settings::default()) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
