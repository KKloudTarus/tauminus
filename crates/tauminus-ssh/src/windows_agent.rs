// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Finding an SSH agent on Windows.
//!
//! With `SSH_AUTH_SOCK` set, that pipe is the agent, as OpenSSH for Windows
//! reads it. Otherwise OpenSSH's agent pipe comes first and PuTTY's Pageant
//! second.
//!
//! Anyone who knows a pipe's name can create it, and a client that trusts the
//! name alone hands its sign requests, and the list of hosts it signs in to,
//! to whoever got there first. As PuTTY does, the client checks who owns the
//! pipe it reached: Pageant's must belong to the current user. An OpenSSH
//! agent pipe may also belong to LocalSystem or Administrators, because
//! Windows runs ssh-agent as a service, and what LocalSystem creates belongs
//! to Administrators by default.
//!
//! Only local pipes are opened. A `\\host\pipe\...` path, or a `\\.\pipe\`
//! path that climbs out with `..`, would send the user's Windows credentials
//! to that host, and the remote side writes the owner the check reads.
//!
//! The check reads the pipe's owner, not the process serving it. That holds
//! because both real agents create their pipes with a DACL that lets no one
//! else add a pipe instance: Pageant grants only the user, and OpenSSH's
//! agent grants other users read and write but not `FILE_CREATE_PIPE_INSTANCE`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use data_encoding::HEXLOWER;
use sha2::{Digest, Sha256};

use crate::AgentSource;

/// The pipe the ssh-agent service of OpenSSH for Windows listens on.
pub(crate) const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// One place an agent may listen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Candidate {
    /// A named pipe of an OpenSSH-style agent.
    Pipe(PathBuf),
    /// Pageant's per-user pipe, from PuTTY 0.75 on.
    Pageant,
}

/// Who may own the pipe of a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owners {
    User,
    UserOrSystem,
}

impl Candidate {
    pub(crate) const fn owners(&self) -> Owners {
        match self {
            Self::Pipe(_) => Owners::UserOrSystem,
            Self::Pageant => Owners::User,
        }
    }
}

/// Where to look, in order, for `source` given `SSH_AUTH_SOCK`.
pub(crate) fn candidates(source: &AgentSource, auth_sock: Option<OsString>) -> Vec<Candidate> {
    match source {
        AgentSource::Environment => match auth_sock {
            Some(sock) if !sock.is_empty() => vec![Candidate::Pipe(sock.into())],
            _ => vec![
                Candidate::Pipe(OPENSSH_AGENT_PIPE.into()),
                Candidate::Pageant,
            ],
        },
        AgentSource::Path(path) => vec![Candidate::Pipe(path.clone())],
        AgentSource::Pageant => vec![Candidate::Pageant],
        AgentSource::None => vec![],
    }
}

/// Whether `path` names a pipe on this machine: `\\.\pipe\` followed by one
/// name with no `\` or `/` in it and that is not `.` or `..`. The prefix is
/// matched without regard to case, as Windows does.
pub(crate) fn is_local_pipe(path: &Path) -> bool {
    const PREFIX: &str = r"\\.\pipe\";
    let Some(text) = path.to_str() else {
        return false;
    };
    let Some(head) = text.get(..PREFIX.len()) else {
        return false;
    };
    let name = &text[PREFIX.len()..];
    head.eq_ignore_ascii_case(PREFIX)
        && !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['\\', '/'])
}

/// Whether a pipe owner passes: the current user, or for `UserOrSystem`
/// also LocalSystem or Administrators.
pub(crate) const fn owner_allowed(
    owners: Owners,
    is_user: bool,
    is_system_or_admins: bool,
) -> bool {
    is_user || (matches!(owners, Owners::UserOrSystem) && is_system_or_admins)
}

/// The user part of Pageant's pipe name: the ANSI bytes of the user name up
/// to the first NUL, cut at the first `@`, as PuTTY's `get_username` and
/// `agent_named_pipe_name` do with a user principal name.
pub(crate) fn pageant_user(ansi: &[u8]) -> &[u8] {
    let name = ansi.split(|&b| b == 0).next().unwrap_or_default();
    name.split(|&b| b == b'@').next().unwrap_or_default()
}

