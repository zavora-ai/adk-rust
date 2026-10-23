//! The control plane's named pipe on Windows, the Unix socket's counterpart with its OS-user
//! trust rule (spec 3.2 §7.11, §10 W2, W4).
//!
//! | Guard | What it stops |
//! |---|---|
//! | A protected DACL with one entry, full access for the user the kernel runs as, who is also the pipe's owner | Any other account opening the pipe or creating an instance of it |
//! | The token a connection was opened with must be that user's: the server impersonates the client at identification level after its first message and compares the token's user SID | A client the DACL let through under another identity |
//! | The client talks to the pipe only when its owner is the user running the client | Another account's process that took the name first answering in the kernel's place |
//! | `FILE_FLAG_FIRST_PIPE_INSTANCE` on the first instance, remote clients rejected | Joining a pipe someone else created; callers from another machine |

use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, HANDLE, LocalFree, WIN32_ERROR};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, RevertToSelf, SECURITY_ATTRIBUTES,
    TOKEN_QUERY,
};
use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

use super::ControlTransport;
use super::server::{admit, operator, os_user};
use crate::error::{KernelError, Result};
use crate::win::{UserSid, wide};

/// How long a connection may take to send its first bytes before it is dropped.
const FIRST_MESSAGE: Duration = Duration::from_secs(10);

/// How long a client waits while every instance of the pipe is busy.
const BUSY_WAIT: Duration = Duration::from_secs(5);

/// A self-relative security descriptor from `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
pub(super) struct Descriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is immutable memory owned by this value alone.
unsafe impl Send for Descriptor {}
// SAFETY: as above; readers never write through the pointer.
unsafe impl Sync for Descriptor {}

impl Descriptor {
    /// Owned by `user`, with a protected DACL whose one entry grants `user` full access.
    pub(super) fn owner_only(user: &UserSid) -> io::Result<Self> {
        let sid = user.to_sid_string()?;
        Self::from_sddl(&format!("O:{sid}D:P(A;;FA;;;{sid})"))
    }

    /// The descriptor an SDDL string describes.
    pub(super) fn from_sddl(sddl: &str) -> io::Result<Self> {
        let sddl = wide(sddl);
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; the descriptor the call allocates is freed on drop.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    /// Creates an instance of the pipe `name` with this descriptor.
    fn create(&self, name: &str, first: bool) -> io::Result<NamedPipeServer> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        };
        // SAFETY: `attributes` and the descriptor it points to outlive the call.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated by the conversion and is freed once.
        unsafe { LocalFree(self.0) };
    }
}

/// Serves `api` on the pipe `name` with an owner-only descriptor, admitting connections
/// opened as the user the kernel runs as.
pub(super) async fn serve(
    name: &str,
    api: Router,
    stop: CancellationToken,
) -> Result<JoinHandle<()>> {
    let owner = UserSid::current()?;
    let descriptor = Descriptor::owner_only(&owner)?;
    serve_with(name, api, stop, descriptor).await
}

/// [`serve`] with `descriptor` on every instance.
pub(super) async fn serve_with(
    name: &str,
    api: Router,
    stop: CancellationToken,
    descriptor: Descriptor,
) -> Result<JoinHandle<()>> {
    let owner = Arc::new(UserSid::current()?);
    let first = match descriptor.create(name, true) {
        Ok(first) => first,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(taken(name).await);
        }
        Err(error) => {
            return Err(KernelError::ControlPlane {
                reason: format!("could not create the kernel pipe {name}: {error}"),
            });
        }
    };
    let name = name.to_string();
    let user = os_user();
    Ok(tokio::spawn(async move {
        let mut listening = first;
        let mut connections = JoinSet::new();
        loop {
            let connected = tokio::select! {
                connected = listening.connect() => connected,
                () = stop.cancelled() => break,
            };
            // A new instance listens before the connected one is served, so the pipe never
            // disappears between clients; an instance whose connection failed is replaced.
            let next = loop {
                match descriptor.create(&name, false) {
                    Ok(next) => break next,
                    Err(error) => {
                        tracing::error!(error = %error, "could not create a kernel pipe instance");
                        tokio::select! {
                            () = tokio::time::sleep(Duration::from_millis(200)) => {}
                            () = stop.cancelled() => return,
                        }
                    }
                }
            };
            let instance = std::mem::replace(&mut listening, next);
            if let Err(error) = connected {
                tracing::debug!(error = %error, "a kernel pipe client left before it connected");
                continue;
            }
            connections.spawn(serve_connection(
                instance,
                owner.clone(),
                user.clone(),
                api.clone(),
                stop.clone(),
            ));
            while connections.try_join_next().is_some() {}
        }
        while connections.join_next().await.is_some() {}
    }))
}

