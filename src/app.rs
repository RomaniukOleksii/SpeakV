use eframe::egui;
use crate::audio::AudioManager;
use crate::network::NetworkManager;
use crate::updater::{UpdateManager, UpdateStatus};
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Instant;

struct User {
    name: String,
    is_speaking: bool,
    is_muted: bool,
    is_deafened: bool,
    is_away: bool,
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
struct ChatMessage {
    username: String,
    message: String,
    timestamp: String,
}

#[derive(PartialEq)]
enum ChatTab {
    Chat,
    Users,
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
    is_host: Arc<Mutex<bool>>,
    public_ip: Arc<Mutex<String>>,
    
    // Chat State
    chat_messages: Vec<ChatMessage>,
    chat_input: String,
    show_chat: bool,
    outgoing_chat_tx: crossbeam_channel::Sender<crate::network::NetworkPacket>,
    participants: Vec<String>,
    typing_users: HashMap<String, Instant>,
    speaking_users: HashMap<String, Instant>,
    user_volumes: Arc<Mutex<HashMap<String, f32>>>,
    last_typing_sent: Instant,
    active_chat_tab: ChatTab,
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

        // Load Username
        let mut username = String::new();
        if let Ok(saved_name) = fs::read_to_string("user_config.txt") {
            let saved_name = saved_name.trim();
            if !saved_name.is_empty() {
                username = saved_name.to_string();
            }
        }

        // Channels
        let channels = vec![
            Channel {
                name: "Lobby".to_owned(),
                users: vec![],
                expanded: true,
            },
            Channel {
                name: "Gaming Room 1".to_owned(),
                users: vec![],
                expanded: true,
            },
            Channel {
                name: "Meeting Room".to_owned(),
                users: vec![],
                expanded: true,
            },
            Channel {
                name: "AFK".to_owned(),
                users: vec![],
                expanded: false,
            },
        ];

        let (tx_out, rx_out) = crossbeam_channel::unbounded();

        let user_volumes = if let Some(net) = &network_manager { net.user_volumes.clone() } else { Arc::new(Mutex::new(HashMap::new())) };

        let app = Self {
            audio_manager,
            network_manager,
            update_manager: UpdateManager::new(),
            username: username.clone(),
            login_input: username,
            password_input: String::new(),
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
            is_host: Arc::new(Mutex::new(false)),
            public_ip: Arc::new(Mutex::new("Fetching...".to_string())),
            
            chat_messages: Vec::new(),
            chat_input: String::new(),
            show_chat: true,
            outgoing_chat_tx: tx_out,
            participants: Vec::new(),
            typing_users: HashMap::new(),
            speaking_users: HashMap::new(),
            user_volumes,
            last_typing_sent: Instant::now(),
            active_chat_tab: ChatTab::Chat,
        };

        // Auto-start server and connect
        if let (Some(net), Some(audio)) = (&app.network_manager, &app.audio_manager) {
            let net_clone = net.clone();
            let input_cons = audio.input_consumer.clone();
            let remote_prod = audio.remote_producer.clone();
            let addr = app.server_address.clone();
            let is_host_clone = app.is_host.clone();
            let public_ip_clone = app.public_ip.clone();
            let tx_out_clone = app.outgoing_chat_tx.clone();
            let username_clone = app.username.clone();

            tokio::spawn(async move {
                // Try to be the host
                if let Err(e) = crate::server::run_server().await {
                    println!("Not starting server (likely already running): {}", e);
                } else {
                    println!("Successfully started server as host.");
                    if let Ok(mut host) = is_host_clone.lock() {
                        *host = true;
                    }
                    
                    // Fetch public IP for the host
                    if let Some(ip) = public_ip::addr().await {
                        if let Ok(mut p_ip) = public_ip_clone.lock() {
                            *p_ip = ip.to_string();
                        }
                    }
                }
                
                // Auto connect
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                net_clone.start(addr, input_cons, remote_prod, rx_out, username_clone.clone());

                // Send handshake
                let _ = tx_out_clone.send(crate::network::NetworkPacket::Handshake { 
                    username: username_clone 
                });
            });
        }

