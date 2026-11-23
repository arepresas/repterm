use crate::chat_view::{self, CHAT_ACCENT};
use crate::events::{classify, Action};
use crate::gitinfo::{
    branches, change_totals, checkout_command, diff_for, expand_home, format_git_label, open_url,
    probe, BranchInfo, ChangedFile, RepoStatus,
};
use crate::llm::{build_messages, ChatMessage, LlmClient, Role, StreamUpdate};
use crate::settings::{self, Settings};
use crate::tabs::Tabs;
use alacritty_terminal::event::WindowSize;
use alacritty_terminal::term::ClipboardType;
use alacritty_terminal::vte::ansi::Rgb;
use egui_term::{
    BackendCommand, Binding, BindingAction, InputKind, TerminalBackend, TerminalMode, TerminalView,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const TAB_TITLE_CHARS: usize = 32;
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

const MAX_DIFF_LINES: usize = 5000;
const ADD_COLOR: egui::Color32 = egui::Color32::from_rgb(0x5f, 0xb8, 0x6a);
const DEL_COLOR: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x6c, 0x6c);
const WARN_COLOR: egui::Color32 = egui::Color32::from_rgb(0xd9, 0xa4, 0x41);
const MAX_TERMINAL_CONTEXT_CHARS: usize = 8000;
const MAX_CONTEXT_LINES: usize = 120;

type DiffKey = (String, String, bool, u32, u32);

pub struct ChatState {
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub partial: String,
    pub error: Option<String>,
    pub cancelled: bool,
    pub cancel: Arc<AtomicBool>,
    pub rx: Option<Receiver<StreamUpdate>>,
}

impl ChatState {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            partial: String::new(),
            error: None,
            cancelled: false,
            cancel: Arc::new(AtomicBool::new(false)),
            rx: None,
        }
    }
}

#[derive(Clone)]
struct SettingsForm {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub reveal_key: bool,
}

