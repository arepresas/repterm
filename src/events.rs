use alacritty_terminal::event::WindowSize;
use alacritty_terminal::term::ClipboardType;
use alacritty_terminal::vte::ansi::Rgb;
use egui_term::PtyEvent;
use std::sync::Arc;

pub enum Action {
    Close,
    SetTitle(String),
    ResetTitle,
    WriteToPty(Vec<u8>),
    StoreClipboard(String),
    LoadClipboard(ClipboardType, Arc<dyn Fn(&str) -> String + Sync + Send>),
    RespondColor(usize, Arc<dyn Fn(Rgb) -> String + Sync + Send>),
    RespondSize(Arc<dyn Fn(WindowSize) -> String + Sync + Send>),
    ChildExited(Option<i32>),
    Ignore,
}

pub fn classify(event: &PtyEvent) -> Action {
    match event {
        PtyEvent::Exit => Action::Close,
        PtyEvent::Title(title) => Action::SetTitle(title.clone()),
        PtyEvent::ResetTitle => Action::ResetTitle,
        PtyEvent::PtyWrite(text) => Action::WriteToPty(text.clone().into_bytes()),
        PtyEvent::ClipboardStore(_, text) => Action::StoreClipboard(text.clone()),
        PtyEvent::ClipboardLoad(kind, formatter) => Action::LoadClipboard(*kind, formatter.clone()),
        PtyEvent::ColorRequest(index, formatter) => Action::RespondColor(*index, formatter.clone()),
        PtyEvent::TextAreaSizeRequest(formatter) => Action::RespondSize(formatter.clone()),
        PtyEvent::ChildExit(status) => Action::ChildExited(status.code()),
        _ => Action::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn exit_status(code: i32) -> std::process::ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw(code)
    }

    fn clipboard_formatter() -> Arc<dyn Fn(&str) -> String + Sync + Send> {
        Arc::new(|text| text.to_owned())
    }

    fn color_formatter() -> Arc<dyn Fn(Rgb) -> String + Sync + Send> {
        Arc::new(|color| format!("{}", color.r))
    }

    fn size_formatter() -> Arc<dyn Fn(WindowSize) -> String + Sync + Send> {
        Arc::new(|size| format!("{}x{}", size.num_cols, size.num_lines))
    }

    #[test]
    fn exit_closes_window() {
        assert!(matches!(classify(&PtyEvent::Exit), Action::Close));
    }

    #[test]
    fn title_updates_window() {
        match classify(&PtyEvent::Title("vim".to_owned())) {
            Action::SetTitle(title) => assert_eq!(title, "vim"),
            _ => panic!("expected SetTitle"),
        }
    }

    #[test]
    fn reset_title_restores_default() {
        assert!(matches!(
            classify(&PtyEvent::ResetTitle),
            Action::ResetTitle
        ));
    }

    #[test]
    fn pty_write_is_forwarded() {
        match classify(&PtyEvent::PtyWrite("hi".to_owned())) {
            Action::WriteToPty(bytes) => assert_eq!(bytes, b"hi".to_vec()),
            _ => panic!("expected WriteToPty"),
        }
    }

    #[test]
    fn clipboard_store_is_forwarded() {
        let event = PtyEvent::ClipboardStore(ClipboardType::Clipboard, "text".to_owned());
        match classify(&event) {
            Action::StoreClipboard(text) => assert_eq!(text, "text"),
            _ => panic!("expected StoreClipboard"),
        }
    }

    #[test]
    fn clipboard_load_is_forwarded() {
        let event = PtyEvent::ClipboardLoad(ClipboardType::Clipboard, clipboard_formatter());
        assert!(matches!(classify(&event), Action::LoadClipboard(..)));
    }

    #[test]
    fn color_request_is_forwarded() {
        let event = PtyEvent::ColorRequest(0, color_formatter());
        assert!(matches!(classify(&event), Action::RespondColor(0, _)));
    }

    #[test]
    fn size_request_is_forwarded() {
        let event = PtyEvent::TextAreaSizeRequest(size_formatter());
        assert!(matches!(classify(&event), Action::RespondSize(_)));
    }

    #[cfg(unix)]
    #[test]
    fn child_exit_is_forwarded() {
        assert!(matches!(
            classify(&PtyEvent::ChildExit(exit_status(1 << 8))),
            Action::ChildExited(Some(1))
        ));
    }

    #[test]
    fn cosmetic_events_are_ignored() {
        assert!(matches!(classify(&PtyEvent::Bell), Action::Ignore));
        assert!(matches!(classify(&PtyEvent::Wakeup), Action::Ignore));
        assert!(matches!(
            classify(&PtyEvent::MouseCursorDirty),
            Action::Ignore
        ));
        assert!(matches!(
            classify(&PtyEvent::CursorBlinkingChange),
            Action::Ignore
        ));
    }
}
