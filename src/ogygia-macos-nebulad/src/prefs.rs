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

use crate::settings::DOMAIN;
use crate::settings::KEY_BRANCH;
use crate::settings::KEY_DEVICE;
use crate::settings::KEY_REPOSITORY_URL;
use crate::settings::Settings;

/// The settings as the daemon sees them: forced values from a configuration
/// profile first, then `/Library/Preferences`.
pub fn load() -> Settings {
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

/// Whether a configuration profile forces either setting.
pub fn managed() -> bool {
    let domain = CFString::new(DOMAIN);
    [KEY_REPOSITORY_URL, KEY_BRANCH, KEY_DEVICE]
        .iter()
        .any(|key| {
            let key = CFString::new(key);
            unsafe {
                CFPreferencesAppValueIsForced(
                    key.as_concrete_TypeRef(),
                    domain.as_concrete_TypeRef(),
                ) != 0
            }
        })
}

/// Store the settings for every user of this Mac, in
/// `/Library/Preferences/dev.ogygia.macos.plist`, removing any that are
/// unset. Only root may write there.
pub fn store(settings: &Settings) -> anyhow::Result<()> {
    let domain = CFString::new(DOMAIN);
    for (key, value) in [
        (KEY_REPOSITORY_URL, &settings.repository_url),
        (KEY_BRANCH, &settings.branch),
        (KEY_DEVICE, &settings.device),
    ] {
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
