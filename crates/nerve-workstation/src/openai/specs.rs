use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub(super) fn tool_specs() -> Vec<Value> {
    vec![spec_openai_image_generate(), spec_openai_models()]
}

fn spec_openai_image_generate() -> Value {
    tool_spec::<ImageGenerateArgs>(
        "openai_image_generate",
        "Generate a PNG image via ChatGPT/Codex subscription OAuth and save it to output_path.",
    )
}

fn spec_openai_models() -> Value {
    tool_spec::<EmptyArgs>(
        "openai_models",
        "List Codex models from the ChatGPT/Codex backend using stored OpenAI OAuth.",
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
struct ImageGenerateArgs {
    prompt: String,
    /// Workspace used to root-gate output_path. Defaults like core tools.
    #[serde(default)]
    workspace: Option<String>,
    /// Destination inside the selected workspace root.
    output_path: String,
    #[serde(default = "default_size")]
    #[schemars(default = "default_size")]
    size: ImageSize,
    #[serde(default = "default_quality")]
    #[schemars(default = "default_quality")]
    quality: ImageQuality,
    #[serde(default = "default_timeout_300")]
    #[schemars(default = "default_timeout_300")]
    timeout_seconds: u64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
enum ImageSize {
    #[serde(rename = "landscape")]
    Landscape,
    #[serde(rename = "square")]
    Square,
    #[serde(rename = "portrait")]
    Portrait,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
enum ImageQuality {
    #[serde(rename = "high")]
    High,
    #[serde(rename = "auto")]
    Auto,
}

fn default_size() -> ImageSize {
    ImageSize::Square
}

fn default_quality() -> ImageQuality {
    ImageQuality::High
}

fn default_timeout_300() -> u64 {
    300
}
