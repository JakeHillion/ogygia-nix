//! Evaluate a flake attribute to JSON, like
//! `nix eval --json <flake>#<attr> [--apply <fn>]`.
//!
//! Usage: `flake_eval [--no-store] [--cache <dir>] <flake> <attr>... [--apply <fn>]`;
//! each attribute is evaluated in turn within one session. `--no-store`
//! fetches every input instead of using the local Nix store.

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take_value = |flag: &str| -> Option<String> {
        let i = args.iter().position(|a| a == flag)?;
        let v = args.remove(i + 1);
        args.remove(i);
        Some(v)
    };
    let apply = take_value("--apply");
    let cache = take_value("--cache");
    let mut options = ogygia_nix_eval::FlakeOptions {
        fetch_cache: cache.map(Into::into),
        ..Default::default()
    };
    if let Some(i) = args.iter().position(|a| a == "--no-store") {
        args.remove(i);
        options.ignore_nix_store = true;
    }
    let flake = args.remove(0);
    ogygia_nix_eval::with_flake_options(&flake, &options, |session| -> anyhow::Result<()> {
        for attr in &args {
            let start = std::time::Instant::now();
            let v = session.eval_json(attr, apply.as_deref())?;
            println!("{v}");
            eprintln!("{attr}: {:?}", start.elapsed());
        }
        Ok(())
    })?
}