impl SettingsForm {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            base_url: settings.base_url.clone(),
            api_key: settings.api_key.clone(),
            model: settings.model.clone(),
            reveal_key: false,
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.base_url.trim().is_empty() {
            Err("base_url required")
        } else {
            Ok(())
        }
    }

    fn to_settings(&self) -> Settings {
        Settings {
            base_url: self.base_url.trim().to_owned(),
            api_key: self.api_key.clone(),
            model: self.model.trim().to_owned(),
            include_terminal_context: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsAction {
    None,
    Save,
    Cancel,
    Refresh,
}

struct Picker {
    query: String,
    branches: Option<Vec<BranchInfo>>,
    selected: usize,
    focus: bool,
}

pub struct App {
    tabs: Tabs,
    repo: Option<RepoStatus>,
    repo_path: Option<String>,
    probe_tx: std::sync::mpsc::Sender<(String, RepoStatus)>,
    probe_rx: std::sync::mpsc::Receiver<(String, RepoStatus)>,
    probing: bool,
    last_probe: Option<std::time::Instant>,
    picker: Option<Picker>,
    branch_tx: std::sync::mpsc::Sender<(String, Vec<BranchInfo>)>,
    branch_rx: std::sync::mpsc::Receiver<(String, Vec<BranchInfo>)>,
    changes_open: bool,
    selected_file: Option<(String, bool)>,
    diff_key: Option<DiffKey>,
    diff_lines: Option<Vec<String>>,
    diff_tx: std::sync::mpsc::Sender<(DiffKey, Vec<String>)>,
    diff_rx: std::sync::mpsc::Receiver<(DiffKey, Vec<String>)>,
    chat_open: bool,
    chat: ChatState,
    chat_focus: bool,
    settings: Settings,
    settings_open: bool,
    settings_form: SettingsForm,
    settings_include_terminal: bool,
    settings_save_error: Option<String>,
    inline_agent_open: bool,
    agent_conversation: bool,
    agent_tab: Option<u64>,
    agent_typed: String,
    agent_output_committed: bool,
    model_rx: Option<Receiver<Result<Vec<String>, String>>>,
    model_list: Option<Vec<String>>,
    model_error: Option<String>,
    models_loading: bool,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut tabs = Tabs::new();
        tabs.add(&cc.egui_ctx)?;
        let (probe_tx, probe_rx) = std::sync::mpsc::channel();
        let (branch_tx, branch_rx) = std::sync::mpsc::channel();
        let (diff_tx, diff_rx) = std::sync::mpsc::channel();
        let settings = settings::load();
        Ok(Self {
            tabs,
            repo: None,
            repo_path: None,
            probe_tx,
            probe_rx,
            probing: false,
            last_probe: None,
            picker: None,
            branch_tx,
            branch_rx,
            changes_open: false,
            selected_file: None,
            diff_key: None,
            diff_lines: None,
            diff_tx,
            diff_rx,
            chat_open: false,
            chat: ChatState::new(),
            chat_focus: false,
            settings: settings.clone(),
            settings_open: false,
            settings_form: SettingsForm::from_settings(&settings),
            settings_include_terminal: settings.include_terminal_context,
            settings_save_error: None,
            inline_agent_open: false,
            agent_conversation: false,
            agent_tab: None,
            agent_typed: String::new(),
            agent_output_committed: false,
            model_rx: None,
            model_list: None,
            model_error: None,
            models_loading: false,
        })
    }

    fn open_settings(&mut self, ctx: &egui::Context) {
        self.picker = None;
        self.settings_form = SettingsForm::from_settings(&self.settings);
        self.settings_include_terminal = self.settings.include_terminal_context;
        self.settings_save_error = None;
        self.settings_open = true;
        self.load_models(ctx);
    }

    fn load_models(&mut self, ctx: &egui::Context) {
        let client = LlmClient {
            base_url: self.settings_form.base_url.clone(),
            api_key: self.settings_form.api_key.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.model_rx = Some(rx);
        self.model_list = None;
        self.model_error = None;
        self.models_loading = true;
        std::thread::spawn(move || {
            let _ = tx.send(client.models());
        });
        ctx.request_repaint();
    }

    fn poll_models(&mut self, ctx: &egui::Context) {
        let results: Vec<_> = self
            .model_rx
            .as_ref()
            .map(|rx| {
                let mut pending = Vec::new();
                while let Ok(result) = rx.try_recv() {
                    pending.push(result);
                }
                pending
            })
            .unwrap_or_default();
        for result in results {
            match result {
                Ok(models) => {
                    self.model_list = Some(models);
                    self.model_error = None;
                }
                Err(err) => {
                    self.model_error = Some(err);
                    self.model_list = None;
                }
            }
            self.models_loading = false;
            ctx.request_repaint();
        }
    }

    fn show_settings(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.settings_open = false;
            return;
        }
        self.poll_models(ctx);

        let mut form = self.settings_form.clone();
        let mut include = self.settings_include_terminal;
        let mut action = SettingsAction::None;
        let save_error = self.settings_save_error.clone().unwrap_or_default();
        let models = self.model_list.clone();
        let loading = self.models_loading;
        let model_error = self.model_error.clone();

        egui::Window::new("Ajustes")
            .resizable(false)
            .default_size(egui::vec2(460.0, 340.0))
            .open(&mut self.settings_open)
            .show(ctx, |ui| {
                ui.label("Base URL");
                ui.text_edit_singleline(&mut form.base_url);

                ui.horizontal(|ui| {
                    ui.label("API key");
                    let eye = if form.reveal_key { "🙈" } else { "👁" };
                    if ui
                        .button(eye)
                        .on_hover_text("Mostrar u ocultar la clave")
                        .clicked()
                    {
                        form.reveal_key = !form.reveal_key;
                    }
                });
                egui::TextEdit::singleline(&mut form.api_key)
                    .password(!form.reveal_key)
                    .show(ui);

                ui.horizontal(|ui| {
                    ui.label("Modelo");
                    if ui
                        .small_button("↻")
                        .on_hover_text("Recargar modelos")
                        .clicked()
                    {
                        action = SettingsAction::Refresh;
                    }
                });
                let selected = if form.model.is_empty() {
                    "manual".to_owned()
                } else {
                    form.model.clone()
                };
                egui::ComboBox::from_id_salt("llm-model")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(form.model.is_empty(), "manual")
                            .clicked()
                        {
                            form.model.clear();
                        }
                        if let Some(list) = &models {
                            for model in list {
                                if ui.selectable_label(form.model == *model, model).clicked() {
                                    form.model = model.clone();
                                }
                            }
                        }
                    });
                ui.text_edit_singleline(&mut form.model);
                if loading {
                    ui.weak("Cargando modelos…");
                }
                if let Some(err) = &model_error {
                    ui.colored_label(WARN_COLOR, "Modelos no disponibles")
                        .on_hover_text(err);
                    if ui.small_button("Reintentar").clicked() {
                        action = SettingsAction::Refresh;
                    }
                }

                ui.checkbox(&mut include, "Incluir contexto del terminal");
                ui.weak("Incluye selección y últimas líneas. Puede contener secretos.");
                if !save_error.is_empty() {
                    ui.colored_label(DEL_COLOR, &save_error);
                }
                ui.horizontal(|ui| {
                    if ui.button("Guardar").clicked() {
                        action = SettingsAction::Save;
                    }
                    if ui.button("Cancelar").clicked() {
                        action = SettingsAction::Cancel;
                    }
                });
            });

        self.settings_form = form;
        self.settings_include_terminal = include;
        self.settings_save_error = if save_error.is_empty() {
            None
        } else {
            Some(save_error)
        };

        match action {
            SettingsAction::Refresh => self.load_models(ctx),
            SettingsAction::Cancel => self.settings_open = false,
            SettingsAction::Save => match self.settings_form.validate() {
                Err(message) => self.settings_save_error = Some(message.to_owned()),
                Ok(()) => {
                    let mut settings = self.settings_form.to_settings();
                    settings.include_terminal_context = self.settings_include_terminal;
                    match settings.save() {
                        Ok(()) => {
                            self.settings = settings;
                            self.settings_open = false;
                            self.settings_save_error = None;
                        }
                        Err(err) => self.settings_save_error = Some(err.to_string()),
                    }
                }
            },
            SettingsAction::None => {}
        }
    }

    fn add_tab(&mut self, ctx: &egui::Context) {
        if let Err(err) = self.tabs.add(ctx) {
            log::error!("failed to add tab: {err}");
        }
    }

    fn open_chat(&mut self, ctx: &egui::Context) {
        self.chat_open = true;
        self.chat_focus = true;
        self.inline_agent_open = false;
        self.picker = None;
        ctx.request_repaint();
    }

    fn close_chat(&mut self, ctx: &egui::Context) {
        self.chat_open = false;
        self.chat_focus = false;
        ctx.memory_mut(|m| m.surrender_focus(chat_composer_id()));
        ctx.request_repaint();
    }

    fn toggle_chat(&mut self, ctx: &egui::Context) {
        if self.chat_open {
            self.close_chat(ctx);
        } else {
            self.open_chat(ctx);
        }
    }

    fn terminal_lines(&mut self) -> Vec<String> {
        let Some(tab) = self.tabs.active_tab_mut() else {
            return Vec::new();
        };
        let mut lines: std::collections::BTreeMap<i32, String> = std::collections::BTreeMap::new();
        for indexed in tab.backend.last_content().grid.display_iter() {
            lines
                .entry(indexed.point.line.0)
                .or_default()
                .push(indexed.c);
        }
        let mut lines: Vec<String> = lines
            .into_values()
            .map(|line| line.trim_end().to_owned())
            .collect();
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        let start = lines.len().saturating_sub(MAX_CONTEXT_LINES);
        lines.split_off(start)
    }

    fn chat_context(&mut self) -> String {
        let cwd = self.active_cwd();
        let selection = self
            .tabs
            .active_tab_mut()
            .map(|tab| tab.backend.selectable_content())
            .unwrap_or_default();
        let selection = selection.trim();
        let lines = self.terminal_lines();
        build_context(
            self.repo.as_ref(),
            cwd.as_deref(),
            Some(selection).filter(|text| !text.is_empty()),
            &lines,
            self.settings.include_terminal_context,
        )
    }

    fn send_chat(&mut self, ctx: &egui::Context, focus_composer: bool) {
        if self.chat.input.trim().is_empty() || self.chat.rx.is_some() {
            return;
        }
        if self.settings.base_url.trim().is_empty() {
            self.chat.error = Some("Configura la Base URL en Ajustes".to_owned());
            return;
        }
        if self.settings.model.trim().is_empty() {
            self.chat.error = Some("Configura un modelo en Ajustes".to_owned());
            return;
        }
        let input = std::mem::take(&mut self.chat.input);
        self.chat.messages.push(ChatMessage::user(input.trim()));
        self.chat.partial.clear();
        self.chat.error = None;
        self.chat.cancelled = false;
        let context = self.chat_context();
        let messages = build_messages(&context, &self.chat.messages);
        let client = LlmClient {
            base_url: self.settings.base_url.clone(),
            api_key: self.settings.api_key.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.chat.cancel = client.cancel.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.chat.rx = Some(rx);
        let model = self.settings.model.clone();
        std::thread::spawn(move || {
            if let Err(err) = client.stream(&model, &messages, &tx) {
                let _ = tx.send(StreamUpdate::Error(err));
            }
        });
        if focus_composer {
            self.chat_focus = true;
        }
        ctx.request_repaint();
    }

    fn poll_chat(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.chat.rx.take() else {
            return;
        };
        let mut finished = false;
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(StreamUpdate::Delta(delta)) => self.chat.partial.push_str(&delta),
                Ok(StreamUpdate::Done) => {
                    finished = true;
                    break;
                }
                Ok(StreamUpdate::Error(err)) => {
                    self.chat.error = Some(err);
                    finished = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if finished || disconnected {
            if !self.chat.partial.is_empty() {
                let partial = std::mem::take(&mut self.chat.partial);
                self.chat.messages.push(ChatMessage::assistant(partial));
            }
            if disconnected && self.chat.error.is_none() {
                self.chat.error = Some("La conexión terminó inesperadamente".to_owned());
            }
            self.chat.cancelled = false;
        } else {
            self.chat.rx = Some(rx);
            ctx.request_repaint();
        }
    }

    fn cancel_chat(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.chat.rx.take() else {
            return;
        };
        self.chat.cancel.store(true, Ordering::Relaxed);
        while let Ok(update) = rx.try_recv() {
            if let StreamUpdate::Delta(delta) = update {
                self.chat.partial.push_str(&delta);
            }
        }
        if !self.chat.partial.is_empty() {
            let partial = std::mem::take(&mut self.chat.partial);
            self.chat.messages.push(ChatMessage::assistant(partial));
        }
        self.chat.cancelled = true;
        ctx.request_repaint();
    }

    fn retry_chat(&mut self, ctx: &egui::Context) {
        self.chat.error = None;
        self.chat.cancelled = false;
        let Some(index) = self
            .chat
            .messages
            .iter()
            .rposition(|message| matches!(message.role, Role::User))
        else {
            return;
        };
        let content = self.chat.messages[index].content.clone();
        self.chat.messages.truncate(index);
        self.chat.input = content;
        self.send_chat(ctx, true);
    }

    fn submit_inline_question(&mut self, typed: &str, ctx: &egui::Context) {
        let question = typed.trim().strip_prefix('!').unwrap_or(typed.trim()).trim();
        if question.is_empty() {
            return;
        }
        if self.chat.rx.is_some() {
            self.chat.error = Some("Ya hay una respuesta del agente en curso".to_owned());
            return;
        }
        self.inline_agent_open = true;
        self.agent_tab = self.tabs.active();
        self.agent_output_committed = false;
        self.agent_typed = typed.trim().to_owned();
        self.chat.input = question.to_owned();
        self.send_chat(ctx, false);
    }

    fn commit_agent_output(&mut self) {
        if self.agent_output_committed || self.chat.rx.is_some() || self.agent_typed.is_empty() {
            return;
        }
        let answer = if let Some(error) = &self.chat.error {
            error.clone()
        } else if !self.chat.partial.is_empty() {
            self.chat.partial.clone()
        } else {
            self.chat
                .messages
                .iter()
                .rev()
                .find(|message| message.role == Role::Assistant)
                .map(|message| message.content.clone())
                .unwrap_or_default()
        };
        let mut output = String::new();
        output.push_str(&self.agent_typed);
        output.push_str("\r\n");
        for line in answer.lines() {
            output.push_str(&visible_terminal_line(line));
            output.push_str("\r\n");
        }
        if let Some(tab) = self.agent_tab.and_then(|id| self.tabs.tab_mut(id)) {
            tab.backend.inject_output_preserving_prompt(output.as_bytes());
        }
        self.agent_output_committed = true;
        self.inline_agent_open = false;
        self.agent_conversation = true;
    }

    fn submit_chat(&mut self, ctx: &egui::Context) {
        let text = self.chat.input.trim().to_owned();
        if text.is_empty() {
            return;
        }
        if is_bare_bang(&text) {
            self.chat.input.clear();
            self.close_chat(ctx);
            return;
        }
        if text.starts_with('!') {
            if text.contains(['\n', '\r']) {
                self.chat.error = Some("Los comandos ! deben ocupar una sola línea".to_owned());
                return;
            }
            if let Some(command) = bang_command(&text) {
                self.run_bang(command, ctx);
                return;
            }
        }
        self.send_chat(ctx, true);
    }

    fn run_bang(&mut self, command: &str, ctx: &egui::Context) {
        let Ok(id) = command_target(self.tabs.active()) else {
            self.chat.error = Some("No hay pestaña activa para ejecutar el comando".to_owned());
            return;
        };
        let Some(tab) = self.tabs.tab_mut(id) else {
            self.chat.error = Some("No hay pestaña activa para ejecutar el comando".to_owned());
            return;
        };
        tab.backend
            .process_command(BackendCommand::Write(format!("{command}\n").into_bytes()));
        self.chat.input.clear();
        self.chat
            .messages
            .push(ChatMessage::user(format!("!{command}")));
        self.chat.error = None;
        self.close_chat(ctx);
        ctx.request_repaint();
    }

    fn insert_command(&mut self, text: &str, ctx: &egui::Context) {
        let Ok(id) = command_target(self.tabs.active()) else {
            self.chat.error = Some("No hay pestaña activa para insertar el comando".to_owned());
            return;
        };
        if let Some(tab) = self.tabs.tab_mut(id) {
            tab.backend
                .process_command(BackendCommand::Write(text.as_bytes().to_vec()));
        }
        self.close_chat(ctx);
        ctx.request_repaint();
    }

    fn inline_agent_lines(&self) -> Vec<String> {
        if !self.inline_agent_open {
            return Vec::new();
        }
        if self.chat.rx.is_some() && self.chat.partial.is_empty() {
            return vec!["…".to_owned()];
        }
        if !self.chat.partial.is_empty() {
            return self.chat.partial.lines().map(str::to_owned).collect();
        }
        if let Some(error) = &self.chat.error {
            return vec![error.clone()];
        }
        self.chat
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::Assistant)
            .map(|message| message.content.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn show_chat(&mut self, ctx: &egui::Context) {
        let streaming = self.chat.rx.is_some();
        let mut close = false;
        let mut open_settings = false;
        let mut submit = false;
        let mut cancel = false;
        let mut retry = false;
        let mut insert: Option<String> = None;
        let composer_id = chat_composer_id();

        egui::TopBottomPanel::top("chat_header")
            .exact_height(34.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.strong("AI chat");
                    let (color, hint) = if self.chat.error.is_some() {
                        (DEL_COLOR, "error")
                    } else if streaming {
                        (WARN_COLOR, "streaming")
                    } else if self.settings.api_key.trim().is_empty() {
                        (WARN_COLOR, "sin clave configurada")
                    } else {
                        (ADD_COLOR, "listo")
                    };
                    ui.colored_label(color, format!("● {hint}"));
                    let model = if self.settings.model.trim().is_empty() {
                        "modelo no configurado"
                    } else {
                        self.settings.model.as_str()
                    };
                    ui.weak(model);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Cerrar chat").clicked() {
                            close = true;
                        }
                        if ui.small_button("Ajustes").clicked() {
                            open_settings = true;
                        }
                        ui.weak(if self.settings.include_terminal_context {
                            "Terminal incluido"
                        } else {
                            "Terminal excluido"
                        });
                    });
                });
            });

        egui::TopBottomPanel::bottom("chat_composer").show(ctx, |ui| {
            ui.add_space(4.0);
            if streaming {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Generando…");
                });
            }
            if let Some(error) = &self.chat.error {
                ui.horizontal(|ui| {
                    ui.colored_label(DEL_COLOR, "Error:");
                    ui.label(error);
                });
            }
            if ctx.memory(|m| m.has_focus(composer_id)) && consume_plain_enter(ctx) && !streaming {
                submit = true;
            }
            let composer = ui.add(
                egui::TextEdit::multiline(&mut self.chat.input)
                    .id(composer_id)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("Pregunta al agente o escribe !comando"),
            );
            if self.chat_focus {
                composer.request_focus();
                self.chat_focus = false;
            }
            ui.horizontal(|ui| {
                let can_send = !self.chat.input.trim().is_empty() && !streaming;
                if ui
                    .add_enabled(can_send, egui::Button::new("Enviar"))
                    .clicked()
                {
                    submit = true;
                }
                if streaming {
                    if ui.button("Cancelar").clicked() {
                        cancel = true;
                    }
                } else if self.chat.error.is_some() && ui.button("Reintentar").clicked() {
                    retry = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak("Enter envía · Shift+Enter salto");
                });
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let visuals = ui.visuals().clone();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add_space(8.0);
                    let max_width = (ui.available_width() * 0.7).max(160.0);
                    for message in &self.chat.messages {
                        chat_bubble(ui, message, max_width, &visuals);
                        if message.role == Role::Assistant {
                            for block in chat_view::code_blocks(&message.content) {
                                if ui.small_button("Insertar en terminal").clicked() {
                                    insert = Some(block);
                                }
                            }
                        }
                        ui.add_space(6.0);
                    }
                    if !self.chat.partial.is_empty() {
                        let cursor = if blink(ui) { "▌" } else { "" };
                        let text = format!("{}{cursor}", self.chat.partial);
                        bubble_text(ui, &text, max_width, &visuals);
                        ui.add_space(6.0);
                    }
                    if self.chat.cancelled {
                        ui.weak("(cancelado)");
                        ui.add_space(6.0);
                    }
                    if self.chat.messages.is_empty()
                        && self.chat.partial.is_empty()
                        && self.chat.error.is_none()
                    {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            ui.weak(
                                "Pregunta sobre el repo o escribe !comando para ejecutarlo en el terminal.",
                            );
                        });
                    }
                });
        });

        let had_insert = insert.is_some();
        if let Some(block) = insert {
            self.insert_command(&block, ctx);
        }
        if cancel {
            self.cancel_chat(ctx);
        }
        if retry {
            self.retry_chat(ctx);
        }
        if submit {
            self.submit_chat(ctx);
        }
        if close {
            self.close_chat(ctx);
        }
        if open_settings {
            self.open_settings(ctx);
        } else if self.chat_open && !had_insert && !close && !ctx.memory(|m| m.has_focus(composer_id)) {
            self.chat_focus = true;
        }
    }

    fn poll_repo(&mut self, ctx: &egui::Context) {
        while let Ok((path, list)) = self.branch_rx.try_recv() {
            if let Some(picker) = self.picker.as_mut() {
                if self.repo_path.as_deref() == Some(path.as_str()) {
                    picker.branches = Some(list);
                }
            }
        }
        while let Ok((key, lines)) = self.diff_rx.try_recv() {
            if self.diff_key.as_ref() == Some(&key) {
                self.diff_lines = Some(lines);
            }
        }
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

    fn open_picker(&mut self, ctx: &egui::Context) {
        let Some(path) = self.repo_path.clone() else {
            return;
        };
        self.picker = Some(Picker {
            query: String::new(),
            branches: None,
            selected: 0,
            focus: true,
        });
        let tx = self.branch_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let list = branches(&path);
            let _ = tx.send((path, list));
            ctx.request_repaint();
        });
    }

    fn show_picker(&mut self, ctx: &egui::Context) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let (esc, enter, up, down) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            )
        });
        if esc {
            self.picker = None;
            return;
        }
        let mut chosen = None;
        egui::Window::new("Switch branch")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 56.0])
            .fixed_size([360.0, 320.0])
            .show(ctx, |ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut picker.query)
                        .hint_text("Search branches…")
                        .desired_width(f32::INFINITY),
                );
                if picker.focus {
                    edit.request_focus();
                    picker.focus = false;
                }
                if edit.changed() {
                    picker.selected = 0;
                }
                ui.separator();
                let Some(list) = picker.branches.as_ref() else {
                    ui.weak("Loading branches…");
                    return;
                };
                let matches: Vec<usize> = (0..list.len())
                    .filter(|&i| matches_query(&list[i].name, &picker.query))
                    .collect();
                if matches.is_empty() {
                    ui.weak("No matching branches");
                    return;
                }
                let moved = up || down;
                if up {
                    picker.selected = picker.selected.saturating_sub(1);
                }
                if down {
                    picker.selected += 1;
                }
                picker.selected = picker.selected.min(matches.len() - 1);
                if enter {
                    chosen = Some(matches[picker.selected]);
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (row, &i) in matches.iter().enumerate() {
                            let b = &list[i];
                            let resp =
                                ui.selectable_label(row == picker.selected, branch_row(ui, b));
                            if moved && row == picker.selected {
                                resp.scroll_to_me(None);
                            }
                            if resp.clicked() {
                                chosen = Some(i);
                            }
                        }
                    });
            });
        if let Some(i) = chosen {
            let picker = self.picker.take();
            let Some(b) = picker.as_ref().and_then(|p| p.branches.as_ref()?.get(i)) else {
                return;
            };
            if !b.is_current {
                if let Some(tab) = self.tabs.active_tab_mut() {
                    tab.backend
                        .process_command(BackendCommand::Write(checkout_command(b).into_bytes()));
                }
                self.last_probe = None;
            }
            ctx.request_repaint();
        }
    }

    fn show_changes(&mut self, ctx: &egui::Context) {
        let files: Vec<ChangedFile> = self
            .repo
            .as_ref()
            .map(|r| r.changes.clone())
            .unwrap_or_default();
        let (count, added, removed) = change_totals(&files);
        let valid =
            |sel: &(String, bool)| files.iter().any(|f| f.path == sel.0 && f.staged == sel.1);
        if !self.selected_file.as_ref().is_some_and(valid) {
            self.selected_file = files.first().map(|f| (f.path.clone(), f.staged));
        }
        let selected = self
            .selected_file
            .as_ref()
            .and_then(|(p, s)| files.iter().find(|f| &f.path == p && f.staged == *s));
        let wanted = selected
            .zip(self.repo_path.as_ref())
            .map(|(f, repo)| (repo.clone(), f.path.clone(), f.staged, f.added, f.removed));
        if wanted != self.diff_key {
            self.diff_key = wanted.clone();
            self.diff_lines = None;
            if let (Some(key), Some(file)) = (wanted, selected.cloned()) {
                let tx = self.diff_tx.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let text = diff_for(&key.0, &file);
                    let lines = text
                        .lines()
                        .take(MAX_DIFF_LINES)
                        .map(str::to_owned)
                        .collect();
                    let _ = tx.send((key, lines));
                    ctx.request_repaint();
                });
            }
        }

        let mut close = false;
        let mut pick = None;
        egui::SidePanel::right("changes")
            .default_width(360.0)
            .width_range(280.0..=560.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Uncommitted changes");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Close").clicked() {
                            close = true;
                        }
                    });
                });
                if count > 0 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.weak(format!("{count} {}", files_noun(count)));
                        ui.colored_label(ADD_COLOR, format!("+{added}"));
                        ui.colored_label(DEL_COLOR, format!("−{removed}"));
                    });
                }
                ui.separator();
                if files.is_empty() {
                    ui.weak("Working tree clean");
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("files")
                    .max_height(ui.available_height() * 0.4)
                    .show(ui, |ui| {
                        for staged in [true, false] {
                            let group: Vec<&ChangedFile> =
                                files.iter().filter(|f| f.staged == staged).collect();
                            if group.is_empty() {
                                continue;
                            }
                            ui.weak(if staged { "Staged" } else { "Unstaged" });
                            for f in group {
                                let is_sel = self.selected_file.as_ref()
                                    == Some(&(f.path.clone(), f.staged));
                                if ui.selectable_label(is_sel, file_row(f)).clicked() {
                                    pick = Some((f.path.clone(), f.staged));
                                }
                            }
                        }
                    });
                ui.separator();
                match (&self.diff_lines, selected) {
                    (None, _) => {
                        ui.weak("Loading diff…");
                    }
                    (Some(lines), Some(f)) if lines.is_empty() => {
                        ui.weak(if f.status == '?' {
                            "Untracked file — no diff"
                        } else {
                            "No diff available"
                        });
                    }
                    (Some(lines), _) => {
                        let height = ui.text_style_height(&egui::TextStyle::Monospace);
                        egui::ScrollArea::both()
                            .id_salt("diff")
                            .auto_shrink([false, false])
                            .show_rows(ui, height, lines.len(), |ui, range| {
                                for line in &lines[range] {
                                    let color = match classify_diff_line(line) {
                                        DiffLine::Add => ADD_COLOR,
                                        DiffLine::Del => DEL_COLOR,
                                        DiffLine::Meta => ui.visuals().weak_text_color(),
                                        DiffLine::Context => ui.visuals().text_color(),
                                    };
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(line).monospace().color(color),
                                        )
                                        .wrap_mode(egui::TextWrapMode::Extend),
                                    );
                                }
                            });
                    }
                }
            });
        if pick.is_some() {
            self.selected_file = pick;
        }
        if close {
            self.changes_open = false;
        }
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

