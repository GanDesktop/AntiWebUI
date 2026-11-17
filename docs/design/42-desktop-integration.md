# Wayland clipboard and foreign parent windows

These APIs let native file managers and portal file choosers exchange data and
associate a chooser with a window belonging to another application. They are
available from `anti_webui::prelude::*` and implemented for Wayland.

## Clipboard

`write_clipboard(kind, Vec<ClipboardData>)` offers several representations of the
same selection together. Each representation has a MIME name and `Arc<[u8]>`.
An empty list clears the selection. Duplicate, empty and NUL-containing names are
rejected by the public API. Files can offer `text/uri-list` together with
`x-special/gnome-copied-files` or KDE copy/cut metadata; images can offer PNG and
other formats. The toolkit does not interpret file-transfer conventions.

`read_clipboard(kind, preferences, max_bytes)` returns a `Task<Result<ClipboardOffer,
ClipboardError>>`. The offer includes available format names and, if one matches,
the bytes of the first available format in **caller preference order**. An empty
preference list only probes the formats. Binary bytes are never decoded or newline
normalized. No matching format gives `data: None`; transport errors are distinct.

Both APIs return owner-scoped tasks: use `.then(...)` for completion on the UI
thread. Disposing the reactive owner removes pending callbacks. A read uses a
snapshot of an offer; a later, separate read may see a different clipboard owner.
`read_clipboard_formats(kind, formats, max_bytes)` reads every requested available
representation from the same offer into a `ClipboardSnapshot`. Its `revision` is
an opaque backend token. `write_clipboard_if_unchanged(kind, data, revision)`
replaces that selection only while the token remains current; it reports
`Changed` if another selection has arrived. File managers use this when removing
completed cut entries, so they cannot clear a newer copy operation. The snapshot
byte limit applies across all returned representations.

The old `set_clipboard` and text-editing copy/paste APIs remain available.

| Backend | Binary clipboard |
| --- | --- |
| Wayland | Core `wl_data_device` and primary-selection protocol; no privileged data-control protocol. Reads require keyboard focus; writes also require an input serial. |
| X11 / macOS / Windows | New binary API reports `Unsupported`; existing text APIs are unchanged. |

Reads enforce the caller's byte limit and a five-second transfer deadline.
Empty selections return an empty offer. Clipboard persistence after the application
exits requires another client to retain the selection.

## Foreign parent and activation

Parse a `wayland:<exported-handle>` portal identifier with
`str::parse::<ExternalParent>()`. The backend uses `xdg-foreign-v2`.

In the chooser's root builder, call `set_external_parent(Some(parent), modal)`.
It replaces the current native parent. Calling it with `None` detaches and clears
modal state. This relationship does not add the other application's window to the
toolkit's internal ownership graph. On Wayland the imported object stays alive
until detachment, replacement, closure, or the exporter's `destroyed` event; that
event also clears the modal state. Modal Wayland requests require `xdg-dialog`.
If required protocols are absent, the task returns `Unsupported` and leaves the
previous relationship intact.

`activate_window(token)` submits an explicit launcher-provided token through
`xdg-activation-v1`. No global environment variable is implicitly consumed or
modified. X11, macOS and Windows report `Unsupported` for these new APIs.

A successful task means the request was submitted. A foreign handle can already
be expired, and focus-stealing policies can deny activation. Neither API promises
that the compositor accepted a foreign handle or granted focus.

## Try it and validate

```sh
cargo run -p anti-webui --no-default-features --features wayland --example desktop_integration
cargo run -p anti-webui --no-default-features --features wayland --example desktop_integration -- 'wayland:EXPORTED_HANDLE' 'ACTIVATION_TOKEN'
cargo test -p anti-webui-core --lib
cargo test -p anti-webui-wayland --test drag_protocol
```

The Wayland protocol harness checks focus restrictions, MIME offers, original
binary bytes, read limits, foreign import lifetime, modal cleanup, and activation
requests. Real compositor acceptance, focus policy and window stacking still need
desktop testing.

Protocol references: [portal parent identifiers](https://flatpak.github.io/xdg-desktop-portal/docs/window-identifiers.html),
[xdg-foreign-v2](https://wayland.app/protocols/xdg-foreign-unstable-v2),
[xdg-dialog](https://wayland.app/protocols/xdg-dialog-v1),
[xdg-activation](https://wayland.app/protocols/xdg-activation-v1).
