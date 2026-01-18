use eframe::egui;
use crate::audio::AudioManager;
use crate::network::NetworkManager;
use crate::updater::{UpdateManager, UpdateStatus};
use std::collections::HashMap;
use std::time::Instant;
use std::sync::{Arc, Mutex};
use std::fs;
use serde::{Serialize, Deserialize};
use rfd::FileDialog;
use image;
use rodio::Source;

struct User {
    name: String,
    is_speaking: bool,
    is_muted: bool,
    is_deafened: bool,
    is_away: bool,
    role: String,
    status: String,
    nick_color: String,
}

struct Channel {
    name: String,
    users: Vec<User>,
    expanded: bool,
}

#[derive(PartialEq)]
enum InputMode {
    PushToTalk,
    VoiceActivity,
}

#[derive(Clone)]
pub struct ChatMessage {
    pub id: uuid::Uuid,
    pub username: String,
    pub message: String,
    pub timestamp: String,
    pub file_data: Option<(String, Vec<u8>, bool)>, // filename, data, is_image
    pub reactions: HashMap<String, Vec<String>>, // Emoji -> Vec of Users
}

pub struct PendingFile {
    pub filename: String,
    pub from: String,
    pub to: Option<String>,
    pub is_image: bool,
    pub timestamp: String,
    pub chunks: Vec<Option<Vec<u8>>>,
    pub total_chunks: usize,
    pub received_count: usize,
}

#[derive(PartialEq)]
enum ChatTab {
    Chat,
    Users,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub username: String,
    pub avatar_url: String,
    pub bio: String,
}

#[derive(Serialize, Deserialize)]
struct AuthConfig {
    username: String,
    password_hash: String,
    remember_me: bool,
}

pub struct SpeakVApp {
    audio_manager: Option<AudioManager>,
    network_manager: Option<NetworkManager>,
    update_manager: UpdateManager,
    
    // State
    username: String,
    password_input: String,
    is_authenticated: bool,
    is_register_mode: bool,
    auth_message: String,
    login_input: String,
    remember_me: bool,

    // Local User State
    is_muted: bool,
    is_deafened: bool,
    is_away: bool,

    channels: Vec<Channel>,
    current_channel_index: Option<usize>,
    push_to_talk_active: bool,
    
    // Settings State
    show_settings: bool,
    input_devices: Vec<String>,
    output_devices: Vec<String>,
    selected_input_device: String,
    selected_output_device: String,
    input_mode: InputMode,
    vad_threshold: f32,
    self_listen: bool,
    
    // UI State
    show_create_channel_dialog: bool,
    new_channel_name: String,
    server_address: String,
    is_connected: bool,
    
    // Chat State
    chat_messages: Vec<ChatMessage>,
    chat_input: String,
    show_chat: bool,
    pub outgoing_chat_tx: tokio::sync::mpsc::UnboundedSender<crate::network::NetworkPacket>,
    pub incoming_chat_rx: tokio::sync::mpsc::UnboundedReceiver<crate::network::NetworkPacket>,
    pub speaking_users_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    participants: Vec<String>,
    typing_users: HashMap<String, Instant>,
    speaking_users: HashMap<String, Instant>,
    user_volumes: Arc<Mutex<HashMap<String, f32>>>,
    last_typing_sent: Instant,
    active_chat_tab: ChatTab,
    role: String,
    status_input: String,
    nick_color_input: String,
    error_message: Option<String>,
    selected_dm_target: Option<String>,
    direct_messages: HashMap<String, Vec<ChatMessage>>,
    image_cache: HashMap<String, egui::TextureHandle>,
    pending_files: HashMap<uuid::Uuid, PendingFile>,
    dark_mode: bool,
    search_query: String,
    
    // v0.9.0.1 Identity & Audio (Stabilizer Update)
    remote_user_levels: Arc<Mutex<HashMap<String, f32>>>,
    show_profile_card: Option<String>,
    user_profiles: HashMap<String, UserProfile>,
    avatar_url_input: String,
    bio_input: String,
}