#[derive(Debug, PartialEq, Eq)]
enum DiffLine {
    Add,
    Del,
    Meta,
    Context,
}

fn classify_diff_line(line: &str) -> DiffLine {
    const META: [&str; 8] = [
        "@@",
        "+++",
        "---",
        "diff ",
        "index ",
        "new file",
        "deleted file",
        "similarity",
    ];
    if META.iter().any(|p| line.starts_with(p)) {
        DiffLine::Meta
    } else if line.starts_with('+') {
        DiffLine::Add
    } else if line.starts_with('-') {
        DiffLine::Del
    } else {
        DiffLine::Context
    }
}

fn matches_query(name: &str, query: &str) -> bool {
    let query = query.trim();
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

fn files_noun(count: usize) -> &'static str {
    if count == 1 {
        "file"
    } else {
        "files"
    }
}

fn chat_composer_id() -> egui::Id {
    egui::Id::new("chat_composer")
}

fn blink(ui: &egui::Ui) -> bool {
    (ui.input(|input| input.time) * 2.0).fract() < 0.5
}

fn consume_plain_enter(ctx: &egui::Context) -> bool {
    let mut consumed = false;
    ctx.input_mut(|input| {
        let position = input.events.iter().position(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    modifiers,
                    ..
                } if !modifiers.shift && !modifiers.ctrl && !modifiers.command && !modifiers.alt
            )
        });
        if let Some(position) = position {
            input.events.remove(position);
            consumed = true;
        }
    });
    consumed
}

