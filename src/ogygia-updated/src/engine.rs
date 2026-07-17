use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use git2::Oid;
use tracing::info;

use crate::config::Config;
use crate::repo::Repo;
use crate::system::Activation;
use crate::system::RealSystem;
use crate::system::System;

/// Result of one update cycle.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The running and next-boot systems already match the branch tip.
    UpToDate,
    /// The running system was not built from the tracked branch, so it
    /// was left alone.
    NotOnBranch { revision: String },
    /// The new system was activated and made the boot default.
    Switched { commit: String },
    /// The next-boot system was not built from the tracked branch, so
    /// the new system was activated without touching the boot profile.
    TestActivated { commit: String },
    /// The new system needs a new kernel; it is the boot default and a
    /// reboot has been scheduled.
    RebootScheduled { commit: String },
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::UpToDate => write!(f, "system is up to date"),
            Outcome::NotOnBranch { revision } => {
                write!(
                    f,
                    "running revision {revision} is not on the tracked branch; not updating"
                )
            }
            Outcome::Switched { commit } => write!(f, "switched to {commit}"),
            Outcome::TestActivated { commit } => {
                write!(f, "activated {commit} without changing the boot default")
            }
            Outcome::RebootScheduled { commit } => {
                write!(f, "staged {commit} for boot and scheduled a reboot")
            }
        }
    }
}

/// What initiated an update cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The daemon's own interval fired. A host deliberately running or
    /// booting an off-branch build is left alone.
    Scheduled,
    /// An operator asked for a cycle over the control socket. The host is
    /// pulled back onto the branch tip regardless of what it is running,
    /// so scheduled updates resume.
    Manual,
}

/// Run one update cycle.
pub fn run_once(config: &Config, trigger: Trigger) -> Result<Outcome> {
    run(config, &RealSystem, trigger)
}

fn run(config: &Config, system: &impl System, trigger: Trigger) -> Result<Outcome> {
    let current = read_revision(&config.host.current_revision_path)?;
    let next_boot = read_revision(&config.next_boot_revision_path())?;

    let repo = Repo::open_or_clone(&config.repo)?;
    repo.fetch()?;
    let tip = repo.tip(&config.repo.branch)?;
    info!(%current, %next_boot, %tip, branch = %config.repo.branch, ?trigger, "fetched");

    // The scheduler leaves a deliberately off-branch host alone; a manual
    // trigger overrides that to pull it back onto the branch.
    if trigger == Trigger::Scheduled && !repo.is_ancestor(current, tip)? {
        return Ok(Outcome::NotOnBranch {
            revision: current.to_string(),
        });
    }

    if current == tip && next_boot == tip {
        return Ok(Outcome::UpToDate);
    }

    repo.checkout(tip)?;

    let flake = format!("git+file://{}", config.repo.path.display());
    let store_path = system.build(&format!("{flake}#{}", config.flake_attr()))?;
    info!(store_path = %store_path.display(), "built new system");

    // A hand-staged off-branch next-boot system is preserved by the
    // scheduler, but a manual trigger resets the boot default to the tip.
    let next_boot_on_branch = match trigger {
        Trigger::Manual => true,
        Trigger::Scheduled => repo.is_ancestor(next_boot, tip)?,
    };
    activate(config, system, &store_path, tip, next_boot_on_branch)
}

fn activate(
    config: &Config,
    system: &impl System,
    store_path: &Path,
    tip: Oid,
    next_boot_on_branch: bool,
) -> Result<Outcome> {
    let commit = tip.to_string();

    // A next-boot system off the tracked branch was deliberately staged
    // by hand; activate the update without clobbering it.
    if !next_boot_on_branch {
        system.switch_to_configuration(store_path, Activation::Test)?;
        return Ok(Outcome::TestActivated { commit });
    }

    if !config.activate.allow_reboot {
        system.set_profile(&config.activate.profile, store_path)?;
        system.switch_to_configuration(store_path, Activation::Switch)?;
        return Ok(Outcome::Switched { commit });
    }

    system.set_profile(&config.activate.profile, store_path)?;
    system.switch_to_configuration(store_path, Activation::Boot)?;
    if system.kernel_links(&config.activate.booted_system) == system.kernel_links(store_path) {
        system.switch_to_configuration(store_path, Activation::Test)?;
        Ok(Outcome::Switched { commit })
    } else {
        system.schedule_reboot(config.activate.reboot_delay_minutes)?;
        Ok(Outcome::RebootScheduled { commit })
    }
}

