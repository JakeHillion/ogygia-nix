//! Who is on the other end of the socket.

use std::ffi::CStr;
use std::ffi::c_int;

use tokio::net::UnixStream;

/// The `admin` group, whose members macOS lets administer the machine.
const ADMIN_GID: libc::gid_t = 80;

/// Whether the peer may change settings: root, or a member of `admin`, as
/// `tailscaled` decides on macOS. No password is asked for, so any process an
/// administrator runs may change them; `render` keeps the settings to the mesh.
pub fn is_admin(stream: &UnixStream) -> bool {
    let Ok(cred) = stream.peer_cred() else {
        return false;
    };
    cred.uid() == 0 || in_group(cred.uid(), ADMIN_GID)
}

fn in_group(uid: libc::uid_t, gid: libc::gid_t) -> bool {
    let mut buf = vec![0u8; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut found,
        )
    };
    if rc != 0 || found.is_null() {
        return false;
    }
    let name = unsafe { CStr::from_ptr(pwd.pw_name) };

    // getgrouplist reports how many groups there are when the buffer is too
    // small, so grow it until they fit.
    let mut groups: Vec<c_int> = vec![0; 64];
    loop {
        let mut count = groups.len() as c_int;
        let rc = unsafe {
            libc::getgrouplist(
                name.as_ptr(),
                pwd.pw_gid as c_int,
                groups.as_mut_ptr(),
                &mut count,
            )
        };
        if rc == 0 {
            groups.truncate(count as usize);
            return groups.contains(&(gid as c_int));
        }
        if groups.len() >= 65536 {
            return false;
        }
        groups.resize(groups.len() * 4, 0);
    }
}
