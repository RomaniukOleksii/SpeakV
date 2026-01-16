use tokio::net::UdpSocket;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use igd_next::{search_gateway, PortMappingProtocol};
use rusqlite::{params, Connection};
use bcrypt::{hash, verify, DEFAULT_COST};
use std::sync::Mutex as StdMutex;

pub async fn run_server() -> anyhow::Result<()> {
    // Try UPnP port forwarding
    tokio::task::spawn_blocking(|| {
        match search_gateway(Default::default()) {
            Ok(gateway) => {
                let local_addr = match local_ip_address::local_ip() {
                    Ok(ip) => ip,
                    Err(_) => return,
                };
                let local_socket_addr = SocketAddr::new(local_addr, 9999);
                match gateway.add_port(
                    PortMappingProtocol::UDP,
                    9999,
                    local_socket_addr,
                    0,
                    "SpeakV Voice Server",
                ) {
                    Ok(_) => println!("UPnP: Port 9999 forwarded successfully."),
                    Err(e) => println!("UPnP: Failed to forward port: {}", e),
                }
            }
            Err(e) => println!("UPnP: Gateway not found: {}", e),
        }
    });

    let socket = match UdpSocket::bind("0.0.0.0:9999").await {
        Ok(s) => s,
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to bind server: {}", e));
        }
    };
    
    println!("SpeakV Server started on 0.0.0.0:9999");

    struct ClientInfo {
        username: String,
        current_channel: String,
        last_seen: tokio::time::Instant,
        is_authenticated: bool,
    }

    // Initialize Database
    let db_conn = Connection::open("users.db")?;
    db_conn.execute(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY,
            username TEXT UNIQUE NOT NULL,
            password_hash TEXT NOT NULL
        )",
        [],
    )?;
    let db = Arc::new(StdMutex::new(db_conn));

    let clients: Arc<Mutex<HashMap<SocketAddr, ClientInfo>>> = Arc::new(Mutex::new(HashMap::new()));
    let channels: Arc<Mutex<std::collections::HashSet<String>>> = Arc::new(Mutex::new(
        std::collections::HashSet::from(["Lobby".to_string(), "AFK".to_string()])
    ));
    let mut buf = [0u8; 4096];

    loop {
        let (len, addr) = socket.recv_from(&mut buf).await?;
        
        if let Ok(packet) = bincode::deserialize::<crate::network::NetworkPacket>(&buf[..len]) {
            let mut clients_guard = clients.lock().await;
            let mut needs_broadcast = false;
            
            match &packet {
                crate::network::NetworkPacket::Handshake { username } => {
                    println!("Logging: {} connected from {}", username, addr);
                    clients_guard.insert(addr, ClientInfo {
                        username: username.clone(),
                        current_channel: "Lobby".to_string(),
                        last_seen: tokio::time::Instant::now(),
                        is_authenticated: false,
                    });
                }
                crate::network::NetworkPacket::Register { username, password } => {
                    let result = {
                        let hashed_pass = hash(password, DEFAULT_COST).unwrap_or_else(|_| String::new());
                        let db_lock = db.lock().unwrap();
                        db_lock.execute(
                            "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
                            params![username, hashed_pass],
                        )
                    };
                    
                    let (success, msg) = match result {
                        Ok(_) => (true, "Registration successful!".to_string()),
                        Err(e) => (false, format!("Registration failed: {}", e)),
                    };

                    let response = crate::network::NetworkPacket::AuthResponse { success, message: msg };
                    if let Ok(encoded) = bincode::serialize(&response) {
                        let _ = socket.send_to(&encoded, addr).await;
                    }
                }
                crate::network::NetworkPacket::Login { username, password } => {
                    let result: Result<String, _> = {
                        let db_lock = db.lock().unwrap();
                        let mut stmt = db_lock.prepare("SELECT password_hash FROM users WHERE username = ?1").unwrap();
                        stmt.query_row(params![username], |row| row.get(0))
                    };

                    let (success, msg) = match result {
                        Ok(stored_hash) => {
                            if verify(password, &stored_hash).unwrap_or(false) {
                                (true, "Login successful!".to_string())
                            } else {
                                (false, "Invalid password".to_string())
                            }
                        }
                        Err(_) => (false, "User not found".to_string()),
                    };

                    if success {
                        if let Some(info) = clients_guard.get_mut(&addr) {
                            info.username = username.clone();
                            info.is_authenticated = true;
                            info.last_seen = tokio::time::Instant::now();
                            println!("Server: {} authenticated via Login", username);
                            needs_broadcast = true;
                        }
                    }

                    let response = crate::network::NetworkPacket::AuthResponse { success, message: msg };
                    if let Ok(encoded) = bincode::serialize(&response) {
                        let _ = socket.send_to(&encoded, addr).await;
                    }
                }
                crate::network::NetworkPacket::Audio { .. } | 
                crate::network::NetworkPacket::ChatMessage { .. } |
                crate::network::NetworkPacket::TypingStatus { .. } => {
                    let (sender_channel, authenticated) = if let Some(info) = clients_guard.get_mut(&addr) {
                        info.last_seen = tokio::time::Instant::now();
                        (info.current_channel.clone(), info.is_authenticated)
                    } else {
                        ("Lobby".to_string(), false)
                    };

                    if authenticated {
                        for (&client_addr, info) in clients_guard.iter() {
                            if client_addr != addr && info.current_channel == sender_channel && info.is_authenticated {
                                let _ = socket.send_to(&buf[..len], client_addr).await;
                            }
                        }
                    }
                }
                crate::network::NetworkPacket::CreateChannel(name) => {
                    if let Some(info) = clients_guard.get(&addr) {
                        if info.is_authenticated {
                            let mut chan_guard = channels.lock().await;
                            if !chan_guard.contains(name) {
                                chan_guard.insert(name.clone());
                                println!("Server: Channel '{}' created by {}", name, addr);
                                needs_broadcast = true;
                            }
                        }
                    }
                }
                crate::network::NetworkPacket::JoinChannel(name) => {
                    if let Some(info) = clients_guard.get_mut(&addr) {
                        if info.is_authenticated {
                            let chan_guard = channels.lock().await;
                            if chan_guard.contains(name) {
                                info.current_channel = name.clone();
                                info.last_seen = tokio::time::Instant::now();
                                println!("Server: {} joined '{}'", info.username, name);
                                needs_broadcast = true;
                            }
                        }
                    }
                }
                crate::network::NetworkPacket::Ping => {
                    if let Some(info) = clients_guard.get_mut(&addr) {
                        info.last_seen = tokio::time::Instant::now();
                    }
                }
                _ => {}
            }
            
            // Clean up old clients (timeout after 10 seconds)
            let initial_count = clients_guard.len();
            clients_guard.retain(|_, info| info.last_seen.elapsed().as_secs() < 10);
            if clients_guard.len() != initial_count {
                needs_broadcast = true;
            }

            // Broadcast channel/user state if needed
            if needs_broadcast {
                let mut state: Vec<(String, Vec<String>)> = Vec::new();
                let chan_guard = channels.lock().await;
                
                for chan in chan_guard.iter() {
                    let mut users_in_chan = Vec::new();
                    for client in clients_guard.values() {
                        if &client.current_channel == chan && client.is_authenticated {
                            users_in_chan.push(client.username.clone());
                        }
                    }
                    state.push((chan.clone(), users_in_chan));
                }

                let update_packet = crate::network::NetworkPacket::UsersUpdate(state);
                if let Ok(encoded) = bincode::serialize(&update_packet) {
                    for &client_addr in clients_guard.keys() {
                        let _ = socket.send_to(&encoded, client_addr).await;
                    }
                }
            }
        }
    }
}
