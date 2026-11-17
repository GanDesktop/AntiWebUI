//! Native window relationships and compositor-granted activation.
use crate::state::State;
use anti_webui_core::{
    event::WindowEvent,
    window::{ExternalParent, ExternalWindowRequest, WindowIntegrationError},
};
use std::collections::HashMap;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::{
    activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1},
    dialog::v1::client::xdg_wm_dialog_v1,
    foreign::zv2::client::{zxdg_imported_v2, zxdg_importer_v2},
};

#[derive(Default)]
pub(crate) struct External {
    pub importer: Option<zxdg_importer_v2::ZxdgImporterV2>,
    pub activation: Option<xdg_activation_v1::XdgActivationV1>,
    pub dialogs: Option<xdg_wm_dialog_v1::XdgWmDialogV1>,
    imported: Option<zxdg_imported_v2::ZxdgImportedV2>,
    activation_tokens: HashMap<u64, xdg_activation_token_v1::XdgActivationTokenV1>,
}
impl State {
    pub(crate) fn external_window(&mut self, request: ExternalWindowRequest) {
        match request {
            ExternalWindowRequest::Parent { id, parent, modal } => {
                let result = self.external_parent(parent, modal);
                self.events.push(WindowEvent::ExternalWindow { id, result });
            }
            ExternalWindowRequest::Activate { id, token } => {
                let result = self.activate_with_token(token);
                self.events.push(WindowEvent::ExternalWindow { id, result });
            }
            ExternalWindowRequest::ActivationToken { id } => {
                if let Err(error) = self.request_activation_token(id) {
                    self.events.push(WindowEvent::ActivationToken {
                        id,
                        result: Err(error),
                    });
                }
            }
            ExternalWindowRequest::ActivateWithToken(token) => {
                // The request came from another live window and carries a compositor token.
                // The requesting window only observes whether routing succeeded.
                let _submitted = self.activate_with_token(token);
            }
        }
    }

    fn activate_with_token(&self, token: String) -> Result<(), WindowIntegrationError> {
        let activation = self
            .external
            .activation
            .as_ref()
            .filter(|_| self.toplevel().is_some())
            .ok_or(WindowIntegrationError::Unsupported)?;
        if token.is_empty() || token.contains('\0') {
            return Err(WindowIntegrationError::InvalidHandle);
        }
        activation.activate(token, &self.surface);
        Ok(())
    }

    fn request_activation_token(&mut self, id: u64) -> Result<(), WindowIntegrationError> {
        let activation = self
            .external
            .activation
            .as_ref()
            .filter(|_| self.toplevel().is_some())
            .ok_or(WindowIntegrationError::Unsupported)?;
        let seat = self
            .seat
            .as_ref()
            .filter(|_| self.press_serial != 0)
            .ok_or(WindowIntegrationError::InvalidHandle)?;
        let token = activation.get_activation_token(&self.qh, id);
        token.set_serial(self.press_serial, seat);
        token.set_surface(&self.surface);
        token.commit();
        self.external.activation_tokens.insert(id, token);
        Ok(())
    }

    pub(crate) fn destroy_activation_tokens(&mut self) {
        for (_, token) in self.external.activation_tokens.drain() {
            token.destroy();
        }
    }

    fn external_parent(
        &mut self,
        parent: Option<ExternalParent>,
        modal: bool,
    ) -> Result<(), WindowIntegrationError> {
        let toplevel = self
            .toplevel()
            .cloned()
            .ok_or(WindowIntegrationError::Unsupported)?;
        let handle = match parent {
            None => None,
            Some(ExternalParent::Wayland(h)) if !h.is_empty() && !h.contains('\0') => Some(h),
            Some(ExternalParent::Wayland(_)) => return Err(WindowIntegrationError::InvalidHandle),
        };
        if handle.is_some()
            && (self.external.importer.is_none() || (modal && self.external.dialogs.is_none()))
        {
            return Err(WindowIntegrationError::Unsupported);
        }
        self.detach_external();
        toplevel.set_parent(None);
        if let Some(handle) = handle {
            let imported = self
                .external
                .importer
                .as_ref()
                .expect("importer checked")
                .import_toplevel(handle, &self.qh, ());
            imported.set_parent_of(&self.surface);
            self.external.imported = Some(imported);
            if modal {
                let dialog = self
                    .external
                    .dialogs
                    .as_ref()
                    .expect("dialog manager checked")
                    .get_xdg_dialog(&toplevel, &self.qh, ());
                dialog.set_modal();
                self.decor.dialog = Some(dialog);
            }
        }
        Ok(())
    }

    pub(crate) fn detach_external(&mut self) {
        if let Some(imported) = self.external.imported.take() {
            imported.destroy();
        }
        if let Some(dialog) = self.decor.dialog.take() {
            dialog.unset_modal();
            dialog.destroy();
        }
    }
}

impl Dispatch<xdg_activation_token_v1::XdgActivationTokenV1, u64> for State {
    fn event(
        state: &mut Self,
        request: &xdg_activation_token_v1::XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event
            && state.external.activation_tokens.remove(id).is_some()
        {
            request.destroy();
            let result = if token.is_empty() {
                Err(WindowIntegrationError::InvalidHandle)
            } else {
                Ok(token)
            };
            state
                .events
                .push(WindowEvent::ActivationToken { id: *id, result });
        }
    }
}
impl Dispatch<zxdg_imported_v2::ZxdgImportedV2, ()> for State {
    fn event(
        state: &mut Self,
        imported: &zxdg_imported_v2::ZxdgImportedV2,
        event: zxdg_imported_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_imported_v2::Event::Destroyed = event {
            if state.external.imported.as_ref() == Some(imported) {
                state.detach_external();
            }
        }
    }
}
delegate_noop!(State: ignore zxdg_importer_v2::ZxdgImporterV2);
delegate_noop!(State: ignore xdg_activation_v1::XdgActivationV1);
