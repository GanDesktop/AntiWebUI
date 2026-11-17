//! `cargo run -p anti-webui --example desktop_integration -- [parent-window] [activation-token]`
//! Supply a portal-style `wayland:...` handle from another application.
use anti_webui::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let parent = args
        .next()
        .map(|p| p.parse::<ExternalParent>())
        .transpose()?;
    let token = args.next();
    App::new()
        .title("Desktop integration")
        .size(640, 240)
        .run(move || {
            let status = signal(String::from(
                "Copy, then paste here or in another application.",
            ));
            if let Some(parent) = parent {
                set_external_parent(Some(parent), true).then(move |result| {
                    status.set(format!("Parent request: {result:?}"));
                });
            }
            if let Some(token) = token {
                activate_window(token).then(move |result| {
                    if let Err(e) = result {
                        status.set(format!("Activation: {e}"));
                    }
                });
            }
            div()
                .flex()
                .flex_col()
                .gap(12.0)
                .p(16.0)
                .child(button("Copy file offer").on_click(move |_| {
                    write_clipboard(
                        ClipboardKind::Clipboard,
                        vec![
                            ClipboardData {
                                mime: "text/uri-list".into(),
                                bytes: b"file:///tmp/example.txt\r\n".as_slice().into(),
                            },
                            ClipboardData {
                                mime: "x-special/gnome-copied-files".into(),
                                bytes: b"copy\nfile:///tmp/example.txt".as_slice().into(),
                            },
                            ClipboardData {
                                mime: "text/plain;charset=utf-8".into(),
                                bytes: b"/tmp/example.txt".as_slice().into(),
                            },
                        ],
                    )
                    .then(move |result| status.set(format!("Copy: {result:?}")));
                }))
                .child(button("Read clipboard").on_click(move |_| {
                    read_clipboard(
                        ClipboardKind::Clipboard,
                        vec![
                            "x-special/gnome-copied-files".into(),
                            "text/uri-list".into(),
                            "image/png".into(),
                            "text/plain;charset=utf-8".into(),
                        ],
                        16 * 1024 * 1024,
                    )
                    .then(move |result| {
                        status.set(match result {
                            Ok(offer) => format!(
                                "Formats: {:?}; read {:?}",
                                offer.formats,
                                offer.data.map(|d| (d.mime, d.bytes.len()))
                            ),
                            Err(e) => e.to_string(),
                        });
                    });
                }))
                .child(text(move || status.get()))
        })
}
