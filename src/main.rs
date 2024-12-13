use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use alfred_rs::error::Error;
use rodio::{Decoder, Device, DeviceTrait, OutputStream, Sink};
use alfred_rs::AlfredModule;
use alfred_rs::log::{debug, error, warn};
use alfred_rs::message::{Message, MessageType};
use alfred_rs::tokio;
use alfred_rs::tokio::sync::{mpsc, Mutex};
use rodio::cpal::traits::HostTrait;

const MODULE_NAME: &str = "audio_out";
const INPUT_TOPIC: &str = "audio_out";
const STOP_TOPIC: &str = "audio_out.stop";
const PLAY_STOP_EVENT: &str = "play_stop";
const PLAY_START_EVENT: &str = "play_start";
const PLAY_END_EVENT: &str = "play_end";

#[derive(Debug)]
enum PlayerCommand {
    Play(String),
    Stop
}

#[derive(Debug)]
enum PlayerEvent {
    Started(String),
    Ended,
    Stopped
}

async fn check_player_status(sink: Arc<Mutex<Sink>>, sender: mpsc::Sender<PlayerEvent>) {
    let mut is_playing = false;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let cur_len = sink.lock().await.len();
        if is_playing {
            if cur_len == 0 {
                is_playing = false;
                sender.send(PlayerEvent::Ended).await.unwrap_or_default();
            }
        } else if cur_len > 0 {
            is_playing = true;
        }
    }
}

fn get_device(device_name: &str) -> Option<Device> {
    match rodio::cpal::default_host().output_devices() {
        Ok(devices) => {
            devices
                .filter(|device| device.name().unwrap_or_default() == device_name)
                .collect::<Vec<Device>>()
                .first()
                .cloned()
        },
        Err(e) => {
            warn!("Failed to get audio device: {:?}", e);
            None
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() -> Result<(), Error> {
    env_logger::init();
    let mut module = AlfredModule::new(MODULE_NAME, env!("CARGO_PKG_VERSION")).await.expect("Failed to create module");
    module.listen(INPUT_TOPIC).await.expect("Failed to listen");
    module.listen(STOP_TOPIC).await.expect("Failed to listen");

    let alfred_msg_recv = module.connection.clone();
    let alfred_event = module.connection.clone();

    let device_name = module.config.get_module_value("device").unwrap_or_else(|| "default".to_string());
    let volume = module.config.get_module_value("volume")
        .map_or(1.0, |s| s.parse::<f32>().expect("Volume parameter is not a number") / 100.0);

    let (alfred_sender, mut player_receiver) = mpsc::channel(10);
    let (player_sender, mut alfred_receiver) = mpsc::channel::<PlayerEvent>(100);
    let player_sender_end_checker = player_sender.clone();

    let device = get_device(device_name.as_str()).unwrap_or_else(|| panic!("Failed to get device {device_name}"));
    let (_output_stream, stream_handle) = OutputStream::try_from_device(&device).expect("Failed to create output stream");
    let sink = Arc::new(Mutex::new(Sink::try_new(&stream_handle).expect("Error creating the sink")));
    sink.lock().await.set_volume(volume);
    let sink_end_checker = sink.clone();

    // alfred event-publisher
    tokio::spawn(async move {
        loop {
            let Some(player_event) = alfred_receiver.recv().await else {
                warn!("Cannot receive message from Alfred");
                continue;
            };
            debug!("Event: {:?}", player_event);
            match player_event {
                PlayerEvent::Started(audio_file) => {
                    let event_message = Message { text: audio_file, message_type: MessageType::Audio, ..Message::default() };
                    alfred_event.send_event(MODULE_NAME, PLAY_START_EVENT, &event_message).await.expect("TODO: panic message");
                }
                PlayerEvent::Ended => {
                    alfred_event.send_event(MODULE_NAME, PLAY_END_EVENT, &Message::empty()).await.expect("TODO: panic message");
                }
                PlayerEvent::Stopped => {
                    alfred_event.send_event(MODULE_NAME, PLAY_STOP_EVENT, &Message::empty()).await.expect("TODO: panic message");
                }
            }
        }
    });

    // alfred subscriber
    tokio::spawn(async move {
        loop {
            let (topic, message) = alfred_msg_recv.receive(MODULE_NAME, &HashMap::new()).await.expect("Failed to receive message");
            match topic.as_str() {
                INPUT_TOPIC => {
                    alfred_sender.send(PlayerCommand::Play(message.text.clone())).await.expect("Cannot send play message");
                },
                STOP_TOPIC => {
                    alfred_sender.send(PlayerCommand::Stop).await.expect("Cannot send stop message");
                }
                _ => {
                    warn!("Unknown topic {topic}");
                }
            }
        }
    });

    // player_end check
    tokio::spawn(async move {
        let sink = sink_end_checker.clone();
        check_player_status(sink.clone(), player_sender_end_checker.clone()).await;
    });

    // player
    loop {
        let sink = sink.clone();
        let player_sender = player_sender.clone();
        if let Err(e) = player_handler(sink, player_sender, &mut player_receiver).await {
            warn!("Error handling player {e:?}");
        }
    }
}

async fn player_handler(sink: Arc<Mutex<Sink>>, player_sender: mpsc::Sender<PlayerEvent>, player_receiver: &mut mpsc::Receiver<PlayerCommand>) -> Result<(), Box<dyn std::error::Error>> {
    let command = player_receiver.recv().await.expect("Player disconnected");
    debug!("Analysing input command: {:?}", command);
    match command {
        PlayerCommand::Play(audio_file) => {
            player_sender.send(PlayerEvent::Stopped).await.unwrap_or_default();
            sink.lock().await.stop();
            if audio_file.is_empty() {
                warn!("Audio file is empty");
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Audio file is empty").into());
            }
            let Ok(file) = File::open(audio_file.clone()) else {
                error!("Error opening audio file {audio_file}");
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Audio file is invalid").into());
            };
            let file = BufReader::new(file);
            let source = Decoder::new(file);
            if let Ok(source) = source {
                player_sender.send(PlayerEvent::Started(audio_file.clone())).await.unwrap_or(());
                sink.lock().await.append(source);
                Ok(())
            } else {
                Err("Audio file is invalid")?
            }
        }
        PlayerCommand::Stop => {
            sink.lock().await.stop();
            Ok(())
        }
    }
}