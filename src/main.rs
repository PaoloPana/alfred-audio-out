use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use alfred_core::error::Error;
use rodio::{Decoder, Device, DeviceTrait, OutputStream, Sink};
use rodio::buffer::SamplesBuffer;
use alfred_core::AlfredModule;
use alfred_core::log::{debug, error, warn};
use alfred_core::message::{Message, MessageType};
use alfred_core::tokio;
use alfred_core::tokio::sync::{mpsc, Mutex};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rodio::cpal::traits::HostTrait;

const MODULE_NAME: &str = "audio_out";
const INPUT_TOPIC: &str = "audio_out";
const STOP_TOPIC: &str = "audio_out.stop";
const PLAY_STOP_EVENT: &str = "play_stop";
const PLAY_START_EVENT: &str = "play_start";
const PLAY_END_EVENT: &str = "play_end";
/// Used when a `StreamAudio` chunk doesn't carry `sample_rate`/`channels` params.
const DEFAULT_SAMPLE_RATE: u32 = 16_000;
const DEFAULT_CHANNELS: u16 = 1;

enum PlayerCommand {
    Play(String),
    PlayChunk { stream_id: String, samples: Vec<i16>, sample_rate: u32, channels: u16 },
    EndStream { stream_id: String },
    Stop
}

impl fmt::Debug for PlayerCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Play(audio_file) => write!(f, "Play({audio_file})"),
            Self::PlayChunk { stream_id, samples, sample_rate, channels } =>
                write!(f, "PlayChunk({stream_id}, {} samples, {sample_rate} Hz, {channels} ch)", samples.len()),
            Self::EndStream { stream_id } => write!(f, "EndStream({stream_id})"),
            Self::Stop => write!(f, "Stop"),
        }
    }
}

#[derive(Debug)]
enum PlayerEvent {
    Started(String),
    StreamStarted(String),
    Ended,
    Stopped
}

