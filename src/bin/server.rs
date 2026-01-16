use tokio::net::UdpSocket;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:9999").await?;
    println!("SpeakV Server listening on 0.0.0.0:9999");

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
