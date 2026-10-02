//! A pure Rust evaluator for the Nix language.
//!
//! A [`Context`] owns compiled sources and filesystem mappings and may be
//! reused across evaluations; each evaluation runs in a session ([`Eval`])
//! whose values live in an arena dropped when the session ends.
//!
//! Evaluation recurses on the native stack, so run it through
//! [`run_with_stack`] rather than on a thread with a default-sized stack.
//! Equivalence with the real Nix evaluator is checked by the tests in
//! `tests/equiv`.

mod builtins;
mod compile;
mod context;
mod derivation;
mod eval;
mod io;
mod ir;
mod path;
mod print;
mod store;
mod symbol;
mod value;

pub use context::Context;
pub use eval::Eval;
pub use eval::Settings;
pub use io::Io;
pub use print::print_strict;
pub use value::EvalError;
pub use value::Value;

/// The system this binary was built for, as a Nix system double.
pub const CURRENT_SYSTEM: &str = current_system();

const fn current_system() -> &'static str {
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "x86_64-linux"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
        "aarch64-linux"
    } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-darwin"
    } else if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        "x86_64-darwin"
    } else {
        "unknown"
    }
}

/// Stack size for evaluation threads. Deeply nested evaluation (the NixOS
/// module system, long `foldl'` chains of thunks) needs far more than the
/// default.
const STACK_SIZE: usize = 1 << 30;

/// Run `f` on a fresh thread with a stack large enough for evaluation.
pub fn run_with_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("nix-eval".to_owned())
            .stack_size(STACK_SIZE)
            .spawn_scoped(s, f)
            .expect("failed to spawn evaluation thread")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// Evaluate the expression in `text`, deeply forcing it, and render it the
/// way `nix-instantiate --eval --strict` does. Relative paths resolve
/// against `base_dir`.
pub fn eval_to_string(text: &str, base_dir: &str, settings: Settings) -> Result<String, String> {
    run_with_stack(|| {
        let ctx = Context::new(Io::default());
        let bump = bumpalo::Bump::new();
        let ev = Eval::new(&ctx, &bump, settings);
        let result = ev
            .eval_string(text, base_dir)
            .and_then(|v| print_strict(&ev, v));
        match result {
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Err(e) => Err(e.to_string()),
        }
    })
}