async fn check_player_status(sink: Arc<Mutex<Sink>>, sender: mpsc::Sender<PlayerEvent>, stream_open: Arc<AtomicBool>) {
    let mut is_playing = false;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let cur_len = sink.lock().await.len();
        if is_playing {
            // an open stream drains between chunks: that is not the end of the playback
            if cur_len == 0 && !stream_open.load(Ordering::Relaxed) {
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

fn get_param<T: std::str::FromStr>(message: &Message, name: &str, default: T) -> T {
    message.params.get(name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Decodes the base64 PCM16 little-endian payload of a `StreamAudio` chunk.
fn decode_samples(text: &str) -> Result<Vec<i16>, base64::DecodeError> {
    let bytes = BASE64.decode(text)?;
    Ok(bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect())
}

fn get_stream_commands(message: &Message) -> Vec<PlayerCommand> {
    let mut commands = Vec::new();
    if !message.text.is_empty() {
        match decode_samples(&message.text) {
            Ok(samples) if !samples.is_empty() => commands.push(PlayerCommand::PlayChunk {
                stream_id: message.stream_id.clone(),
                samples,
                sample_rate: get_param(message, "sample_rate", DEFAULT_SAMPLE_RATE),
                channels: get_param(message, "channels", DEFAULT_CHANNELS),
            }),
            Ok(_) => warn!("Empty audio chunk on stream {}", message.stream_id),
            Err(e) => warn!("Cannot decode audio chunk: {e:?}"),
        }
    }
    if message.is_final {
        commands.push(PlayerCommand::EndStream { stream_id: message.stream_id.clone() });
    }
    commands
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
    let stream_open = Arc::new(AtomicBool::new(false));
    let stream_open_end_checker = stream_open.clone();

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
                PlayerEvent::StreamStarted(stream_id) => {
                    let event_message = Message { message_type: MessageType::StreamAudio, stream_id, ..Message::default() };
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
            let (topic, message) = alfred_msg_recv.receive(MODULE_NAME, &BTreeMap::new()).await.expect("Failed to receive message");
            match topic.as_str() {
                INPUT_TOPIC => {
                    let commands = match message.message_type {
                        MessageType::StreamAudio => get_stream_commands(&message),
                        MessageType::Unknown | MessageType::Text | MessageType::Audio | MessageType::Photo
                        | MessageType::StreamText | MessageType::StreamPhoto | MessageType::ModuleInfo =>
                            vec![PlayerCommand::Play(message.text.clone())],
                    };
                    for command in commands {
                        alfred_sender.send(command).await.expect("Cannot send play message");
                    }
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
        check_player_status(sink.clone(), player_sender_end_checker.clone(), stream_open_end_checker).await;
    });

    // player
    let mut current_stream = None;
    loop {
        let sink = sink.clone();
        let player_sender = player_sender.clone();
        if let Err(e) = player_handler(sink, player_sender, &mut player_receiver, &mut current_stream, &stream_open).await {
            warn!("Error handling player {e:?}");
        }
    }
}

async fn player_handler(
    sink: Arc<Mutex<Sink>>,
    player_sender: mpsc::Sender<PlayerEvent>,
    player_receiver: &mut mpsc::Receiver<PlayerCommand>,
    current_stream: &mut Option<String>,
    stream_open: &Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let command = player_receiver.recv().await.expect("Player disconnected");
    debug!("Analysing input command: {:?}", command);
    match command {
        PlayerCommand::Play(audio_file) => {
            player_sender.send(PlayerEvent::Stopped).await.unwrap_or_default();
            sink.lock().await.stop();
            *current_stream = None;
            stream_open.store(false, Ordering::Relaxed);
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
        PlayerCommand::PlayChunk { stream_id, samples, sample_rate, channels } => {
            if current_stream.as_ref() != Some(&stream_id) {
                // a new stream interrupts whatever is playing, as a new file does
                player_sender.send(PlayerEvent::Stopped).await.unwrap_or_default();
                sink.lock().await.stop();
                player_sender.send(PlayerEvent::StreamStarted(stream_id.clone())).await.unwrap_or(());
                *current_stream = Some(stream_id);
            }
            stream_open.store(true, Ordering::Relaxed);
            sink.lock().await.append(SamplesBuffer::new(channels, sample_rate, samples));
            Ok(())
        }
        PlayerCommand::EndStream { stream_id } => {
            // the chunks already queued still have to play out: the status check ends the playback
            if current_stream.as_ref() == Some(&stream_id) {
                stream_open.store(false, Ordering::Relaxed);
            }
            Ok(())
        }
        PlayerCommand::Stop => {
            sink.lock().await.stop();
            *current_stream = None;
            stream_open.store(false, Ordering::Relaxed);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use super::*;

    fn chunk(text: &str, is_final: bool, params: BTreeMap<String, String>) -> Message {
        Message {
            text: text.to_string(),
            message_type: MessageType::StreamAudio,
            stream_id: String::from("stream-1"),
            is_final,
            params,
            ..Message::default()
        }
    }

    #[test]
    fn decodes_pcm16_little_endian() {
        // -2, 1, 256
        let samples = decode_samples(&BASE64.encode([0xFE, 0xFF, 0x01, 0x00, 0x00, 0x01]))
            .expect("valid base64");
        assert_eq!(samples, vec![-2, 1, 256]);
    }

    #[test]
    fn chunk_produces_a_play_command_with_the_declared_format() {
        let params = BTreeMap::from([
            (String::from("sample_rate"), String::from("24000")),
            (String::from("channels"), String::from("2")),
        ]);
        let commands = get_stream_commands(&chunk(&BASE64.encode([0x01, 0x00]), false, params));
        match commands.as_slice() {
            [PlayerCommand::PlayChunk { stream_id, samples, sample_rate, channels }] => {
                assert_eq!(stream_id, "stream-1");
                assert_eq!(samples, &vec![1]);
                assert_eq!(*sample_rate, 24_000);
                assert_eq!(*channels, 2);
            }
            other => panic!("unexpected commands: {other:?}"),
        }
    }

    #[test]
    fn missing_params_fall_back_to_the_defaults() {
        let commands = get_stream_commands(&chunk(&BASE64.encode([0x01, 0x00]), false, BTreeMap::new()));
        match commands.as_slice() {
            [PlayerCommand::PlayChunk { sample_rate, channels, .. }] => {
                assert_eq!(*sample_rate, DEFAULT_SAMPLE_RATE);
                assert_eq!(*channels, DEFAULT_CHANNELS);
            }
            other => panic!("unexpected commands: {other:?}"),
        }
    }

    #[test]
    fn final_chunk_plays_its_audio_and_then_ends_the_stream() {
        let commands = get_stream_commands(&chunk(&BASE64.encode([0x01, 0x00]), true, BTreeMap::new()));
        assert!(matches!(commands.as_slice(), [PlayerCommand::PlayChunk { .. }, PlayerCommand::EndStream { .. }]));
    }

    #[test]
    fn empty_final_chunk_only_ends_the_stream() {
        let commands = get_stream_commands(&chunk("", true, BTreeMap::new()));
        assert!(matches!(commands.as_slice(), [PlayerCommand::EndStream { stream_id }] if stream_id == "stream-1"));
    }

    #[test]
    fn undecodable_chunk_is_dropped() {
        assert!(get_stream_commands(&chunk("not base64!", false, BTreeMap::new())).is_empty());
    }
}