fn user_layout(text: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::monospace(12.0),
            color: egui::Color32::WHITE,
            ..Default::default()
        },
    );
    job
}

fn bubble_frame(
    ui: &mut egui::Ui,
    fill: egui::Color32,
    max_width: f32,
    add: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::NONE
        .fill(fill)
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_max_width(max_width);
            add(ui);
        });
}

fn chat_bubble(ui: &mut egui::Ui, message: &ChatMessage, max_width: f32, visuals: &egui::Visuals) {
    let wrap_width = (max_width - 24.0).max(80.0);
    if message.role == Role::User {
        let mut job = user_layout(&message.content);
        job.wrap.max_width = wrap_width;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            bubble_frame(ui, CHAT_ACCENT, max_width, |ui| {
                ui.add(egui::Label::new(job));
            });
        });
    } else {
        let mut job = chat_view::markdown_layout(&message.content, visuals);
        job.wrap.max_width = wrap_width;
        bubble_frame(
            ui,
            visuals.widgets.noninteractive.weak_bg_fill,
            max_width,
            |ui| {
                ui.add(egui::Label::new(job));
            },
        );
    }
}

fn bubble_text(ui: &mut egui::Ui, text: &str, max_width: f32, visuals: &egui::Visuals) {
    let mut job = chat_view::markdown_layout(text, visuals);
    job.wrap.max_width = (max_width - 24.0).max(80.0);
    bubble_frame(
        ui,
        visuals.widgets.noninteractive.weak_bg_fill,
        max_width,
        |ui| {
            ui.add(egui::Label::new(job));
        },
    );
}

