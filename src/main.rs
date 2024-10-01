use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use alfred_rs::connection::{Receiver, Sender};
use alfred_rs::error::Error;
use rodio::{Decoder, Device, DeviceTrait, OutputStream, Sink};
use alfred_rs::interface_module::InterfaceModule;
use alfred_rs::log::{debug, warn};
use alfred_rs::message::{Message, MessageType};
use alfred_rs::tokio;
use alfred_rs::tokio::sync::Mutex;
use alfred_rs::tokio::sync::mpsc;
use rodio::cpal::traits::HostTrait;

const MODULE_NAME: &'static str = "audio_out";
const INPUT_TOPIC: &'static str = "audio_out";
const STOP_TOPIC: &'static str = "audio_out.stop";
const PLAY_STOP_EVENT: &'static str = "play_stop";
const PLAY_START_EVENT: &'static str = "play_start";
const PLAY_END_EVENT: &'static str = "play_end";

#[derive(Debug)]
enum PlayerCommand {
    Play(String),
    Stop
}

impl PlayerCommand {
    pub(crate) fn clone(&self) -> Self {
        match self {
            PlayerCommand::Play(val) => PlayerCommand::Play(val.clone()),
            PlayerCommand::Stop => PlayerCommand::Stop
        }
    }
}

#[derive(Debug)]
enum PlayerEvent {
    PlayStarted(String),
    PlayEnded,
    PlayStopped
}

async fn check_player_status(sink: Arc<Mutex<Sink>>, sender: mpsc::Sender<PlayerEvent>) {
    let mut is_playing = false;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let cur_len = sink.lock().await.len();
        if is_playing {
            if cur_len == 0 {
                is_playing = false;
                sender.send(PlayerEvent::PlayEnded).await.unwrap();
            }
        } else {
            if cur_len > 0 {
                is_playing = true;
            }
        }
    }
}

fn get_device(device_name: String) -> Option<Device> {
    let devices = rodio::cpal::default_host().output_devices().unwrap();
    for device in devices {
        let cur_dev_name = device.name().unwrap();
        if device_name == cur_dev_name {
            return Some(device);
        }
    }
    None
}

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() -> Result<(), Error> {
    env_logger::init();
    let module = InterfaceModule::new(MODULE_NAME.to_string()).await.expect("Failed to create module");
    let subscriber = Arc::new(Mutex::new(module.connection.subscriber));
    let publisher = Arc::new(Mutex::new(module.connection.publisher));
    subscriber.lock().await.listen(INPUT_TOPIC.to_string()).await.expect("Failed to listen");
    subscriber.lock().await.listen(STOP_TOPIC.to_string()).await.expect("Failed to listen");

    let device_name = module.config.get_module_value("device".to_string()).unwrap_or("default".to_string());

    let (alfred_sender, mut player_receiver) = mpsc::channel(10);
    let (player_sender, mut alfred_receiver) = mpsc::channel::<PlayerEvent>(100);
    let player_sender_end_checker = player_sender.clone();

    let device = get_device(device_name.clone()).expect(format!("Failed to get device {device_name}").as_str());
    let (_output_stream, stream_handle) = OutputStream::try_from_device(&device).unwrap();
    let sink = Arc::new(Mutex::new(Sink::try_new(&stream_handle).expect("Error creating the sink")));
    let sink_end_checker = sink.clone();

    // alfred event-publisher
    tokio::spawn(async move {
        loop {
            let player_event = alfred_receiver.recv().await.unwrap();
            debug!("Event: {:?}", player_event);
            match player_event {
                PlayerEvent::PlayStarted(audio_file) => {
                    let event_message = Message { text: audio_file, message_type: MessageType::AUDIO, ..Message::default() };
                    publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_START_EVENT.to_string(), &event_message).await.expect("TODO: panic message");
                }
                PlayerEvent::PlayEnded => {
                    publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_END_EVENT.to_string(), &Message::empty()).await.expect("TODO: panic message");
                }
                PlayerEvent::PlayStopped => {
                    publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_STOP_EVENT.to_string(), &Message::empty()).await.expect("TODO: panic message");
                }
            }
        }
    });

    // alfred subscriber
    tokio::spawn(async move {
        loop {
            let (topic, message) = subscriber.lock().await.receive().await.expect("");
            match topic.as_str() {
                INPUT_TOPIC => {
                    alfred_sender.send(PlayerCommand::Play(message.text.clone())).await.unwrap();
                },
                STOP_TOPIC => {
                    alfred_sender.send(PlayerCommand::Stop).await.unwrap();
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
        let command = player_receiver.recv().await.unwrap().clone();
        debug!("Analysing input command: {:?}", command);
        match command {
            PlayerCommand::Play(audio_file) => {
                player_sender.send(PlayerEvent::PlayStopped).await.unwrap();
                sink.lock().await.stop();
                if audio_file.len() == 0 {
                    continue;
                }
                let file = BufReader::new(File::open(audio_file.clone()).unwrap());
                let source = Decoder::new(file).unwrap();
                let mut play_start_msg = Message::default();
                play_start_msg.text = audio_file.clone();
                player_sender.send(PlayerEvent::PlayStarted(audio_file.clone())).await.unwrap();
                sink.lock().await.append(source);
            }
            PlayerCommand::Stop => {
                sink.lock().await.stop();
            }
        }
    }
}