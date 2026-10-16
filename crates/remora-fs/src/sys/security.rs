//! Security identifiers and descriptors (Windows), built from structured values
//! (spec 5.1 rule 6). The pipe DACL and blob homes on other volumes reuse them.

use std::io;
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx, CONTAINER_INHERIT_ACE, CopySid,
    CreateWellKnownSid, GetLengthSid, GetTokenInformation, InitializeAcl,
    InitializeSecurityDescriptor, IsValidSid, OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, SECURITY_DESCRIPTOR, SECURITY_MAX_SID_SIZE, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// A security identifier in an owned, `u32`-aligned buffer.
pub(crate) struct Sid(Vec<u32>);

impl Sid {
    /// The user of the process token. An elevated process has the same user SID,
    /// which is why it is set as owner explicitly (spec 7.7.2 rule 3).
    pub(crate) fn current_user() -> io::Result<Self> {
        let mut token = std::ptr::null_mut();
        // SAFETY: the pseudo handle of the current process needs no closing, and
        // `token` is a valid out pointer.
        check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) })?;
        // SAFETY: `token` was just opened and is owned by nothing else.
        let token = unsafe {
            <std::os::windows::io::OwnedHandle as std::os::windows::io::FromRawHandle>::from_raw_handle(token)
        };
        let raw = std::os::windows::io::AsRawHandle::as_raw_handle(&token);
        let mut len = 0;
        // SAFETY: a size query with no buffer; `len` is a valid out pointer.
        let ok = unsafe { GetTokenInformation(raw, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        // SAFETY: reads the calling thread's last error.
        if ok == 0 && unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u32; (len as usize).div_ceil(4)];
        // SAFETY: `buf` holds at least `len` writable bytes, aligned for TOKEN_USER.
        check(unsafe {
            GetTokenInformation(raw, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
        })?;
        // SAFETY: the call above filled `buf` with a TOKEN_USER whose SID pointer
        // points inside `buf`, which is alive here.
        let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        Self::copy(sid)
    }

    /// `S-1-5-18`, the local `SYSTEM` account.
    pub(crate) fn local_system() -> io::Result<Self> {
        let mut size = SECURITY_MAX_SID_SIZE;
        let mut buf = vec![0u32; (size as usize).div_ceil(4)];
        // SAFETY: `buf` holds `size` writable bytes; no domain SID is needed.
        check(unsafe {
            CreateWellKnownSid(
                WinLocalSystemSid,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        })?;
        Ok(Self(buf))
    }

    fn copy(sid: PSID) -> io::Result<Self> {
        // SAFETY: `sid` points into the caller's live buffer; IsValidSid only reads it.
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(io::Error::other("the token holds no valid user SID"));
        }
        // SAFETY: `sid` was checked to be a valid SID above.
        let len = unsafe { GetLengthSid(sid) };
        let mut buf = vec![0u32; (len as usize).div_ceil(4)];
        // SAFETY: `buf` holds `len` writable bytes and `sid` is valid.
        check(unsafe { CopySid(len, buf.as_mut_ptr().cast(), sid) })?;
        Ok(Self(buf))
    }

    pub(super) fn as_psid(&self) -> PSID {
        self.0.as_ptr().cast_mut().cast()
    }

    fn len(&self) -> u32 {
        // SAFETY: the buffer holds a valid SID.
        unsafe { GetLengthSid(self.as_psid()) }
    }

    /// The SID in string form, such as `S-1-5-18`.
    #[cfg(test)]
    pub(crate) fn to_string_form(&self) -> io::Result<String> {
        test_support::sid_string(self.as_psid())
    }
}

/// An absolute security descriptor with the buffers it points into.
pub(crate) struct Descriptor {
    descriptor: Box<SECURITY_DESCRIPTOR>,
    // Read through raw pointers held by `descriptor`; kept alive and never moved
    // (heap buffers stay put when the struct moves).
    _owner: Sid,
    _system: Option<Sid>,
    _acl: Vec<u32>,
}

impl Descriptor {
    /// Owner the current user; a protected DACL granting full control to the user
    /// and `SYSTEM` only, inherited by files and subdirectories.
    pub(crate) fn owner_only_dir() -> io::Result<Self> {
        let owner = Sid::current_user()?;
        let system = Sid::local_system()?;
        let ace_size =
            |sid: &Sid| size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid.len() as usize;
        let acl_size = size_of::<ACL>() + ace_size(&owner) + ace_size(&system);
        let mut acl = vec![0u32; acl_size.div_ceil(4)];
        let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
        let acl_len = u32::try_from(acl_size).map_err(io::Error::other)?;
        // SAFETY: `acl` holds `acl_size` writable, u32-aligned bytes.
        check(unsafe { InitializeAcl(acl_ptr, acl_len, ACL_REVISION) })?;
        for sid in [&owner, &system] {
            // SAFETY: the ACL was sized for both ACEs and `sid` is a valid SID.
            check(unsafe {
                AddAccessAllowedAceEx(
                    acl_ptr,
                    ACL_REVISION,
                    OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                    FILE_ALL_ACCESS,
                    sid.as_psid(),
                )
            })?;
        }
        let mut descriptor = Self::with_owner(owner)?;
        let sd = descriptor.as_mut_psd();
        // SAFETY: `sd` is an initialized absolute descriptor; `acl_ptr` points into
        // `acl`, which moves into `descriptor` below and lives as long as it.
        check(unsafe { SetSecurityDescriptorDacl(sd, 1, acl_ptr, 0) })?;
        // SAFETY: `sd` is an initialized descriptor.
        check(unsafe { SetSecurityDescriptorControl(sd, SE_DACL_PROTECTED, SE_DACL_PROTECTED) })?;
        descriptor._system = Some(system);
        descriptor._acl = acl;
        Ok(descriptor)
    }