pub fn bang_command(input: &str) -> Option<&str> {
    let command = input.strip_prefix('!')?.trim();
    if command.is_empty() || command.contains(['\n', '\r']) {
        None
    } else {
        Some(command)
    }
}

fn visible_terminal_line(line: &str) -> String {
    line.chars()
        .filter(|ch| *ch == '\t' || !ch.is_control())
        .collect()
}

fn is_bare_bang(input: &str) -> bool {
    input.trim() == "!"
}

fn command_target(active: Option<u64>) -> Result<u64, ()> {
    active.ok_or(())
}

fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        text.to_owned()
    } else {
        text.chars().skip(count - max).collect()
    }
}

fn build_context(
    repo: Option<&RepoStatus>,
    cwd: Option<&str>,
    selection: Option<&str>,
    lines: &[String],
    include_terminal: bool,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("Directorio: {}\n", cwd.unwrap_or("(desconocido)")));
    if let Some(repo) = repo {
        if let Some(git) = &repo.git {
            out.push_str(&format!("Rama: {}\n", format_git_label(git)));
        }
        if repo.changes.is_empty() {
            out.push_str("Cambios pendientes: ninguno\n");
        } else {
            out.push_str("Cambios pendientes:\n");
            for file in &repo.changes {
                let staged = if file.staged { "staged" } else { "unstaged" };
                out.push_str(&format!(
                    "- {} [{staged}] +{} −{}\n",
                    file.path, file.added, file.removed
                ));
            }
        }
    }
    if include_terminal {
        if let Some(selection) = selection.map(str::trim).filter(|text| !text.is_empty()) {
            out.push_str("Selección del terminal:\n");
            out.push_str(&tail_chars(selection, MAX_TERMINAL_CONTEXT_CHARS));
            out.push('\n');
        }
        if !lines.is_empty() {
            let joined = lines.join("\n");
            out.push_str("Últimas líneas del terminal:\n```\n");
            out.push_str(&tail_chars(&joined, MAX_TERMINAL_CONTEXT_CHARS));
            out.push_str("\n```\n");
        }
    }
    out
}

