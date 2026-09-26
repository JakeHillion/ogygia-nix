//! Whether the blob on disk should be replaced by one bound to the Tang
//! servers reachable right now.
//!
//! A blob is scored by how many of the spec's pins it is bound to that are
//! reachable right now: pins that are down or no longer in the spec do not
//! help anyone decrypt today. The candidate binds every reachable specced
//! pin, so it can never score lower than the current blob and wins exactly
//! when some reachable specced pin is missing from the current blob. A
//! candidate with no pins never wins, so a blob is never traded for
//! nothing, and a temporarily unreachable host costs nothing until a
//! strictly better binding is available.

use std::fmt;

use crate::config::Pin;
use crate::config::Spec;

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Bind to exactly these pins, in spec order. `previously` is how many
    /// of them the current blob already covers.
    Replace {
        pins: Vec<Pin>,
        previously: usize,
    },
    Keep(Reason),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reason {
    NoImprovement { current: usize, candidate: usize },
    BelowThreshold { candidate: usize, t: usize },
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoImprovement { current, candidate } => write!(
                f,
                "current blob covers {current} of {candidate} reachable specced pins"
            ),
            Self::BelowThreshold { candidate, t } => {
                write!(
                    f,
                    "{candidate} reachable specced pins is below threshold {t}"
                )
            }
        }
    }
}

pub fn decide(spec: &Spec, current: &[Pin], reachable: &[Pin]) -> Decision {
    let candidate: Vec<Pin> = spec
        .pins
        .tang
        .iter()
        .filter(|pin| reachable.contains(pin))
        .cloned()
        .collect();
    let previously = candidate.iter().filter(|pin| current.contains(pin)).count();
    if candidate.len() <= previously {
        return Decision::Keep(Reason::NoImprovement {
            current: previously,
            candidate: candidate.len(),
        });
    }
    if candidate.len() < spec.t {
        return Decision::Keep(Reason::BelowThreshold {
            candidate: candidate.len(),
            t: spec.t,
        });
    }
    Decision::Replace {
        pins: candidate,
        previously,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Pins;

    fn pin(name: &str) -> Pin {
        Pin {
            url: format!("http://{}:7654", name.trim_end_matches(char::is_numeric)),
            thp: format!("{name}-thp"),
        }
    }

    fn pins(names: &[&str]) -> Vec<Pin> {
        names.iter().map(|name| pin(name)).collect()
    }

    fn spec(t: usize, names: &[&str]) -> Spec {
        Spec {
            t,
            pins: Pins { tang: pins(names) },
        }
    }

    fn replace(names: &[&str], previously: usize) -> Decision {
        Decision::Replace {
            pins: pins(names),
            previously,
        }
    }

    #[test]
    fn binds_a_newly_reachable_pin() {
        let decision = decide(&spec(1, &["a", "b"]), &pins(&["a"]), &pins(&["a", "b"]));
        assert_eq!(decision, replace(&["a", "b"], 1));
    }

    #[test]
    fn swaps_a_down_pin_for_a_reachable_one() {
        // {a,b,c} with c down covers two reachable pins; {a,b,d} covers three.
        let decision = decide(
            &spec(1, &["a", "b", "c", "d"]),
            &pins(&["a", "b", "c"]),
            &pins(&["a", "b", "d"]),
        );
        assert_eq!(decision, replace(&["a", "b", "d"], 2));
    }

    #[test]
    fn never_regresses_to_fewer_reachable_pins() {
        let decision = decide(
            &spec(1, &["a", "b", "c", "d"]),
            &pins(&["a", "b"]),
            &pins(&["a"]),
        );
        assert_eq!(
            decision,
            Decision::Keep(Reason::NoImprovement {
                current: 1,
                candidate: 1
            })
        );
    }

    #[test]
    fn converges_on_the_full_spec() {
        let all = ["a", "b", "c", "d"];
        let decision = decide(&spec(1, &all), &pins(&["a", "b", "d"]), &pins(&all));
        assert_eq!(decision, replace(&all, 3));
    }

    #[test]
    fn is_stable_once_fully_bound() {
        let all = ["a", "b", "c", "d"];
        let decision = decide(&spec(1, &all), &pins(&all), &pins(&all));
        assert_eq!(
            decision,
            Decision::Keep(Reason::NoImprovement {
                current: 4,
                candidate: 4
            })
        );
    }

    #[test]
    fn rotated_keys_count_for_nothing() {
        // The blob holds the old keys of both servers; only a's new key is up.
        let decision = decide(
            &spec(1, &["a1", "b1"]),
            &pins(&["a0", "b0"]),
            &pins(&["a1"]),
        );
        assert_eq!(decision, replace(&["a1"], 0));
    }

    #[test]
    fn never_replaces_with_nothing() {
        let decision = decide(&spec(1, &["a1", "b1"]), &pins(&["a0", "b0"]), &[]);
        assert_eq!(
            decision,
            Decision::Keep(Reason::NoImprovement {
                current: 0,
                candidate: 0
            })
        );
    }

    #[test]
    fn keeps_pins_dropped_from_the_spec_until_something_improves() {
        let decision = decide(
            &spec(1, &["a", "b"]),
            &pins(&["a", "b", "c"]),
            &pins(&["a", "b"]),
        );
        assert_eq!(
            decision,
            Decision::Keep(Reason::NoImprovement {
                current: 2,
                candidate: 2
            })
        );
    }

    #[test]
    fn will_not_build_below_the_threshold() {
        let decision = decide(&spec(2, &["a", "b", "c"]), &pins(&["a"]), &pins(&["b"]));
        assert_eq!(
            decision,
            Decision::Keep(Reason::BelowThreshold { candidate: 1, t: 2 })
        );
    }

    #[test]
    fn replaces_at_the_threshold() {
        let decision = decide(
            &spec(2, &["a1", "b1"]),
            &pins(&["a0", "b0"]),
            &pins(&["b1", "a1"]),
        );
        assert_eq!(decision, replace(&["a1", "b1"], 0));
    }

    #[test]
    fn candidate_follows_spec_order_and_ignores_unspecced_pins() {
        let decision = decide(&spec(1, &["a", "b", "c"]), &[], &pins(&["z", "c", "a"]));
        assert_eq!(decision, replace(&["a", "c"], 0));
    }
}
