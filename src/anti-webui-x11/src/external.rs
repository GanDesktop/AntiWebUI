//! Portal parent windows and startup notification IDs.
use crate::state::State;
use anti_webui_core::{
    event::WindowEvent,
    window::{ExternalParent, ExternalWindowRequest, WindowIntegrationError},
};
use x11rb::{
    protocol::xproto::{AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, PropMode},
    wrapper::ConnectionExt as _,
};
fn io(e: impl std::fmt::Display) -> WindowIntegrationError {
    WindowIntegrationError::Io(e.to_string())
}
impl State {
    pub(crate) fn external_window(&mut self, request: ExternalWindowRequest) {
        let id = request.id();
        let result = self.apply_external(request);
        self.events.push(WindowEvent::ExternalWindow { id, result });
    }
    fn apply_external(
        &mut self,
        request: ExternalWindowRequest,
    ) -> Result<(), WindowIntegrationError> {
        let a = &self.atoms;
        match request {
            ExternalWindowRequest::Parent { parent, modal, .. } => {
                let parent = match parent {
                    Some(ExternalParent::X11(id)) if id != 0 && id != self.window => Some(id),
                    Some(ExternalParent::X11(_)) => {
                        return Err(WindowIntegrationError::InvalidHandle);
                    }
                    None => None,
                    _ => return Err(WindowIntegrationError::Unsupported),
                };
                if let Some(parent) = parent {
                    self.conn
                        .get_window_attributes(parent)
                        .map_err(io)?
                        .reply()
                        .map_err(io)?;
                    self.conn
                        .change_property32(
                            PropMode::REPLACE,
                            self.window,
                            a.WM_TRANSIENT_FOR,
                            AtomEnum::WINDOW,
                            &[parent],
                        )
                        .map_err(io)?
                        .check()
                        .map_err(io)?;
                } else {
                    self.conn
                        .delete_property(self.window, a.WM_TRANSIENT_FOR)
                        .map_err(io)?
                        .check()
                        .map_err(io)?;
                }
                let window_type = if parent.is_some() {
                    a._NET_WM_WINDOW_TYPE_DIALOG
                } else {
                    a._NET_WM_WINDOW_TYPE_NORMAL
                };
                self.conn
                    .change_property32(
                        PropMode::REPLACE,
                        self.window,
                        a._NET_WM_WINDOW_TYPE,
                        AtomEnum::ATOM,
                        &[window_type],
                    )
                    .map_err(io)?
                    .check()
                    .map_err(io)?;
                let event = ClientMessageEvent::new(
                    32,
                    self.window,
                    a._NET_WM_STATE,
                    [
                        u32::from(parent.is_some() && modal),
                        a._NET_WM_STATE_MODAL,
                        0,
                        1,
                        0,
                    ],
                );
                self.conn
                    .send_event(
                        false,
                        self.root,
                        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                        event,
                    )
                    .map_err(io)?
                    .check()
                    .map_err(io)?;
            }
            ExternalWindowRequest::Activate { token, .. } => {
                if token.is_empty() || token.contains('\0') {
                    return Err(WindowIntegrationError::InvalidHandle);
                }
                self.conn
                    .change_property8(
                        PropMode::REPLACE,
                        self.window,
                        a._NET_STARTUP_ID,
                        a.UTF8_STRING,
                        token.as_bytes(),
                    )
                    .map_err(io)?
                    .check()
                    .map_err(io)?;
                let time = startup_time(&token).unwrap_or(self.last_time);
                let event = ClientMessageEvent::new(
                    32,
                    self.window,
                    a._NET_ACTIVE_WINDOW,
                    [1, time, 0, 0, 0],
                );
                self.conn
                    .send_event(
                        false,
                        self.root,
                        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                        event,
                    )
                    .map_err(io)?
                    .check()
                    .map_err(io)?;
            }
        }
        Ok(())
    }
}
fn startup_time(token: &str) -> Option<u32> {
    token.rsplit_once("_TIME")?.1.parse().ok()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_startup_timestamp_without_treating_arbitrary_tokens_as_times() {
        assert_eq!(startup_time("launcher_TIME12345"), Some(12345));
        assert_eq!(startup_time("opaque-token"), None);
        assert_eq!(startup_time("launcher_TIME123oops"), None);
    }
}
