use tokio::net::UdpSocket;
use std::sync::{Arc, Mutex};
use ringbuf::{HeapRb, traits::{Consumer, Producer, Observer}};
use anyhow::Result;
use std::net::SocketAddr;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum NetworkPacket {
    Handshake { username: String },
    Audio(Vec<f32>),
    ChatMessage { username: String, message: String, timestamp: String },
    UsersUpdate(Vec<String>),
    TypingStatus { username: String, is_typing: bool },
    Ping,
}

type LocalProducer = ringbuf::CachingProd<Arc<HeapRb<f32>>>;
type LocalConsumer = ringbuf::CachingCons<Arc<HeapRb<f32>>>;

#[derive(Clone)]
pub struct NetworkManager {
    is_running: Arc<Mutex<bool>>,
    pub is_connected: Arc<Mutex<bool>>,
    pub can_transmit: Arc<Mutex<bool>>,
    runtime: tokio::runtime::Handle,
    pub chat_tx: crossbeam_channel::Sender<NetworkPacket>,
    pub chat_rx: crossbeam_channel::Receiver<NetworkPacket>,
}

impl NetworkManager {
    pub fn new() -> Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        Ok(Self {
            is_running: Arc::new(Mutex::new(false)),
            is_connected: Arc::new(Mutex::new(false)),
            can_transmit: Arc::new(Mutex::new(false)),
            runtime: tokio::runtime::Handle::current(),
            chat_tx: tx,
            chat_rx: rx,
        })
    }

    pub fn start(
        &self,
        addr_str: String,
        input_consumer: Arc<Mutex<LocalConsumer>>,
        remote_producer: Arc<Mutex<LocalProducer>>,
        outgoing_chat_rx: crossbeam_channel::Receiver<NetworkPacket>,
    ) {
        let is_running = self.is_running.clone();
        let is_connected = self.is_connected.clone();
        let can_transmit = self.can_transmit.clone();
        let incoming_chat_tx = self.chat_tx.clone();
        
        self.runtime.spawn(async move {
            let addr: SocketAddr = match addr_str.parse() {
                Ok(a) => a,
                Err(_) => {
                    eprintln!("Network: Invalid address {}", addr_str);
                    return;
                }
            };

            let socket = match UdpSocket::bind("0.0.0.0:0").await {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    eprintln!("Network: Failed to bind socket: {}", e);
                    return;
                }
            };

            if let Err(e) = socket.connect(addr).await {
                eprintln!("Network: Failed to connect to {}: {}", addr, e);
                return;
            }

            *is_connected.lock().unwrap() = true;
            *is_running.lock().unwrap() = true;
            println!("Network: Connected to {}", addr);

            let mut input_buf = vec![0.0f32; 480]; // 10ms at 48kHz
            let mut receive_buf = vec![0u8; 4096]; // Increased buffer for packets

            loop {
                if !*is_running.lock().unwrap() {
                    break;
                }

                // 1. Send Outgoing Chat Messages
                while let Ok(packet) = outgoing_chat_rx.try_recv() {
                    if let Ok(encoded) = bincode::serialize(&packet) {
                        let _ = socket.send(&encoded).await;
                    }
                }

                // 2. Send Audio
                let mut has_audio = false;
                {
                    let mut cons = input_consumer.lock().unwrap();
                    if Observer::occupied_len(&*cons) >= 480 {
                        if *can_transmit.lock().unwrap() {
                            for sample in input_buf.iter_mut() {
                                *sample = cons.try_pop().unwrap_or(0.0);
                            }
                            has_audio = true;
                        } else {
                            cons.clear();
                        }
                    }
                }

                if has_audio {
                    let packet = NetworkPacket::Audio(input_buf.clone());
                    if let Ok(encoded) = bincode::serialize(&packet) {
                        let _ = socket.send(&encoded).await;
                    }
                }

                // 3. Receive Packets
                tokio::select! {
                    Ok(len) = socket.recv(&mut receive_buf) => {
                        if let Ok(packet) = bincode::deserialize::<NetworkPacket>(&receive_buf[..len]) {
                            match packet {
                                NetworkPacket::Audio(samples) => {
                                    let mut prod = remote_producer.lock().unwrap();
                                    for &sample in &samples {
                                        let _ = prod.try_push(sample);
                                    }
                                }
                                NetworkPacket::ChatMessage { .. } => {
                                    let _ = incoming_chat_tx.send(packet);
                                }
                                _ => {}
                            }
                        }
                    }
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(1)) => {}
                }
            }
            
            *is_connected.lock().unwrap() = false;
            println!("Network: Disconnected");
        });
    }

    pub fn stop(&self) {
        if let Ok(mut running) = self.is_running.lock() {
            *running = false;
        }
    }
}
