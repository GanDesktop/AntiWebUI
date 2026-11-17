//! Window relationships across applications, using portal parent identifiers.
use crate::{
    platform::PlatformRequest,
    reactive::on_cleanup,
    task::{Completer, Task},
    tree::with_doc,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    thread::LocalKey,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalParent {
    Wayland(String),
}
impl std::str::FromStr for ExternalParent {
    type Err = WindowIntegrationError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(handle) = value
            .strip_prefix("wayland:")
            .filter(|h| !h.is_empty() && !h.contains('\0'))
        {
            return Ok(Self::Wayland(handle.into()));
        }
        Err(WindowIntegrationError::InvalidHandle)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowIntegrationError {
    Unsupported,
    InvalidHandle,
}
impl std::fmt::Display for WindowIntegrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WindowIntegrationError {}

#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum ExternalWindowRequest {
    Parent {
        id: u64,
        parent: Option<ExternalParent>,
        modal: bool,
    },
    Activate {
        id: u64,
        token: String,
    },
    ActivationToken {
        id: u64,
    },
    ActivateWithToken(String),
}
impl ExternalWindowRequest {
    pub fn unsupported(self) {
        let error = WindowIntegrationError::Unsupported;
        match self {
            Self::Parent { id, .. } | Self::Activate { id, .. } => {
                complete_external(id, Err(error))
            }
            Self::ActivationToken { id } => complete_activation_token(id, Err(error)),
            Self::ActivateWithToken(_) => {}
        }
    }
}

type Pending<T> = RefCell<HashMap<u64, Completer<Result<T, WindowIntegrationError>>>>;
thread_local! {
    static NEXT: Cell<u64> = const { Cell::new(0) };
    static PENDING: Pending<()> = RefCell::default();
    static TOKENS: Pending<String> = RefCell::default();
}
fn request<T: Send + 'static>(
    pending: &'static LocalKey<Pending<T>>,
    make: impl FnOnce(u64) -> ExternalWindowRequest,
) -> Task<Result<T, WindowIntegrationError>> {
    let id = NEXT.with(|n| n.replace(n.get() + 1));
    let (task, done) = Task::pending();
    pending.with(|p| p.borrow_mut().insert(id, done));
    on_cleanup(move || {
        pending.with(|p| p.borrow_mut().remove(&id));
    });
    with_doc(|d| d.request(PlatformRequest::ExternalWindow(make(id))));
    task
}
/// Replaces the current window's native parent. `None` detaches and removes modal state.
///
/// Call from the window's root builder for a portal chooser. Success means that the
/// request was submitted, not that the compositor accepted the foreign handle. Wayland
/// clears the association and modal state if the exporting window disappears. This does
/// not make the foreign window part of AntiWebUI's ownership or input-disabling graph.
pub fn set_external_parent(
    parent: Option<ExternalParent>,
    modal: bool,
) -> Task<Result<(), WindowIntegrationError>> {
    request(&PENDING, move |id| ExternalWindowRequest::Parent {
        id,
        parent,
        modal,
    })
}
/// Requests Wayland activation with a token supplied by the launching application.
/// The token is never read implicitly from the process environment.
/// Success does not guarantee focus: the compositor decides.
pub fn activate_window(token: String) -> Task<Result<(), WindowIntegrationError>> {
    if !valid_token(&token) {
        return Task::ready(Err(WindowIntegrationError::InvalidHandle));
    }
    request(&PENDING, move |id| ExternalWindowRequest::Activate {
        id,
        token,
    })
}

impl super::WindowHandle {
    /// Requests activation of this window using an input event from the current window.
    ///
    /// Call from the window handling the user's action so it can supply the Wayland
    /// surface and seat serial. The token is sent to the target window's own connection.
    /// Success means the activation was queued. The compositor decides whether to focus it.
    pub fn request_activation(&self) -> Task<Result<(), WindowIntegrationError>> {
        if !self.is_open() {
            return Task::ready(Err(WindowIntegrationError::InvalidHandle));
        }
        let target = self.clone();
        request(&TOKENS, |id| ExternalWindowRequest::ActivationToken { id }).map(move |result| {
            let token = result?;
            if !target.is_open() || !valid_token(&token) {
                return Err(WindowIntegrationError::InvalidHandle);
            }
            target.send(super::Command::ActivateWithToken(token));
            Ok(())
        })
    }
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && !token.contains('\0')
}

#[doc(hidden)]
pub fn complete_external(id: u64, result: Result<(), WindowIntegrationError>) {
    complete(&PENDING, id, result);
}

pub(crate) fn complete_activation_token(id: u64, result: Result<String, WindowIntegrationError>) {
    complete(&TOKENS, id, result);
}

fn complete<T: Send + 'static>(
    pending: &'static LocalKey<Pending<T>>,
    id: u64,
    result: Result<T, WindowIntegrationError>,
) {
    let done = pending.with(|p| p.borrow_mut().remove(&id));
    if let Some(done) = done {
        done.complete(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portal_handles_keep_their_native_identity() {
        assert_eq!(
            "wayland:opaque-handle".parse(),
            Ok(ExternalParent::Wayland("opaque-handle".into()))
        );
        assert!("x11:1a2b".parse::<ExternalParent>().is_err());
        assert!("wayland:".parse::<ExternalParent>().is_err());
        assert!("wayland:bad\0handle".parse::<ExternalParent>().is_err());
    }
}
