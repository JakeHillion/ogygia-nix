//! Evaluate an installable and print it like `nix eval --json
//! --no-update-lock-file`.
//!
//! Usage: `eval-flake [--apply <expr>] [--store <dir>] <installable>`

use std::path::PathBuf;

use ogygia_nix_eval::Fetcher;
use ogygia_nix_eval::FlakeEnv;
use ogygia_nix_eval::Method;
use ogygia_nix_eval::Request;
use ogygia_nix_eval::Response;
use ogygia_nix_eval::Settings;

/// Makes requests over HTTP.
struct Http(reqwest::blocking::Client);

impl Fetcher for Http {
    fn request(&self, request: &Request<'_>) -> anyhow::Result<Response> {
        let method = match request.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
        };
        let mut builder = self.0.request(method, request.url);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        if request.method == Method::Post {
            builder = builder.body(request.body.to_vec());
        }
        let response = builder.send()?.error_for_status()?;
        let headers = response
            .headers()
            .iter()
            .map(|(n, v)| {
                (
                    n.to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        Ok(Response {
            headers,
            body: response.bytes()?.to_vec(),
        })
    }
}

fn main() {
    let mut apply = "x: x".to_owned();
    let mut store = None;
    let mut installable = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = args.next().expect("--apply needs an argument"),
            "--store" => {
                store = Some(PathBuf::from(
                    args.next().expect("--store needs an argument"),
                ))
            }
            _ => installable = Some(arg),
        }
    }
    let installable =
        installable.expect("usage: eval-flake [--apply <expr>] [--store <dir>] <installable>");
    let env = FlakeEnv {
        cwd: std::env::current_dir().expect("current directory"),
        store,
        fetcher: Some(Box::new(Http(reqwest::blocking::Client::new()))),
    };
    match ogygia_nix_eval::eval_flake_json(&installable, &apply, &env, Settings::default()) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