/// Why the pipe `name` could not be created: another kernel of this user answers on it, or
/// another account holds the name.
async fn taken(name: &str) -> KernelError {
    let reason = match open(name).await {
        Ok(_) => format!(
            "a kernel already listens on {name}: attach to it with `adk-rust attach`, or stop it \
             before starting another with the same home"
        ),
        Err(error) => format!(
            "another process holds the kernel pipe {name} ({error}); find it with \
             `Get-ChildItem \\\\.\\pipe\\`, or give the kernel another home"
        ),
    };
    KernelError::ControlPlane { reason }
}

/// The first bytes the client sent, or `None` for a client that sent nothing in time: a
/// probe such as `ControlClient::connect_local`, or an idle peer.
async fn first_message(pipe: &NamedPipeServer) -> Option<Vec<u8>> {
    let mut first = vec![0u8; 8192];
    let read = async {
        loop {
            pipe.readable().await?;
            match pipe.try_read(&mut first) {
                Ok(read) => return Ok(read),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) => return Err(error),
            }
        }
    };
    match tokio::time::timeout(FIRST_MESSAGE, read).await {
        Ok(Ok(read)) if read > 0 => {
            first.truncate(read);
            Some(first)
        }
        _ => None,
    }
}

/// Whether the token the client opened `pipe` with is `owner`'s.
///
/// Impersonation needs a message from the client first, which [`first_message`] read; the
/// client's tokio default grants identification level, which reads the token and nothing
/// more. The thread reverts before this returns, and the process aborts when it cannot.
fn client_is(pipe: HANDLE, owner: &UserSid) -> bool {
    // SAFETY: `pipe` is a connected server instance this process owns.
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        tracing::warn!(error = %io::Error::last_os_error(), "could not identify a kernel pipe client");
        return false;
    }
    let mut token = std::ptr::null_mut();
    // SAFETY: the pseudo-handle needs no closing; `token` is writable. `OpenAsSelf` is TRUE
    // because an identification-level token cannot open objects as the client.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut token) };
    // SAFETY: `token` was just opened and nothing else owns it.
    let opened = (opened != 0).then(|| unsafe { OwnedHandle::from_raw_handle(token) });
    // SAFETY: reverts the impersonation this thread started above.
    if unsafe { RevertToSelf() } == 0 {
        // Running on as the client would be worse than stopping.
        std::process::abort();
    }
    match opened {
        Some(token) => {
            UserSid::of_token(token.as_raw_handle()).is_ok_and(|user| owner.is(user.sid()))
        }
        None => false,
    }
}

/// Serves one connection, as the OS user `user` when the client's token is `owner`'s.
async fn serve_connection(
    pipe: NamedPipeServer,
    owner: Arc<UserSid>,
    user: String,
    api: Router,
    stop: CancellationToken,
) {
    let Some(first) = first_message(&pipe).await else {
        return;
    };
    let context = client_is(pipe.as_raw_handle(), &owner).then(|| operator(&user));
    let app = api.layer(middleware::from_fn(move |request: Request<Body>, next: Next| {
        let context = context.clone();
        async move {
            match context {
                Some(context) => admit(request, next, context, ControlTransport::Pipe).await,
                None => KernelError::Unauthorized {
                    reason: "the kernel pipe admits only the OS user that runs the kernel".into(),
                }
                .into_response(),
            }
        }
    }));
    let (read, write) = tokio::io::split(pipe);
    let io = tokio::io::join(std::io::Cursor::new(first).chain(read), write);
    let connection = hyper::server::conn::http1::Builder::new().serve_connection(
        hyper_util::rt::TokioIo::new(io),
        hyper_util::service::TowerToHyperService::new(app),
    );
    tokio::pin!(connection);
    tokio::select! {
        served = connection.as_mut() => {
            if let Err(error) = served {
                tracing::debug!(error = %error, "kernel pipe connection closed");
            }
        }
        () = stop.cancelled() => {
            connection.as_mut().graceful_shutdown();
            let _ = connection.await;
        }
    }
}