fn read_revision(path: &Path) -> Result<Oid> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading build revision {}", path.display()))?;
    Oid::from_str(raw.trim()).with_context(|| {
        format!(
            "parsing build revision {:?} from {}",
            raw.trim(),
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::config::ActivateConfig;
    use crate::config::BuildConfig;
    use crate::config::DaemonConfig;
    use crate::config::HostConfig;
    use crate::config::RepoConfig;
    use crate::repo::testutil::commit;
    use crate::repo::testutil::init_origin;

    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Build(String),
        SetProfile(PathBuf),
        SwitchToConfiguration(Activation),
        ScheduleReboot(u32),
    }

    #[derive(Default)]
    struct MockSystem {
        calls: RefCell<Vec<Call>>,
        booted_kernel: Option<&'static str>,
        built_kernel: Option<&'static str>,
    }

    impl System for MockSystem {
        fn build(&self, flake_ref: &str) -> Result<PathBuf> {
            self.calls
                .borrow_mut()
                .push(Call::Build(flake_ref.to_string()));
            Ok(PathBuf::from("/nix/store/new-system"))
        }

        fn set_profile(&self, profile: &Path, _store_path: &Path) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(Call::SetProfile(profile.to_path_buf()));
            Ok(())
        }

        fn switch_to_configuration(&self, _store_path: &Path, action: Activation) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(Call::SwitchToConfiguration(action));
            Ok(())
        }

        fn kernel_links(&self, system: &Path) -> [Option<PathBuf>; 3] {
            let kernel = if system == Path::new("/run/booted-system") {
                self.booted_kernel
            } else {
                self.built_kernel
            };
            [kernel.map(PathBuf::from), None, None]
        }

        fn schedule_reboot(&self, delay_minutes: u32) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(Call::ScheduleReboot(delay_minutes));
            Ok(())
        }
    }

    struct Fixture {
        // Held so the temporary directory outlives the test.
        _dir: TempDir,
        origin: git2::Repository,
        config: Config,
    }

    impl Fixture {
        fn new() -> (Self, Oid) {
            let dir = TempDir::new().unwrap();
            let origin_path = dir.path().join("origin");
            let (origin, initial) = init_origin(&origin_path);

            let profile = dir.path().join("profile");
            fs::create_dir_all(profile.join("sw/share/ogygia")).unwrap();

            let config = Config {
                repo: RepoConfig {
                    url: origin_path.to_str().unwrap().to_string(),
                    path: dir.path().join("clone"),
                    branch: "main".to_string(),
                },
                host: HostConfig {
                    name: "host.example.com".to_string(),
                    current_revision_path: dir.path().join("current-revision"),
                },
                build: BuildConfig::default(),
                activate: ActivateConfig {
                    profile,
                    ..ActivateConfig::default()
                },
                daemon: DaemonConfig::default(),
            };

            let fixture = Self {
                _dir: dir,
                origin,
                config,
            };
            (fixture, initial)
        }

        fn set_current(&self, revision: Oid) {
            fs::write(
                &self.config.host.current_revision_path,
                format!("{revision}\n"),
            )
            .unwrap();
        }

        fn set_next_boot(&self, revision: Oid) {
            fs::write(
                self.config.next_boot_revision_path(),
                format!("{revision}\n"),
            )
            .unwrap();
        }
    }

    #[test]
    fn up_to_date_makes_no_changes() {
        let (fixture, initial) = Fixture::new();
        fixture.set_current(initial);
        fixture.set_next_boot(initial);

        let system = MockSystem::default();
        let outcome = run(&fixture.config, &system, Trigger::Scheduled).unwrap();

        assert_eq!(outcome, Outcome::UpToDate);
        assert!(system.calls.borrow().is_empty());
    }

    #[test]
    fn update_switches_to_the_new_commit() {
        let (fixture, initial) = Fixture::new();
        fixture.set_current(initial);
        fixture.set_next_boot(initial);
        let second = commit(&fixture.origin, "main", &[initial], "second");

        let system = MockSystem::default();
        let outcome = run(&fixture.config, &system, Trigger::Scheduled).unwrap();

        assert_eq!(
            outcome,
            Outcome::Switched {
                commit: second.to_string()
            }
        );
        let flake_ref = format!(
            "git+file://{}#nixosConfigurations.\"host.example.com\".config.system.build.toplevel",
            fixture.config.repo.path.display()
        );
        assert_eq!(
            *system.calls.borrow(),
            vec![
                Call::Build(flake_ref),
                Call::SetProfile(fixture.config.activate.profile.clone()),
                Call::SwitchToConfiguration(Activation::Switch),
            ]
        );

        // The working tree was left checked out at the new commit.
        assert_eq!(
            fs::read_to_string(fixture.config.repo.path.join("marker")).unwrap(),
            "second"
        );
    }

    #[test]
    fn off_branch_current_revision_blocks_the_update() {
        let (fixture, initial) = Fixture::new();
        let side = commit(&fixture.origin, "side", &[initial], "side");
        commit(&fixture.origin, "main", &[initial], "second");
        fixture.set_current(side);
        fixture.set_next_boot(initial);

        let system = MockSystem::default();
        let outcome = run(&fixture.config, &system, Trigger::Scheduled).unwrap();

        assert_eq!(
            outcome,
            Outcome::NotOnBranch {
                revision: side.to_string()
            }
        );
        assert!(system.calls.borrow().is_empty());
    }

    #[test]
    fn off_branch_next_boot_gets_test_activation_only() {
        let (fixture, initial) = Fixture::new();
        let side = commit(&fixture.origin, "side", &[initial], "side");
        let second = commit(&fixture.origin, "main", &[initial], "second");
        fixture.set_current(initial);
        fixture.set_next_boot(side);

        let system = MockSystem::default();
        let outcome = run(&fixture.config, &system, Trigger::Scheduled).unwrap();

        assert_eq!(
            outcome,
            Outcome::TestActivated {
                commit: second.to_string()
            }
        );
        let calls = system.calls.borrow();
        assert_eq!(
            *calls,
            vec![
                Call::Build(format!(
                    "git+file://{}#nixosConfigurations.\"host.example.com\".config.system.build.toplevel",
                    fixture.config.repo.path.display()
                )),
                Call::SwitchToConfiguration(Activation::Test),
            ]
        );
    }

    #[test]
    fn manual_recovers_an_off_branch_running_system() {
        let (fixture, initial) = Fixture::new();
        let side = commit(&fixture.origin, "side", &[initial], "side");
        let second = commit(&fixture.origin, "main", &[initial], "second");
        fixture.set_current(side);
        fixture.set_next_boot(initial);

        let system = MockSystem::default();
        let outcome = run(&fixture.config, &system, Trigger::Manual).unwrap();

        // The off-branch guard is overridden and the host is switched onto
        // the branch tip.
        assert_eq!(
            outcome,
            Outcome::Switched {
                commit: second.to_string()
            }
        );
        assert_eq!(
            *system.calls.borrow(),
            vec![
                Call::Build(format!(
                    "git+file://{}#nixosConfigurations.\"host.example.com\".config.system.build.toplevel",
                    fixture.config.repo.path.display()
                )),
                Call::SetProfile(fixture.config.activate.profile.clone()),
                Call::SwitchToConfiguration(Activation::Switch),
            ]
        );
    }

    #[test]
    fn manual_resets_an_off_branch_next_boot() {
        let (fixture, initial) = Fixture::new();
        let side = commit(&fixture.origin, "side", &[initial], "side");
        let second = commit(&fixture.origin, "main", &[initial], "second");
        fixture.set_current(initial);
        fixture.set_next_boot(side);

        let system = MockSystem::default();
        let outcome = run(&fixture.config, &system, Trigger::Manual).unwrap();

        // Rather than test-activating around the hand-staged boot entry,
        // the manual trigger resets the boot default to the tip.
        assert_eq!(
            outcome,
            Outcome::Switched {
                commit: second.to_string()
            }
        );
        assert_eq!(
            *system.calls.borrow(),
            vec![
                Call::Build(format!(
                    "git+file://{}#nixosConfigurations.\"host.example.com\".config.system.build.toplevel",
                    fixture.config.repo.path.display()
                )),
                Call::SetProfile(fixture.config.activate.profile.clone()),
                Call::SwitchToConfiguration(Activation::Switch),
            ]
        );
    }

    #[test]
    fn unchanged_kernel_switches_without_reboot() {
        let (fixture, initial) = Fixture::new();
        fixture.set_current(initial);
        fixture.set_next_boot(initial);
        let second = commit(&fixture.origin, "main", &[initial], "second");
        let mut config = fixture.config;
        config.activate.allow_reboot = true;
        config.activate.booted_system = PathBuf::from("/run/booted-system");

        let system = MockSystem {
            booted_kernel: Some("kernel-1"),
            built_kernel: Some("kernel-1"),
            ..MockSystem::default()
        };
        let outcome = run(&config, &system, Trigger::Scheduled).unwrap();

        assert_eq!(
            outcome,
            Outcome::Switched {
                commit: second.to_string()
            }
        );
        assert_eq!(
            system.calls.borrow()[1..],
            [
                Call::SetProfile(config.activate.profile.clone()),
                Call::SwitchToConfiguration(Activation::Boot),
                Call::SwitchToConfiguration(Activation::Test),
            ]
        );
    }

    #[test]
    fn changed_kernel_schedules_a_reboot() {
        let (fixture, initial) = Fixture::new();
        fixture.set_current(initial);
        fixture.set_next_boot(initial);
        let second = commit(&fixture.origin, "main", &[initial], "second");
        let mut config = fixture.config;
        config.activate.allow_reboot = true;
        config.activate.booted_system = PathBuf::from("/run/booted-system");

        let system = MockSystem {
            booted_kernel: Some("kernel-1"),
            built_kernel: Some("kernel-2"),
            ..MockSystem::default()
        };
        let outcome = run(&config, &system, Trigger::Scheduled).unwrap();

        assert_eq!(
            outcome,
            Outcome::RebootScheduled {
                commit: second.to_string()
            }
        );
        assert_eq!(
            system.calls.borrow()[1..],
            [
                Call::SetProfile(config.activate.profile.clone()),
                Call::SwitchToConfiguration(Activation::Boot),
                Call::ScheduleReboot(15),
            ]
        );
    }
}
