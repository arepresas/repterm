use crate::events::{classify, Action};
use crate::gitinfo::{expand_home, format_git_label, open_url, probe, RepoStatus};
use crate::tabs::Tabs;
use alacritty_terminal::event::WindowSize;
use alacritty_terminal::term::ClipboardType;
use alacritty_terminal::vte::ansi::Rgb;
use egui_term::{
    BackendCommand, Binding, BindingAction, InputKind, TerminalBackend, TerminalMode, TerminalView,
};

const TAB_TITLE_CHARS: usize = 32;
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct App {
    tabs: Tabs,
    repo: Option<RepoStatus>,
    repo_path: Option<String>,
    probe_tx: std::sync::mpsc::Sender<(String, RepoStatus)>,
    probe_rx: std::sync::mpsc::Receiver<(String, RepoStatus)>,
    probing: bool,
    last_probe: Option<std::time::Instant>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut tabs = Tabs::new();
        tabs.add(&cc.egui_ctx)?;
        let (probe_tx, probe_rx) = std::sync::mpsc::channel();
        Ok(Self {
            tabs,
            repo: None,
            repo_path: None,
            probe_tx,
            probe_rx,
            probing: false,
            last_probe: None,
        })
    }

    fn add_tab(&mut self, ctx: &egui::Context) {
        if let Err(err) = self.tabs.add(ctx) {
            log::error!("failed to add tab: {err}");
        }
    }

    fn poll_repo(&mut self, ctx: &egui::Context) {
        while let Ok((path, status)) = self.probe_rx.try_recv() {
            self.probing = false;
            if self.repo_path.as_deref() == Some(path.as_str()) {
                self.repo = Some(status);
            }
        }
        let cwd = self.active_cwd();
        if cwd != self.repo_path {
            self.repo_path = cwd.clone();
            self.repo = None;
            self.last_probe = None;
        }
        let due = self
            .last_probe
            .map(|instant| instant.elapsed() >= PROBE_INTERVAL)
            .unwrap_or(true);
        if due {
            if let Some(path) = cwd {
                self.spawn_probe(ctx, path);
            }
        }
        ctx.request_repaint_after(PROBE_INTERVAL);
    }

    fn active_cwd(&self) -> Option<String> {
        let id = self.tabs.active()?;
        let title = self.tabs.title(id)?;
        expand_home(title, std::env::var("HOME").ok().as_deref())
    }

    fn spawn_probe(&mut self, ctx: &egui::Context, path: String) {
        if self.probing {
            return;
        }
        self.probing = true;
        self.last_probe = Some(std::time::Instant::now());
        let tx = self.probe_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let status = probe(&path);
            let _ = tx.send((path, status));
            ctx.request_repaint();
        });
    }
}

fn tab_label(title: &str) -> String {
    let title = title.trim();
    let Some(host_path) = title.split_once('@').map(|(_, rest)| rest) else {
        return title.to_owned();
    };
    let Some((_, path)) = host_path.split_once(':') else {
        return title.to_owned();
    };
    let path = path.trim();
    if path.is_empty() {
        title.to_owned()
    } else {
        path.to_owned()
    }
}

fn clip_title(title: &str, max_chars: usize) -> String {
    let count = title.chars().count();
    if count <= max_chars {
        return title.to_owned();
    }
    let keep = max_chars.saturating_sub(1);
    let tail: String = title.chars().skip(count - keep).collect();
    format!("…{tail}")
}

fn close_tab(ctx: &egui::Context, tabs: &mut Tabs, id: u64) -> bool {
    tabs.remove(id);
    if tabs.is_empty() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        true
    } else {
        false
    }
}

fn apply_action(action: Action, ctx: &egui::Context, tab_id: u64, tabs: &mut Tabs) -> bool {
    match action {
        Action::Close => {
            if close_tab(ctx, tabs, tab_id) {
                return true;
            }
        }
        Action::SetTitle(title) => {
            tabs.set_title(tab_id, tab_label(&title));
            if tabs.active() == Some(tab_id) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
            }
        }
        Action::ResetTitle => {
            tabs.set_title(tab_id, "shell".to_owned());
            if tabs.active() == Some(tab_id) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title("repterm".to_owned()));
            }
        }
        Action::WriteToPty(bytes) => {
            if let Some(tab) = tabs.tab_mut(tab_id) {
                tab.backend.process_command(BackendCommand::Write(bytes));
            }
        }
        Action::StoreClipboard(text) => ctx.copy_text(text),
        Action::LoadClipboard(kind, formatter) => {
            if kind == ClipboardType::Selection {
                log::debug!("selection clipboard requested; using system clipboard");
            }
            let text = read_clipboard().unwrap_or_else(|err| {
                log::warn!("clipboard read failed: {err}");
                String::new()
            });
            if let Some(tab) = tabs.tab_mut(tab_id) {
                write_response(&mut tab.backend, formatter(&text));
            }
        }
        Action::RespondColor(index, formatter) => {
            log::debug!("color request for index {index}");
            if let Some(tab) = tabs.tab_mut(tab_id) {
                write_response(&mut tab.backend, formatter(foreground_color(ctx)));
            }
        }
        Action::RespondSize(formatter) => {
            if let Some(tab) = tabs.tab_mut(tab_id) {
                write_response(&mut tab.backend, formatter(default_window_size()));
            }
        }
        Action::ChildExited(code) => {
            log::debug!("tab {tab_id} shell exited with code {code}");
        }
        Action::Ignore => {}
    }
    false
}

