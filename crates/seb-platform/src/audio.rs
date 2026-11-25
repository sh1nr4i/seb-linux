use pulseaudio::protocol::{
    self, AuthParams, ChannelVolume, Command, CommandReply, GetSinkInfo, Props,
    SetDeviceMuteParams, SetDeviceVolumeParams, TagStructRead, TagStructReader, TagStructWrite,
    TagStructWriter, Volume,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::process::Command as ProcessCommand;

const PA_VOLUME_NORM: u64 = 0x1_0000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AudioState {
    pub available: bool,
    pub muted: bool,
    pub volume_percent: i32,
}

#[derive(Default)]
struct AckReply;

impl TagStructRead for AckReply {
    fn read(_ts: &mut TagStructReader<'_>, _protocol_version: u16) -> Result<Self, protocol::ProtocolError> {
        Ok(Self)
    }
}

impl TagStructWrite for AckReply {
    fn write(
        &self,
        _w: &mut TagStructWriter<'_>,
        _protocol_version: u16,
    ) -> Result<(), protocol::ProtocolError> {
        Ok(())
    }
}

impl CommandReply for AckReply {}

struct PaSyncConn {
    reader: BufReader<UnixStream>,
    proto: u16,
    seq: u32,
}

impl PaSyncConn {
    fn connect() -> Option<Self> {
        let sock_path = pulseaudio::socket_path_from_env()?;
        let sock = UnixStream::connect(sock_path).ok()?;
        let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(800)));
        let _ = sock.set_write_timeout(Some(std::time::Duration::from_millis(800)));
        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_default();

        let mut conn = Self {
            reader: BufReader::new(sock),
            proto: protocol::MAX_VERSION,
            seq: 0,
        };

        let auth_reply: protocol::AuthReply = conn
            .roundtrip(Command::Auth(AuthParams {
                version: protocol::MAX_VERSION,
                supports_shm: false,
                supports_memfd: false,
                cookie,
            }))
            .ok()?;
        conn.proto = protocol::MAX_VERSION.min(auth_reply.version);

        let mut props = Props::new();
        props.set(protocol::Prop::ApplicationName, c"Safe Exam Browser");
        let _ = conn.roundtrip::<protocol::SetClientNameReply>(Command::SetClientName(props)).ok()?;
        Some(conn)
    }

    fn roundtrip<R: CommandReply>(&mut self, cmd: Command) -> Result<R, ()> {
        let req_seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        protocol::write_command_message(self.reader.get_mut(), req_seq, &cmd, self.proto)
            .map_err(|_| ())?;
        let (reply_seq, reply) =
            protocol::read_reply_message(&mut self.reader, self.proto).map_err(|_| ())?;
        if reply_seq != req_seq {
            return Err(());
        }
        Ok(reply)
    }
}

fn pa_default_sink(conn: &mut PaSyncConn) -> Option<protocol::SinkInfo> {
    let srv: protocol::ServerInfo = conn.roundtrip(Command::GetServerInfo).ok()?;
    let sink_name = srv
        .default_sink_name
        .or_else(|| Some(protocol::DEFAULT_SINK.to_owned()))?;
    conn.roundtrip(Command::GetSinkInfo(GetSinkInfo {
        index: None,
        name: Some(sink_name),
    }))
    .ok()
}

fn pa_query_audio() -> Option<AudioState> {
    let mut conn = PaSyncConn::connect()?;
    let sink = pa_default_sink(&mut conn)?;
    let channels = sink.cvolume.channels();
    let pct = if channels.is_empty() {
        0
    } else {
        let avg: u64 = channels.iter().map(|v| v.as_u32() as u64).sum::<u64>() / channels.len() as u64;
        ((avg * 100 + PA_VOLUME_NORM / 2) / PA_VOLUME_NORM).clamp(0, 100) as i32
    };
    Some(AudioState {
        available: true,
        muted: sink.muted,
        volume_percent: pct,
    })
}

fn pa_set_muted(muted: bool) -> bool {
    let Some(mut conn) = PaSyncConn::connect() else {
        return false;
    };
    let Some(sink) = pa_default_sink(&mut conn) else {
        return false;
    };
    conn.roundtrip::<AckReply>(Command::SetSinkMute(SetDeviceMuteParams {
        device_index: Some(sink.index),
        device_name: None,
        mute: muted,
    }))
    .is_ok()
}

fn pa_set_volume(percent: i32) -> bool {
    let Some(mut conn) = PaSyncConn::connect() else {
        return false;
    };
    let Some(sink) = pa_default_sink(&mut conn) else {
        return false;
    };
    let bounded = percent.clamp(0, 100) as u64;
    let raw = ((bounded * PA_VOLUME_NORM + 50) / 100) as u32;
    let count = sink.cvolume.channels().len().max(1);
    let mut cvol = ChannelVolume::empty();
    for _ in 0..count {
        cvol.push(Volume::from_u32_clamped(raw));
    }
    let sink_name: CString = sink.name;
    conn.roundtrip::<AckReply>(Command::SetSinkVolume(SetDeviceVolumeParams {
        device_index: None,
        device_name: Some(sink_name),
        volume: cvol,
    }))
    .is_ok()
}

fn run_cmd(program: &str, args: &[&str]) -> String {
    ProcessCommand::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn parse_wpctl_volume(output: &str) -> AudioState {
    let mut next = AudioState::default();
    if output.is_empty() {
        return next;
    }
    next.available = true;
    next.muted = output.to_ascii_lowercase().contains("[muted]");
    if let Ok(re) = Regex::new(r"([0-9]+(?:\.[0-9]+)?)") {
        if let Some(caps) = re.captures(output) {
            if let Ok(val) = caps[1].parse::<f64>() {
                next.volume_percent = ((val * 100.0).round() as i32).clamp(0, 100);
            }
        }
    }
    next
}

pub fn query_audio() -> AudioState {
    // 1. Native Pure-Rust PulseAudio socket (works on PulseAudio & pipewire-pulse)
    if let Some(state) = pa_query_audio() {
        return state;
    }
    // 2. WirePlumber `wpctl` fallback
    let out = run_cmd("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]);
    if !out.is_empty() {
        return parse_wpctl_volume(&out);
    }
    // 3. ALSA `amixer` fallback for minimal non-PulseAudio / non-PipeWire systems
    let amix = run_cmd("amixer", &["get", "Master"]);
    if !amix.is_empty() {
        let mut next = AudioState {
            available: true,
            muted: amix.contains("[off]"),
            volume_percent: 0,
        };
        if let Ok(re) = Regex::new(r"\[([0-9]{1,3})%\]") {
            if let Some(caps) = re.captures(&amix) {
                if let Ok(v) = caps[1].parse::<i32>() {
                    next.volume_percent = v.clamp(0, 100);
                }
            }
        }
        return next;
    }
    AudioState::default()
}

pub fn set_audio_muted(muted: bool) {
    if pa_set_muted(muted) {
        return;
    }
    if !run_cmd(
        "wpctl",
        &["set-mute", "@DEFAULT_AUDIO_SINK@", if muted { "1" } else { "0" }],
    )
    .is_empty()
    {
        return;
    }
    let _ = run_cmd("amixer", &["set", "Master", if muted { "mute" } else { "unmute" }]);
}

pub fn set_audio_volume(percent: i32) {
    let bounded = percent.clamp(0, 100);
    if pa_set_volume(bounded) {
        return;
    }
    let arg = format!("{bounded}%");
    if ProcessCommand::new("wpctl")
        .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &arg])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return;
    }
    let _ = run_cmd("amixer", &["set", "Master", &arg]);
}