/// Opens the pipe `name` as a client, waiting while every instance is busy, and only when
/// its owner is the user running this process.
///
/// # Errors
///
/// `NotFound` when no pipe has the name, `PermissionDenied` when the DACL refuses this user
/// or another account owns the pipe, and the system's error otherwise.
pub(super) async fn open(name: &str) -> io::Result<NamedPipeClient> {
    let deadline = tokio::time::Instant::now() + BUSY_WAIT;
    let client = loop {
        match ClientOptions::new().open(name) {
            Ok(client) => break client,
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error),
        }
    };
    if !owned_by(client.as_raw_handle(), &UserSid::current()?)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{name} is owned by another account, so it is not this user's kernel"),
        ));
    }
    Ok(client)
}

/// Whether the kernel object `handle` is owned by `user`.
fn owned_by(handle: HANDLE, user: &UserSid) -> io::Result<bool> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the out-pointers are writable; the descriptor the call allocates is freed
    // below, and `owner` points into it.
    let status: WIN32_ERROR = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let owned = user.is(owner);
    // SAFETY: `descriptor` was allocated by `GetSecurityInfo` and is freed once.
    unsafe { LocalFree(descriptor) };
    Ok(owned)
}

/// One HTTP/1.1 exchange over the pipe `name`.
pub(super) async fn request(
    name: &str,
    request: hyper::Request<http_body_util::Full<hyper::body::Bytes>>,
) -> std::result::Result<hyper::Response<hyper::body::Incoming>, String> {
    let pipe = open(name).await.map_err(|error| error.to_string())?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(pipe))
            .await
            .map_err(|error| error.to_string())?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::debug!(error = %error, "kernel pipe connection closed");
        }
    });
    sender.send_request(request).await.map_err(|error| error.to_string())
}

/// Whether a kernel answers on the pipe `name`: `Ok(false)` when no pipe has the name.
///
/// # Errors
///
/// The pipe exists but this user may not use it.
pub(super) async fn answers(name: &str) -> io::Result<bool> {
    match open(name).await {
        Ok(mut pipe) => {
            // A probe sends nothing; closing it at once lets the server drop it.
            let _ = pipe.shutdown().await;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
pub(super) mod testing {
    //! Checks the pipe tests make on Windows.

    use super::*;
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

    /// Whether the pipe `handle` carries the owner-only descriptor: owned by the current
    /// user, with a protected DACL of one entry granting that user full access.
    pub(crate) fn is_owner_only(handle: HANDLE) -> io::Result<bool> {
        let user = UserSid::current()?;
        let (mut owner, mut dacl, mut descriptor): (PSID, *mut ACL, PSECURITY_DESCRIPTOR) =
            (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: the out-pointers are writable; the descriptor is freed below.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &raw mut owner,
                std::ptr::null_mut(),
                &raw mut dacl,
                std::ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let (mut control, mut revision, mut size) = (0u16, 0u32, ACL_SIZE_INFORMATION::default());
        let mut ace = std::ptr::null_mut();
        // SAFETY: `descriptor`, `owner` and `dacl` stay valid until `LocalFree`; every
        // out-pointer is writable.
        let owner_only = unsafe {
            user.is(owner)
                && !dacl.is_null()
                && GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision)
                    != 0
                && GetAclInformation(
                    dacl,
                    (&raw mut size).cast(),
                    size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                ) != 0
                && control & SE_DACL_PROTECTED != 0
                && size.AceCount == 1
                && GetAce(dacl, 0, &raw mut ace) != 0
                && {
                    let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
                    u32::from((*ace).Header.AceType) == ACCESS_ALLOWED_ACE_TYPE
                        && (*ace).Mask == FILE_ALL_ACCESS
                        && user.is((&raw mut (*ace).SidStart).cast())
                }
        };
        // SAFETY: `descriptor` was allocated by `GetSecurityInfo` and is freed once.
        unsafe { LocalFree(descriptor) };
        Ok(owner_only)
    }
}