fn write_response(backend: &mut TerminalBackend, response: String) {
    backend.process_command(BackendCommand::Write(response.into_bytes()));
}

fn read_clipboard() -> Result<String, arboard::Error> {
    arboard::Clipboard::new()?.get_text()
}

fn foreground_color(ctx: &egui::Context) -> Rgb {
    let color = ctx.style().visuals.text_color();
    Rgb {
        r: color.r(),
        g: color.g(),
        b: color.b(),
    }
}

fn default_window_size() -> WindowSize {
    WindowSize {
        num_lines: 50,
        num_cols: 80,
        cell_width: 1,
        cell_height: 1,
    }
}

fn show_repo_bar(ctx: &egui::Context, repo: &RepoStatus) {
    egui::TopBottomPanel::top("git").show(ctx, |ui| {
        ui.horizontal(|ui| {
            if let Some(git) = &repo.git {
                ui.label(format_git_label(git));
            }
            if let Some(pr) = &repo.pr {
                let label = format!("#{} {}", pr.number, pr.state.to_lowercase());
                if ui.link(label).on_hover_text(&pr.title).clicked() && !open_url(&pr.url) {
                    log::warn!("failed to open pull request");
                }
            }
        });
    });
}

fn ctrl_backspace_bindings() -> Vec<(Binding<InputKind>, BindingAction)> {
    vec![(
        Binding {
            target: InputKind::KeyCode(egui::Key::Backspace),
            modifiers: egui::Modifiers::CTRL,
            terminal_mode_include: TerminalMode::empty(),
            terminal_mode_exclude: TerminalMode::empty(),
        },
        BindingAction::Esc("\x1b\x7f".to_owned()),
    )]
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            self.tabs.clear();
        }

        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::T)) {
            self.add_tab(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
            if let Some(id) = self.tabs.active() {
                if close_tab(ctx, &mut self.tabs, id) {
                    return;
                }
            }
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Delete)) {
            if let Some(id) = self.tabs.active() {
                if close_tab(ctx, &mut self.tabs, id) {
                    return;
                }
            }
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::PageUp)) {
            self.tabs.select_relative(-1);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::PageDown)) {
            self.tabs.select_relative(1);
        }

        self.poll_repo(ctx);

        let events: Vec<_> = self.tabs.try_iter().take(256).collect();
        for (id, event) in events {
            if apply_action(classify(&event), ctx, id, &mut self.tabs) {
                return;
            }
        }

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for id in self.tabs.ids() {
                    let title = clip_title(self.tabs.title(id).unwrap_or("shell"), TAB_TITLE_CHARS);
                    let tab = ui.selectable_label(self.tabs.active() == Some(id), title);
                    if tab.clicked() {
                        self.tabs.set_active(id);
                    }
                    if tab.middle_clicked() {
                        close_tab(ctx, &mut self.tabs, id);
                    }
                    tab.surrender_focus();
                    if ui.small_button("×").clicked() {
                        close_tab(ctx, &mut self.tabs, id);
                    }
                }
                if ui.button("[+]").clicked() {
                    self.add_tab(ctx);
                }
            });
        });

        if let Some(repo) = self.repo.clone().filter(|repo| repo.git.is_some()) {
            show_repo_bar(ctx, &repo);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let size = ui.available_size();
            if let Some(tab) = self.tabs.active_tab_mut() {
                let terminal = TerminalView::new(ui, &mut tab.backend)
                    .set_focus(true)
                    .set_size(size)
                    .add_bindings(ctrl_backspace_bindings());
                ui.add(terminal);
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("No tabs. Press Ctrl+T to open one.");
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{clip_title, ctrl_backspace_bindings, tab_label};
    use egui_term::{BindingAction, InputKind};

    #[test]
    fn clip_title_keeps_short_text() {
        assert_eq!(clip_title("shell", 18), "shell");
    }

    #[test]
    fn clip_title_keeps_path_tail() {
        assert_eq!(clip_title("abcdefghijklmnopqrstuvwxyz", 4), "…xyz");
    }

    #[test]
    fn tab_label_keeps_path_after_user_host() {
        assert_eq!(
            tab_label("overlag@overlag-B550i-GAMING:~/Dev/Perso/repterm"),
            "~/Dev/Perso/repterm"
        );
    }

    #[test]
    fn tab_label_trims_space_after_colon() {
        assert_eq!(tab_label("overlag@host: /tmp"), "/tmp");
    }

    #[test]
    fn tab_label_keeps_plain_path() {
        assert_eq!(tab_label("~/Dev/Perso/repterm"), "~/Dev/Perso/repterm");
    }

    #[test]
    fn ctrl_backspace_sends_meta_backspace() {
        let bindings = ctrl_backspace_bindings();
        assert_eq!(bindings.len(), 1);
        assert_eq!(
            bindings[0].0.target,
            InputKind::KeyCode(egui::Key::Backspace)
        );
        assert_eq!(bindings[0].0.modifiers, egui::Modifiers::CTRL);
        assert_eq!(bindings[0].1, BindingAction::Esc("\x1b\x7f".to_owned()));
    }
}