        app
    }

    fn save_username(&self) {
        let _ = fs::write("user_config.txt", &self.username);
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
}

impl eframe::App for SpeakVApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle incoming network chat messages
        if let Some(net) = &self.network_manager {
            self.is_connected = *net.is_connected.lock().unwrap();
            while let Ok(packet) = net.chat_rx.try_recv() {
                if let crate::network::NetworkPacket::ChatMessage { username, message, timestamp } = packet {
                    self.chat_messages.push(ChatMessage {
                        username,
                        message,
                        timestamp,
                    });
                } else if let crate::network::NetworkPacket::AuthResponse { success, message } = packet {
                    self.is_authenticated = success;
                    self.auth_message = message;
                    if success {
                        self.username = self.login_input.clone();
                        self.save_username();
                    }
                } else if let crate::network::NetworkPacket::UsersUpdate(chan_state) = packet {
                    // Update participants (flat list)
                    self.participants.clear();
                    for (_chan_name, users) in &chan_state {
                        for user in users {
                            if !self.participants.contains(user) {
                                self.participants.push(user.clone());
                            }
                        }
                    }

                    // Rebuild channels list while preserving expansion state
                    let mut new_channels = Vec::new();
                    for (chan_name, users) in chan_state {
                        let expanded = self.channels.iter()
                            .find(|c| c.name == chan_name)
                            .map(|c| c.expanded)
                            .unwrap_or(true);
                        
                        let mut user_list = Vec::new();
                        for user_name in users {
                            user_list.push(User {
                                name: user_name.clone(),
                                is_speaking: self.speaking_users.contains_key(&user_name),
                                is_muted: false, // These will be handled by per-user volume/context menu
                                is_deafened: false,
                                is_away: false,
                            });
                        }

                        new_channels.push(Channel {
                            name: chan_name,
                            users: user_list,
                            expanded,
                        });
                    }
                    self.channels = new_channels;

                    // Update current channel index
                    if let Some(_net) = &self.network_manager {
                        // We don't have a direct "current channel" from server in hand yet, 
                        // but we can find where the current user is.
                        for (idx, chan) in self.channels.iter().enumerate() {
                            if chan.users.iter().any(|u| u.name == self.username) {
                                self.current_channel_index = Some(idx);
                                break;
                            }
                        }
                    }
                } else if let crate::network::NetworkPacket::TypingStatus { username, is_typing } = packet {
                    if is_typing {
                        self.typing_users.insert(username, Instant::now());
                    } else {
                        self.typing_users.remove(&username);
                    }
                }
            }
        }

        // Clean up old typing statuses (older than 3 seconds)
        self.typing_users.retain(|_, &mut last_seen| last_seen.elapsed().as_secs_f32() < 3.0);
        
