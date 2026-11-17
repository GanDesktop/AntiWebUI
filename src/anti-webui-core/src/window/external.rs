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
}
impl ExternalWindowRequest {
    pub fn id(&self) -> u64 {
        match self {
            Self::Parent { id, .. } | Self::Activate { id, .. } => *id,
        }
    }
}
type Pending = HashMap<u64, Completer<Result<(), WindowIntegrationError>>>;
thread_local! {
    static NEXT: Cell<u64> = const { Cell::new(0) };
    static PENDING: RefCell<Pending> = RefCell::default();
}
fn request(
    make: impl FnOnce(u64) -> ExternalWindowRequest,
) -> Task<Result<(), WindowIntegrationError>> {
    let id = NEXT.with(|n| n.replace(n.get() + 1));
    let (task, done) = Task::pending();
    PENDING.with(|p| p.borrow_mut().insert(id, done));
    on_cleanup(move || {
        PENDING.with(|p| p.borrow_mut().remove(&id));
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
    request(move |id| ExternalWindowRequest::Parent { id, parent, modal })
}
/// Requests Wayland activation with a token supplied by the launching application.
/// The token is never read implicitly from the process environment.
/// Success does not guarantee focus: the compositor decides.
pub fn activate_window(token: String) -> Task<Result<(), WindowIntegrationError>> {
    if token.is_empty() || token.contains('\0') {
        return Task::ready(Err(WindowIntegrationError::InvalidHandle));
    }
    request(move |id| ExternalWindowRequest::Activate { id, token })
}
#[doc(hidden)]
pub fn complete_external(id: u64, result: Result<(), WindowIntegrationError>) {
    let done = PENDING.with(|p| p.borrow_mut().remove(&id));
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