impl SpeakVApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Customize look and feel
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = egui::Color32::from_rgb(20, 20, 25); // Darker background
        visuals.panel_fill = egui::Color32::from_rgb(30, 30, 35);
        cc.egui_ctx.set_visuals(visuals);

        // Initialize Audio and Network
        let audio_manager = AudioManager::new().ok();
        let network_manager = NetworkManager::new().ok();
        
        // Get Devices
        let input_devices = AudioManager::get_input_devices();
        let output_devices = AudioManager::get_output_devices();
        let selected_input_device = input_devices.first().cloned().unwrap_or_default();
        let selected_output_device = output_devices.first().cloned().unwrap_or_default();

        // Load Auth Config
        let mut username = String::new();
        let mut password_input = String::new();
        let mut remember_me = false;
        
        if let Ok(config_json) = fs::read_to_string("auth_config.json") {
            if let Ok(config) = serde_json::from_str::<AuthConfig>(&config_json) {
                username = config.username.clone();
                password_input = config.password_hash.clone(); // In a real app, this should be a token or securely stored
                remember_me = config.remember_me;
            }
        } else if let Ok(saved_name) = fs::read_to_string("user_config.txt") {
            // Migration from old config
            let saved_name = saved_name.trim();
            if !saved_name.is_empty() {
                username = saved_name.to_string();
            }
        }

        // Channels
        let channels: Vec<Channel> = Vec::new(); // Will be populated by server

        let (outgoing_chat_tx, outgoing_chat_rx) = tokio::sync::mpsc::unbounded_channel();
        let (incoming_chat_tx, incoming_chat_rx) = tokio::sync::mpsc::unbounded_channel();
        let (speaking_users_tx, speaking_users_rx) = tokio::sync::mpsc::unbounded_channel();

        let user_volumes = if let Some(net) = &network_manager { net.user_volumes.clone() } else { Arc::new(Mutex::new(HashMap::new())) };
        let remote_user_levels = if let Some(net) = &network_manager { net.user_levels.clone() } else { Arc::new(Mutex::new(HashMap::new())) };

        let app = Self {
            audio_manager,
            network_manager,
            update_manager: UpdateManager::new(),
            username: username.clone(),
            login_input: username,
            password_input,
            remember_me,
            is_authenticated: false,
            is_register_mode: false,
            auth_message: String::new(),
            
            is_muted: false,
            is_deafened: false,
            is_away: false,
            
            channels,
            current_channel_index: Some(0),
            push_to_talk_active: false,
            
            show_settings: false,
            input_devices,
            output_devices,
            selected_input_device,
            selected_output_device,
            input_mode: InputMode::PushToTalk,
            vad_threshold: 0.05,
            self_listen: false,
            
            show_create_channel_dialog: false,
            new_channel_name: String::new(),
            server_address: "127.0.0.1:9999".to_string(),
            is_connected: false,
            
            chat_messages: Vec::new(),
            chat_input: String::new(),
            show_chat: true,
            outgoing_chat_tx: outgoing_chat_tx.clone(),
            incoming_chat_rx,
            speaking_users_rx,
            participants: Vec::new(),
            typing_users: HashMap::new(),
            speaking_users: HashMap::new(),
            user_volumes,
            last_typing_sent: Instant::now(),
            active_chat_tab: ChatTab::Chat,
            role: "User".to_string(),
            status_input: String::new(),
            nick_color_input: "#FFFFFF".to_string(),
            error_message: None,
            selected_dm_target: None,
            direct_messages: HashMap::new(),
            image_cache: HashMap::new(),
            pending_files: HashMap::new(),
            dark_mode: true,
            search_query: String::new(),

            // v0.9.0.1
            remote_user_levels,
            show_profile_card: None,
            user_profiles: HashMap::new(),
            avatar_url_input: String::new(),
            bio_input: String::new(),
        };

        // Auto-connect and auto-login if remember_me is true
        if let (Some(net), Some(audio)) = (&app.network_manager, &app.audio_manager) {
            let net_clone = net.clone();
            let input_cons = audio.input_consumer.clone();
            let remote_prod = audio.remote_producer.clone();
            let addr = app.server_address.clone();
            let outgoing_tx = app.outgoing_chat_tx.clone();
            let username_clone = app.username.clone();
            let password_clone = app.password_input.clone();
            let remember_me_clone = app.remember_me;
            let ctx_clone = cc.egui_ctx.clone();
            
            // Channel ends for network task
            let network_out_rx = outgoing_chat_rx;
            let network_in_tx = incoming_chat_tx;
            let network_speaking_tx = speaking_users_tx;

            tokio::spawn(async move {
                let _ = net_clone.start(addr, input_cons, remote_prod, network_out_rx, network_in_tx, network_speaking_tx, ctx_clone, username_clone.clone());

                // Send handshake
                let _ = outgoing_tx.send(crate::network::NetworkPacket::Handshake { 
                    username: username_clone.clone() 
                });

                // Auto-login
                if remember_me_clone && !username_clone.is_empty() && !password_clone.is_empty() {
                    let _ = outgoing_tx.send(crate::network::NetworkPacket::Login {
                        username: username_clone,
                        password: password_clone,
                    });
                }
            });
        }

        app
    }

    fn save_auth_config(&self) {
        let config = AuthConfig {
            username: self.username.clone(),
            password_hash: if self.remember_me { self.password_input.clone() } else { String::new() },
            remember_me: self.remember_me,
        };
        if let Ok(config_json) = serde_json::to_string(&config) {
            let _ = fs::write("auth_config.json", config_json);
        }
    }

    fn logout(&mut self) {
        self.is_authenticated = false;
        self.username.clear();
        self.login_input.clear();
        self.password_input.clear();
        self.remember_me = false;
        self.chat_messages.clear();
        self.direct_messages.clear();
        self.channels.clear();
        self.save_auth_config();
        
        // Also remove legacy config
        let _ = fs::remove_file("user_config.txt");
    }

    fn render_update_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!("Current Version: {}", self.update_manager.current_version));
        });
        
        ui.add_space(5.0);
        
        // Display update status
        if let Ok(mut status) = self.update_manager.status.lock() {
            match &*status {
                UpdateStatus::Idle => {
                    if ui.button("🔍 Check for Updates").clicked() {
                        self.update_manager.check_for_updates("RomaniukOleksii", "SpeakV");
                    }
                }
                UpdateStatus::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Checking for updates...");
                    });
                }
                UpdateStatus::UpdateAvailable(version) => {
                    ui.label(egui::RichText::new(format!("✨ Update available: v{}", version))
                        .color(egui::Color32::GREEN)
                        .strong());
                    ui.add_space(5.0);
                    if ui.button("⬇ Download and Install").clicked() {
                        self.update_manager.download_and_install("RomaniukOleksii", "SpeakV");
                    }
                }
                UpdateStatus::NoUpdateAvailable => {
                    ui.label(egui::RichText::new("✓ You're up to date!")
                        .color(egui::Color32::GREEN));
                    ui.add_space(5.0);
                    if ui.button("🔄 Check Again").clicked() {
                        self.update_manager.check_for_updates("RomaniukOleksii", "SpeakV");
                    }
                }
                UpdateStatus::Downloading => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Downloading update...");
                    });
                }
                UpdateStatus::Installing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Installing update...");
                    });
                }
                UpdateStatus::Success => {
                    ui.label(egui::RichText::new("✓ Update installed successfully!")
                        .color(egui::Color32::GREEN)
                        .strong());
                    ui.label("Would you like to restart now?");
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        if ui.button("🚀 Restart Now").clicked() {
                            std::process::exit(0);
                        }
                        if ui.button("⏱ Later").clicked() {
                            *status = UpdateStatus::Idle; // Reset to idle so it goes away
                        }
                    });
                }
                UpdateStatus::Error(err) => {
                    ui.label(egui::RichText::new(format!("❌ Error: {}", err))
                        .color(egui::Color32::RED));
                    ui.add_space(5.0);
                    if ui.button("🔄 Try Again").clicked() {
                        self.update_manager.check_for_updates("RomaniukOleksii", "SpeakV");
                    }
                }
            }
        }
    }

    fn render_markdown_text(&self, ui: &mut egui::Ui, text: &str) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let mut current = text;
            while !current.is_empty() {
                if current.starts_with("**") {
                    if let Some(end) = current[2..].find("**") {
                        let inner = &current[2..2+end];
                        ui.label(egui::RichText::new(inner).strong());
                        current = &current[2+end+2..];
                        continue;
                    }
                }
                if current.starts_with("*") {
                    if let Some(end) = current[1..].find("*") {
                        let inner = &current[1..1+end];
                        ui.label(egui::RichText::new(inner).italics());
                        current = &current[1+end+1..];
                        continue;
                    }
                }
                if current.starts_with("`") {
                    if let Some(end) = current[1..].find("`") {
                        let inner = &current[1..1+end];
                        ui.add(egui::Label::new(
                            egui::RichText::new(inner)
                                .monospace()
                                .background_color(ui.visuals().code_bg_color)
                        ));
                        current = &current[1+end+1..];
                        continue;
                    }
                }
                let next_trigger = ["**", "*", "`"].iter()
                    .filter_map(|t| current[1..].find(*t).map(|i| i + 1))
                    .min()
                    .unwrap_or(current.len());
                ui.label(&current[..next_trigger]);
                current = &current[next_trigger..];
            }
        });
    }
}

fn play_notification_beep() {
    std::thread::spawn(|| {
        if let Ok((_stream, stream_handle)) = rodio::OutputStream::try_default() {
            let sink = rodio::Sink::try_new(&stream_handle).unwrap();
            let source = rodio::source::SineWave::new(880.0)
                .take_duration(std::time::Duration::from_millis(100))
                .amplify(0.2);
            sink.append(source);
            sink.sleep_until_end();
        }
    });
}

fn hex_to_color(hex: &str) -> Result<egui::Color32, ()> {
    if !hex.starts_with('#') || hex.len() != 7 {
        return Err(());
    }
    let r = u8::from_str_radix(&hex[1..3], 16).map_err(|_| ())?;
    let g = u8::from_str_radix(&hex[3..5], 16).map_err(|_| ())?;
    let b = u8::from_str_radix(&hex[5..7], 16).map_err(|_| ())?;
    Ok(egui::Color32::from_rgb(r, g, b))
}

fn render_waveform(ui: &mut egui::Ui, level: f32, color: egui::Color32) {
    let count = 5;
    let spacing = 2.0;
    let width = 2.0;
    let max_height = 10.0;
    
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(count as f32 * (width + spacing), max_height),
        egui::Sense::hover()
    );
    
    let time = ui.input(|i| i.time);
    
    for i in 0..count {
        let phase = i as f64 * 0.5 + time * 10.0;
        let anim = (phase.sin() as f32 + 1.0) * 0.5;
        let h = (level * max_height * anim).max(1.0);
        let x = rect.left() + i as f32 * (width + spacing);
        let y = rect.center().y - h / 2.0;
        
        ui.painter().rect_filled(
            egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width, h)),
            2.0,
            color
        );
    }
}

