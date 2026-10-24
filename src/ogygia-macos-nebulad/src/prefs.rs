//! Reading and writing [`Settings`] through CFPreferences, the API behind
//! `defaults`.

use core_foundation::base::CFType;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::preferences::CFPreferencesAppSynchronize;
use core_foundation_sys::preferences::CFPreferencesAppValueIsForced;
use core_foundation_sys::preferences::CFPreferencesCopyAppValue;
use core_foundation_sys::preferences::CFPreferencesSetValue;
use core_foundation_sys::preferences::CFPreferencesSynchronize;
use core_foundation_sys::preferences::kCFPreferencesAnyHost;
use core_foundation_sys::preferences::kCFPreferencesAnyUser;

use crate::config::ConfigMode;
use crate::config::LocalConfig;
use crate::preferences::Preferences;
use crate::settings::DOMAIN;
use crate::settings::KEY_BRANCH;
use crate::settings::KEY_CONFIG_MODE;
use crate::settings::KEY_DEVICE;
use crate::settings::KEY_LOCAL_CONFIG;
use crate::settings::KEY_REPOSITORY_URL;
use crate::settings::Settings;

/// The settings as the daemon sees them: forced values from a configuration
/// profile first, then `/Library/Preferences`.
fn load() -> Settings {
    let domain = CFString::new(DOMAIN);
    // Drop cfprefsd's cached copy so a `defaults write` made since the last
    // read is seen.
    unsafe { CFPreferencesAppSynchronize(domain.as_concrete_TypeRef()) };
    Settings {
        repository_url: string(&domain, KEY_REPOSITORY_URL),
        branch: string(&domain, KEY_BRANCH),
        device: string(&domain, KEY_DEVICE),
    }
}

/// Whether a configuration profile forces any of the settings.
fn managed() -> bool {
    [KEY_REPOSITORY_URL, KEY_BRANCH, KEY_DEVICE]
        .iter()
        .any(|key| forced(key))
}

/// Whether to follow the fleet's Nebula config, and the local one, which a
/// value that does not parse leaves at its defaults.
fn load_config() -> (ConfigMode, LocalConfig) {
    let domain = CFString::new(DOMAIN);
    unsafe { CFPreferencesAppSynchronize(domain.as_concrete_TypeRef()) };
    let mode = match string(&domain, KEY_CONFIG_MODE).as_deref() {
        Some("local") => ConfigMode::Local,
        _ => ConfigMode::Fleet,
    };
    let local = string(&domain, KEY_LOCAL_CONFIG)
        .and_then(|json| match serde_json::from_str(&json) {
            Ok(local) => Some(local),
            Err(e) => {
                tracing::warn!(error = %e, "ignoring a local config that does not parse");
                None
            }
        })
        .unwrap_or_default();
    (mode, local)
}

/// Whether a configuration profile forces the config mode or the local
/// config.
fn config_mode_managed() -> bool {
    forced(KEY_CONFIG_MODE) || forced(KEY_LOCAL_CONFIG)
}

fn forced(key: &str) -> bool {
    let domain = CFString::new(DOMAIN);
    let key = CFString::new(key);
    unsafe {
        CFPreferencesAppValueIsForced(key.as_concrete_TypeRef(), domain.as_concrete_TypeRef()) != 0
    }
}

/// Store the settings for every user of this Mac, in
/// `/Library/Preferences/dev.ogygia.macos.plist`, removing any that are
/// unset. Only root may write there.
fn store(settings: &Settings) -> anyhow::Result<()> {
    write([
        (KEY_REPOSITORY_URL, settings.repository_url.clone()),
        (KEY_BRANCH, settings.branch.clone()),
        (KEY_DEVICE, settings.device.clone()),
    ])
}

/// Store the config mode and the local config beside the settings.
fn store_config(mode: ConfigMode, local: &LocalConfig) -> anyhow::Result<()> {
    let mode = match mode {
        ConfigMode::Fleet => "fleet",
        ConfigMode::Local => "local",
    };
    write([
        (KEY_CONFIG_MODE, Some(mode.to_owned())),
        (KEY_LOCAL_CONFIG, Some(serde_json::to_string(local)?)),
    ])
}

fn write<const N: usize>(values: [(&str, Option<String>); N]) -> anyhow::Result<()> {
    let domain = CFString::new(DOMAIN);
    for (key, value) in values {
        let key = CFString::new(key);
        let value = value.as_deref().map(CFString::new);
        unsafe {
            CFPreferencesSetValue(
                key.as_concrete_TypeRef(),
                // A null value removes the key.
                value
                    .as_ref()
                    .map_or(std::ptr::null(), |v| v.as_CFTypeRef()),
                domain.as_concrete_TypeRef(),
                kCFPreferencesAnyUser,
                kCFPreferencesAnyHost,
            );
        }
    }
    let synced = unsafe {
        CFPreferencesSynchronize(
            domain.as_concrete_TypeRef(),
            kCFPreferencesAnyUser,
            kCFPreferencesAnyHost,
        )
    };
    if synced == 0 {
        anyhow::bail!("could not write the settings to /Library/Preferences/{DOMAIN}.plist");
    }
    Ok(())
}

fn string(domain: &CFString, key: &str) -> Option<String> {
    let key = CFString::new(key);
    let value = unsafe {
        CFPreferencesCopyAppValue(key.as_concrete_TypeRef(), domain.as_concrete_TypeRef())
    };
    if value.is_null() {
        return None;
    }
    // Copy rule: the returned reference is ours to release.
    let value = unsafe { CFType::wrap_under_create_rule(value) };
    let value = value.downcast_into::<CFString>()?.to_string();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// The `dev.ogygia.macos` domain, which the launchd job uses.
pub struct SystemPreferences;

impl Preferences for SystemPreferences {
    fn load(&self) -> Settings {
        load()
    }

    fn managed(&self) -> bool {
        managed()
    }

    fn store(&self, settings: &Settings) -> anyhow::Result<()> {
        store(settings)
    }

    fn load_config(&self) -> (ConfigMode, LocalConfig) {
        load_config()
    }

    fn config_mode_managed(&self) -> bool {
        config_mode_managed()
    }

    fn store_config(&self, mode: ConfigMode, local: &LocalConfig) -> anyhow::Result<()> {
        store_config(mode, local)
    }
}
