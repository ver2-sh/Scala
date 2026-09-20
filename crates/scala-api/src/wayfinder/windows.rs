use super::{ApplicationStream, Result};
use std::{os::windows::io::AsRawHandle, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree},
    Security::{
        Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT},
        EqualSid, GetTokenInformation, IsWellKnownSid, OWNER_SECURITY_INFORMATION, TOKEN_QUERY,
        TOKEN_USER, TokenUser, WinLocalSystemSid,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

/// Trust only a pipe owned by our account or LocalSystem. No private Wayfinder
/// files or credentials are needed, including when the daemon is absent.
pub(super) fn verify_owner(stream: &ApplicationStream) -> Result<()> {
    // SAFETY: all out pointers are valid; the pipe remains alive throughout.
    // Token storage is pointer-aligned and sized by the OS. Handles and the
    // allocated descriptor are released on success and failure.
    unsafe {
        let mut owner = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        let error = GetSecurityInfo(
            stream.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        );
        if error != 0 {
            return Err(format!("Cannot verify Wayfinder pipe owner: {error}"));
        }
        let result = (|| -> Result<()> {
            if owner.is_null() {
                return Err("Missing Wayfinder pipe owner".into());
            }
            if IsWellKnownSid(owner, WinLocalSystemSid) != 0 {
                return Ok(());
            }
            let mut token = ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let checked = (|| -> Result<()> {
                let mut size = 0;
                GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
                if size == 0 {
                    return Err("Cannot size application account token".into());
                }
                let mut buffer =
                    vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
                if GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    size,
                    &mut size,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
                if EqualSid(owner, user.User.Sid) == 0 {
                    return Err(
                        "Wayfinder application pipe must belong to the same account or LocalSystem"
                            .into(),
                    );
                }
                Ok(())
            })();
            CloseHandle(token);
            checked
        })();
        LocalFree(descriptor);
        result
    }
}
