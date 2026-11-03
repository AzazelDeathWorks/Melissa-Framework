//! Offline utility: measure_wire TOKENIZER_JSON BLAKE3 NATIVE_REQUEST_JSON.
//! No HTTP transport is constructed and no credentials are resolved.
use melissa_model_gateway::{
    Protocol,
    local_tokenizer::{LocalTokenizerConfig, LocalWireTokenizer},
    transport::{ApiKey, EndpointTrust, ProviderEndpoint},
};
use std::{fs::File, io::Read};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "--digest" {
        let mut bytes = Vec::new();
        File::open(&args[1])?
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err("tokenizer exceeds 32 MiB".into());
        }
        println!("{}", blake3::hash(&bytes).to_hex());
        return Ok(());
    }
    if args.len() != 3 {
        return Err("usage: measure_wire TOKENIZER_JSON BLAKE3 NATIVE_REQUEST_JSON".into());
    }
    let mut raw = Vec::new();
    File::open(&args[2])?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 2 * 1024 * 1024 {
        return Err("wire measurement exceeds 2 MiB".into());
    }
    let wire: serde_json::Value = serde_json::from_slice(&raw)?;
    let model = wire["model"]
        .as_str()
        .ok_or("native JSON requires a model field")?;
    let endpoint = ProviderEndpoint::new(
        "offline".into(),
        "provider:offline".into(),
        "authorization:offline".into(),
        Protocol::OpenAiResponses,
        EndpointTrust::RemoteHttps,
        "https://provider.example.invalid/v1/responses",
        ApiKey::new("offline-placeholder-never-used".into())?,
    )?;
    let tokenizer = LocalWireTokenizer::load(
        &LocalTokenizerConfig {
            path: args[0].clone().into(),
            blake3: args[1].to_str().ok_or("digest must be UTF-8")?.into(),
        },
        endpoint.url.as_str(),
        endpoint.protocol,
        model,
    )?;
    let measurement = tokenizer
        .measure(&endpoint, model, &wire)?
        .ok_or("wire not measurable")?;
    println!("{}", serde_json::to_string_pretty(&measurement)?);
    Ok(())
}