        // Handle speaking indicators
        if let Some(net) = &self.network_manager {
            while let Ok(username) = net.speaking_users_rx.try_recv() {
                self.speaking_users.insert(username, Instant::now());
            }
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
                            ui.add_space(10.0);
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
                                            let (tx_out, rx_out) = crossbeam_channel::unbounded();
                                            self.outgoing_chat_tx = tx_out.clone();

                                            net.start(
                                                self.server_address.clone(),
                                                audio.input_consumer.clone(),
                                                audio.remote_producer.clone(),
                                                rx_out,
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
                                    let (tx_out, rx_out) = crossbeam_channel::unbounded();
                                    self.outgoing_chat_tx = tx_out.clone();

                                    net.start(
                                        self.server_address.clone(),
                                        audio.input_consumer.clone(),
                                        audio.remote_producer.clone(),
                                        rx_out,
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
                    if let Ok(host) = self.is_host.lock() {
                        if *host {
                            ui.label(egui::RichText::new("HOST").strong().color(egui::Color32::GOLD));
                            if let Ok(ip) = self.public_ip.lock() {
                                ui.label(egui::RichText::new(format!("(IP: {})", *ip)).color(egui::Color32::from_rgb(200, 150, 50)));
                            }
                        }
                    }
                    ui.label(egui::RichText::new(format!("Logged in as: {}", self.username)).color(egui::Color32::LIGHT_GRAY));
                });
            });
            ui.add_space(8.0);
        });

        // Left Panel: Channel Tree
        egui::SidePanel::left("left_panel")
            .resizable(true)
            .default_width(250.0)
            .show(ctx, |ui| {
                ui.add_space(10.0);
                ui.heading(egui::RichText::new("Server Tree").color(egui::Color32::WHITE));
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
                                        let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::JoinChannel(channel.name.clone()));
                                    }
                                }

                                for user in &channel.users {
                                    ui.horizontal(|ui| {
                                        let mut icon = "👤";
                                        let mut color = egui::Color32::LIGHT_GRAY;
                                        
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

                                        ui.label(egui::RichText::new(format!("{} {}", icon, user.name)).color(color));
                                    });
                                }
                                
                                if is_current {
                                    ui.horizontal(|ui| {
                                        let mut icon = "👤";
                                        let mut color = egui::Color32::WHITE;
                                        
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
                                        }
                                        
                                        ui.label(egui::RichText::new(format!("{} {} (You)", icon, self.username)).color(color).strong());
                                    });
                                }
                            });
                        });
                        ui.add_space(4.0);
                    }

                    if let Some(idx) = channel_to_join {
                        self.current_channel_index = Some(idx);
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
                                        
                                        let label = egui::RichText::new(user)
                                            .color(egui::Color32::WHITE);
                                        
                                        let resp = ui.label(label);
                                        
                                        // Context menu for volume
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
                                            } else {
                                                ui.label("This is you!");
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
                            ui.add_space(10.0);
                            
                            // Chat input area
                            ui.horizontal(|ui| {
                                let response = ui.add(
                                    egui::TextEdit::singleline(&mut self.chat_input)
                                        .hint_text("Type a message...")
                                        .desired_width(ui.available_width() - 60.0)
                                );
                                
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
                                        let msg = ChatMessage {
                                            username: self.username.clone(),
                                            message: self.chat_input.clone(),
                                            timestamp,
                                        };
                                        
                                        let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::ChatMessage {
                                            username: msg.username.clone(),
                                            message: msg.message.clone(),
                                            timestamp: msg.timestamp.clone(),
                                        });

                                        let _ = self.outgoing_chat_tx.send(crate::network::NetworkPacket::TypingStatus {
                                            username: self.username.clone(),
                                            is_typing: false,
                                        });

                                        self.chat_messages.push(msg);
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
                            
                            // Message history
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        for msg in &self.chat_messages {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(egui::RichText::new(&msg.timestamp)
                                                    .size(10.0)
                                                    .color(egui::Color32::GRAY));
                                                ui.label(egui::RichText::new(format!("{}:", msg.username))
                                                    .strong()
                                                    .color(egui::Color32::from_rgb(100, 200, 255)));
                                            });
                                            ui.label(&msg.message);
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
                    ui.heading("Audio Settings");
                    ui.separator();
                    
                    egui::Grid::new("settings_grid")
                        .num_columns(2)
                        .spacing([40.0, 10.0])
                        .show(ui, |ui| {
                            ui.label("Input Device:");
                            egui::ComboBox::from_id_source("input_dev")
                                .selected_text(&self.selected_input_device)
                                .show_ui(ui, |ui| {
                                    for device in &self.input_devices {
                                        ui.selectable_value(&mut self.selected_input_device, device.clone(), device);
                                    }
                                });
                            ui.end_row();

                            ui.label("Output Device:");
                            egui::ComboBox::from_id_source("output_dev")
                                .selected_text(&self.selected_output_device)
                                .show_ui(ui, |ui| {
                                    for device in &self.output_devices {
                                        ui.selectable_value(&mut self.selected_output_device, device.clone(), device);
                                    }
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
                    ui.horizontal(|ui| {
                        if ui.button("Close").clicked() {
                            self.show_settings = false;
                        }
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
    }
}