fn text_format(ui: &egui::Ui, color: egui::Color32) -> egui::TextFormat {
    egui::TextFormat {
        font_id: egui::TextStyle::Body.resolve(ui.style()),
        color,
        ..Default::default()
    }
}

fn branch_row(ui: &egui::Ui, b: &BranchInfo) -> egui::text::LayoutJob {
    let normal = ui.visuals().text_color();
    let dim = ui.visuals().weak_text_color();
    let mut job = egui::text::LayoutJob::default();
    if b.is_current {
        job.append("● ", 0.0, text_format(ui, ADD_COLOR));
    } else {
        job.append("   ", 0.0, text_format(ui, dim));
    }
    match b.name.split_once('/').filter(|_| b.is_remote) {
        Some((remote, rest)) => {
            job.append(&format!("{remote}/"), 0.0, text_format(ui, dim));
            job.append(rest, 0.0, text_format(ui, normal));
        }
        None => job.append(&b.name, 0.0, text_format(ui, normal)),
    }
    job
}

fn file_row(f: &ChangedFile) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font = egui::FontId::monospace(12.0);
    let mut part = |text: &str, color: egui::Color32| {
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
    };
    let status_color = match f.status {
        'A' | '?' => ADD_COLOR,
        'D' => DEL_COLOR,
        _ => WARN_COLOR,
    };
    part(&format!("{} ", f.status), status_color);
    part(&clip_title(&f.path, 34), egui::Color32::GRAY);
    if f.added > 0 || f.removed > 0 {
        part(&format!("  +{}", f.added), ADD_COLOR);
        part(&format!(" −{}", f.removed), DEL_COLOR);
    }
    job
}

