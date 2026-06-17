use super::{
    DEFAULT_CHAT_MODEL, DEFAULT_IMAGE_MODEL, DEFAULT_TTS_LANGUAGE, DEFAULT_TTS_VOICE,
    DEFAULT_VIDEO_MODEL, DEFAULT_WEB_SEARCH_MODEL, DEFAULT_X_SEARCH_MODEL,
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub(super) fn tool_specs() -> Vec<Value> {
    vec![
        spec_xai_models(),
        spec_xai_responses(),
        spec_x_search(),
        spec_xai_x_search(),
        spec_web_search(),
        spec_xai_web_search(),
        spec_xai_image_generate(),
        spec_xai_tts(),
        spec_xai_transcribe(),
        spec_xai_video_generate(),
    ]
}

pub(super) fn spec_xai_models() -> Value {
    tool_spec::<EmptyArgs>(
        "xai_models",
        "List models from xAI using the stored xAI OAuth bearer.",
    )
}

pub(super) fn spec_xai_responses() -> Value {
    tool_spec::<XaiResponsesArgs>(
        "xai_responses",
        "Call xAI /responses with arbitrary Responses API payload. Requires `nerve auth login xai` first.",
    )
}

pub(super) fn spec_x_search() -> Value {
    x_search_spec(
        "x_search",
        "Search X/Twitter using xAI OAuth. This is the preferred generic X search tool for agents.",
    )
}

pub(super) fn spec_xai_x_search() -> Value {
    x_search_spec(
        "xai_x_search",
        "Search X/Twitter through xAI's built-in x_search Responses API tool using xAI OAuth.",
    )
}

fn x_search_spec(name: &str, description: &str) -> Value {
    tool_spec::<XSearchArgs>(name, description)
}

pub(super) fn spec_web_search() -> Value {
    web_search_spec(
        "web_search",
        "Search the web using xAI OAuth. This is the preferred generic web search tool for agents.",
    )
}

pub(super) fn spec_xai_web_search() -> Value {
    web_search_spec(
        "xai_web_search",
        "Search the web through xAI's web_search Responses API tool using xAI OAuth.",
    )
}

fn web_search_spec(name: &str, description: &str) -> Value {
    tool_spec::<WebSearchArgs>(name, description)
}

pub(super) fn spec_xai_image_generate() -> Value {
    tool_spec::<ImageGenerateArgs>(
        "xai_image_generate",
        "Generate an image with xAI Grok Imagine and save it to output_path.",
    )
}

pub(super) fn spec_xai_tts() -> Value {
    tool_spec::<TtsArgs>(
        "xai_tts",
        "Generate speech via xAI /tts and save audio to output_path.",
    )
}

pub(super) fn spec_xai_transcribe() -> Value {
    tool_spec::<TranscribeArgs>(
        "xai_transcribe",
        "Transcribe an audio file via xAI /stt using multipart upload.",
    )
}

pub(super) fn spec_xai_video_generate() -> Value {
    tool_spec::<VideoGenerateArgs>(
        "xai_video_generate",
        "Generate video with xAI Grok Imagine. Returns video_url and optionally downloads to output_path.",
    )
}

fn tool_spec<T: JsonSchema>(name: &str, description: &str) -> Value {
    let mut spec = Map::new();
    spec.insert("name".to_string(), Value::String(name.to_string()));
    spec.insert(
        "description".to_string(),
        Value::String(description.to_string()),
    );
    spec.insert("inputSchema".to_string(), input_schema::<T>());
    Value::Object(spec)
}

fn input_schema<T: JsonSchema>() -> Value {
    let mut schema = serde_json::to_value(schema_for!(T)).expect("schema serializes");
    if let Value::Object(object) = &mut schema {
        object.remove("$schema");
    }
    schema
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct XaiResponsesArgs {
    /// Responses API input: string or message array/object.
    input: Value,
    #[serde(default = "default_chat_model")]
    #[schemars(default = "default_chat_model")]
    model: String,
    /// Optional Responses API tools.
    #[serde(default)]
    tools: Vec<Value>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    max_output_tokens: Option<u64>,
    #[serde(default)]
    reasoning: Option<Value>,
    #[serde(default = "default_timeout_180")]
    #[schemars(default = "default_timeout_180")]
    timeout_seconds: u64,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct XSearchArgs {
    query: String,
    #[serde(default = "default_x_search_model")]
    #[schemars(default = "default_x_search_model")]
    model: String,
    /// Max 10 handles; mutually exclusive with excluded_x_handles.
    #[serde(default)]
    allowed_x_handles: Vec<String>,
    /// Max 10 handles; mutually exclusive with allowed_x_handles.
    #[serde(default)]
    excluded_x_handles: Vec<String>,
    /// YYYY-MM-DD
    #[serde(default)]
    from_date: Option<String>,
    /// YYYY-MM-DD
    #[serde(default)]
    to_date: Option<String>,
    #[serde(default)]
    #[schemars(default)]
    enable_image_understanding: bool,
    #[serde(default)]
    #[schemars(default)]
    enable_video_understanding: bool,
    #[serde(default = "default_timeout_180")]
    #[schemars(default = "default_timeout_180")]
    timeout_seconds: u64,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WebSearchArgs {
    query: String,
    #[serde(default = "default_limit_5")]
    #[schemars(default = "default_limit_5", range(min = 1, max = 100))]
    limit: u64,
    #[serde(default = "default_web_search_model")]
    #[schemars(default = "default_web_search_model")]
    model: String,
    /// Max 5; mutually exclusive with excluded_domains.
    #[serde(default)]
    allowed_domains: Vec<String>,
    /// Max 5; mutually exclusive with allowed_domains.
    #[serde(default)]
    excluded_domains: Vec<String>,
    #[serde(default = "default_timeout_90")]
    #[schemars(default = "default_timeout_90")]
    timeout_seconds: u64,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImageGenerateArgs {
    prompt: String,
    /// Workspace used to root-gate output_path. Defaults like core tools.
    #[serde(default)]
    workspace: Option<String>,
    /// Destination inside the selected workspace root.
    output_path: String,
    #[serde(default = "default_image_model")]
    #[schemars(default = "default_image_model")]
    model: String,
    #[serde(default = "default_image_aspect_ratio")]
    #[schemars(default = "default_image_aspect_ratio")]
    aspect_ratio: String,
    #[serde(default = "default_image_resolution")]
    #[schemars(default = "default_image_resolution")]
    resolution: ImageResolution,
    #[serde(default = "default_true")]
    #[schemars(default = "default_true")]
    download_url: bool,
    #[serde(default = "default_timeout_120")]
    #[schemars(default = "default_timeout_120")]
    timeout_seconds: u64,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TtsArgs {
    text: String,
    /// Workspace used to root-gate output_path. Defaults like core tools.
    #[serde(default)]
    workspace: Option<String>,
    /// Destination inside the selected workspace root.
    output_path: String,
    #[serde(default = "default_tts_voice")]
    #[schemars(default = "default_tts_voice")]
    voice_id: String,
    #[serde(default = "default_tts_language")]
    #[schemars(default = "default_tts_language")]
    language: String,
    /// Optional xAI output_format, e.g. {codec,sample_rate,bit_rate}.
    #[serde(default)]
    output_format: Option<Value>,
    #[serde(default = "default_timeout_60")]
    #[schemars(default = "default_timeout_60")]
    timeout_seconds: u64,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TranscribeArgs {
    /// Workspace used to root-gate file_path. Defaults like core tools.
    #[serde(default)]
    workspace: Option<String>,
    /// Audio file inside the selected workspace root.
    file_path: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default = "default_true")]
    #[schemars(default = "default_true")]
    format: bool,
    #[serde(default)]
    #[schemars(default)]
    diarize: bool,
    #[serde(default = "default_timeout_120")]
    #[schemars(default = "default_timeout_120")]
    timeout_seconds: u64,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct VideoGenerateArgs {
    prompt: String,
    /// Workspace used to root-gate output_path. Defaults like core tools.
    #[serde(default)]
    workspace: Option<String>,
    /// Optional destination inside the selected workspace root.
    #[serde(default)]
    output_path: Option<String>,
    #[serde(default = "default_video_model")]
    #[schemars(default = "default_video_model")]
    model: String,
    /// Optional image URL/data URI for image-to-video.
    #[serde(default)]
    image_url: Option<String>,
    /// Optional reference images; max 7.
    #[serde(default)]
    reference_image_urls: Vec<String>,
    #[serde(default = "default_video_duration")]
    #[schemars(default = "default_video_duration", range(min = 1, max = 15))]
    duration: u64,
    #[serde(default = "default_video_aspect_ratio")]
    #[schemars(default = "default_video_aspect_ratio")]
    aspect_ratio: String,
    #[serde(default = "default_video_resolution")]
    #[schemars(default = "default_video_resolution")]
    resolution: VideoResolution,
    #[serde(default = "default_timeout_240")]
    #[schemars(default = "default_timeout_240")]
    timeout_seconds: u64,
    #[serde(default = "default_poll_interval")]
    #[schemars(default = "default_poll_interval")]
    poll_interval_seconds: u64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
enum ImageResolution {
    #[serde(rename = "1k")]
    OneK,
    #[serde(rename = "2k")]
    TwoK,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
enum VideoResolution {
    #[serde(rename = "480p")]
    P480,
    #[serde(rename = "720p")]
    P720,
}

fn default_chat_model() -> String {
    DEFAULT_CHAT_MODEL.to_string()
}

fn default_x_search_model() -> String {
    DEFAULT_X_SEARCH_MODEL.to_string()
}

fn default_web_search_model() -> String {
    DEFAULT_WEB_SEARCH_MODEL.to_string()
}

fn default_image_model() -> String {
    DEFAULT_IMAGE_MODEL.to_string()
}

fn default_video_model() -> String {
    DEFAULT_VIDEO_MODEL.to_string()
}

fn default_tts_voice() -> String {
    DEFAULT_TTS_VOICE.to_string()
}

fn default_tts_language() -> String {
    DEFAULT_TTS_LANGUAGE.to_string()
}

fn default_image_aspect_ratio() -> String {
    "1:1".to_string()
}

fn default_video_aspect_ratio() -> String {
    "16:9".to_string()
}

fn default_image_resolution() -> ImageResolution {
    ImageResolution::OneK
}

fn default_video_resolution() -> VideoResolution {
    VideoResolution::P720
}

fn default_true() -> bool {
    true
}

fn default_limit_5() -> u64 {
    5
}

fn default_timeout_60() -> u64 {
    60
}

fn default_timeout_90() -> u64 {
    90
}

fn default_timeout_120() -> u64 {
    120
}

fn default_timeout_180() -> u64 {
    180
}

fn default_timeout_240() -> u64 {
    240
}

fn default_video_duration() -> u64 {
    8
}

fn default_poll_interval() -> u64 {
    5
}