    /// Owner the current user and no DACL, so the object inherits its parent's.
    pub(crate) fn owner_inheriting() -> io::Result<Self> {
        Self::with_owner(Sid::current_user()?)
    }

    fn with_owner(owner: Sid) -> io::Result<Self> {
        let mut descriptor = Self {
            descriptor: Box::default(),
            _owner: owner,
            _system: None,
            _acl: Vec::new(),
        };
        let sd = descriptor.as_mut_psd();
        // SAFETY: `sd` points to a writable SECURITY_DESCRIPTOR in the box.
        check(unsafe { InitializeSecurityDescriptor(sd, SECURITY_DESCRIPTOR_REVISION) })?;
        // SAFETY: `sd` is initialized; the owner SID lives in `descriptor`.
        check(unsafe { SetSecurityDescriptorOwner(sd, descriptor._owner.as_psid(), 0) })?;
        Ok(descriptor)
    }

    fn as_mut_psd(&mut self) -> PSECURITY_DESCRIPTOR {
        std::ptr::from_mut(&mut *self.descriptor).cast()
    }

    pub(super) fn as_ptr(&self) -> *const SECURITY_DESCRIPTOR {
        &*self.descriptor
    }
}

fn check(ok: windows_sys::core::BOOL) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Reading a descriptor back, for tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io;
    use std::os::windows::io::{AsRawHandle, BorrowedHandle};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, DACL_SECURITY_INFORMATION, GetAce,
        GetSecurityDescriptorControl, OWNER_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED,
    };

    pub(crate) struct Ace {
        pub(crate) sid: String,
        pub(crate) kind: u8,
        pub(crate) flags: u8,
        pub(crate) mask: u32,
    }

    pub(crate) struct Info {
        pub(crate) owner: String,
        pub(crate) protected: bool,
        pub(crate) aces: Vec<Ace>,
    }

    pub(crate) fn read(handle: BorrowedHandle<'_>) -> io::Result<Info> {
        let (mut owner, mut dacl, mut sd) = (
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        // SAFETY: the handle is open; every out pointer refers to a local.
        let rc = unsafe {
            GetSecurityInfo(
                handle.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        let result = (|| {
            let (mut control, mut revision) = (0, 0);
            // SAFETY: `sd` was returned by GetSecurityInfo and is freed below.
            if unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if dacl.is_null() {
                return Err(io::Error::other("null DACL"));
            }
            let mut aces = Vec::new();
            // SAFETY: `dacl` points into `sd`, which is alive.
            for i in 0..u32::from(unsafe { (*dacl).AceCount }) {
                let mut ace = std::ptr::null_mut();
                // SAFETY: `i` is below the ACE count.
                if unsafe { GetAce(dacl, i, &mut ace) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: every ACE starts with an ACE_HEADER; the allowed-ACE layout
                // is read only after checking its type (0, ACCESS_ALLOWED_ACE_TYPE).
                let header = unsafe { &*ace.cast::<ACE_HEADER>() };
                let (sid, mask) = if header.AceType == 0 {
                    // SAFETY: as above; the SID starts at `SidStart`.
                    let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
                    let sid = std::ptr::from_ref(&allowed.SidStart).cast_mut().cast();
                    (sid_string(sid)?, allowed.Mask)
                } else {
                    (String::new(), 0)
                };
                aces.push(Ace {
                    sid,
                    kind: header.AceType,
                    flags: header.AceFlags,
                    mask,
                });
            }
            Ok(Info {
                owner: sid_string(owner)?,
                protected: control & SE_DACL_PROTECTED != 0,
                aces,
            })
        })();
        // SAFETY: `sd` was allocated by GetSecurityInfo with LocalAlloc.
        unsafe { LocalFree(sd) };
        result
    }

    pub(crate) fn sid_string(sid: PSID) -> io::Result<String> {
        let mut wide = std::ptr::null_mut();
        // SAFETY: `sid` is valid for the caller; `wide` is an out pointer.
        if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0;
        // SAFETY: the returned string is NUL-terminated.
        while unsafe { *wide.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` characters before the NUL are readable.
        let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(wide, len) });
        // SAFETY: the string was allocated with LocalAlloc.
        unsafe { LocalFree(wide.cast()) };
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_sid_is_a_user_sid() {
        let sid = Sid::current_user().unwrap().to_string_form().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }

    #[test]
    fn local_system_is_s_1_5_18() {
        assert_eq!(
            Sid::local_system().unwrap().to_string_form().unwrap(),
            "S-1-5-18"
        );
    }

    #[test]
    fn descriptors_build() {
        Descriptor::owner_only_dir().unwrap();
        Descriptor::owner_inheriting().unwrap();
    }
}