fn chip(ui: &mut egui::Ui, job: egui::text::LayoutJob) -> egui::Response {
    ui.add(
        egui::Button::new(job)
            .fill(ui.visuals().widgets.inactive.weak_bg_fill)
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(10.0)
            .min_size(egui::vec2(0.0, 20.0)),
    )
}

/// Draws the git chip bar; returns (branch chip clicked, changes chip clicked).
fn show_repo_bar(ctx: &egui::Context, repo: &RepoStatus) -> (bool, bool) {
    let mut clicks = (false, false);
    egui::TopBottomPanel::top("git").show(ctx, |ui| {
        ui.horizontal(|ui| {
            let text = ui.visuals().text_color();
            if let Some(git) = &repo.git {
                let mut job = egui::text::LayoutJob::default();
                job.append(
                    &format!("{} ▾", format_git_label(git)),
                    0.0,
                    text_format(ui, text),
                );
                clicks.0 = chip(ui, job).on_hover_text("Switch branch").clicked();
            }
            let (count, added, removed) = change_totals(&repo.changes);
            if count > 0 {
                let mut job = egui::text::LayoutJob::default();
                job.append(
                    &format!("{count} {} · ", files_noun(count)),
                    0.0,
                    text_format(ui, WARN_COLOR),
                );
                job.append(&format!("+{added} "), 0.0, text_format(ui, ADD_COLOR));
                job.append(&format!("−{removed}"), 0.0, text_format(ui, DEL_COLOR));
                clicks.1 = chip(ui, job).on_hover_text("View changes").clicked();
            }
            if let Some(pr) = &repo.pr {
                let label = format!("#{} {}", pr.number, pr.state.to_lowercase());
                if ui.link(label).on_hover_text(&pr.title).clicked() && !open_url(&pr.url) {
                    log::warn!("failed to open pull request");
                }
            }
        });
    });
    clicks
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

        self.poll_chat(ctx);
        self.commit_agent_output();

        let mut exit_agent = false;
        if (self.inline_agent_open || self.agent_conversation)
            && agent_tab_is_active(self.tabs.active(), self.agent_tab)
            && !self.chat_open
            && !self.settings_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            exit_agent = true;
            if self.chat.rx.is_some() {
                self.cancel_chat(ctx);
            }
            self.commit_agent_output();
            self.agent_conversation = false;
            self.inline_agent_open = false;
            self.agent_tab = None;
            ctx.request_repaint();
        }

        if !self.settings_open {
            if ctx.input_mut(|i| {
                i.consume_key(
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::Enter,
                )
            }) {
                self.toggle_chat(ctx);
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
            let (branch, changes) = show_repo_bar(ctx, &repo);
            if branch {
                self.open_picker(ctx);
            }
            if changes {
                self.changes_open = !self.changes_open;
            }
        }
        if self.changes_open {
            self.show_changes(ctx);
        }
        self.show_picker(ctx);

        if self.settings_open {
            self.show_settings(ctx);
            return;
        }

        if self.chat_open {
            self.show_chat(ctx);
        } else {
            let mut inline_question = None;
            let mut bang_draft = None;
            let agent_tab_active = agent_tab_is_active(self.tabs.active(), self.agent_tab);
            let inline_lines = if agent_tab_active {
                self.inline_agent_lines()
            } else {
                Vec::new()
            };
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(tab) = self.tabs.active_tab_mut() {
                    let size = ui.available_size();
                    let terminal = TerminalView::new(ui, &mut tab.backend)
                        .set_focus(
                            !self.chat_open && !self.settings_open && self.picker.is_none(),
                        )
                        .set_size(size)
                        .set_bang_submit(&mut inline_question)
                        .set_bang_draft(&mut bang_draft)
                        .set_inline_lines(&inline_lines)
                        .set_agent_conversation(agent_tab_active && self.agent_conversation)
                        .set_release_overlay(agent_tab_active && self.agent_output_committed)
                        .set_clear_draft(exit_agent)
                        .add_bindings(ctrl_backspace_bindings());
                    ui.add(terminal);
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label("No tabs. Press Ctrl+T to open one.");
                    });
                }
            });

            if let Some(question) = inline_question {
                self.submit_inline_question(&question, ctx);
            }
        }
    }
}

