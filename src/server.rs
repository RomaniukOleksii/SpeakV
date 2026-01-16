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

    let clients: Arc<Mutex<HashMap<SocketAddr, tokio::time::Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut buf = [0u8; 2048];

    loop {
        let (len, addr) = socket.recv_from(&mut buf).await?;
        
        let mut clients_guard = clients.lock().await;
        clients_guard.insert(addr, tokio::time::Instant::now());

        // Relay to all other clients
        for (&client_addr, _) in clients_guard.iter() {
            if client_addr != addr {
                let _ = socket.send_to(&buf[..len], client_addr).await;
            }
        }
        
        // Clean up old clients (timeout after 5 seconds)
        clients_guard.retain(|_, last_seen| last_seen.elapsed().as_secs() < 5);
    }
}
