use tokio::net::UdpSocket;
use std::sync::{Arc, Mutex};
use ringbuf::{HeapRb, traits::{Consumer, Producer, Observer}};
use anyhow::Result;
use std::net::SocketAddr;

type LocalProducer = ringbuf::CachingProd<Arc<HeapRb<f32>>>;
type LocalConsumer = ringbuf::CachingCons<Arc<HeapRb<f32>>>;

#[derive(Clone)]
pub struct NetworkManager {
    is_running: Arc<Mutex<bool>>,
    pub is_connected: Arc<Mutex<bool>>,
    pub can_transmit: Arc<Mutex<bool>>,
    runtime: tokio::runtime::Handle,
}

impl NetworkManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            is_running: Arc::new(Mutex::new(false)),
            is_connected: Arc::new(Mutex::new(false)),
            can_transmit: Arc::new(Mutex::new(false)),
            runtime: tokio::runtime::Handle::current(),
        })
    }

    pub fn start(
        &self,
        addr_str: String,
        input_consumer: Arc<Mutex<LocalConsumer>>,
        remote_producer: Arc<Mutex<LocalProducer>>,
    ) {
        let is_running = self.is_running.clone();
        let is_connected = self.is_connected.clone();
        let can_transmit = self.can_transmit.clone();
        
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
            let mut receive_buf = vec![0u8; 2048];

            loop {
                if !*is_running.lock().unwrap() {
                    break;
                }

                // Send Audio
                let mut has_data = false;
                {
                    let mut cons = input_consumer.lock().unwrap();
                    if Observer::occupied_len(&*cons) >= 480 {
                        if *can_transmit.lock().unwrap() {
                            for sample in input_buf.iter_mut() {
                                *sample = cons.try_pop().unwrap_or(0.0);
                            }
                            has_data = true;
                        } else {
                            cons.clear();
                        }
                    }
                }

                if has_data {
                    let bytes: &[u8] = unsafe {
                        std::slice::from_raw_parts(
                            input_buf.as_ptr() as *const u8,
                            input_buf.len() * 4
                        )
                    };
                    let _ = socket.send(bytes).await;
                }

                // Receive Audio
                tokio::select! {
                    Ok(len) = socket.recv(&mut receive_buf) => {
                        let samples_count = len / 4;
                        if samples_count > 0 {
                            let samples: &[f32] = unsafe {
                                std::slice::from_raw_parts(
                                    receive_buf.as_ptr() as *const f32,
                                    samples_count
                                )
                            };
                            
                            let mut prod = remote_producer.lock().unwrap();
                            for &sample in samples {
                                let _ = prod.try_push(sample);
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