fn agent_tab_is_active(active: Option<u64>, agent_tab: Option<u64>) -> bool {
    active.is_some() && active == agent_tab
}

#[cfg(test)]
mod tests {
    use super::{
        agent_tab_is_active, bang_command, build_context, classify_diff_line, clip_title, command_target,
        ctrl_backspace_bindings, files_noun, is_bare_bang, matches_query, tab_label, DiffLine,
        SettingsForm,
    };
    use egui_term::{BindingAction, InputKind};

    #[test]
    fn agent_mode_is_only_active_on_its_originating_tab() {
        assert!(agent_tab_is_active(Some(1), Some(1)));
        assert!(!agent_tab_is_active(Some(2), Some(1)));
        assert!(!agent_tab_is_active(None, None));
    }

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

    #[test]
    fn diff_lines_are_classified() {
        assert_eq!(classify_diff_line("+++ b/a"), DiffLine::Meta);
        assert_eq!(classify_diff_line("@@ -1 +1 @@"), DiffLine::Meta);
        assert_eq!(classify_diff_line("+x"), DiffLine::Add);
        assert_eq!(classify_diff_line("-x"), DiffLine::Del);
        assert_eq!(classify_diff_line(" x"), DiffLine::Context);
    }

    #[test]
    fn branch_filter_is_case_insensitive() {
        assert!(matches_query("origin/Feature-X", "feature"));
        assert!(matches_query("main", "  "));
        assert!(!matches_query("main", "dev"));
    }

    #[test]
    fn files_noun_pluralizes() {
        assert_eq!(files_noun(1), "file");
        assert_eq!(files_noun(2), "files");
    }

    #[test]
    fn bang_command_strips_prefix_and_ignores_bare_bang() {
        assert_eq!(bang_command("!git status"), Some("git status"));
        assert_eq!(bang_command("!"), None);
        assert_eq!(bang_command("git status"), None);
    }

    #[test]
    fn bare_bang_is_a_mode_switch_not_a_command() {
        assert!(is_bare_bang(" ! "));
        assert!(!is_bare_bang("!git status"));
    }

    #[test]
    fn bang_command_rejects_multiple_lines() {
        assert_eq!(bang_command("!echo ok\nrm -rf /"), None);
        assert_eq!(bang_command("!echo ok\recho bad"), None);
    }

    #[test]
    fn command_target_requires_active_tab() {
        assert_eq!(command_target(Some(7)), Ok(7));
        assert_eq!(command_target(None), Err(()));
    }

    #[test]
    fn settings_form_requires_base_url_and_keeps_other_fields() {
        let mut form = SettingsForm {
            base_url: "".into(),
            api_key: "k".into(),
            model: "m".into(),
            reveal_key: false,
        };
        assert_eq!(form.validate(), Err("base_url required"));
        form.base_url = "http://localhost:4000/v1".into();
        assert_eq!(form.validate(), Ok(()));
        let settings = form.to_settings();
        assert_eq!(settings.api_key, "k");
        assert_eq!(settings.model, "m");
    }

    #[test]
    fn context_respects_privacy_switch() {
        let lines = vec!["SECRETO_LINEA".to_owned()];
        let full = build_context(None, Some("/repo"), Some("SECRETO_SELECCION"), &lines, true);
        let redacted = build_context(
            None,
            Some("/repo"),
            Some("SECRETO_SELECCION"),
            &lines,
            false,
        );
        assert!(full.contains("SECRETO_LINEA") && full.contains("SECRETO_SELECCION"));
        assert!(redacted.contains("/repo"));
        assert!(!redacted.contains("SECRETO_LINEA") && !redacted.contains("SECRETO_SELECCION"));
    }
}
