use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{HeapRb, traits::{Consumer, Producer, Split}};
use std::sync::{Arc, Mutex};
use anyhow::Result;

type LocalProducer = ringbuf::CachingProd<Arc<HeapRb<f32>>>;
type LocalConsumer = ringbuf::CachingCons<Arc<HeapRb<f32>>>;

pub struct AudioManager {
    input_stream: Option<cpal::Stream>,
    output_stream: Option<cpal::Stream>,
    is_recording: bool,
    pub current_volume: Arc<Mutex<f32>>,
    pub is_input_muted: Arc<Mutex<bool>>,
    pub is_output_muted: Arc<Mutex<bool>>,
    pub is_self_listen: Arc<Mutex<bool>>,
    
    pub current_input_device: String,
    pub current_output_device: String,

    pub local_producer: Arc<Mutex<LocalProducer>>,
    pub remote_producer: Arc<Mutex<LocalProducer>>,
    pub input_consumer: Arc<Mutex<LocalConsumer>>,
}

impl AudioManager {
    pub fn get_input_devices() -> Vec<String> {
        let host = cpal::default_host();
        match host.input_devices() {
            Ok(devices) => devices.map(|d| d.name().unwrap_or_else(|_| "Unknown Device".to_string())).collect(),
            Err(_) => vec![],
        }
    }

    pub fn get_output_devices() -> Vec<String> {
        let host = cpal::default_host();
        match host.output_devices() {
            Ok(devices) => devices.map(|d| d.name().unwrap_or_else(|_| "Unknown Device".to_string())).collect(),
            Err(_) => vec![],
        }
    }

    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let input_device = host.default_input_device().ok_or(anyhow::anyhow!("No input device"))?;
        let output_device = host.default_output_device().ok_or(anyhow::anyhow!("No output device"))?;
        
        let input_name = input_device.name()?;
        let output_name = output_device.name()?;

        let input_rb = Arc::new(HeapRb::<f32>::new(48000 * 2));
        let (input_prod, input_cons) = input_rb.split();

        let local_rb = Arc::new(HeapRb::<f32>::new(48000 * 2));
        let (local_prod, local_cons) = local_rb.split();

        let remote_rb = Arc::new(HeapRb::<f32>::new(48000 * 2));
        let (remote_prod, remote_cons) = remote_rb.split();
        
        let mut manager = Self {
            input_stream: None,
            output_stream: None,
            is_recording: false,
            current_volume: Arc::new(Mutex::new(0.0)),
            is_input_muted: Arc::new(Mutex::new(false)),
            is_output_muted: Arc::new(Mutex::new(false)),
            is_self_listen: Arc::new(Mutex::new(false)),
            
            current_input_device: input_name.clone(),
            current_output_device: output_name.clone(),
            
            local_producer: Arc::new(Mutex::new(local_prod)),
            remote_producer: Arc::new(Mutex::new(remote_prod)),
            input_consumer: Arc::new(Mutex::new(input_cons)),
        };

        manager.setup_streams(&input_name, &output_name, input_prod, local_cons, remote_cons)?;
        Ok(manager)
    }

    fn setup_streams(
        &mut self, 
        input_device_name: &str, 
        output_device_name: &str,
        mut input_prod: LocalProducer,
        mut local_cons: LocalConsumer,
        mut remote_cons: LocalConsumer,
    ) -> Result<()> {
        let host = cpal::default_host();
        
        let input_device = host.input_devices()?
            .filter(|d| d.name().map(|n| n == input_device_name).unwrap_or(false))
            .next()
            .ok_or(anyhow::anyhow!("Input device not found"))?;

        let output_device = host.output_devices()?
            .filter(|d| d.name().map(|n| n == output_device_name).unwrap_or(false))
            .next()
            .ok_or(anyhow::anyhow!("Output device not found"))?;

        let input_config = input_device.default_input_config()?;
        let output_config = output_device.default_output_config()?;

        let volume_clone = self.current_volume.clone();
        let input_muted_clone = self.is_input_muted.clone();
        let output_muted_clone = self.is_output_muted.clone();
        let self_listen_clone = self.is_self_listen.clone();
        let local_prod_mutex = self.local_producer.clone();

        let input_stream = input_device.build_input_stream(
            &input_config.into(),
            move |data: &[f32], _: &_| {
                let muted = *input_muted_clone.lock().unwrap();
                let self_listen = *self_listen_clone.lock().unwrap();

                if muted {
                    if let Ok(mut vol) = volume_clone.lock() {
                        *vol = 0.0;
                    }
                    return;
                }

                let mut sum_sq = 0.0;
                let mut local_prod = local_prod_mutex.lock().unwrap();
                for &sample in data {
                    sum_sq += sample * sample;
                    let _ = input_prod.try_push(sample);
                    if self_listen {
                        let _ = local_prod.try_push(sample);
                    }
                }
                let rms = (sum_sq / data.len() as f32).sqrt();
                if let Ok(mut vol) = volume_clone.lock() {
                    *vol = *vol * 0.8 + rms * 0.2; 
                }
            },
            |err| eprintln!("Input stream error: {}", err),
            None
        )?;

        let output_stream = output_device.build_output_stream(
            &output_config.into(),
            move |data: &mut [f32], _: &_| {
                if *output_muted_clone.lock().unwrap() {
                    data.fill(0.0);
                    return;
                }
                for sample in data.iter_mut() {
                    let local = local_cons.try_pop().unwrap_or(0.0);
                    let remote = remote_cons.try_pop().unwrap_or(0.0);
                    *sample = local + remote;
                }
            },
            |err| eprintln!("Output stream error: {}", err),
            None
        )?;

        if self.is_recording {
            input_stream.play()?;
            output_stream.play()?;
        }

        self.input_stream = Some(input_stream);
        self.output_stream = Some(output_stream);
        self.current_input_device = input_device_name.to_string();
        self.current_output_device = output_device_name.to_string();

        Ok(())
    }

    pub fn set_input_muted(&self, muted: bool) {
        if let Ok(mut m) = self.is_input_muted.lock() {
            *m = muted;
        }
    }

    pub fn set_output_muted(&self, muted: bool) {
        if let Ok(mut m) = self.is_output_muted.lock() {
            *m = muted;
        }
    }

    pub fn set_self_listen(&self, listen: bool) {
        if let Ok(mut l) = self.is_self_listen.lock() {
            *l = listen;
        }
    }

    pub fn start_recording(&mut self) {
        if !self.is_recording {
            self.is_recording = true;
            if let Some(stream) = &self.input_stream {
                let _ = stream.play();
            }
            if let Some(stream) = &self.output_stream {
                let _ = stream.play();
            }
        }
    }

    pub fn stop_recording(&mut self) {
        if self.is_recording {
            self.is_recording = false;
            if let Some(stream) = &self.input_stream {
                let _ = stream.pause();
            }
            if let Some(stream) = &self.output_stream {
                let _ = stream.pause();
            }
            if let Ok(mut vol) = self.current_volume.lock() {
                *vol = 0.0;
            }
        }
    }
}
