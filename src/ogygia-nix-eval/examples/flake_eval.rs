//! Evaluate a flake attribute to JSON, like
//! `nix eval --json <flake>#<attr> [--apply <fn>]`.
//!
//! Usage: `flake_eval <flake> <attr>... [--apply <fn>]`; each attribute is
//! evaluated in turn within one session.

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let apply = match args.iter().position(|a| a == "--apply") {
        Some(i) => {
            let f = args.remove(i + 1);
            args.remove(i);
            Some(f)
        }
        None => None,
    };
    let flake = args.remove(0);
    ogygia_nix_eval::with_flake(&flake, |session| -> anyhow::Result<()> {
        for attr in &args {
            let start = std::time::Instant::now();
            let v = session.eval_json(attr, apply.as_deref())?;
            println!("{v}");
            eprintln!("{attr}: {:?}", start.elapsed());
        }
        Ok(())
    })?
}
