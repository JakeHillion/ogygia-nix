//! Evaluate an expression and print it like `nix-instantiate --eval --strict`.
//!
//! Usage: `eval [-I prefix=path]... <expr>`

use ogygia_nix_eval::Settings;

fn main() {
    let mut settings = Settings::default();
    let mut args = std::env::args().skip(1);
    let mut expr = None;
    while let Some(arg) = args.next() {
        if arg == "-I" {
            let entry = args.next().expect("-I needs an argument");
            let (prefix, path) = entry.split_once('=').unwrap_or(("", &entry));
            settings.nix_path.push((prefix.to_owned(), path.to_owned()));
        } else {
            expr = Some(arg);
        }
    }
    let expr = expr.expect("usage: eval [-I prefix=path]... <expr>");
    let cwd = std::env::current_dir().expect("current directory");
    match ogygia_nix_eval::eval_to_string(&expr, &cwd.to_string_lossy(), settings) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
