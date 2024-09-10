use std::fs::File;
use std::io::BufReader;
use std::time::Duration;
use alfred_rs::connection::{Receiver, Sender};
use alfred_rs::error::Error;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};
use alfred_rs::interface_module::InterfaceModule;
use alfred_rs::log::warn;
use alfred_rs::message::Message;
use alfred_rs::tokio;
use alfred_rs::tokio::spawn;
use alfred_rs::tokio::time::sleep;

const MODULE_NAME: &'static str = "audio_out";
const INPUT_TOPIC: &'static str = "audio_out";
const PLAY_STOP_EVENT: &'static str = "play_stop";
const PLAY_START_EVENT: &'static str = "play_start";
const PLAY_END_EVENT: &'static str = "play_end";

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

#[tokio::main]
async fn main() -> Result<(), Error> {
    env_logger::init();
    let mut audio_module = AudioModule::new().await;
    audio_module.module.listen(INPUT_TOPIC.to_string()).await?;

    loop {
        let (topic, message) = audio_module.module.receive().await?;
        match topic.as_str() {
            INPUT_TOPIC => {
                audio_module.play(message.text.clone()).await?;
                tokio::spawn(async move {
                    on_end(&mut audio_module).await;
                });
            },
            _ => {
                warn!("Unknown topic {topic}");
            }
        }
    }
}