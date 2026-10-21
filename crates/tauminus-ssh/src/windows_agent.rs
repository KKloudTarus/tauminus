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
//! Windows runs ssh-agent as a service.

use std::ffi::OsString;
use std::path::PathBuf;

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

/// Whether a pipe owner passes: the current user, or for `UserOrSystem`
/// also LocalSystem or Administrators.
pub(crate) const fn owner_allowed(
    owners: Owners,
    is_user: bool,
    is_system_or_admins: bool,
) -> bool {
    is_user || (matches!(owners, Owners::UserOrSystem) && is_system_or_admins)
}

/// Pageant's pipe name: `\\.\pipe\pageant.<user>.<suffix>`, where the suffix
/// is the hex SHA-256 of the 32-bit big-endian length of `obfuscated`
/// followed by `obfuscated` itself, the string "Pageant" run through
/// `CryptProtectMemory`. That is PuTTY's `agent_named_pipe_name`. Only
/// processes of the same logon session compute the same suffix.
pub(crate) fn pageant_pipe_name(user: &str, obfuscated: &[u8]) -> String {
    let length = u32::try_from(obfuscated.len()).unwrap_or(u32::MAX);
    let mut hash = Sha256::new();
    hash.update(length.to_be_bytes());
    hash.update(obfuscated);
    let suffix = HEXLOWER.encode(&hash.finalize());
    // PuTTY drops the domain of a user principal name.
    let user = user.split_once('@').map_or(user, |(name, _)| name);
    format!(r"\\.\pipe\pageant.{user}.{suffix}")
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
    use std::ffi::{CStr, OsStr};
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::ptr;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_BUSY, ERROR_SUCCESS, HANDLE, LocalFree,
    };
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

    use super::{Candidate, Owners, owner_allowed, pageant_pipe_name, pageant_plaintext};

    /// Opens the candidate's pipe and checks its owner.
    pub(crate) async fn connect(candidate: &Candidate) -> io::Result<NamedPipeClient> {
        let path = match candidate {
            Candidate::Pipe(path) => path.clone().into_os_string(),
            Candidate::Pageant => pageant_pipe()?.into(),
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

    /// Pageant's pipe name for this user and logon session.
    fn pageant_pipe() -> io::Result<String> {
        let user = user_name()?;
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
        // Without the obfuscation the name is guessable; do not fall back.
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(pageant_pipe_name(&user, &block))
    }

    /// The user principal name if the machine has one, else the account
    /// name, as PuTTY picks.
    fn user_name() -> io::Result<String> {
        let mut buffer = [0_u8; 512];
        let mut length = 512_u32;
        // SAFETY: the buffer is writable for `length` bytes.
        if unsafe { GetUserNameExA(NameUserPrincipal, buffer.as_mut_ptr(), &raw mut length) } {
            return c_string(&buffer);
        }
        let mut length = 512_u32;
        // SAFETY: as above.
        if unsafe { GetUserNameA(buffer.as_mut_ptr(), &raw mut length) } == 0 {
            return Err(io::Error::last_os_error());
        }
        c_string(&buffer)
    }

    fn c_string(buffer: &[u8]) -> io::Result<String> {
        CStr::from_bytes_until_nul(buffer)
            .ok()
            .and_then(|name| name.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "user name is not UTF-8"))
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
    fn pageant_pipe_name_matches_putty() {
        // sha256 of 00 00 00 10 followed by 16 zero bytes, from Python's hashlib.
        let name = pageant_pipe_name("alice@corp.example", &[0; 16]);
        assert_eq!(
            name,
            r"\\.\pipe\pageant.alice.0d3fc7ca63979d9b01e0546b5523c37c6230cee3f3d053cd9b71f51ff98191b2"
        );
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
    async fn a_missing_agent_is_an_error() {
        let missing = Candidate::Pipe(r"\\.\pipe\tauminus-no-such-agent".into());
        assert!(connect(&missing).await.is_err());
    }
}
