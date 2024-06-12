use std::fs::File;
use std::io::BufReader;
use alfred_rs::connection::Receiver;
use rodio::{Decoder, OutputStream, Sink};
use alfred_rs::error::Error;
use alfred_rs::interface_module::InterfaceModule;
use alfred_rs::log::warn;
use alfred_rs::tokio;

const MODULE_NAME: &'static str = "audio_out";
const INPUT_TOPIC: &'static str = "audio_out";

fn play_audio(audio_file: String) {
    let (_stream, stream_handle) = OutputStream::try_default().unwrap();
    let sink = Sink::try_new(&stream_handle).unwrap();
    let file = BufReader::new(File::open(audio_file).unwrap());
    let source = Decoder::new(file).unwrap();
    sink.append(source);
    sink.sleep_until_end();
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    env_logger::init();
    let mut module = InterfaceModule::new(MODULE_NAME.to_string()).await?;
    module.listen(INPUT_TOPIC.to_string()).await?;
    loop {
        let (topic, message) = module.receive().await?;
        match topic.as_str() {
            INPUT_TOPIC => play_audio(message.text),
            _ => {
                warn!("Unknown topic {topic}");
            }
        }
    }
}