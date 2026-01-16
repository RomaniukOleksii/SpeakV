use tokio::net::UdpSocket;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use igd_next::{search_gateway, PortMappingProtocol};

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
        last_seen: tokio::time::Instant,
    }

    let clients: Arc<Mutex<HashMap<SocketAddr, ClientInfo>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut buf = [0u8; 4096];

    loop {
        let (len, addr) = socket.recv_from(&mut buf).await?;
        
        if let Ok(packet) = bincode::deserialize::<crate::network::NetworkPacket>(&buf[..len]) {
            let mut clients_guard = clients.lock().await;
            let mut needs_broadcast = false;
            
            match &packet {
                crate::network::NetworkPacket::Handshake { username } => {
                    println!("Server: {} connected from {}", username, addr);
                    clients_guard.insert(addr, ClientInfo {
                        username: username.clone(),
                        last_seen: tokio::time::Instant::now(),
                    });
                    needs_broadcast = true;
                }
                crate::network::NetworkPacket::Audio(_) | 
                crate::network::NetworkPacket::ChatMessage { .. } |
                crate::network::NetworkPacket::TypingStatus { .. } => {
                    // Update last seen
                    if let Some(info) = clients_guard.get_mut(&addr) {
                        info.last_seen = tokio::time::Instant::now();
                    }

                    // Relay to all other clients
                    for (&client_addr, _) in clients_guard.iter() {
                        if client_addr != addr {
                            let _ = socket.send_to(&buf[..len], client_addr).await;
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

            // Broadcast user list if needed
            if needs_broadcast {
                let usernames: Vec<String> = clients_guard.values().map(|c| c.username.clone()).collect();
                let update_packet = crate::network::NetworkPacket::UsersUpdate(usernames);
                if let Ok(encoded) = bincode::serialize(&update_packet) {
                    for &client_addr in clients_guard.keys() {
                        let _ = socket.send_to(&encoded, client_addr).await;
                    }
                }
            }
        }
    }
}
