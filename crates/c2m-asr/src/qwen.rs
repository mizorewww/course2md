//! Qwen ASR's synchronous chat interface (`qwen3-asr-flash` on an OpenAI-compatible
//! `/chat/completions` endpoint, such as Alibaba Cloud Model Studio's
//! `compatible-mode/v1`).
//!
//! Its request differs from a generic audio chat: the user message holds only the
//! audio, as a `data:` URI and with no instruction text; an optional system message is
//! context that biases recognition (names, terms), never an instruction; and the
//! recognition options travel in `asr_options`. Transcription and the desktop service
//! check build their requests here, so the two cannot drift apart.

use serde_json::{Value, json};

/// The service's accepted audio length per request, in seconds: "Audio length must be
/// between 1s and 300s" (`InvalidFile.AudioLengthError`).
pub const MAX_AUDIO_SECONDS: f64 = 300.0;
/// Shorter chunks are padded with silence to this length, clear of the 1 s minimum.
pub const MIN_AUDIO_SECONDS: f64 = 1.2;
/// The largest WAV one request may carry inline: the documented limit is 10 MB after
/// Base64 encoding, and the source file should stay under 7 MB.
pub const MAX_INLINE_WAV_BYTES: usize = 7_000_000;
/// Bytes per second of the 16 kHz mono 16-bit WAV chunks the engine cuts.
const WAV_BYTES_PER_SECOND: f64 = 16_000.0 * 2.0;
/// Header allowance around a chunk's samples.
const WAV_HEADER_BYTES: f64 = 1024.0;

/// The longest segment the engine sends, well inside the service's limits: about
/// 3.8 MB of WAV, 5.1 MB once encoded, so one request uploads within the engine's
/// 120 s request timeout on an uplink of about 1 Mbit/s.
pub const MAX_SEGMENT_SECONDS: f64 = 119.0;

/// The longest speech segment that keeps a cut chunk within the service's limits and
/// [`MAX_SEGMENT_SECONDS`], with room for the silence padding added on each side.
pub fn max_segment_seconds() -> f64 {
    let by_size = (MAX_INLINE_WAV_BYTES as f64 - WAV_HEADER_BYTES) / WAV_BYTES_PER_SECOND;
    (MAX_AUDIO_SECONDS.min(by_size) - 1.0)
        .floor()
        .min(MAX_SEGMENT_SECONDS)
}

const DATA_URI_PREFIX: &str = "data:audio/wav;base64,";

/// The audio as the `data:` URI the service reads.
pub fn wav_data_uri(wav: &[u8]) -> String {
    use base64::Engine as _;
    let mut uri = String::with_capacity(DATA_URI_PREFIX.len() + wav.len().div_ceil(3) * 4);
    uri.push_str(DATA_URI_PREFIX);
    base64::engine::general_purpose::STANDARD.encode_string(wav, &mut uri);
    uri
}

/// One recognition request for a WAV chunk. `context` (course or terminology hints)
/// becomes the system message when it holds any text; recognition options leave
/// language detection to the service and keep its output's text as spoken
/// (`enable_itn: false`, the service's default).
pub fn request_body(model: &str, wav: &[u8], context: Option<&str>) -> Value {
    let mut messages = Vec::new();
    if let Some(context) = context.map(str::trim).filter(|text| !text.is_empty()) {
        messages.push(json!({"role": "system", "content": context}));
    }
    messages.push(json!({"role": "user", "content": [
        {"type": "input_audio", "input_audio": {"data": wav_data_uri(wav)}}
    ]}));
    json!({
        "model": model,
        "messages": messages,
        "stream": false,
        "asr_options": {"enable_itn": false},
    })
}

/// The language the service detected, when its reply says (`annotations` of type
/// `audio_info`).
pub fn detected_language(reply: &Value) -> Option<&str> {
    reply["choices"][0]["message"]["annotations"]
        .as_array()?
        .iter()
        .find(|annotation| annotation["type"] == "audio_info")?["language"]
        .as_str()
        .filter(|language| !language.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_holds_only_audio_as_a_data_uri_with_optional_context() {
        let wav = b"RIFF....WAVEfmt ";
        let body = request_body("qwen3-asr-flash", wav, None);
        assert_eq!(
            body,
            json!({
                "model": "qwen3-asr-flash",
                "messages": [{"role": "user", "content": [
                    {"type": "input_audio", "input_audio": {"data": "data:audio/wav;base64,UklGRi4uLi5XQVZFZm10IA=="}}
                ]}],
                "stream": false,
                "asr_options": {"enable_itn": false},
            })
        );
        // Blank context sends no system message; text does, as context only.
        assert_eq!(
            request_body("m", wav, Some("  ")),
            request_body("m", wav, None)
        );
        let body = request_body("m", wav, Some(" 线性代数：特征值 "));
        assert_eq!(
            body["messages"][0],
            json!({"role": "system", "content": "线性代数：特征值"})
        );
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn replies_name_the_detected_language() {
        let reply = json!({"choices": [{"message": {
            "role": "assistant",
            "content": "欢迎使用阿里云。",
            "annotations": [{"type": "audio_info", "language": "zh", "emotion": "neutral"}]
        }}]});
        assert_eq!(detected_language(&reply), Some("zh"));
        let plain = json!({"choices": [{"message": {"content": "a"}}]});
        assert_eq!(detected_language(&plain), None);
    }

    #[test]
    fn a_full_segment_stays_within_both_limits() {
        let seconds = max_segment_seconds();
        assert_eq!(seconds, MAX_SEGMENT_SECONDS);
        // A segment may grow by its silence padding (0.25 s each side).
        let padded = seconds + 0.5;
        assert!(padded <= MAX_AUDIO_SECONDS);
        let wav = (padded * WAV_BYTES_PER_SECOND + 44.0) as usize;
        assert!(wav <= MAX_INLINE_WAV_BYTES);
        let encoded = wav_data_uri(&vec![0; wav]).len();
        assert!(encoded <= 10_000_000);
        // About 5.1 MB: one request uploads on a slow uplink within the request timeout.
        assert!(encoded <= 5_200_000, "{encoded}");
    }
}