/// The last part of Pageant's pipe name: the hex SHA-256 of the 32-bit
/// big-endian length of `obfuscated` followed by `obfuscated`, the string
/// "Pageant" run through `CryptProtectMemory` with
/// `CRYPTPROTECTMEMORY_CROSS_PROCESS`. That key is the same for every
/// process on the machine until it restarts, so the suffix is no secret:
/// the owner check is what stops another user taking the name.
pub(crate) fn pageant_suffix(obfuscated: &[u8]) -> String {
    let length = u32::try_from(obfuscated.len()).unwrap_or(u32::MAX);
    let mut hash = Sha256::new();
    hash.update(length.to_be_bytes());
    hash.update(obfuscated);
    HEXLOWER.encode(&hash.finalize())
}

/// "Pageant" with its terminating NUL, zero-padded to one
/// `CryptProtectMemory` block of 16 bytes.
pub(crate) fn pageant_plaintext() -> [u8; 16] {
    let mut block = [0; 16];
    block[..7].copy_from_slice(b"Pageant");
    block
}

#[cfg(windows)]
pub(crate) use sys::connect;

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "Win32 calls that read a pipe's owner, the user's SID and Pageant's pipe name"
)]
mod sys {
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use std::ptr;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_BUSY, ERROR_SUCCESS, HANDLE, LocalFree,
    };
    use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};
    use windows_sys::Win32::Security::Authentication::Identity::{
        GetUserNameExA, NameUserPrincipal,
    };
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::Cryptography::{
        CRYPTPROTECTMEMORY_CROSS_PROCESS, CryptProtectMemory,
    };
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
        WinBuiltinAdministratorsSid, WinLocalSystemSid,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::Win32::System::WindowsProgramming::GetUserNameA;

    use super::{
        Candidate, Owners, is_local_pipe, owner_allowed, pageant_plaintext, pageant_suffix,
        pageant_user,
    };

    /// Opens the candidate's pipe and checks its owner.
    pub(crate) async fn connect(candidate: &Candidate) -> io::Result<NamedPipeClient> {
        let path = match candidate {
            Candidate::Pipe(path) if is_local_pipe(path) => path.clone().into_os_string(),
            Candidate::Pipe(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "only pipes on this machine (\\\\.\\pipe\\name) can be agents",
                ));
            }
            Candidate::Pageant => pageant_pipe()?,
        };
        let pipe = open(&path).await?;
        if !owned_by(&pipe, candidate.owners())? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the agent pipe belongs to another user",
            ));
        }
        Ok(pipe)
    }

    /// Opens a pipe, waiting up to a second while all its instances are busy.
    async fn open(path: &OsStr) -> io::Result<NamedPipeClient> {
        let mut tries = 0;
        loop {
            match ClientOptions::new().open(path) {
                Err(error)
                    if error.raw_os_error() == Some(ERROR_PIPE_BUSY.cast_signed())
                        && tries < 20 =>
                {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                result => return result,
            }
        }
    }

    fn owned_by(pipe: &NamedPipeClient, owners: Owners) -> io::Result<bool> {
        let user = UserSid::current()?;
        let mut owner: PSID = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: the handle is the open pipe, borrowed for this call. The
        // out pointers are live locals. On success the system allocates the
        // descriptor, `owner` points into it, and `Local` frees it below.
        let status = unsafe {
            GetSecurityInfo(
                pipe.as_raw_handle() as HANDLE,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &raw mut owner,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status.cast_signed()));
        }
        let _descriptor = Local(descriptor);
        if owner.is_null() {
            return Ok(false);
        }
        // SAFETY: `owner` points into the descriptor, which lives until the
        // end of this function, and `user.sid()` into the token buffer.
        let is_user = unsafe { EqualSid(owner, user.sid()) } != 0;
        // SAFETY: as above; IsWellKnownSid only reads the SID.
        let is_system_or_admins = unsafe {
            IsWellKnownSid(owner, WinLocalSystemSid) != 0
                || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
        };
        Ok(owner_allowed(owners, is_user, is_system_or_admins))
    }

    /// The `TOKEN_USER` of this process, in a buffer aligned for it.
    struct UserSid(Vec<u64>);

    impl UserSid {
        fn current() -> io::Result<Self> {
            let mut token: HANDLE = ptr::null_mut();
            // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
            // closing; `token` is a live out pointer.
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let _token = Handle(token);
            let mut length = 0;
            // SAFETY: a null buffer of length 0 only asks for the size; the
            // call fails with ERROR_INSUFFICIENT_BUFFER, which is expected.
            unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &raw mut length) };
            let mut buffer = vec![0_u64; (length as usize).div_ceil(8)];
            // SAFETY: the buffer holds at least `length` bytes, with the
            // 8-byte alignment TOKEN_USER needs.
            let ok = unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    length,
                    &raw mut length,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(buffer))
        }

        fn sid(&self) -> PSID {
            // SAFETY: GetTokenInformation wrote a TOKEN_USER at the start of
            // the buffer; its SID points into the same buffer.
            unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
        }
    }

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenProcessToken and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    struct Local(PSECURITY_DESCRIPTOR);

    impl Drop for Local {
        fn drop(&mut self) {
            // SAFETY: the descriptor came from GetSecurityInfo, which says to
            // free it with LocalFree, once.
            unsafe { LocalFree(self.0) };
        }
    }

    /// Pageant's pipe name for this user, as Pageant builds it.
    fn pageant_pipe() -> io::Result<OsString> {
        let ansi = user_name_ansi()?;
        let mut block = pageant_plaintext();
        // SAFETY: the buffer is writable and one CryptProtectMemory block
        // (16 bytes) long, as the call requires.
        let ok = unsafe {
            CryptProtectMemory(
                block.as_mut_ptr().cast(),
                16,
                CRYPTPROTECTMEMORY_CROSS_PROCESS,
            )
        };
        // Without it the name cannot match the one Pageant made.
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut name = OsString::from(r"\\.\pipe\pageant.");
        name.push(ansi_to_os(pageant_user(&ansi))?);
        name.push(".");
        name.push(pageant_suffix(&block));
        Ok(name)
    }

    /// The user principal name if the machine has one, else the account
    /// name, as PuTTY picks, in the ANSI code page PuTTY reads it in.
    fn user_name_ansi() -> io::Result<Vec<u8>> {
        let mut length = 0_u32;
        // SAFETY: a null buffer only asks for the size, in bytes with the NUL.
        unsafe { GetUserNameExA(NameUserPrincipal, ptr::null_mut(), &raw mut length) };
        if length > 0 {
            let mut buffer = vec![0_u8; length as usize + 1];
            let mut size = length;
            // SAFETY: the buffer is writable for `size` bytes.
            if unsafe { GetUserNameExA(NameUserPrincipal, buffer.as_mut_ptr(), &raw mut size) } {
                return Ok(buffer);
            }
        }
        // No principal name off a domain: the account name, at most 257 bytes.
        let mut buffer = vec![0_u8; 258];
        let mut size = 258_u32;
        // SAFETY: as above.
        if unsafe { GetUserNameA(buffer.as_mut_ptr(), &raw mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buffer)
    }

    /// ANSI code page bytes as the UTF-16 Windows uses for pipe names.
    fn ansi_to_os(ansi: &[u8]) -> io::Result<OsString> {
        if ansi.is_empty() {
            return Ok(OsString::new());
        }
        let length = i32::try_from(ansi.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "user name too long"))?;
        // SAFETY: the input is `length` readable bytes; a null output asks
        // for the size in UTF-16 units.
        let units =
            unsafe { MultiByteToWideChar(CP_ACP, 0, ansi.as_ptr(), length, ptr::null_mut(), 0) };
        if units <= 0 {
            return Err(io::Error::last_os_error());
        }
        let mut wide = vec![0_u16; units.unsigned_abs() as usize];
        // SAFETY: the output holds `units` UTF-16 units, as the first call asked.
        let written = unsafe {
            MultiByteToWideChar(CP_ACP, 0, ansi.as_ptr(), length, wide.as_mut_ptr(), units)
        };
        if written <= 0 {
            return Err(io::Error::last_os_error());
        }
        wide.truncate(written.unsigned_abs() as usize);
        Ok(OsString::from_wide(&wide))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_prefers_ssh_auth_sock_alone() {
        let found = candidates(&AgentSource::Environment, Some(r"\\.\pipe\my-agent".into()));
        assert_eq!(found, [Candidate::Pipe(r"\\.\pipe\my-agent".into())]);
    }

    #[test]
    fn environment_tries_openssh_then_pageant() {
        for unset in [None, Some(OsString::new())] {
            assert_eq!(
                candidates(&AgentSource::Environment, unset),
                [
                    Candidate::Pipe(OPENSSH_AGENT_PIPE.into()),
                    Candidate::Pageant
                ]
            );
        }
    }

    #[test]
    fn explicit_sources_use_only_themselves() {
        let sock = Some(r"\\.\pipe\ignored".into());
        assert_eq!(
            candidates(&AgentSource::Pageant, sock.clone()),
            [Candidate::Pageant]
        );
        assert_eq!(
            candidates(&AgentSource::Path(r"\\.\pipe\x".into()), sock.clone()),
            [Candidate::Pipe(r"\\.\pipe\x".into())]
        );
        assert_eq!(candidates(&AgentSource::None, sock), []);
    }

    #[test]
    fn only_pipes_on_this_machine_are_agents() {
        for good in [
            OPENSSH_AGENT_PIPE,
            r"\\.\PIPE\openssh-ssh-agent",
            r"\\.\pipe\1password.agent",
        ] {
            assert!(is_local_pipe(Path::new(good)), "{good}");
        }
        for bad in [
            r"\\attacker.example\pipe\agent",
            r"\\.\pipe\..\UNC\attacker.example\pipe\agent",
            r"\\.\pipe\sub\agent",
            r"\\.\pipe\a/b",
            r"\\.\pipe\..",
            r"\\.\pipe\.",
            r"\\.\pipe\",
            r"\\?\pipe\agent",
            r"C:\agent",
            "",
        ] {
            assert!(!is_local_pipe(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn pageant_must_belong_to_the_user_and_an_openssh_pipe_may_belong_to_the_system() {
        assert!(owner_allowed(Owners::User, true, false));
        assert!(!owner_allowed(Owners::User, false, true));
        assert!(!owner_allowed(Owners::User, false, false));
        assert!(owner_allowed(Owners::UserOrSystem, false, true));
        assert!(!owner_allowed(Owners::UserOrSystem, false, false));
        assert_eq!(Candidate::Pageant.owners(), Owners::User);
        assert_eq!(
            Candidate::Pipe(OPENSSH_AGENT_PIPE.into()).owners(),
            Owners::UserOrSystem
        );
    }

    #[test]
    fn pageant_name_parts_match_putty() {
        // sha256 of 00 00 00 10 followed by 16 zero bytes, from Python's hashlib.
        assert_eq!(
            pageant_suffix(&[0; 16]),
            "0d3fc7ca63979d9b01e0546b5523c37c6230cee3f3d053cd9b71f51ff98191b2"
        );
        assert_eq!(pageant_user(b"alice@corp.example\0junk"), b"alice");
        assert_eq!(pageant_user(b"Ngh\xeca\0"), b"Ngh\xeca");
        assert_eq!(pageant_user(b"\0"), b"");
        assert_eq!(&pageant_plaintext()[..8], b"Pageant\0");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_pipe_this_user_created_is_accepted() {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = format!(r"\\.\pipe\tauminus-test-{}", std::process::id());
        let _server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        connect(&Candidate::Pipe(name.into()))
            .await
            .expect("our own pipe passes the owner check");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_missing_or_remote_agent_is_an_error() {
        let missing = Candidate::Pipe(r"\\.\pipe\tauminus-no-such-agent".into());
        assert!(connect(&missing).await.is_err());
        let remote = Candidate::Pipe(r"\\192.0.2.1\pipe\agent".into());
        let error = connect(&remote).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
