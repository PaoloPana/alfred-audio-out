use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use alfred_rs::connection::{Receiver, Sender};
use alfred_rs::error::Error;
use rodio::{Decoder, OutputStream, Sink};
use alfred_rs::interface_module::InterfaceModule;
use alfred_rs::log::warn;
use alfred_rs::message::Message;
use alfred_rs::tokio;
use alfred_rs::tokio::sync::Mutex;
use alfred_rs::tokio::sync::mpsc;
//use std::sync::mpsc;

const MODULE_NAME: &'static str = "audio_out";
const INPUT_TOPIC: &'static str = "audio_out";
const PLAY_STOP_EVENT: &'static str = "play_stop";
const PLAY_START_EVENT: &'static str = "play_start";
const PLAY_END_EVENT: &'static str = "play_end";
/*
async fn on_end(audio_module: &AudioModule) {
    sleep(Duration::from_millis(1000)).await;
    println!("{:?}", audio_module.sink.empty());
    audio_module.sink.sleep_until_end();
    let mut play_end_msg = Message::default();
    //audio_module.module.send_event(MODULE_NAME.to_string(), PLAY_END_EVENT.to_string(), &play_end_msg).await;
}

struct AudioModule {
    pub module: InterfaceModule,
    pub sink: Sink,
    pub output_stream: OutputStream,
    pub stream_handle: OutputStreamHandle
}

unsafe impl Send for AudioModule {}

impl AudioModule {
    async fn new() -> Self {
        let (output_stream, stream_handle) = OutputStream::try_default().unwrap();
        let mut module = InterfaceModule::new(MODULE_NAME.to_string()).await.expect("Failed to create module");
        module.listen(INPUT_TOPIC.to_string()).await.expect("Failed to listen");
        Self {
            module,
            sink: Sink::try_new(&stream_handle).expect("Error creating the sink"),
            output_stream, stream_handle
        }
    }

    async fn on_end(&mut self) {
        sleep(Duration::from_millis(1000)).await;
        println!("{:?}", self.sink.empty());
        self.sink.sleep_until_end();
        let mut play_end_msg = Message::default();
        self.module.send_event(MODULE_NAME.to_string(), PLAY_END_EVENT.to_string(), &play_end_msg).await;
    }

    async fn _play(&mut self, audio_file: String) -> Result<(), Error> {
        self.module.send_event(MODULE_NAME.to_string(), PLAY_STOP_EVENT.to_string(), &Message::default()).await?;
        self.sink.stop();
        if audio_file.len() == 0 {
            return Ok(());
        }
        let file = BufReader::new(File::open(audio_file.clone()).unwrap());
        let source = Decoder::new(file).unwrap();
        let mut play_start_msg = Message::default();
        play_start_msg.text = audio_file.clone();
        self.module.send_event(MODULE_NAME.to_string(), PLAY_START_EVENT.to_string(), &play_start_msg).await?;
        self.sink.append(source);
        Ok(())
    }

    async fn play(&mut self, audio_file: String) -> Result<(), Error> {
        self._play(audio_file).await

    }
}
 */

#[derive(Debug)]
enum PlayerCommand {
    Play(String),
    Stop
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

    let (alfred_sender, mut player_receiver) = mpsc::channel(1);
    let (player_sender, mut alfred_receiver) = mpsc::channel(1);

    // let player_sender = Arc::new(Mutex::new(player_sender));
    // let player_receiver = Arc::new(Mutex::new(player_receiver));

    let (_output_stream, stream_handle) = OutputStream::try_default().unwrap();
    let sink = Arc::new(Mutex::new(Sink::try_new(&stream_handle).expect("Error creating the sink")));
    let sink2 = sink.clone();
    tokio::spawn(async move {
        //let player_receiver = player_receiver.clone();
        let sink = sink.clone();
        async move {
            loop {
                let command = player_receiver.recv().await.unwrap();
                match command {
                    PlayerCommand::Play(audio_file) => {
                        let res = player_sender.send(PlayerEvent::PlayStopped).await;
                        if res.is_err() {
                            println!("Error sending play stop event: {:?}", res.err().unwrap().to_string());
                        }
                        sink.lock().await.stop();
                        if audio_file.len() == 0 {
                            return;
                        }
                        let file = BufReader::new(File::open(audio_file.clone()).unwrap());
                        let source = Decoder::new(file).unwrap();
                        let mut play_start_msg = Message::default();
                        play_start_msg.text = audio_file.clone();
                        player_sender.send(PlayerEvent::PlayStarted(audio_file.clone())).await.unwrap();
                        sink.lock().await.append(source);
                        sink.lock().await.sleep_until_end();
                        player_sender.send(PlayerEvent::PlayEnded(audio_file.clone())).await.unwrap()
                    }
                    PlayerCommand::Stop => {
                        sink.lock().await.stop();
                    }
                }
            }
        }
    });
    tokio::spawn(async move {
        let sink = sink2.clone();
        async move {
            loop {
                if sink.lock().await.len() > 0 {
                    sink.lock().await.sleep_until_end();
                    println!("Song finished");
                } else {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    });
    tokio::spawn(async move {
        loop {
            println!("CIAO");
            let player_event = alfred_receiver.recv().await;
            if player_event.is_none() {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            match player_event.unwrap() {
                PlayerEvent::PlayStarted(audio_file) => {
                    publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_START_EVENT.to_string(), &Message::empty()).await.expect("TODO: panic message");
                }
                PlayerEvent::PlayEnded(audio_file) => {
                    publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_END_EVENT.to_string(), &Message::empty()).await.expect("TODO: panic message");
                }
                PlayerEvent::PlayStopped => {
                    publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_STOP_EVENT.to_string(), &Message::empty()).await.expect("TODO: panic message");
                }
            }
        }
    });
    /*
    std::thread::spawn(move || {
        async move {
            loop {
                println!("CIAO");
                let player_event = alfred_receiver.recv().unwrap();
                match player_event {
                    PlayerEvent::PlayStarted(audio_file) => {
                        publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_START_EVENT.to_string(), &Message::empty()).await.unwrap();
                    }
                    PlayerEvent::PlayEnded(audio_file) => {
                        publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_END_EVENT.to_string(), &Message::empty()).await.unwrap();
                    }
                    PlayerEvent::PlayStopped => {
                        publisher.lock().await.send_event(MODULE_NAME.to_string(), PLAY_STOP_EVENT.to_string(), &Message::empty()).await.unwrap();
                    }
                }
            }
        }
    });

     */
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
}