//! Platform facts the presentation layer needs but cannot observe for itself.
//!
//! Right now that is exactly one thing: the **integrity level** of the running
//! process. Windows refuses to let a Low-integrity process write to any object
//! that is not itself Low-labelled — and a sandboxed or restricted launcher is
//! precisely what produces a Low token. When that happens, *every* ordinary
//! folder (`C:\Users\…\Videos`, `D:\CODE`, the desktop, …) answers
//! `拒绝访问。 (os error 5)`, no matter how permissive its ACL looks, because the
//! mandatory check runs before the discretionary one.
//!
//! That is worth naming, because the symptom is indistinguishable from "this
//! folder has no write permission" and sends people looking for a permission
//! problem that does not exist. The kernel reports the fact here; the front end
//! decides how to word it.

/// How far the operating system trusts this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integrity {
    /// Not readable: a non-Windows build, or the query itself failed.
    Unknown,
    /// Writes to ordinary folders are refused by the mandatory policy.
    Low,
    /// A normal interactive process.
    Medium,
    /// Elevated.
    High,
    /// A system service.
    System,
}

impl Integrity {
    /// True when the mandatory policy will refuse writes to ordinary folders.
    pub fn is_low(self) -> bool {
        matches!(self, Integrity::Low)
    }

    /// Short label for a log line or a self-check.
    pub fn label(self) -> &'static str {
        match self {
            Integrity::Unknown => "unknown",
            Integrity::Low => "low",
            Integrity::Medium => "medium",
            Integrity::High => "high",
            Integrity::System => "system",
        }
    }

    /// Stable code for the C ABI: 0 unknown, 1 low, 2 medium, 3 high, 4 system.
    pub fn code(self) -> i32 {
        match self {
            Integrity::Unknown => 0,
            Integrity::Low => 1,
            Integrity::Medium => 2,
            Integrity::High => 3,
            Integrity::System => 4,
        }
    }

    /// The level a C ABI code stands for.
    pub fn from_code(code: i32) -> Integrity {
        match code {
            1 => Integrity::Low,
            2 => Integrity::Medium,
            3 => Integrity::High,
            4 => Integrity::System,
            _ => Integrity::Unknown,
        }
    }
}

/// `S-1-16-4096` (low), `S-1-16-8192` (medium), … — the integrity level is
/// encoded as the last sub-authority of the token's mandatory label SID.
#[cfg(windows)]
const SECURITY_MANDATORY_LOW_RID: u32 = 0x0000_1000;
#[cfg(windows)]
const SECURITY_MANDATORY_MEDIUM_RID: u32 = 0x0000_2000;
#[cfg(windows)]
const SECURITY_MANDATORY_HIGH_RID: u32 = 0x0000_3000;
#[cfg(windows)]
const SECURITY_MANDATORY_SYSTEM_RID: u32 = 0x0000_4000;

/// The integrity level of the current process.
#[cfg(windows)]
pub fn integrity() -> Integrity {
    use std::ffi::c_void;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(
            process: *mut c_void,
            desired_access: u32,
            token: *mut *mut c_void,
        ) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: u32,
            info: *mut c_void,
            len: u32,
            needed: *mut u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_INTEGRITY_LEVEL: u32 = 25;

    /// The last sub-authority of a SID, which is where the integrity RID lives.
    ///
    /// ```text
    /// SID = Revision(1) Count(1) IdentifierAuthority(6) SubAuthority[Count](4 each)
    /// ```
    ///
    /// # Safety
    ///
    /// `sid` must point at a valid SID, as returned by `GetTokenInformation`.
    unsafe fn last_sub_authority(sid: *const u8) -> Option<u32> {
        // SAFETY: the caller guarantees a valid SID; the layout is fixed by
        // Windows and the count byte bounds the read.
        unsafe {
            let count = *sid.add(1) as usize;
            if count == 0 {
                return None;
            }
            let at = sid.add(8 + (count - 1) * 4);
            let mut bytes = [0u8; 4];
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = *at.add(i);
            }
            Some(u32::from_le_bytes(bytes))
        }
    }

    // SAFETY: every call is checked, the token is closed on every path that
    // opened it, the buffer is sized by the API itself, and the only raw pointer
    // dereference happens inside `last_sub_authority`, which is documented above
    // and only ever sees a SID Windows just handed us.
    unsafe {
        let mut token: *mut c_void = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Integrity::Unknown;
        }

        // Ask for the size first: `TokenIntegrityLevel` returns a
        // `TOKEN_MANDATORY_LABEL` whose SID lives *inside the same buffer*, so a
        // buffer of just `size_of::<SID_AND_ATTRIBUTES>()` is too small and the
        // call fails with ERROR_INSUFFICIENT_BUFFER. `needed` covers the SID too.
        let mut needed: u32 = 0;
        GetTokenInformation(
            token,
            TOKEN_INTEGRITY_LEVEL,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
        if needed == 0 {
            CloseHandle(token);
            return Integrity::Unknown;
        }

        let mut buffer = vec![0u8; needed as usize];
        let ok = GetTokenInformation(
            token,
            TOKEN_INTEGRITY_LEVEL,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            &mut needed,
        );
        CloseHandle(token);
        if ok == 0 {
            return Integrity::Unknown;
        }

        // The buffer starts with `SID_AND_ATTRIBUTES { PSID Sid; DWORD Attributes; }`.
        let sid = buffer.as_ptr().cast::<*const u8>().read();
        if sid.is_null() {
            return Integrity::Unknown;
        }

        match last_sub_authority(sid) {
            Some(rid) => {
                match rid {
                    r if r >= SECURITY_MANDATORY_SYSTEM_RID => Integrity::System,
                    r if r >= SECURITY_MANDATORY_HIGH_RID => Integrity::High,
                    r if r >= SECURITY_MANDATORY_MEDIUM_RID => Integrity::Medium,
                    r if r >= SECURITY_MANDATORY_LOW_RID => Integrity::Low,
                    _ => Integrity::Unknown,
                }
            }
            None => Integrity::Unknown,
        }
    }
}

/// The integrity level of the current process.
#[cfg(not(windows))]
pub fn integrity() -> Integrity {
    Integrity::Unknown
}

/// True when writes to ordinary folders will be refused by the mandatory policy.
pub fn is_low_integrity() -> bool {
    integrity().is_low()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        for level in [
            Integrity::Unknown,
            Integrity::Low,
            Integrity::Medium,
            Integrity::High,
            Integrity::System,
        ] {
            assert_eq!(Integrity::from_code(level.code()), level);
        }
    }

    #[test]
    fn only_low_is_flagged() {
        assert!(Integrity::Low.is_low());
        assert!(!Integrity::Medium.is_low());
        assert!(!Integrity::Unknown.is_low());
    }

    #[cfg(windows)]
    #[test]
    fn the_current_process_has_a_readable_integrity_level() {
        // Whatever this machine is doing, the query itself must work — an
        // `Unknown` here would silently disable the diagnosis in the front end.
        let level = integrity();
        assert_ne!(level, Integrity::Unknown, "the integrity query failed");
    }
}
