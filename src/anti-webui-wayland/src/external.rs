//! Foreign toplevel handles live until they are detached or the exporting client destroys them.
use crate::state::State;
use anti_webui_core::{
    event::WindowEvent,
    window::{ExternalParent, ExternalWindowRequest, WindowIntegrationError},
};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::{
    activation::v1::client::xdg_activation_v1,
    dialog::v1::client::xdg_wm_dialog_v1,
    foreign::zv2::client::{zxdg_imported_v2, zxdg_importer_v2},
};

#[derive(Default)]
pub(crate) struct External {
    pub importer: Option<zxdg_importer_v2::ZxdgImporterV2>,
    pub activation: Option<xdg_activation_v1::XdgActivationV1>,
    pub dialogs: Option<xdg_wm_dialog_v1::XdgWmDialogV1>,
    imported: Option<zxdg_imported_v2::ZxdgImportedV2>,
}
impl State {
    pub(crate) fn external_window(&mut self, request: ExternalWindowRequest) {
        let id = request.id();
        let result = match request {
            ExternalWindowRequest::Parent { parent, modal, .. } => {
                self.external_parent(parent, modal)
            }
            ExternalWindowRequest::Activate { token, .. } => match &self.external.activation {
                Some(activation)
                    if self.toplevel().is_some() && !token.is_empty() && !token.contains('\0') =>
                {
                    activation.activate(token, &self.surface);
                    Ok(())
                }
                _ => Err(WindowIntegrationError::Unsupported),
            },
        };
        self.events.push(WindowEvent::ExternalWindow { id, result });
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
