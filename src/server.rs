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
        role: String, // "Admin", "User"
        is_muted: bool,
        status: String,
        nick_color: String,
    }

    // Initialize Database
    let db_conn = Connection::open("users.db")?;
    db_conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY,
            username TEXT UNIQUE NOT NULL,
            password_hash TEXT NOT NULL,
            role TEXT DEFAULT 'User',
            is_banned INTEGER DEFAULT 0,
            nick_color TEXT DEFAULT '#FFFFFF',
            avatar_url TEXT DEFAULT '',
            bio TEXT DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS chat_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            msg_id TEXT NOT NULL,
            username TEXT NOT NULL,
            channel TEXT NOT NULL,
            message BLOB NOT NULL,
            timestamp TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS channels (
            name TEXT PRIMARY KEY NOT NULL
        );
        CREATE TABLE IF NOT EXISTS private_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            msg_id TEXT NOT NULL,
            sender TEXT NOT NULL,
            recipient TEXT NOT NULL,
            message BLOB NOT NULL,
            timestamp TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS file_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            msg_id TEXT NOT NULL,
            channel TEXT NOT NULL,
            recipient TEXT, -- NULL for channel files
            filename TEXT NOT NULL,
            data BLOB NOT NULL,
            is_image INTEGER NOT NULL,
            timestamp TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS reactions (
            msg_id TEXT NOT NULL,
            username TEXT NOT NULL,
            emoji TEXT NOT NULL
        );"
    )?;
    
    // Default channels
    let _ = db_conn.execute("INSERT OR IGNORE INTO channels (name) VALUES ('Lobby')", []);
    let _ = db_conn.execute("INSERT OR IGNORE INTO channels (name) VALUES ('AFK')", []);

    let db = Arc::new(StdMutex::new(db_conn));

    let mut initial_channels = std::collections::HashSet::new();
    {
        if let Ok(db_lock) = db.lock() {
            if let Ok(mut stmt) = db_lock.prepare("SELECT name FROM channels") {
                if let Ok(chan_rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                    for chan in chan_rows {
                        if let Ok(c) = chan {
                            initial_channels.insert(c);
                        }
                    }
                }
            }
        }
    }
    println!("Server: Loaded channels from DB: {:?}", initial_channels);

    let clients: Arc<Mutex<HashMap<SocketAddr, ClientInfo>>> = Arc::new(Mutex::new(HashMap::new()));
    let channels: Arc<Mutex<std::collections::HashSet<String>>> = Arc::new(Mutex::new(initial_channels));
    let file_reassemblers: Arc<Mutex<HashMap<uuid::Uuid, crate::app::PendingFile>>> = Arc::new(Mutex::new(HashMap::new()));

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
                        role: "User".to_string(),
                        is_muted: false,
                        status: String::new(),
                        nick_color: "#FFFFFF".to_string(),
                    });
                    needs_broadcast = true;
                }
                crate::network::NetworkPacket::Register { username, password } => {
                    let result = {
                        let hashed_pass = hash(password, DEFAULT_COST).unwrap_or_else(|_| String::new());
                        let db_lock = db.lock().unwrap();
                        
                        // Check if any users exist to assign Admin role to the first one
                        let user_count: i64 = db_lock.query_row("SELECT count(*) FROM users", [], |row| row.get(0)).unwrap_or(0);
                        let role = if user_count == 0 { "Admin" } else { "User" };

                        db_lock.execute(
                            "INSERT INTO users (username, password_hash, role) VALUES (?1, ?2, ?3)",
                            params![username, hashed_pass, role],
                        )
                    };
                    
                    let (success, msg) = match result {
                        Ok(_) => (true, "Registration successful!".to_string()),
                        Err(e) => (false, format!("Registration failed: {}", e)),
                    };

                    let response = crate::network::NetworkPacket::AuthResponse { 
                        success, 
                        message: msg, 
                        role: None,
                        status: None,
                        nick_color: None,
                    };
                    if let Ok(encoded) = bincode::serialize(&response) {
                        let _ = socket.send_to(&encoded, addr).await;
                    }
                }
                crate::network::NetworkPacket::Login { username, password } => {
                    let result: Result<(String, String, bool, String, String), _> = (|| {
                        let db_lock = db.lock().map_err(|_| rusqlite::Error::ExecuteReturnedResults)?;
                        let mut stmt = db_lock.prepare("SELECT password_hash, role, is_banned, status, nick_color FROM users WHERE username = ?1")?;
                        stmt.query_row(params![username], |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i32>(2)? != 0, row.get(3)?, row.get(4)?)))
                    })();

                    let (success, msg, role, status, color) = match result {
                        Ok((stored_hash, role, is_banned, status, color)) => {
                            if is_banned {
                                (false, "You are banned from this server".to_string(), role, status, color)
                            } else if verify(password, &stored_hash).unwrap_or(false) {
                                (true, "Login successful!".to_string(), role, status, color)
                            } else {
                                (false, "Invalid password".to_string(), role, status, color)
                            }
                        }
                        Err(_) => (false, "User not found".to_string(), "User".to_string(), String::new(), "#FFFFFF".to_string()),
                    };

                    if success {
                        if let Some(info) = clients_guard.get_mut(&addr) {
                            info.username = username.clone();
                            info.is_authenticated = true;
                            info.role = role.clone();
                            info.status = status.clone();
                            info.nick_color = color.clone();
                            info.last_seen = tokio::time::Instant::now();
                            println!("Server: {} authenticated via Login as {}", username, info.role);
                            needs_broadcast = true;
                        }
                    }

                    let response = crate::network::NetworkPacket::AuthResponse { 
                        success, 
                        message: msg, 
                        role: if success { Some(role) } else { None },
                        status: if success { Some(status) } else { None },
                        nick_color: if success { Some(color) } else { None },
                    };
                    if let Ok(encoded) = bincode::serialize(&response) {
                        let _ = socket.send_to(&encoded, addr).await;
                    }
                }
                crate::network::NetworkPacket::UpdateProfile { status, nick_color } => {
                    if let Some(info) = clients_guard.get_mut(&addr) {
                        if info.is_authenticated {
                            info.status = status.clone();
                            info.nick_color = nick_color.clone();
                            
                            // Save to DB
                            {
                                let db_lock = db.lock().unwrap();
                                let _ = db_lock.execute(
                                    "UPDATE users SET status = ?1, nick_color = ?2 WHERE username = ?3",
                                    params![status, nick_color, info.username],
                                );
                            }
                            println!("Server: Profile updated for {}", info.username);
                            needs_broadcast = true;
                        }
                    }
                }
                crate::network::NetworkPacket::Audio { .. } | 
                crate::network::NetworkPacket::TypingStatus { .. } => {
                    let (sender_channel, authenticated, is_muted) = if let Some(info) = clients_guard.get_mut(&addr) {
                        info.last_seen = tokio::time::Instant::now();
                        (info.current_channel.clone(), info.is_authenticated, info.is_muted)
                    } else {
                        ("Lobby".to_string(), false, false)
                    };

                    if authenticated && !is_muted {
                        for (&client_addr, info) in clients_guard.iter() {
                            if client_addr != addr && info.current_channel == sender_channel && info.is_authenticated {
                                let _ = socket.send_to(&buf[..len], client_addr).await;
                            }
                        }
                    }
                }
                crate::network::NetworkPacket::ChatMessage { id, username, message, timestamp } => {
                    let (sender_channel, authenticated, is_muted) = if let Some(info) = clients_guard.get_mut(&addr) {
                        info.last_seen = tokio::time::Instant::now();
                        (info.current_channel.clone(), info.is_authenticated, info.is_muted)
                    } else {
                        ("Lobby".to_string(), false, false)
                    };

                    if authenticated && !is_muted {
                        // Store in DB
                        {
                            let db_lock = db.lock().unwrap();
                            let _ = db_lock.execute(
                                "INSERT INTO chat_messages (msg_id, username, channel, message, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
                                params![id.to_string(), username, sender_channel, message, timestamp],
                            );
                        }

                        // Relay to others in the same channel
                        for (&client_addr, info) in clients_guard.iter() {
                            if client_addr != addr && info.current_channel == sender_channel && info.is_authenticated {
                                let _ = socket.send_to(&buf[..len], client_addr).await;
                            }
                        }
                    }
                }
                crate::network::NetworkPacket::AdminAction { target, action } => {
                    let mut admin_name = String::new();
                    let is_admin = if let Some(info) = clients_guard.get(&addr) {
                        admin_name = info.username.clone();
                        info.role == "Admin"
                    } else {
                        false
                    };

                    if is_admin {
                        match action {
                            crate::network::AdminActionType::Kick => {
                                clients_guard.retain(|_, v| &v.username != target);
                                println!("Admin Action: {} kicked {}", admin_name, target);
                                needs_broadcast = true;
                            }
                            crate::network::AdminActionType::Ban => {
                                {
                                    let db_lock = db.lock().unwrap();
                                    let _ = db_lock.execute("UPDATE users SET is_banned = 1 WHERE username = ?1", params![target]);
                                }
                                clients_guard.retain(|_, v| &v.username != target);
                                println!("Admin Action: {} banned {}", admin_name, target);
                                needs_broadcast = true;
                            }
                            crate::network::AdminActionType::Mute => {
                                for info in clients_guard.values_mut() {
                                    if &info.username == target {
                                        info.is_muted = true;
                                    }
                                }
                                println!("Admin Action: {} muted {}", admin_name, target);
                                needs_broadcast = true;
                            }
                            crate::network::AdminActionType::Unmute => {
                                for info in clients_guard.values_mut() {
                                    if &info.username == target {
                                        info.is_muted = false;
                                    }
                                }
                                println!("Admin Action: {} unmuted {}", admin_name, target);
                                needs_broadcast = true;
                            }
                        }
                    }
                }
                crate::network::NetworkPacket::RequestChatHistory { channel } => {
                    if let Some(info) = clients_guard.get(&addr) {
                        if info.is_authenticated {
                            let history_result: Result<Vec<crate::network::NetworkPacket>, rusqlite::Error> = (|| {
                                let db_lock = db.lock().unwrap();
                                let mut final_history = Vec::new();

                                // Fetch chat messages
                                if let Ok(mut stmt) = db_lock.prepare(
                                    "SELECT msg_id, username, message, timestamp FROM chat_messages 
                                     WHERE channel = ?1 ORDER BY id DESC LIMIT 50"
                                ) {
                                    if let Ok(rows) = stmt.query_map(params![channel], |row| {
                                        let msg_id_str: String = row.get(0)?;
                                        Ok(crate::network::NetworkPacket::ChatMessage {
                                            id: uuid::Uuid::parse_str(&msg_id_str).unwrap_or_default(),
                                            username: row.get(1)?,
                                            message: row.get::<_, Vec<u8>>(2)?,
                                            timestamp: row.get(3)?,
                                        })
                                    }) {
                                        for r in rows { if let Ok(p) = r { final_history.push(p); } }
                                    }
                                }
                                
                                // Fetch file messages
                                if let Ok(mut stmt_files) = db_lock.prepare(
                                    "SELECT msg_id, username, filename, data, is_image, timestamp FROM file_messages 
                                     WHERE channel = ?1 AND recipient IS NULL ORDER BY id DESC LIMIT 50"
                                ) {
                                    if let Ok(file_rows) = stmt_files.query_map(params![channel], |row| {
                                        let msg_id_str: String = row.get(0)?;
                                        Ok(crate::network::NetworkPacket::FileMessage {
                                            id: uuid::Uuid::parse_str(&msg_id_str).unwrap_or_default(),
                                            from: row.get(1)?,
                                            to: None,
                                            filename: row.get(2)?,
                                            data: row.get::<_, Vec<u8>>(3)?,
                                            is_image: row.get::<_, i32>(4)? == 1,
                                            timestamp: row.get(5)?,
                                        })
                                    }) {
                                        for r in file_rows { if let Ok(p) = r { final_history.push(p); } }
                                    }
                                }
                                
                                // Fetch reactions
                                if let Ok(mut stmt_react) = db_lock.prepare(
                                    "SELECT msg_id, username, emoji FROM reactions 
                                     WHERE msg_id IN (SELECT msg_id FROM chat_messages WHERE channel = ?1)
                                     OR msg_id IN (SELECT msg_id FROM file_messages WHERE channel = ?1)"
                                ) {
                                    if let Ok(react_rows) = stmt_react.query_map(params![channel], |row| {
                                        let msg_id_str: String = row.get(0)?;
                                        Ok(crate::network::NetworkPacket::Reaction {
                                            msg_id: uuid::Uuid::parse_str(&msg_id_str).unwrap_or_default(),
                                            from: row.get(1)?,
                                            emoji: row.get(2)?,
                                        })
                                    }) {
                                        for r in react_rows { if let Ok(p) = r { final_history.push(p); } }
                                    }
                                }

                                // Sort combined by timestamp
                                final_history.sort_by(|a, b| {
                                    let get_ts = |p: &crate::network::NetworkPacket| match p {
                                        crate::network::NetworkPacket::ChatMessage { timestamp, .. } => timestamp.clone(),
                                        crate::network::NetworkPacket::FileMessage { timestamp, .. } => timestamp.clone(),
                                        _ => "".to_string(),
                                    };
                                    get_ts(a).cmp(&get_ts(b))
                                });
                                
                                Ok(final_history)
                            })();
                            
                            match history_result {
                                Ok(history) => {
                                    let packet = crate::network::NetworkPacket::ChatHistory(history);
                                    if let Ok(encoded) = bincode::serialize(&packet) {
                                        let _ = socket.send_to(&encoded, addr).await;
                                    }
                                },
                                Err(e) => {
                                    eprintln!("DB Error fetching history for channel '{}': {}", channel, e);
                                    // Send partial/empty history or error packet? 
                                    // For now, just logging is better than crashing.
                                }
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
                                // Save to DB
                                {
                                    let db_lock = db.lock().unwrap();
                                    let _ = db_lock.execute("INSERT OR IGNORE INTO channels (name) VALUES (?1)", params![name]);
                                }
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
                crate::network::NetworkPacket::PrivateMessage { id, from, to, message, timestamp } => {
                    if let Some(info) = clients_guard.get(&addr) {
                        if info.is_authenticated && &info.username == from {
                            // Store in DB
                            {
                                let db_lock = db.lock().unwrap();
                                let _ = db_lock.execute(
                                    "INSERT INTO private_messages (msg_id, sender, recipient, message, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
                                    params![id.to_string(), from, to, message, timestamp],
                                );
                            }

                            // Relay to recipient if online
                            let recipient_addr = clients_guard.iter()
                                .find(|(_, info)| &info.username == to)
                                .map(|(&addr, _)| addr);

                            if let Some(target_addr) = recipient_addr {
                                let _ = socket.send_to(&buf[..len], target_addr).await;
                            }
                        }
                    }
                }
                crate::network::NetworkPacket::RequestDirectHistory { target } => {
                    if let Some(info) = clients_guard.get(&addr) {
                        if info.is_authenticated {
                            let me = info.username.clone();
                            let history_result: Result<Vec<crate::network::NetworkPacket>, rusqlite::Error> = (|| {
                                let db_lock = db.lock().unwrap();
                                let mut final_history = Vec::new();

                                // Fetch private messages
                                if let Ok(mut stmt) = db_lock.prepare(
                                    "SELECT msg_id, sender, recipient, message, timestamp FROM private_messages 
                                     WHERE (sender = ?1 AND recipient = ?2) OR (sender = ?2 AND recipient = ?1)
                                     ORDER BY id DESC LIMIT 50"
                                ) {
                                    if let Ok(rows) = stmt.query_map(params![me, target], |row| {
                                        let msg_id_str: String = row.get(0)?;
                                        Ok(crate::network::NetworkPacket::PrivateMessage {
                                            id: uuid::Uuid::parse_str(&msg_id_str).unwrap_or_default(),
                                            from: row.get(1)?,
                                            to: row.get(2)?,
                                            message: row.get::<_, Vec<u8>>(3)?,
                                            timestamp: row.get(4)?,
                                        })
                                    }) {
                                        for r in rows { if let Ok(p) = r { final_history.push(p); } }
                                    }
                                }

                                // Fetch file messages for this DM
                                if let Ok(mut stmt_files) = db_lock.prepare(
                                    "SELECT msg_id, username, recipient, filename, data, is_image, timestamp FROM file_messages 
                                     WHERE (username = ?1 AND recipient = ?2) OR (username = ?2 AND recipient = ?1)
                                     ORDER BY id DESC LIMIT 50"
                                ) {
                                    if let Ok(file_rows) = stmt_files.query_map(params![me, target], |row| {
                                        let msg_id_str: String = row.get(0)?;
                                        Ok(crate::network::NetworkPacket::FileMessage {
                                            id: uuid::Uuid::parse_str(&msg_id_str).unwrap_or_default(),
                                            from: row.get(1)?,
                                            to: Some(row.get(2)?),
                                            filename: row.get(3)?,
                                            data: row.get::<_, Vec<u8>>(4)?,
                                            is_image: row.get::<_, i32>(5)? == 1,
                                            timestamp: row.get(6)?,
                                        })
                                    }) {
                                        for r in file_rows { if let Ok(p) = r { final_history.push(p); } }
                                    }
                                }

                                // Fetch reactions for DM
                                if let Ok(mut stmt_react) = db_lock.prepare(
                                    "SELECT msg_id, username, emoji FROM reactions 
                                     WHERE msg_id IN (SELECT msg_id FROM private_messages WHERE (sender = ?1 AND recipient = ?2) OR (sender = ?2 AND recipient = ?1))
                                     OR msg_id IN (SELECT msg_id FROM file_messages WHERE (username = ?1 AND recipient = ?2) OR (username = ?2 AND recipient = ?1))"
                                ) {
                                    if let Ok(react_rows) = stmt_react.query_map(params![me, target], |row| {
                                        let msg_id_str: String = row.get(0)?;
                                        Ok(crate::network::NetworkPacket::Reaction {
                                            msg_id: uuid::Uuid::parse_str(&msg_id_str).unwrap_or_default(),
                                            from: row.get(1)?,
                                            emoji: row.get(2)?,
                                        })
                                    }) {
                                        for r in react_rows { if let Ok(p) = r { final_history.push(p); } }
                                    }
                                }
                                
                                final_history.sort_by(|a, b| {
                                    let get_ts = |p: &crate::network::NetworkPacket| match p {
                                        crate::network::NetworkPacket::ChatMessage { timestamp, .. } => timestamp.clone(),
                                        crate::network::NetworkPacket::PrivateMessage { timestamp, .. } => timestamp.clone(),
                                        crate::network::NetworkPacket::FileMessage { timestamp, .. } => timestamp.clone(),
                                        _ => "".to_string(),
                                    };
                                    get_ts(a).cmp(&get_ts(b))
                                });
                                
                                Ok(final_history)
                            })();

                            match history_result {
                                Ok(history) => {
                                    let response = crate::network::NetworkPacket::DirectHistory(history);
                                    if let Ok(encoded) = bincode::serialize(&response) {
                                        let _ = socket.send_to(&encoded, addr).await;
                                    }
                                },
                                Err(e) => {
                                    eprintln!("DB Error fetching DM history for '{}' and '{}': {}", me, target, e);
                                }
                            }
                        }
                    }
                }
                crate::network::NetworkPacket::FileStart { id, from, to, filename, total_chunks, is_image, timestamp } => {
                    let mut sender_channel = "Lobby".to_string();
                    let mut authenticated = false;
                    if let Some(info) = clients_guard.get(&addr) {
                        sender_channel = info.current_channel.clone();
                        authenticated = info.is_authenticated;
                    }

                    if authenticated {
                        let mut reassemblers = file_reassemblers.lock().await;
                        reassemblers.insert(*id, crate::app::PendingFile {
                            filename: filename.clone(),
                            from: from.clone(),
                            to: to.clone(),
                            is_image: *is_image,
                            timestamp: timestamp.clone(),
                            chunks: vec![None; *total_chunks],
                            total_chunks: *total_chunks,
                            received_count: 0,
                        });

                        if let Some(target) = to {
                            let recipient_addr = clients_guard.iter()
                                .find(|(_, info)| info.username == *target)
                                .map(|(&addr, _)| addr);
                            if let Some(target_addr) = recipient_addr {
                                let _ = socket.send_to(&buf[..len], target_addr).await;
                            }
                        } else {
                            for (&client_addr, info) in clients_guard.iter() {
                                if client_addr != addr && info.current_channel == sender_channel && info.is_authenticated {
                                    let _ = socket.send_to(&buf[..len], client_addr).await;
                                }
                            }
                        }
                    }
                }
                crate::network::NetworkPacket::FileChunk { id, chunk_index, data } => {
                     // In a real high-perf server, we'd have a reassembler here.
                     // For now, let's JUST relay the chunks. 
                     // To store in DB, we'd need to reassemble on server too.
                     // Let's implement a simple server-side reassembler for DB persistence.
                     
                     // Relay first
                     let mut sender_channel = "Lobby".to_string();
                     let mut authenticated = false;
                     if let Some(info) = clients_guard.get(&addr) {
                         sender_channel = info.current_channel.clone();
                         authenticated = info.is_authenticated;
                     }
                     
                     if authenticated {
                        // Relay
                        for (&client_addr, info) in clients_guard.iter() {
                             if client_addr != addr && info.is_authenticated {
                                 let _ = socket.send_to(&buf[..len], client_addr).await;
                             }
                        }

                        // Reassemble for DB
                        let mut reassemblers = file_reassemblers.lock().await;
                        if let Some(pending) = reassemblers.get_mut(id) {
                            if *chunk_index < pending.total_chunks && pending.chunks[*chunk_index].is_none() {
                                pending.chunks[*chunk_index] = Some(data.clone());
                                pending.received_count += 1;

                                if pending.received_count == pending.total_chunks {
                                    let mut full_data = Vec::new();
                                    for chunk in pending.chunks.drain(..) {
                                        if let Some(c) = chunk { full_data.extend(c); }
                                    }
                                    
                                    let from = pending.from.clone();
                                    let channel = sender_channel.clone();
                                    let recipient = pending.to.clone();
                                    let filename = pending.filename.clone();
                                    let is_image = pending.is_image;
                                    let timestamp = pending.timestamp.clone();
                                    
                                    let db_lock = db.lock().unwrap();
                                    let _ = db_lock.execute(
                                        "INSERT INTO file_messages (msg_id, username, channel, recipient, filename, data, is_image, timestamp) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                                        params![id.to_string(), from, channel, recipient, filename, full_data, if is_image { 1 } else { 0 }, timestamp],
                                    );
                                    reassemblers.remove(&id);
                                }
                            }
                        }
                     }
                }
                crate::network::NetworkPacket::Reaction { msg_id, emoji, from } => {
                    if let Some(info) = clients_guard.get(&addr) {
                        if info.is_authenticated && &info.username == from {
                            // Store in DB
                            {
                                let db_lock = db.lock().unwrap();
                                let _ = db_lock.execute(
                                    "INSERT INTO reactions (msg_id, username, emoji) VALUES (?1, ?2, ?3)",
                                    params![msg_id.to_string(), from, emoji],
                                );
                            }

                            // Broadcast to all relevant clients
                            for &client_addr in clients_guard.keys() {
                                let _ = socket.send_to(&buf[..len], client_addr).await;
                            }
                        }
                    }
                }
                crate::network::NetworkPacket::RequestProfile(target_user) => {
                    let mut avatar_url = String::new();
                    let mut bio = String::new();
                    
                    let db_lock = db.lock().unwrap();
                    let _ = db_lock.query_row(
                        "SELECT avatar_url, bio FROM users WHERE username = ?",
                        [target_user.clone()],
                        |row| {
                            avatar_url = row.get(0)?;
                            bio = row.get(1)?;
                            Ok(())
                        }
                    );
                    
                    let response = crate::network::NetworkPacket::ProfileUpdate {
                        username: target_user.to_string(),
                        avatar_url,
                        bio,
                    };
                    if let Ok(encoded) = bincode::serialize(&response) {
                        let _ = socket.send_to(&encoded, addr).await;
                    }
                }
                crate::network::NetworkPacket::ProfileUpdate { username: _, avatar_url, bio } => {
                    if let Some(info) = clients_guard.get(&addr) {
                        if info.is_authenticated {
                            let username = info.username.clone();
                            // Update in DB
                            {
                                let db_lock = db.lock().unwrap();
                                let _ = db_lock.execute(
                                    "UPDATE users SET avatar_url = ?, bio = ? WHERE username = ?",
                                    [avatar_url.clone(), bio.clone(), username.clone()],
                                );
                            }
                            
                            // Broadcast to all
                            let update = crate::network::NetworkPacket::ProfileUpdate {
                                username,
                                avatar_url: avatar_url.clone(),
                                bio: bio.clone(),
                            };
                            if let Ok(encoded) = bincode::serialize(&update) {
                                for &client_addr in clients_guard.keys() {
                                    let _ = socket.send_to(&encoded, client_addr).await;
                                }
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
            clients_guard.retain(|_, info| info.last_seen.elapsed().as_secs() < 30);
            if clients_guard.len() != initial_count {
                needs_broadcast = true;
            }

            // Broadcast channel/user state if needed
            if needs_broadcast {
                let mut state: Vec<(String, Vec<crate::network::UserInfo>)> = Vec::new();
                let chan_guard = channels.lock().await;
                
                for chan in chan_guard.iter() {
                    let mut users_in_chan = Vec::new();
                    for client in clients_guard.values() {
                        if &client.current_channel == chan && client.is_authenticated {
                            users_in_chan.push(crate::network::UserInfo {
                                username: client.username.clone(),
                                role: client.role.clone(),
                                is_muted: client.is_muted,
                                status: client.status.clone(),
                                nick_color: client.nick_color.clone(),
                            });
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
