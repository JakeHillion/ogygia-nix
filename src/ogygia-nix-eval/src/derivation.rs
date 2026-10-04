//! Store derivations: their ATerm serialisation and the hashes that give
//! output and `.drv` paths, as described in the Nix manual's "Store
//! Derivation" section.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use sha2::Digest;
use sha2::Sha256;

use crate::store;

#[derive(Clone, Debug, Default)]
pub struct Output {
    pub path: String,
    /// `r:sha256` style for fixed outputs, empty otherwise.
    pub hash_algo: String,
    /// Hex digest for fixed outputs, empty otherwise.
    pub hash: String,
}

#[derive(Clone, Debug, Default)]
pub struct Derivation {
    pub name: String,
    pub outputs: BTreeMap<String, Output>,
    pub input_drvs: BTreeMap<String, BTreeSet<String>>,
    pub input_srcs: BTreeSet<String>,
    pub platform: Vec<u8>,
    pub builder: Vec<u8>,
    pub args: Vec<Vec<u8>>,
    pub env: BTreeMap<String, Vec<u8>>,
}

fn aterm_str(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for &c in s {
        match c {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            c => out.push(c),
        }
    }
    out.push(b'"');
}

fn aterm_list<T>(
    out: &mut Vec<u8>,
    items: impl IntoIterator<Item = T>,
    mut f: impl FnMut(&mut Vec<u8>, T),
) {
    out.push(b'[');
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        f(out, item);
    }
    out.push(b']');
}

impl Derivation {
    pub fn is_fixed_output(&self) -> bool {
        self.outputs.len() == 1 && self.outputs.get("out").is_some_and(|o| !o.hash.is_empty())
    }

    /// The ATerm text, with the input derivations given by `inputs` (paths, or
    /// hash-modulo hex digests in place of paths).
    pub fn unparse(&self, inputs: &BTreeMap<String, BTreeSet<String>>) -> Vec<u8> {
        let mut out = b"Derive(".to_vec();
        aterm_list(&mut out, &self.outputs, |out, (name, o)| {
            out.push(b'(');
            aterm_str(out, name.as_bytes());
            out.push(b',');
            aterm_str(out, o.path.as_bytes());
            out.push(b',');
            aterm_str(out, o.hash_algo.as_bytes());
            out.push(b',');
            aterm_str(out, o.hash.as_bytes());
            out.push(b')');
        });
        out.push(b',');
        aterm_list(&mut out, inputs, |out, (path, outs)| {
            out.push(b'(');
            aterm_str(out, path.as_bytes());
            out.push(b',');
            aterm_list(out, outs, |out, o| aterm_str(out, o.as_bytes()));
            out.push(b')');
        });
        out.push(b',');
        aterm_list(&mut out, &self.input_srcs, |out, s| {
            aterm_str(out, s.as_bytes())
        });
        out.push(b',');
        // Nix writes the platform without escaping it.
        out.push(b'"');
        out.extend_from_slice(&self.platform);
        out.push(b'"');
        out.push(b',');
        aterm_str(&mut out, &self.builder);
        out.push(b',');
        aterm_list(&mut out, &self.args, |out, a| aterm_str(out, a));
        out.push(b',');
        aterm_list(&mut out, &self.env, |out, (k, v)| {
            out.push(b'(');
            aterm_str(out, k.as_bytes());
            out.push(b',');
            aterm_str(out, v);
            out.push(b')');
        });
        out.push(b')');
        out
    }

    /// `hashDerivationModulo`: a hash that identifies the derivation without
    /// depending on the paths of its fixed-output inputs. `input_hash` gives
    /// the modulo hash of each input derivation.
    pub fn hash_modulo(
        &self,
        input_hash: &dyn Fn(&str) -> anyhow::Result<[u8; 32]>,
    ) -> anyhow::Result<[u8; 32]> {
        if self.is_fixed_output() {
            let o = &self.outputs["out"];
            let s = format!("fixed:out:{}:{}:{}", o.hash_algo, o.hash, o.path);
            return Ok(Sha256::digest(s.as_bytes()).into());
        }
        let mut inputs = BTreeMap::new();
        for (path, outs) in &self.input_drvs {
            inputs.insert(hex::encode(input_hash(path)?), outs.clone());
        }
        Ok(Sha256::digest(self.unparse(&inputs)).into())
    }

    /// The `.drv` path this derivation is written to.
    pub fn drv_path(&self) -> String {
        let text = self.unparse(&self.input_drvs);
        let mut refs: Vec<&str> = self.input_srcs.iter().map(String::as_str).collect();
        refs.extend(self.input_drvs.keys().map(String::as_str));
        store::text_path(&text, &format!("{}.drv", self.name), &refs)
    }
}

/// The store path name of output `output` of a derivation called `name`.
pub fn output_path_name(name: &str, output: &str) -> String {
    if output == "out" {
        name.to_owned()
    } else {
        format!("{name}-{output}")
    }
}

/// The path of an input-addressed output given the derivation's modulo hash.
pub fn input_addressed_path(modulo: &[u8; 32], name: &str, output: &str) -> String {
    store::make_store_path(
        &format!("output:{output}"),
        modulo,
        &output_path_name(name, output),
    )
}
