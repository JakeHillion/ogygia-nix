//! Evaluate an expression and print it like `nix-instantiate --eval --strict`.
//!
//! Usage: `eval [-I prefix=path]... [--dump-drvs <dir>] <expr>`
//!
//! `--dump-drvs` writes every derivation the evaluation created to `<dir>`,
//! for diffing against the `.drv` files Nix writes for the same expression.

use ogygia_nix_eval::Context;
use ogygia_nix_eval::Eval;
use ogygia_nix_eval::Io;
use ogygia_nix_eval::Settings;

fn main() {
    let mut settings = Settings::default();
    let mut args = std::env::args().skip(1);
    let mut expr = None;
    let mut dump = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-I" => {
                let entry = args.next().expect("-I needs an argument");
                let (prefix, path) = entry.split_once('=').unwrap_or(("", &entry));
                settings.nix_path.push((prefix.to_owned(), path.to_owned()));
            }
            "--dump-drvs" => dump = Some(args.next().expect("--dump-drvs needs a directory")),
            _ => expr = Some(arg),
        }
    }
    let expr = expr.expect("usage: eval [-I prefix=path]... [--dump-drvs <dir>] <expr>");
    let cwd = std::env::current_dir().expect("current directory");
    let result = ogygia_nix_eval::run_with_stack(|| {
        let ctx = Context::new(Io::default());
        let bump = bumpalo::Bump::new();
        let ev = Eval::new(&ctx, &bump, settings);
        let result = ev
            .eval_string(&expr, &cwd.to_string_lossy())
            .and_then(|v| ogygia_nix_eval::print_strict(&ev, v))
            .map(|out| String::from_utf8_lossy(&out).into_owned())
            .map_err(|e| e.to_string());
        if let Some(dir) = dump {
            ev.dump_derivations(std::path::Path::new(&dir))
                .expect("writing derivations");
        }
        result
    });
    match result {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
