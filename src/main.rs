use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use alfred_rs::connection::{Receiver, Sender};
use alfred_rs::error::Error;
use rodio::{Decoder, OutputStream, Sink};
use alfred_rs::interface_module::InterfaceModule;
use alfred_rs::log::{info, warn};
use alfred_rs::message::{Message, MessageType};
use alfred_rs::tokio;
use alfred_rs::tokio::sync::Mutex;
use alfred_rs::tokio::sync::mpsc;

const MODULE_NAME: &'static str = "audio_out";
const INPUT_TOPIC: &'static str = "audio_out";
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
    PlayEnded(String),
    PlayStopped
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    env_logger::init();
    let module = InterfaceModule::new(MODULE_NAME.to_string()).await.expect("Failed to create module");
    let subscriber = Arc::new(Mutex::new(module.connection.subscriber));
    let publisher = Arc::new(Mutex::new(module.connection.publisher));
    subscriber.lock().await.listen(INPUT_TOPIC.to_string()).await.expect("Failed to listen");

    let (alfred_sender, mut player_receiver) = mpsc::channel(10);
    let (player_sender, mut alfred_receiver) = mpsc::channel::<PlayerEvent>(100);

    let (_output_stream, stream_handle) = OutputStream::try_default().unwrap();

    // alfred event-publisher
    tokio::spawn(async move {
        loop {
            let player_event = alfred_receiver.recv().await.unwrap();
            info!("Event: {:?}", player_event);
            match player_event {
                PlayerEvent::PlayStarted(audio_file) => {
                    let event_message = Message { text: audio_file, message_type: MessageType::AUDIO, ..Message::default() };
                    publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_START_EVENT.to_string(), &event_message).await.expect("TODO: panic message");
                }
                PlayerEvent::PlayEnded(audio_file) => {
                    let event_message = Message { text: audio_file, message_type: MessageType::AUDIO, ..Message::default() };
                    publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_END_EVENT.to_string(), &event_message).await.expect("TODO: panic message");
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
                _ => {
                    warn!("Unknown topic {topic}");
                }
            }
        }
    });

    // player
    loop {
        let sink = Sink::try_new(&stream_handle).expect("Error creating the sink");
        let player_sender = player_sender.clone();
        let command = player_receiver.recv().await.unwrap().clone();
        info!("Analysing input command: {:?}", command);
        match command {
            PlayerCommand::Play(audio_file) => {
                info!("Playing {}", audio_file);
                player_sender.send(PlayerEvent::PlayStopped).await.unwrap();
                sink.stop();
                if audio_file.len() == 0 {
                    continue;
                }
                let file = BufReader::new(File::open(audio_file.clone()).unwrap());
                let source = Decoder::new(file).unwrap();
                let mut play_start_msg = Message::default();
                play_start_msg.text = audio_file.clone();
                player_sender.send(PlayerEvent::PlayStarted(audio_file.clone())).await.unwrap();
                sink.append(source);
                sink.sleep_until_end();
                player_sender.send(PlayerEvent::PlayEnded(audio_file.clone())).await.unwrap()
            }
            PlayerCommand::Stop => {
                sink.stop();
            }
        }
    }
}