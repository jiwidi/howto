//! Download-free qualification harness for an already-exported GGUF.
//!
//! Example:
//! cargo run --release --example benchmark_embedded -- \
//!   /absolute/model.gguf --profile balanced --runs 20

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use std::process::Command;

use howto::embedded::{
    ChatMessage, ChatRole, EmbeddedEngine, EngineOptions, GenerationOptions, LoraAdapterOptions,
};
use howto::model_catalog::ModelProfile;
use howto::platform::Platform;
use serde_json::json;
use sha2::{Digest, Sha256};

const PROMPTS: [&str; 5] = [
    "list hidden files in the current directory",
    "show which process is using port 8080",
    "find files larger than 100 MB",
    "show the current git branch",
    "print disk usage for each item here, sorted largest first",
];

struct Arguments {
    model: PathBuf,
    profile: ModelProfile,
    lora: Option<PathBuf>,
    runs: usize,
    prompt_profile: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = parse_arguments()?;
    let entry = arguments.profile.entry();
    let options = EngineOptions {
        context_size: 2_048,
        batch_size: 512,
        threads: std::thread::available_parallelism()
            .map_or(4, std::num::NonZero::get)
            .clamp(1, 8),
        lora_adapter: arguments.lora.as_ref().map(|path| LoraAdapterOptions {
            path: path.clone(),
            scale: 1.0,
        }),
        ..EngineOptions::default()
    };

    let load_started = Instant::now();
    let engine = EmbeddedEngine::load(&arguments.model, options)?;
    let load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;
    let platform = Platform::current();
    let system_prompt = match arguments.prompt_profile.as_str() {
        "production" => platform.system_prompt(false),
        "training" => concat!(
            "You translate a natural-language request into a single POSIX/bash command line.\n",
            "Rules:\n",
            "- Output ONLY the command. No prose, no markdown fences, no explanation.\n",
            "- The command runs with the working directory set to the task workspace.\n",
            "- Do not use sudo, network access, or destructive operations unless explicitly asked.\n"
        )
        .to_owned(),
        _ => unreachable!("validated by parse_arguments"),
    };
    let mut latencies = Vec::with_capacity(arguments.runs);
    let mut outputs = Vec::with_capacity(arguments.runs.min(PROMPTS.len()));

    for index in 0..arguments.runs {
        let messages = [
            ChatMessage::new(ChatRole::System, system_prompt.clone()),
            ChatMessage::new(ChatRole::User, PROMPTS[index % PROMPTS.len()]),
        ];
        let mut generation = GenerationOptions {
            max_tokens: 96,
            temperature: 0.0,
            repeat_penalty: 1.08,
            repeat_last_n: 64,
            stop_strings: entry
                .stop_strings
                .iter()
                .map(|stop| (*stop).to_owned())
                .collect(),
            stop_token_ids: entry.stop_token_ids.to_vec(),
            deadline: Some(Instant::now() + Duration::from_secs(60)),
            ..GenerationOptions::default()
        };
        let result = match entry.prompt_format.render_explicit(&messages) {
            None => engine.generate_chat(&messages, None, &generation)?,
            Some(prompt) => {
                generation.add_bos = false;
                engine.generate_rendered(&prompt, &generation)?
            }
        };
        latencies.push(result.elapsed.as_secs_f64() * 1_000.0);
        if outputs.len() < PROMPTS.len() {
            outputs.push(json!({
                "request": PROMPTS[index % PROMPTS.len()],
                "output": result.text,
                "prompt_tokens": result.prompt_tokens,
                "generated_tokens": result.generated_tokens,
            }));
        }
    }

    latencies.sort_by(f64::total_cmp);
    let mean_ms = latencies.iter().sum::<f64>() / latencies.len() as f64;
    let p95_index = ((latencies.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(latencies.len() - 1);
    let model_bytes = fs::metadata(&arguments.model)?.len();
    let lora_bytes = arguments
        .lora
        .as_deref()
        .map(fs::metadata)
        .transpose()?
        .map_or(0, |metadata| metadata.len());
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema": 1,
            "profile": arguments.profile,
            "platform": platform.name(),
            "prompt_profile": arguments.prompt_profile,
            "system_prompt_sha256": hex::encode(Sha256::digest(system_prompt.as_bytes())),
            "model_path": arguments.model,
            "lora_path": arguments.lora,
            "artifact_bytes": model_bytes.saturating_add(lora_bytes),
            "load_ms": load_ms,
            "runs": arguments.runs,
            "mean_generation_ms": mean_ms,
            "p95_generation_ms": latencies[p95_index],
            "resident_bytes_after_runs": resident_bytes(),
            "outputs": outputs,
            "note": "Performance only; run the execution-grounded evaluation separately for pass@1.",
        }))?
    );
    Ok(())
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut values = env::args().skip(1);
    let model = values.next().map(PathBuf::from).ok_or_else(usage)?;
    if !model.is_absolute() {
        return Err("MODEL must be an absolute path".into());
    }
    let mut profile = None;
    let mut lora = None;
    let mut runs = 20_usize;
    let mut prompt_profile = "production".to_owned();
    while let Some(option) = values.next() {
        match option.as_str() {
            "--profile" => {
                profile = Some(
                    values
                        .next()
                        .ok_or_else(usage)?
                        .parse::<ModelProfile>()
                        .map_err(|error| error.to_string())?,
                );
            }
            "--lora" => {
                let path = PathBuf::from(values.next().ok_or_else(usage)?);
                if !path.is_absolute() {
                    return Err("--lora must be an absolute path".into());
                }
                lora = Some(path);
            }
            "--runs" => {
                runs = values
                    .next()
                    .ok_or_else(usage)?
                    .parse::<usize>()
                    .map_err(|_| "--runs must be an integer between 1 and 1000".to_owned())?;
                if !(1..=1_000).contains(&runs) {
                    return Err("--runs must be between 1 and 1000".into());
                }
            }
            "--prompt-profile" => {
                prompt_profile = values.next().ok_or_else(usage)?;
                if !matches!(prompt_profile.as_str(), "production" | "training") {
                    return Err("--prompt-profile must be production or training".into());
                }
            }
            _ => return Err(usage()),
        }
    }
    let profile = profile.ok_or_else(usage)?;
    Ok(Arguments {
        model,
        profile,
        lora,
        runs,
        prompt_profile,
    })
}

fn usage() -> String {
    "usage: benchmark_embedded MODEL --profile <legacy|compact|balanced|accurate> [--lora PATH] [--runs N] [--prompt-profile production|training]"
        .into()
}

#[cfg(target_os = "linux")]
fn resident_bytes() -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
        .and_then(|kib| kib.checked_mul(1_024))
}

#[cfg(target_os = "macos")]
fn resident_bytes() -> Option<u64> {
    let output = Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|kib| kib.checked_mul(1_024))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn resident_bytes() -> Option<u64> {
    None
}