impl eframe::App for SpeakVApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.dark_mode {
            ctx.set_visuals(egui::Visuals::dark());
        } else {
            ctx.set_visuals(egui::Visuals::light());
        }

        // Process incoming packets
        // Handle incoming network chat messages
        if let Some(net) = &self.network_manager {
            self.is_connected = *net.is_connected.lock().unwrap();
            while let Ok(packet) = self.incoming_chat_rx.try_recv() {
                match packet {
                    crate::network::NetworkPacket::ChatMessage { id, username, message, timestamp } => {
                        let decrypted_msg = crate::network::decrypt_bytes(&message)
                            .and_then(|b| String::from_utf8(b).ok())
                            .unwrap_or_else(|| "[Decryption Failed]".to_string());

                        self.chat_messages.push(ChatMessage {
                            id,
                            username: username.clone(),
                            message: decrypted_msg,
                            timestamp,
                            file_data: None,
                            reactions: HashMap::new(),
                        });
                        if username != self.username {
                            play_notification_beep();
                        }
                    }
                    crate::network::NetworkPacket::AuthResponse { success, message, role, status, nick_color } => {
                        self.is_authenticated = success;
                        self.auth_message = message;
                        if success {
                            self.username = self.login_input.clone();
                            if let Some(r) = role { self.role = r; }
                            if let Some(s) = status { self.status_input = s; }
                            if let Some(c) = nick_color { self.nick_color_input = c; }
                            self.save_auth_config();
                        }
                    }
                    crate::network::NetworkPacket::UsersUpdate(chan_state) => {
                        self.participants.clear();
                        for (_chan_name, users) in &chan_state {
                            for user_info in users {
                                if !self.participants.contains(&user_info.username) {
                                    self.participants.push(user_info.username.clone());
                                }
                            }
                        }

                        let mut new_channels = Vec::new();
                        for (chan_name, users) in chan_state {
                            let expanded = self.channels.iter()
                                .find(|c| c.name == chan_name)
                                .map(|c| c.expanded)
                                .unwrap_or(true);
                            
                            let mut user_list = Vec::new();
                            for user_info in users {
                                user_list.push(User {
                                    name: user_info.username.clone(),
                                    is_speaking: self.speaking_users.contains_key(&user_info.username),
                                    is_muted: user_info.is_muted,
                                    is_deafened: false,
                                    is_away: false,
                                    role: user_info.role,
                                    status: user_info.status,
                                    nick_color: user_info.nick_color,
                                });
                            }

                            new_channels.push(Channel {
                                name: chan_name,
                                users: user_list,
                                expanded,
                            });
                        }
                        self.channels = new_channels;

                        if let Some(_net) = &self.network_manager {
                            for (idx, chan) in self.channels.iter().enumerate() {
                                if chan.users.iter().any(|u| u.name == self.username) {
                                    self.current_channel_index = Some(idx);
                                    break;
                                }
                            }
                        }
                    }
                    crate::network::NetworkPacket::NetworkError(msg) => {
                        self.error_message = Some(msg);
                        self.is_connected = false;
                    }
                    crate::network::NetworkPacket::TypingStatus { username, is_typing } => {
                        if is_typing {
                            self.typing_users.insert(username, Instant::now());
                        } else {
                            self.typing_users.remove(&username);
                        }
                    }
                    crate::network::NetworkPacket::ProfileUpdate { username, avatar_url, bio } => {
                        self.user_profiles.insert(username.clone(), UserProfile {
                            username,
                            avatar_url,
                            bio,
                        });
                    }
                    crate::network::NetworkPacket::PrivateMessage { id, from, to, message, timestamp } => {
                        let decrypted_msg = crate::network::decrypt_bytes(&message)
                            .and_then(|b| String::from_utf8(b).ok())
                            .unwrap_or_else(|| "[Decryption Failed]".to_string());

                        let other = if from == self.username { to.clone() } else { from.clone() };
                        self.direct_messages.entry(other.clone()).or_default().push(ChatMessage {
                            id,
                            username: from,
                            message: decrypted_msg,
                            timestamp,
                            file_data: None,
                            reactions: HashMap::new(),
                        });
                        play_notification_beep();
                    }
                    crate::network::NetworkPacket::FileMessage { id, from, to, filename, data, is_image, timestamp } => {
                        let other = if from == self.username { to.clone().unwrap_or_default() } else { from.clone() };
                        if !other.is_empty() {
                            self.direct_messages.entry(other).or_default().push(ChatMessage {
                                id,
                                username: from,
                                message: format!("Sent a file: {}", filename),
                                timestamp,
                                file_data: Some((filename, data, is_image)),
                                reactions: HashMap::new(),
                            });
                        } else {
                            self.chat_messages.push(ChatMessage {
                                id,
                                username: from,
                                message: format!("Sent a file: {}", filename),
                                timestamp,
                                file_data: Some((filename, data, is_image)),
                                reactions: HashMap::new(),
                            });
                        }
                        play_notification_beep();
                    }
                    crate::network::NetworkPacket::DirectHistory(history) => {
                        if let Some(target) = &self.selected_dm_target {
                            let msgs = self.direct_messages.entry(target.clone()).or_default();
                            msgs.clear();
                            for p in history {
                                match p {
                                    crate::network::NetworkPacket::PrivateMessage { id, from, to: _, message, timestamp } => {
                                        let decrypted_msg = crate::network::decrypt_bytes(&message)
                                            .and_then(|b| String::from_utf8(b).ok())
                                            .unwrap_or_else(|| "[Decryption Failed]".to_string());
                                        let display_name = if from == self.username { "You".to_string() } else { from };
                                        msgs.push(ChatMessage {
                                            id,
                                            username: display_name,
                                            message: decrypted_msg,
                                            timestamp,
                                            file_data: None,
                                            reactions: HashMap::new(),
                                        });
                                    }
                                    crate::network::NetworkPacket::FileMessage { id, from, to: _, filename, data, is_image, timestamp } => {
                                        let display_name = if from == self.username { "You".to_string() } else { from };
                                        msgs.push(ChatMessage {
                                            id,
                                            username: display_name,
                                            message: format!("Sent a file: {}", filename),
                                            timestamp,
                                            file_data: Some((filename, data, is_image)),
                                            reactions: HashMap::new(),
                                        });
                                    }
                                    crate::network::NetworkPacket::Reaction { msg_id, emoji, from } => {
                                        for m in msgs.iter_mut() {
                                            if m.id == msg_id {
                                                m.reactions.entry(emoji.clone()).or_default().push(from.clone());
                                                break;
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    crate::network::NetworkPacket::ChatHistory(history) => {
                        self.chat_messages.clear();
                        for p in history {
                            match p {
                                crate::network::NetworkPacket::ChatMessage { id, username, message, timestamp } => {
                                    let decrypted_msg = crate::network::decrypt_bytes(&message)
                                        .and_then(|b| String::from_utf8(b).ok())
                                        .unwrap_or_else(|| "[Decryption Failed]".to_string());
                                    self.chat_messages.push(ChatMessage {
                                        id,
                                        username,
                                        message: decrypted_msg,
                                        timestamp,
                                        file_data: None,
                                        reactions: HashMap::new(),
                                    });
                                }
                                crate::network::NetworkPacket::FileMessage { id, from, to: _, filename, data, is_image, timestamp } => {
                                    self.chat_messages.push(ChatMessage {
                                        id,
                                        username: from,
                                        message: format!("Sent a file: {}", filename),
                                        timestamp,
                                        file_data: Some((filename, data, is_image)),
                                        reactions: HashMap::new(),
                                    });
                                }
                                crate::network::NetworkPacket::Reaction { msg_id, emoji, from } => {
                                    for m in self.chat_messages.iter_mut() {
                                        if m.id == msg_id {
                                            m.reactions.entry(emoji.clone()).or_default().push(from.clone());
                                            break;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    crate::network::NetworkPacket::FileStart { id, from, to, filename, total_chunks, is_image, timestamp } => {
                        self.pending_files.insert(id, PendingFile {
                            filename, from, to, is_image, timestamp,
                            chunks: vec![None; total_chunks],
                            received_count: 0,
                            total_chunks,
                        });
                    }
                    crate::network::NetworkPacket::FileChunk { id, chunk_index, data } => {
                        if let Some(pending) = self.pending_files.get_mut(&id) {
                            if chunk_index < pending.total_chunks && pending.chunks[chunk_index].is_none() {
                                pending.chunks[chunk_index] = Some(data);
                                pending.received_count += 1;
                                
                                if pending.received_count == pending.total_chunks {
                                    let mut full_data = Vec::new();
                                    for chunk in pending.chunks.drain(..) {
                                        if let Some(c) = chunk { full_data.extend(c); }
                                    }
                                    let from = pending.from.clone();
                                    let to = pending.to.clone();
                                    let filename = pending.filename.clone();
                                    let is_image = pending.is_image;
                                    let timestamp = pending.timestamp.clone();
                                    
                                    if let Some(target_dm) = to {
                                        let other = if from == self.username { target_dm } else { from.clone() };
                                        self.direct_messages.entry(other.clone()).or_default().push(ChatMessage {
                                            id,
                                            username: from,
                                            message: format!("Sent a file: {}", filename),
                                            timestamp,
                                            file_data: Some((filename, full_data, is_image)),
                                            reactions: HashMap::new(),
                                        });
                                    } else {
                                        self.chat_messages.push(ChatMessage {
                                            id,
                                            username: from,
                                            message: format!("Sent a file: {}", filename),
                                            timestamp,
                                            file_data: Some((filename, full_data, is_image)),
                                            reactions: HashMap::new(),
                                        });
                                    }
                                    play_notification_beep();
                                    self.pending_files.remove(&id);
                                }
                            }
                        }
                    }
                    crate::network::NetworkPacket::Reaction { msg_id, emoji, from } => {
                        let mut found = false;
                        for m in self.chat_messages.iter_mut() {
                            if m.id == msg_id {
                                m.reactions.entry(emoji.clone()).or_default().push(from.clone());
                                found = true; break;
                            }
                        }
                        if !found {
                            for msgs in self.direct_messages.values_mut() {
                                for m in msgs.iter_mut() {
                                    if m.id == msg_id {
                                        m.reactions.entry(emoji.clone()).or_default().push(from.clone());
                                        found = true; break;
                                    }
                                }
                                if found { break; }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Clean up old typing statuses (older than 3 seconds)
        self.typing_users.retain(|_, &mut last_seen| last_seen.elapsed().as_secs_f32() < 3.0);
        
        // Handle speaking indicators
        while let Ok(username) = self.speaking_users_rx.try_recv() {
            self.speaking_users.insert(username, Instant::now());
        }
        self.speaking_users.retain(|_, &mut last_seen| last_seen.elapsed().as_secs_f32() < 0.2);

        // Auth Screen
        if !self.is_authenticated {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(100.0);
                    ui.heading(egui::RichText::new("SpeakV").size(40.0).strong().color(egui::Color32::from_rgb(0, 255, 128)));
                    ui.label(egui::RichText::new("Secure Communication").size(16.0).color(egui::Color32::GRAY));
                    ui.add_space(40.0);
                    
                    let frame = egui::Frame::group(ui.style())
                        .fill(egui::Color32::from_rgb(40, 40, 45))
                        .rounding(8.0)
                        .inner_margin(20.0);

                    frame.show(ui, |ui| {
                        ui.set_width(300.0);
                        ui.heading(if self.is_register_mode { "Register" } else { "Login" });
                        ui.add_space(10.0);

                        ui.label("Username:");
                        ui.text_edit_singleline(&mut self.login_input);
                        ui.add_space(10.0);

                        ui.label("Password:");
                        ui.add(egui::TextEdit::singleline(&mut self.password_input).password(true));
                        ui.add_space(20.0);

                        if !self.auth_message.is_empty() {
                            let color = if self.is_authenticated { egui::Color32::GREEN } else { egui::Color32::LIGHT_RED };
                            ui.label(egui::RichText::new(&self.auth_message).color(color));
                        }

                        ui.horizontal(|ui| {
                            let btn_text = if self.is_register_mode { "Register" } else { "Login" };
                            if ui.add(egui::Button::new(btn_text).min_size(egui::vec2(100.0, 30.0))).clicked() {
                                if self.login_input.trim().is_empty() || self.password_input.trim().is_empty() {
                                    self.auth_message = "Please enter both username and password".to_string();
                                } else {
                                    self.auth_message = "Connecting...".to_string();
                                    
                                    // Connect if not connected
                                    if !self.is_connected {
                                        if let (Some(net), Some(audio)) = (&mut self.network_manager, &self.audio_manager) {
                                            let (tx_out, rx_out) = tokio::sync::mpsc::unbounded_channel();
                                            let (tx_in, rx_in) = tokio::sync::mpsc::unbounded_channel();
                                            let (tx_sp, rx_sp) = tokio::sync::mpsc::unbounded_channel();
                                            
                                            self.outgoing_chat_tx = tx_out.clone();
                                            self.incoming_chat_rx = rx_in;
                                            self.speaking_users_rx = rx_sp;

                                            net.start(
                                                self.server_address.clone(),
                                                audio.input_consumer.clone(),
                                                audio.remote_producer.clone(),
                                                rx_out,
                                                tx_in,
                                                tx_sp,
                                                ctx.clone(),
                                                self.login_input.clone(),
                                            );

                                            // Explicitly send handshake
                                            let _ = tx_out.send(crate::network::NetworkPacket::Handshake { 
                                                username: self.login_input.clone() 
                                            });
                                        }
                                    }

                                    // Send Auth Packet
                                    let packet = if self.is_register_mode {
                                        crate::network::NetworkPacket::Register { 
                                            username: self.login_input.clone(), 
                                            password: self.password_input.clone() 
                                        }
                                    } else {
                                        crate::network::NetworkPacket::Login { 
                                            username: self.login_input.clone(), 
                                            password: self.password_input.clone() 
                                        }
                                    };
                                    let _ = self.outgoing_chat_tx.send(packet);
                                }
                            }
                            
                            ui.checkbox(&mut self.remember_me, "Remember Me");
                            ui.add_space(5.0);

                            if ui.button(if self.is_register_mode { "Switch to Login" } else { "Switch to Register" }).clicked() {
                                self.is_register_mode = !self.is_register_mode;
                                self.auth_message.clear();
                                self.password_input.clear();
                            }
                        });
                    });

                    ui.add_space(40.0);
                    ui.label("Server Address:");
                    ui.text_edit_singleline(&mut self.server_address);
                    
                    ui.add_space(20.0);
                    ui.separator();
                    ui.add_space(10.0);
                    self.render_update_section(ui);
                });
            });
            return;
        }

        // Main App UI
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("SpeakV").strong().color(egui::Color32::from_rgb(0, 255, 128)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙ Settings").clicked() {
                        self.show_settings = true;
                    }
                    ui.add_space(10.0);
                    
                    // Away Button
                    let away_icon = if self.is_away { "🌙" } else { "☀️" };
                    let away_btn = egui::Button::new(away_icon).fill(if self.is_away { egui::Color32::from_rgb(100, 100, 255) } else { egui::Color32::from_rgb(60, 60, 60) });
                    if ui.add(away_btn).on_hover_text("Toggle Away Status").clicked() {
                        self.is_away = !self.is_away;
                    }

                    ui.add_space(5.0);

                    // Mute/Deafen Buttons
                    let mute_icon = if self.is_muted { "🔇" } else { "🎤" };
                    let mute_btn = egui::Button::new(mute_icon).fill(if self.is_muted { egui::Color32::RED } else { egui::Color32::from_rgb(60, 60, 60) });
                    if ui.add(mute_btn).on_hover_text("Mute Microphone").clicked() {
                        self.is_muted = !self.is_muted;
                        if let Some(audio) = &self.audio_manager {
                            audio.set_input_muted(self.is_muted);
                        }
                    }

                    ui.add_space(5.0);

                    let deafen_icon = if self.is_deafened { "🙉" } else { "🎧" };
                    let deafen_btn = egui::Button::new(deafen_icon).fill(if self.is_deafened { egui::Color32::RED } else { egui::Color32::from_rgb(60, 60, 60) });
                    if ui.add(deafen_btn).on_hover_text("Deafen (Mute Sound)").clicked() {
                        self.is_deafened = !self.is_deafened;
                        if self.is_deafened {
                            self.is_muted = true;
                            if let Some(audio) = &self.audio_manager {
                                audio.set_input_muted(true);
                            }
                        }
                        if let Some(audio) = &self.audio_manager {
                            audio.set_output_muted(self.is_deafened);
                        }
                    }

                    ui.add_space(10.0);
                    if ui.button("➕ Create Channel").clicked() {
                        self.show_create_channel_dialog = true;
                    }
                    ui.add_space(10.0);

                    // Connection UI
                    ui.horizontal(|ui| {
                        ui.label("Server:");
                        ui.add(egui::TextEdit::singleline(&mut self.server_address).desired_width(120.0));
                        
                        let (btn_text, btn_color) = if self.is_connected {
                            ("Disconnect", egui::Color32::from_rgb(200, 50, 50))
                        } else {
                            ("Connect", egui::Color32::from_rgb(50, 150, 50))
                        };

                        if ui.add(egui::Button::new(btn_text).fill(btn_color)).clicked() {
                            if self.is_connected {
                                if let Some(net) = &self.network_manager {
                                    net.stop();
                                }
                            } else {
                                if let (Some(net), Some(audio)) = (&mut self.network_manager, &self.audio_manager) {
                                    let (tx_out, rx_out) = tokio::sync::mpsc::unbounded_channel();
                                    let (tx_in, rx_in) = tokio::sync::mpsc::unbounded_channel();
                                    let (tx_sp, rx_sp) = tokio::sync::mpsc::unbounded_channel();
                                    
                                    self.outgoing_chat_tx = tx_out.clone();
                                    self.incoming_chat_rx = rx_in;
                                    self.speaking_users_rx = rx_sp;

                                    net.start(
                                        self.server_address.clone(),
                                        audio.input_consumer.clone(),
                                        audio.remote_producer.clone(),
                                        rx_out,
                                        tx_in,
                                        tx_sp,
                                        ctx.clone(),
                                        self.username.clone(),
                                    );

                                    // Send handshake
                                    let _ = tx_out.send(crate::network::NetworkPacket::Handshake { 
                                        username: self.username.clone() 
                                    });
                                }
                            }
                        }
                        
                        // Sync connection state
                        if let Some(net) = &self.network_manager {
                            if let Ok(connected) = net.is_connected.lock() {
                                self.is_connected = *connected;
                            }
                        }
                    });

                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(format!("Logged in as: {}", self.username)).color(egui::Color32::LIGHT_GRAY));
                });
            });
        });

        // Left Panel: Channel Tree
        egui::SidePanel::left("left_panel")
            .resizable(true)
            .default_width(250.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(egui::RichText::new("Server Tree").color(egui::Color32::WHITE));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("➕").on_hover_text("Create New Channel").clicked() {
                            self.show_create_channel_dialog = true;
                        }
                    });
                });
                ui.separator();
                
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let channel_to_join = None;

                    for (idx, channel) in self.channels.iter_mut().enumerate() {
                        ui.push_id(idx, |ui| {
                            let header_text = egui::RichText::new(&channel.name)
                                .strong()
                                .color(egui::Color32::from_rgb(200, 200, 200));
                                
                            let header = egui::CollapsingHeader::new(header_text)
                                .default_open(channel.expanded);

                            header.show(ui, |ui| {
                                let is_current = self.current_channel_index == Some(idx);
                                let label_text = if is_current { 
                                    egui::RichText::new("Connected").color(egui::Color32::GREEN) 
                                } else { 
                                    egui::RichText::new("Join Channel").color(egui::Color32::GRAY) 
                                };
                                
                                if ui.selectable_label(is_current, label_text).clicked() {
                                    if let Some(_net) = &self.network_manager {
                                        self.chat_messages.clear(); // Clear old messages immediately
                                        let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::JoinChannel(channel.name.clone()));
                                        let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::RequestChatHistory { channel: channel.name.clone() });
                                    }
                                }

                                for user in &channel.users {
                                    ui.horizontal(|ui| {
                                        let is_me = user.name == self.username;
                                        let mut icon = "👤";
                                        let mut color = egui::Color32::LIGHT_GRAY;
                                        
                                        if is_me {
                                            if self.is_away {
                                                icon = "🌙";
                                                color = egui::Color32::from_rgb(100, 100, 255);
                                            } else if self.is_deafened {
                                                icon = "🙉";
                                                color = egui::Color32::RED;
                                            } else if self.is_muted {
                                                icon = "🔇";
                                                color = egui::Color32::RED;
                                            } else if self.push_to_talk_active {
                                                icon = "🟢";
                                                color = egui::Color32::GREEN;
                                            } else {
                                                color = egui::Color32::WHITE;
                                            }
                                        } else {
                                            if user.is_away {
                                                icon = "🌙";
                                                color = egui::Color32::from_rgb(100, 100, 255);
                                            } else if user.is_deafened {
                                                icon = "🙉";
                                                color = egui::Color32::RED;
                                            } else if user.is_muted {
                                                icon = "🔇";
                                                color = egui::Color32::RED;
                                            } else if user.is_speaking {
                                                icon = "🔵";
                                                color = egui::Color32::from_rgb(100, 200, 255);
                                            }
                                        }

                                        let level = {
                                            let levels = self.remote_user_levels.lock().unwrap();
                                            *levels.get(&user.name).unwrap_or(&0.0)
                                        };

                                        if level > 0.01 {
                                            render_waveform(ui, level.min(1.0), egui::Color32::from_rgb(0, 255, 128));
                                            ui.add_space(4.0);
                                        }

                                        let display_name = if is_me { format!("{} (You)", user.name) } else { user.name.clone() };
                                        let mut label = egui::RichText::new(format!("{} {}", icon, display_name)).color(color);
                                        
                                        // Apply nick color
                                        if let Ok(c) = hex_to_color(&user.nick_color) {
                                            label = label.color(c);
                                        }

                                        if user.role == "Admin" {
                                            label = label.strong();
                                        }

                                        let resp = ui.add(egui::Button::new(label).frame(false)).on_hover_text("Click to view profile");
                                        if resp.clicked() {
                                            self.show_profile_card = Some(user.name.clone());
                                            let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::RequestProfile(user.name.clone()));
                                        }
                                        if !user.status.is_empty() {
                                            ui.label(egui::RichText::new(format!("({})", user.status)).size(10.0).color(egui::Color32::GRAY));
                                        }

                                        // Mixer & DM buttons
                                        if !is_me {
                                            ui.add_space(5.0);
                                            // DM Button
                                            if ui.button("✉").on_hover_text("Send Private Message").clicked() {
                                                self.selected_dm_target = Some(user.name.clone());
                                                // Request history if not loaded? Or just always request.
                                                let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::RequestDirectHistory { target: user.name.clone() });
                                            }

                                            // Volume Slider
                                            let mut volumes = self.user_volumes.lock().unwrap();
                                            let vol = volumes.entry(user.name.clone()).or_insert(1.0);
                                            ui.add(egui::Slider::new(vol, 0.0..=2.0).show_value(false).text("🔊"));
                                        }
                                        
                                        // Admin context menu
                                        if self.role == "Admin" && user.name != self.username {
                                            resp.context_menu(|ui| {
                                                ui.heading(format!("Admin Action for {}", user.name));
                                                if ui.button("🔇 Mute (Server-wide)").clicked() {
                                                    let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::AdminAction { 
                                                        target: user.name.clone(), 
                                                        action: crate::network::AdminActionType::Mute 
                                                    });
                                                    ui.close_menu();
                                                }
                                                if ui.button("🔊 Unmute").clicked() {
                                                    let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::AdminAction { 
                                                        target: user.name.clone(), 
                                                        action: crate::network::AdminActionType::Unmute 
                                                    });
                                                    ui.close_menu();
                                                }
                                                ui.separator();
                                                if ui.button("🚪 Kick").clicked() {
                                                    let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::AdminAction { 
                                                        target: user.name.clone(), 
                                                        action: crate::network::AdminActionType::Kick 
                                                    });
                                                    ui.close_menu();
                                                }
                                                if ui.button("🚫 BAN").clicked() {
                                                    let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::AdminAction { 
                                                        target: user.name.clone(), 
                                                        action: crate::network::AdminActionType::Ban 
                                                    });
                                                    ui.close_menu();
                                                }
                                            });
                                        }
                                    });
                                }
                                
                            });
                        });
                        ui.add_space(4.0);
                    }

                    if let Some(idx) = channel_to_join {
                        self.current_channel_index = Some(idx);
                    }

                    ui.add_space(20.0);
                    ui.separator();
                    ui.heading(egui::RichText::new("Direct Messages").color(egui::Color32::WHITE));
                    
                    let mut dms_to_show: Vec<String> = self.direct_messages.keys().cloned().collect();
                    dms_to_show.sort();
                    
                    if dms_to_show.is_empty() {
                        ui.label(egui::RichText::new("No active DMs").small().color(egui::Color32::GRAY));
                    } else {
                        for other in dms_to_show {
                            let is_current = self.selected_dm_target.as_ref() == Some(&other);
                            if ui.selectable_label(is_current, format!("✉ {}", other)).clicked() {
                                self.selected_dm_target = Some(other.clone());
                                let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::RequestDirectHistory { target: other });
                            }
                        }
                    }
                });
            });

        // Right Panel: Chat
        if self.show_chat {
            egui::SidePanel::right("chat_panel")
                .resizable(true)
                .default_width(300.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(self.active_chat_tab == ChatTab::Chat, "💬 Chat").clicked() {
                            self.active_chat_tab = ChatTab::Chat;
                        }
                        if ui.selectable_label(self.active_chat_tab == ChatTab::Users, "👥 Users").clicked() {
                            self.active_chat_tab = ChatTab::Users;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("✖").clicked() {
                                self.show_chat = false;
                            }
                        });
                    });
                    ui.separator();

                    if self.active_chat_tab == ChatTab::Users {
                        // Participants list tab
                        ui.label(egui::RichText::new("👥 Connected Participants").strong());
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            ui.vertical(|ui| {
                                for user in &self.participants {
                                    let is_speaking = self.speaking_users.contains_key(user);
                                    
                                    let badge_color = if user == &self.username {
                                        egui::Color32::from_rgb(0, 150, 255) // Blue for self
                                    } else if is_speaking {
                                        egui::Color32::from_rgb(0, 200, 50) // Green for speaking
                                    } else {
                                        egui::Color32::from_rgb(80, 80, 80) // Gray for others
                                    };
                                    
                                    ui.horizontal(|ui| {
                                        let (rect, _resp) = ui.allocate_at_least(egui::vec2(10.0, 10.0), egui::Sense::hover());
                                        ui.painter().circle_filled(rect.center(), 5.0, badge_color);
                                        
                                        let level = {
                                            let levels = self.remote_user_levels.lock().unwrap();
                                            *levels.get(user).unwrap_or(&0.0)
                                        };

                                        if level > 0.01 {
                                            render_waveform(ui, level.min(1.0), egui::Color32::from_rgb(0, 255, 128));
                                            ui.add_space(4.0);
                                        }

                                        let label = egui::RichText::new(user)
                                            .color(egui::Color32::WHITE);
                                        
                                        let resp = ui.add(egui::Button::new(label).frame(false)).on_hover_text("Click to view profile");
                                        if resp.clicked() {
                                            self.show_profile_card = Some(user.clone());
                                            let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::RequestProfile(user.clone()));
                                        }
                                        
                                        // Context menu for volume and admin
                                        resp.context_menu(|ui| {
                                            ui.heading(format!("Settings for {}", user));
                                            if user != &self.username {
                                                let mut volumes = self.user_volumes.lock().unwrap();
                                                let vol = volumes.entry(user.clone()).or_insert(1.0);
                                                ui.horizontal(|ui| {
                                                    ui.label("Volume:");
                                                    ui.add(egui::Slider::new(vol, 0.0..=2.0).text("x"));
                                                });
                                                if ui.button("Reset").clicked() {
                                                    *vol = 1.0;
                                                }
                                                
                                                // Admin section in context menu
                                                if self.role == "Admin" {
                                                    ui.separator();
                                                    ui.heading("Admin Actions");
                                                    if ui.button("Kick").clicked() {
                                                        let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::AdminAction { 
                                                            target: user.clone(), 
                                                            action: crate::network::AdminActionType::Kick 
                                                        });
                                                        ui.close_menu();
                                                    }
                                                    if ui.button("BAN").clicked() {
                                                        let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::AdminAction { 
                                                            target: user.clone(), 
                                                            action: crate::network::AdminActionType::Ban 
                                                        });
                                                        ui.close_menu();
                                                    }
                                                }
                                            } else {
                                                ui.label("This is you!");
                                                ui.label(format!("Role: {}", self.role));
                                            }
                                        });
                                    });
                                    ui.add_space(4.0);
                                }
                            });
                        });
                    } else {
                        // Chat tab
                        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                            let chat_title = if let Some(target) = &self.selected_dm_target {
                                format!("Private Chat with {}", target)
                            } else if let Some(idx) = self.current_channel_index {
                                format!("Channel: {}", self.channels[idx].name)
                            } else {
                                "Chat".to_string()
                            };

                            ui.horizontal(|ui| {
                                ui.heading(egui::RichText::new(chat_title).size(16.0).strong());
                                if self.selected_dm_target.is_some() {
                                    if ui.button("❌ Close DM").clicked() {
                                        self.selected_dm_target = None;
                                    }
                                }
                            });
                            ui.separator();
                            ui.add_space(10.0);
                            
                            // Chat input area
                            ui.horizontal(|ui| {
                                let response = ui.add(
                                    egui::TextEdit::singleline(&mut self.chat_input)
                                        .hint_text("Type a message...")
                                        .desired_width(ui.available_width() - 100.0) // Adjusted for 📎 button
                                );
                                
                                if ui.button("📎").on_hover_text("Send a file or photo").clicked() {
                                    if let Some(path) = FileDialog::new()
                                        .add_filter("Images/Files", &["png", "jpg", "jpeg", "gif", "txt", "pdf", "zip"])
                                        .pick_file() 
                                    {
                                        if let Ok(data) = std::fs::read(&path) {
                                            if data.len() > 10 * 1024 * 1024 {
                                                self.error_message = Some("File too large (max 10MB)".to_string());
                                            } else {
                                                let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                                                let is_image = filename.ends_with(".png") || filename.ends_with(".jpg") || filename.ends_with(".jpeg") || filename.ends_with(".gif");
                                                let timestamp = chrono::Local::now().format("%H:%M").to_string();
                                                let id = uuid::Uuid::new_v4();
                                                
                                                let chunk_size = 32 * 1024;
                                                let total_chunks = (data.len() + chunk_size - 1) / chunk_size;
                                                
                                                // Send FileStart
                                                let start_packet = crate::network::NetworkPacket::FileStart {
                                                    id,
                                                    from: self.username.clone(),
                                                    to: self.selected_dm_target.clone(),
                                                    filename: filename.clone(),
                                                    total_chunks,
                                                    is_image,
                                                    timestamp: timestamp.clone(),
                                                };
                                                let _ = self.outgoing_chat_tx.send(start_packet);
                                                
                                                // Send Chunks
                                                for (idx, chunk) in data.chunks(chunk_size).enumerate() {
                                                    let chunk_packet = crate::network::NetworkPacket::FileChunk {
                                                        id,
                                                        chunk_index: idx,
                                                        data: chunk.to_vec(),
                                                    };
                                                    let _ = self.outgoing_chat_tx.send(chunk_packet);
                                                }
                                                
                                                // Locally add to history
                                                if let Some(target) = &self.selected_dm_target {
                                                    self.direct_messages.entry(target.clone()).or_default().push(ChatMessage {
                                                        id,
                                                        username: "You".to_string(),
                                                        message: format!("Sent a file: {}", filename),
                                                        timestamp: timestamp.clone(),
                                                        file_data: Some((filename, data, is_image)),
                                                        reactions: HashMap::new(),
                                                    });
                                                } else {
                                                    self.chat_messages.push(ChatMessage {
                                                        id,
                                                        username: "You".to_string(),
                                                        message: format!("Sent a file: {}", filename),
                                                        timestamp: timestamp.clone(),
                                                        file_data: Some((filename, data, is_image)),
                                                        reactions: HashMap::new(),
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                                
                                if response.changed() {
                                    if self.last_typing_sent.elapsed().as_secs_f32() > 0.5 {
                                        let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::TypingStatus {
                                            username: self.username.clone(),
                                            is_typing: !self.chat_input.trim().is_empty(),
                                        });
                                        self.last_typing_sent = Instant::now();
                                    }
                                }

                                let send_clicked = ui.button("Send").clicked();
                                if (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) || send_clicked {
                                    if !self.chat_input.trim().is_empty() {
                                        let timestamp = chrono::Local::now().format("%H:%M").to_string();
                                        let msg_id = uuid::Uuid::new_v4();
                                        let msg_text = self.chat_input.clone();
                                        
                                        let encrypted = crate::network::encrypt_bytes(msg_text.as_bytes());
                                        
                                        if let Some(target) = &self.selected_dm_target {
                                            let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::PrivateMessage {
                                                id: msg_id,
                                                from: self.username.clone(),
                                                to: target.clone(),
                                                message: encrypted,
                                                timestamp: timestamp.clone(),
                                            });
                                            // Locally add to DM history
                                            self.direct_messages.entry(target.clone()).or_default().push(ChatMessage {
                                                id: msg_id,
                                                username: "You".to_string(),
                                                message: msg_text,
                                                timestamp,
                                                file_data: None,
                                                reactions: HashMap::new(),
                                            });
                                        } else {
                                            let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::ChatMessage {
                                                id: msg_id,
                                                username: self.username.clone(),
                                                message: encrypted,
                                                timestamp: timestamp.clone(),
                                            });
                                            // Locally add to chat history
                                            self.chat_messages.push(ChatMessage {
                                                id: msg_id,
                                                username: "You".to_string(),
                                                message: msg_text,
                                                timestamp,
                                                file_data: None,
                                                reactions: HashMap::new(),
                                            });
                                        }

                                        let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::TypingStatus {
                                            username: self.username.clone(),
                                            is_typing: false,
                                        });

                                        self.chat_input.clear();
                                    }
                                }
                            });
                            
                            // Typing indicators
                            if !self.typing_users.is_empty() {
                                let typing_names: Vec<String> = self.typing_users.keys().cloned().collect();
                                let text = if typing_names.len() == 1 {
                                    format!("{} is typing...", typing_names[0])
                                } else if typing_names.len() < 4 {
                                    format!("{} are typing...", typing_names.join(", "))
                                } else {
                                    "Multiple users are typing...".to_string()
                                };
                                ui.label(egui::RichText::new(text).small().italics().color(egui::Color32::GRAY));
                            }
                            
                            ui.separator();

                            // Search bar
                            ui.horizontal(|ui| {
                                ui.label("🔍");
                                ui.text_edit_singleline(&mut self.search_query);
                                if ui.button("Clear").clicked() {
                                    self.search_query.clear();
                                }
                            });
                            
                            ui.separator();
                            
                            // Message history
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        let messages = if let Some(target) = &self.selected_dm_target {
                                            self.direct_messages.get(target).map(|v| v.as_slice()).unwrap_or(&[])
                                        } else {
                                            &self.chat_messages
                                        };

                                        for msg in messages {
                                            if !self.search_query.is_empty() && !msg.message.to_lowercase().contains(&self.search_query.to_lowercase()) && !msg.username.to_lowercase().contains(&self.search_query.to_lowercase()) {
                                                continue;
                                            }
                                            
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(egui::RichText::new(&msg.timestamp)
                                                    .size(10.0)
                                                    .color(egui::Color32::GRAY));
                                                ui.label(egui::RichText::new(format!("{}:", msg.username))
                                                    .strong()
                                                    .color(egui::Color32::from_rgb(100, 200, 255)));
                                            });
                                            
                                            self.render_markdown_text(ui, &msg.message);
                                            
                                            // Reactions display
                                            if !msg.reactions.is_empty() {
                                                ui.horizontal_wrapped(|ui| {
                                                    for (emoji, users) in &msg.reactions {
                                                        let count = users.len();
                                                        let tooltip = users.join(", ");
                                                        if ui.button(format!("{} {}", emoji, count)).on_hover_text(tooltip).clicked() {
                                                            let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::Reaction {
                                                                msg_id: msg.id,
                                                                emoji: emoji.clone(),
                                                                from: self.username.clone(),
                                                            });
                                                        }
                                                    }
                                                });
                                            }

                                            // Add reaction button
                                            ui.horizontal(|ui| {
                                                ui.menu_button("➕", |ui| {
                                                    for emoji in ["👍", "❤️", "😂", "😮", "😢", "🔥", "🚀"] {
                                                        if ui.button(emoji).clicked() {
                                                            let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::Reaction {
                                                                msg_id: msg.id,
                                                                emoji: emoji.to_string(),
                                                                from: self.username.clone(),
                                                            });
                                                            ui.close_menu();
                                                        }
                                                    }
                                                });
                                            });

                                            // Render file attachment
                                            if let Some((filename, data, is_image)) = &msg.file_data {
                                                if *is_image {
                                                    let cache_key = format!("{}_{}", msg.username, filename);
                                                    if let Some(texture) = self.image_cache.get(&cache_key) {
                                                        ui.add(egui::Image::new(texture).max_width(200.0));
                                                    } else {
                                                        // Decode and load texture
                                                        if let Ok(img) = image::load_from_memory(data) {
                                                            let size = [img.width() as _, img.height() as _];
                                                            let pixels = img.to_rgba8().into_raw();
                                                            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                                                            let texture = ui.ctx().load_texture(&cache_key, color_image, Default::default());
                                                            self.image_cache.insert(cache_key, texture);
                                                        } else {
                                                            ui.label(egui::RichText::new("[Image Corrupted]").color(egui::Color32::RED));
                                                        }
                                                    }
                                                } else {
                                                    if ui.button(format!("💾 Save {}", filename)).clicked() {
                                                        if let Some(path) = FileDialog::new()
                                                            .set_file_name(filename)
                                                            .save_file() 
                                                        {
                                                            let _ = std::fs::write(path, data);
                                                        }
                                                    }
                                                }
                                            }
                                            ui.add_space(8.0);
                                        }
                                    });
                                });
                        });
                    }
                });
        } else {
            // Show chat button when chat is hidden
            egui::SidePanel::right("chat_toggle")
                .resizable(false)
                .exact_width(40.0)
                .show(ctx, |ui| {
                    ui.add_space(10.0);
                    if ui.button("💬").clicked() {
                        self.show_chat = true;
                    }
                });
        }

        // Central Panel
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(50.0);
                
                if let Some(idx) = self.current_channel_index {
                    ui.heading(egui::RichText::new(format!("Connected to: {}", self.channels[idx].name)).size(24.0).strong());
                } else {
                    ui.heading(egui::RichText::new("Not connected").color(egui::Color32::RED));
                }
                
                ui.add_space(50.0);
                
                let (btn_color, btn_text) = if self.push_to_talk_active { 
                    (egui::Color32::from_rgb(0, 200, 0), "TRANSMITTING")
                } else { 
                    (egui::Color32::from_rgb(60, 60, 70), "PUSH TO TALK")
                };
                
                let ptt_btn = egui::Button::new(
                    egui::RichText::new(btn_text)
                        .size(24.0)
                        .strong()
                        .color(egui::Color32::WHITE)
                )
                .min_size(egui::vec2(200.0, 200.0))
                .fill(btn_color)
                .rounding(100.0);

                let ptt_response = ui.add(ptt_btn);

                if !self.is_muted && !self.is_deafened && !self.is_away {
                    match self.input_mode {
                        InputMode::PushToTalk => {
                             if ptt_response.is_pointer_button_down_on() {
                                if !self.push_to_talk_active {
                                    self.push_to_talk_active = true;
                                    if let Some(audio) = &mut self.audio_manager {
                                        audio.start_recording();
                                    }
                                    if let Some(net) = &self.network_manager {
                                        *net.can_transmit.lock().unwrap() = true;
                                    }
                                }
                            } else {
                                if self.push_to_talk_active {
                                    self.push_to_talk_active = false;
                                    if let Some(audio) = &mut self.audio_manager {
                                        audio.stop_recording();
                                    }
                                    if let Some(net) = &self.network_manager {
                                        *net.can_transmit.lock().unwrap() = false;
                                    }
                                }
                            }
                        },
                        InputMode::VoiceActivity => {
                            // Ensure recording is started for VAD
                            if let Some(audio) = &mut self.audio_manager {
                                audio.start_recording();
                            }

                            if let Some(audio) = &self.audio_manager {
                                if let Ok(vol) = audio.current_volume.lock() {
                                    if *vol > self.vad_threshold {
                                        if !self.push_to_talk_active {
                                            self.push_to_talk_active = true;
                                            if let Some(net) = &self.network_manager {
                                                *net.can_transmit.lock().unwrap() = true;
                                            }
                                        }
                                    } else {
                                         if self.push_to_talk_active {
                                            self.push_to_talk_active = false;
                                            if let Some(net) = &self.network_manager {
                                                *net.can_transmit.lock().unwrap() = false;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    if self.push_to_talk_active {
                        self.push_to_talk_active = false;
                        if let Some(audio) = &mut self.audio_manager {
                            audio.stop_recording();
                        }
                        if let Some(net) = &self.network_manager {
                            *net.can_transmit.lock().unwrap() = false;
                        }
                    }
                }
                
                ui.add_space(20.0);
                if self.push_to_talk_active {
                    ui.label(egui::RichText::new("Microphone Active").color(egui::Color32::GREEN));
                    
                    if let Some(audio) = &self.audio_manager {
                        if let Ok(vol) = audio.current_volume.lock() {
                            let volume = *vol;
                            let bar_width = 200.0 * (volume * 5.0).min(1.0);
                            
                            let (rect, _response) = ui.allocate_exact_size(egui::vec2(200.0, 10.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 5.0, egui::Color32::from_rgb(50, 50, 50));
                            
                            let mut filled_rect = rect;
                            filled_rect.set_width(bar_width);
                            ui.painter().rect_filled(filled_rect, 5.0, egui::Color32::GREEN);
                        }
                    }
                } else {
                    if self.is_away {
                        ui.label(egui::RichText::new("Away (AFK)").color(egui::Color32::from_rgb(100, 100, 255)));
                    } else if self.is_deafened {
                        ui.label(egui::RichText::new("Sound Muted (Deafened)").color(egui::Color32::RED));
                    } else if self.is_muted {
                        ui.label(egui::RichText::new("Microphone Muted").color(egui::Color32::RED));
                    } else {
                        let help_text = match self.input_mode {
                            InputMode::PushToTalk => "Hold button to speak",
                            InputMode::VoiceActivity => "Speak to activate",
                        };
                        ui.label(egui::RichText::new(help_text).color(egui::Color32::GRAY));
                    }
                }
            });
        });

        // Settings Window
        if self.show_settings {
            egui::Window::new("Settings")
                .collapsible(false)
                .resizable(true)
                .default_width(400.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.heading("Audio Settings");
                    ui.separator();
                    
                    egui::Grid::new("settings_grid")
                        .num_columns(2)
                        .spacing([40.0, 10.0])
                        .show(ui, |ui| {
                            // Theme toggle
                            ui.label("Theme:");
                            ui.horizontal(|ui| {
                                if ui.selectable_label(self.dark_mode, "🌙 Dark").clicked() {
                                    self.dark_mode = true;
                                }
                                if ui.selectable_label(!self.dark_mode, "☀ Light").clicked() {
                                    self.dark_mode = false;
                                }
                            });
                            ui.end_row();

                            ui.label("Input Device:");
                            egui::ComboBox::from_id_salt("input_dev")
                                .selected_text(&self.selected_input_device)
                                .show_ui(ui, |ui| {
                                    for device in &self.input_devices {
                                        ui.selectable_value(&mut self.selected_input_device, device.clone(), device);
                                    }
                                });
                            ui.end_row();

                            ui.label("Output Device:");
                            egui::ComboBox::from_id_salt("output_dev")
                                .selected_text(&self.selected_output_device)
                                .show_ui(ui, |ui| {
                                    for device in &self.output_devices {
                                        ui.selectable_value(&mut self.selected_output_device, device.clone(), device);
                                    }
                                });
                            ui.end_row();
                            
                            ui.end_row();

                            ui.label("Levels:");
                            ui.horizontal(|ui| {
                                let vol = if let Some(audio) = &self.audio_manager {
                                    *audio.current_volume.lock().unwrap()
                                } else { 0.0 };
                                ui.add(egui::ProgressBar::new(vol.min(1.0)).show_percentage());
                                ui.label(egui::RichText::new("Mic Level").small());
                            });
                            ui.end_row();
                            
                            ui.label("Input Mode:");
                            ui.horizontal(|ui| {
                                let prev_mode = self.input_mode == InputMode::VoiceActivity;
                                ui.radio_value(&mut self.input_mode, InputMode::PushToTalk, "Push to Talk");
                                ui.radio_value(&mut self.input_mode, InputMode::VoiceActivity, "Voice Activity");
                                
                                if self.input_mode == InputMode::VoiceActivity && !prev_mode {
                                    if let Some(audio) = &mut self.audio_manager {
                                        audio.start_recording();
                                    }
                                } else if self.input_mode == InputMode::PushToTalk && prev_mode {
                                    if let Some(audio) = &mut self.audio_manager {
                                        audio.stop_recording();
                                    }
                                }
                            });
                            ui.end_row();

                            if self.input_mode == InputMode::VoiceActivity {
                                ui.label("VAD Threshold:");
                                ui.add(egui::Slider::new(&mut self.vad_threshold, 0.0..=1.0).text("Volume"));
                                ui.end_row();
                            }

                            ui.separator();
                            ui.end_row();
                            
                            ui.label("Profile Avatar:");
                            ui.add(egui::TextEdit::singleline(&mut self.avatar_url_input).hint_text("https://..."));
                            ui.end_row();
                            
                            ui.label("Profile Bio:");
                            ui.add(egui::TextEdit::multiline(&mut self.bio_input).hint_text("Tell us about yourself..."));
                            ui.end_row();
                            
                            ui.label("");
                            if ui.button("💾 Update Profile").clicked() {
                                let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::ProfileUpdate {
                                    username: self.username.clone(),
                                    avatar_url: self.avatar_url_input.clone(),
                                    bio: self.bio_input.clone(),
                                });
                            }
                            ui.end_row();

                            ui.label("Self Listen:");
                            if ui.checkbox(&mut self.self_listen, "Listen to self").changed() {
                                if let Some(audio) = &self.audio_manager {
                                    audio.set_self_listen(self.self_listen);
                                }
                            }
                            ui.end_row();
                        });
                    
                    ui.add_space(20.0);
                    ui.separator();
                    
                    // Update Section
                    ui.heading("Updates");
                    ui.separator();
                    ui.add_space(10.0);
                    
                    self.render_update_section(ui);
                    
                    ui.add_space(20.0);
                    ui.separator();
                    ui.heading("User Profile");
                    ui.add_space(5.0);
                    egui::Grid::new("profile_grid")
                        .num_columns(2)
                        .spacing([20.0, 10.0])
                        .show(ui, |ui| {
                            ui.label("Status:");
                            ui.text_edit_singleline(&mut self.status_input);
                            ui.end_row();

                            ui.label("Nick Color (#RRGGBB):");
                            ui.horizontal(|ui| {
                                ui.text_edit_singleline(&mut self.nick_color_input);
                                if let Ok(c) = hex_to_color(&self.nick_color_input) {
                                    let (rect, _resp) = ui.allocate_at_least(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                    ui.painter().circle_filled(rect.center(), 8.0, c);
                                }
                            });
                            ui.end_row();
                        });
                    
                    if ui.button("💾 Save Profile").clicked() {
                        let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::UpdateProfile { 
                            status: self.status_input.clone(), 
                            nick_color: self.nick_color_input.clone() 
                        });
                    }

                    ui.add_space(20.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Close").clicked() {
                            self.show_settings = false;
                        }
                        
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(egui::RichText::new("🚪 Logout").color(egui::Color32::LIGHT_RED)).clicked() {
                                self.logout();
                                self.show_settings = false;
                            }
                        });
                    });
                });
            });
        }

        // Create Channel Dialog
        if self.show_create_channel_dialog {
            egui::Window::new("Create New Channel")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label("Channel Name:");
                    ui.text_edit_singleline(&mut self.new_channel_name);
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Create").clicked() {
                            if !self.new_channel_name.is_empty() {
                                if let Some(_net) = &self.network_manager {
                                    let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::CreateChannel(self.new_channel_name.clone()));
                                }
                                self.new_channel_name.clear();
                                self.show_create_channel_dialog = false;
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_create_channel_dialog = false;
                        }
                    });
                });
        }

        // --- Profile Card ---
        if let Some(profile_username) = self.show_profile_card.clone() {
            egui::Window::new(format!("👤 Profile: {}", profile_username))
                .collapsible(false)
                .resizable(true)
                .default_width(320.0)
                .show(ctx, |ui| {
                    if let Some(profile) = self.user_profiles.get(&profile_username) {
                        ui.vertical_centered(|ui| {
                            if !profile.avatar_url.is_empty() {
                                ui.group(|ui| {
                                    ui.label(egui::RichText::new("🖼 Avatar Link:").small().color(egui::Color32::GRAY));
                                    ui.hyperlink(&profile.avatar_url);
                                });
                                ui.add_space(8.0);
                            }
                            
                            ui.heading(egui::RichText::new(&profile.username).color(egui::Color32::WHITE));
                            ui.add_space(4.0);
                            
                            ui.separator();
                            ui.add_space(8.0);
                            
                            ui.label(egui::RichText::new("Biography").strong());
                            ui.add_space(4.0);
                            
                            egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                                ui.label(&profile.bio);
                            });
                        });
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.spinner();
                            ui.label("Fetching profile...");
                        });
                    }
                    
                    ui.add_space(16.0);
                    ui.separator();
                    if ui.button("Close").clicked() {
                        self.show_profile_card = None;
                    }
                });
        }

        // Error Popup
        if let Some(msg) = self.error_message.clone() {
            egui::Window::new("⚠️ Connection Error")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(&msg).color(egui::Color32::LIGHT_RED));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            self.error_message = None;
                        }
                    });
                });
        }
    }
}
