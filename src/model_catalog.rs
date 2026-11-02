use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::engine_runtime::PromptFormat;
use crate::platform::Platform;

/// llama.cpp commit vendored by the pinned `llama-cpp-2` runtime dependency.
///
/// Model qualification is rejected unless it used this exact inference core.
pub const EMBEDDED_LLAMA_CPP_REVISION: &str = "5f55650a78f92aff4d48d671423e888fac0469ff";

/// A stable, user-facing model tier stored in the HowTo configuration.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelProfile {
    /// The original nl2sh model, retained for configuration migration.
    #[default]
    Legacy,
    /// Lowest disk and memory use.
    Compact,
    /// Recommended quality/resource trade-off.
    Balanced,
    /// Highest measured command-generation quality.
    Accurate,
}

impl ModelProfile {
    pub const ALL: [Self; 4] = [Self::Legacy, Self::Compact, Self::Balanced, Self::Accurate];

    /// Profiles intended for the new setup chooser, in display order.
    pub const USER_CHOICES: [Self; 3] = [Self::Compact, Self::Balanced, Self::Accurate];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Compact => "compact",
            Self::Balanced => "balanced",
            Self::Accurate => "accurate",
        }
    }

    #[must_use]
    pub const fn entry(self) -> &'static ModelEntry {
        match self {
            Self::Legacy => &CATALOG[0],
            Self::Compact => &CATALOG[1],
            Self::Balanced => &CATALOG[2],
            Self::Accurate => &CATALOG[3],
        }
    }
}

impl fmt::Display for ModelProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ModelProfile {
    type Err = ParseModelProfileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "compact" => Ok(Self::Compact),
            "balanced" => Ok(Self::Balanced),
            "accurate" => Ok(Self::Accurate),
            _ => Err(ParseModelProfileError(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseModelProfileError(String);

impl fmt::Display for ParseModelProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown model profile {:?}; expected legacy, compact, balanced, or accurate",
            self.0
        )
    }
}

impl std::error::Error for ParseModelProfileError {}

/// A pinned, downloadable GGUF file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Artifact {
    pub filename: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size_bytes: u64,
}

/// Files needed to run an entry. `base` may itself be a merged tuned model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactFiles {
    pub base: Artifact,
    pub lora: Option<Artifact>,
}

/// Publication state is explicit so setup cannot accidentally install research artifacts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Release {
    Published(ArtifactFiles),
    Unpublished { reason: &'static str },
}

impl Release {
    #[must_use]
    pub const fn artifacts(self) -> Option<ArtifactFiles> {
        match self {
            Self::Published(files) => Some(files),
            Self::Unpublished { .. } => None,
        }
    }

    #[must_use]
    pub const fn unavailable_reason(self) -> Option<&'static str> {
        match self {
            Self::Published(_) => None,
            Self::Unpublished { reason } => Some(reason),
        }
    }
}

/// Reproducible benchmark data used for relative setup guidance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Benchmark {
    pub executable_passes: u16,
    pub task_count: u16,
    pub mean_latency_micros: u64,
    /// Peak accelerator memory in hundredths of a decimal GB, matching the source report.
    pub peak_accelerator_memory_centigb: u16,
    pub environment: &'static str,
    pub caveat: &'static str,
}

impl Benchmark {
    #[must_use]
    pub fn pass_rate_percent(self) -> f64 {
        f64::from(self.executable_passes) * 100.0 / f64::from(self.task_count)
    }
}

/// Measurements made against the exact downloadable GGUF artifact.
///
/// These are deliberately distinct from [`Benchmark`], whose numbers describe
/// the BF16 research run. Setup should prefer this record whenever it is
/// present because it reflects the model and runtime users actually install.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProductionBenchmark {
    pub target: RuntimeTarget,
    /// Fraction in the inclusive range 0.0 through 1.0.
    pub executable_pass_at_1: f64,
    pub task_count: u16,
    /// Qualified mean latency. `platform` records the measurement environment
    /// and whether the release qualification used warm/end-to-end requests.
    pub mean_latency_ms: f64,
    pub peak_rss_bytes: u64,
    pub recommended_ram_bytes: u64,
    pub platform: &'static str,
    pub prompt_policy_id: &'static str,
    pub system_prompt_sha256: &'static str,
    pub rendered_prompt_sha256: &'static str,
    /// Exact deterministic decoding policy shared by quality and performance runs.
    pub generation_policy_id: &'static str,
    pub generation_policy_sha256: &'static str,
    /// Immutable held-out evaluation corpus and split contract.
    pub task_corpus_sha256: &'static str,
    pub task_set_sha256: &'static str,
    pub split_policy_id: &'static str,
    pub split_policy_sha256: &'static str,
    /// Quality and performance use different harnesses, each pinned independently.
    pub quality_harness_revision: &'static str,
    pub performance_harness_revision: &'static str,
    pub quality_report_schema_version: u16,
    pub performance_report_schema_version: u16,
    pub quality_report_sha256: &'static str,
    pub performance_report_sha256: &'static str,
    pub qualification_report_sha256: &'static str,
    pub runtime_llama_cpp_revision: &'static str,
}

/// A native target with an independently qualified runtime measurement.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RuntimeTarget {
    #[serde(rename = "macos-arm64")]
    MacosArm64,
    #[serde(rename = "macos-x86_64")]
    MacosX86_64,
    #[serde(rename = "linux-arm64")]
    LinuxArm64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
}

impl RuntimeTarget {
    pub const ALL: [Self; 4] = [
        Self::MacosArm64,
        Self::MacosX86_64,
        Self::LinuxArm64,
        Self::LinuxX86_64,
    ];

    #[must_use]
    pub const fn platform(self) -> Platform {
        match self {
            Self::MacosArm64 | Self::MacosX86_64 => Platform::Macos,
            Self::LinuxArm64 | Self::LinuxX86_64 => Platform::Linux,
        }
    }

    #[must_use]
    pub const fn current() -> Option<Self> {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Self::MacosArm64)
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            Some(Self::MacosX86_64)
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Some(Self::LinuxArm64)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Self::LinuxX86_64)
        } else {
            None
        }
    }
}

/// Exact artifact qualification on every supported runtime platform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProductionBenchmarks {
    pub macos_arm64: ProductionBenchmark,
    pub macos_x86_64: ProductionBenchmark,
    pub linux_arm64: ProductionBenchmark,
    pub linux_x86_64: ProductionBenchmark,
}

impl ProductionBenchmarks {
    #[must_use]
    pub const fn for_target(self, target: RuntimeTarget) -> ProductionBenchmark {
        match target {
            RuntimeTarget::MacosArm64 => self.macos_arm64,
            RuntimeTarget::MacosX86_64 => self.macos_x86_64,
            RuntimeTarget::LinuxArm64 => self.linux_arm64,
            RuntimeTarget::LinuxX86_64 => self.linux_x86_64,
        }
    }

    #[must_use]
    pub const fn for_current_target(self) -> Option<ProductionBenchmark> {
        match RuntimeTarget::current() {
            Some(target) => Some(self.for_target(target)),
            None => None,
        }
    }

    /// Compatibility helper for callers transitioning to [`Self::for_current_target`].
    #[must_use]
    pub const fn for_platform(self, platform: Platform) -> ProductionBenchmark {
        match (platform, cfg!(target_arch = "aarch64")) {
            (Platform::Macos, true) => self.macos_arm64,
            (Platform::Macos, false) => self.macos_x86_64,
            (Platform::Linux, true) => self.linux_arm64,
            (Platform::Linux, false) => self.linux_x86_64,
        }
    }
}

/// License facts are kept separate from publication so a permissive base license cannot be
/// mistaken for approval to redistribute a tuned derivative.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LicenseMetadata {
    pub base_spdx: &'static str,
    pub adapter_spdx: Option<&'static str>,
    pub distribution_approved: bool,
    pub note: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelEntry {
    pub profile: ModelProfile,
    pub artifact_id: &'static str,
    pub display_name: &'static str,
    pub summary: &'static str,
    pub base_repository: &'static str,
    /// Exact training-base revision. `None` keeps an unqualified research
    /// profile from implying provenance that the training run did not record.
    pub base_revision: Option<&'static str>,
    pub parameter_count_millions: u32,
    /// Exact download size when published; provisional reference size otherwise.
    pub reference_disk_bytes: u64,
    /// Conservative setup guidance; final runtime qualification may revise it.
    pub recommended_ram_mib: Option<u32>,
    pub benchmark: Option<Benchmark>,
    pub production_benchmarks: Option<ProductionBenchmarks>,
    pub runtime_llama_cpp_revision: Option<&'static str>,
    pub prompt_format: PromptFormat,
    pub stop_strings: &'static [&'static str],
    pub stop_token_ids: &'static [i32],
    pub license: LicenseMetadata,
    pub release: Release,
}

impl ModelEntry {
    #[must_use]
    pub const fn is_installable(self) -> bool {
        matches!(self.release, Release::Published(_))
    }

    #[must_use]
    pub const fn artifacts(self) -> Option<ArtifactFiles> {
        self.release.artifacts()
    }
}

const UNPUBLISHED_REASON: &str =
    "awaiting redistribution clearance and qualification of immutable merged GGUF artifacts";

const LEGACY_ARTIFACT: Artifact = Artifact {
    filename: "nl2sh-1.5b-Q4_K_M.gguf",
    url: "https://huggingface.co/ThorOdinson246/nl2sh-1.5b-Q4_K_M/resolve/36a06980dccf66995c8544aa4e33bf060cb26299/nl2sh-1.5b-Q4_K_M.gguf",
    sha256: "6f8a17a11129a31074c944f4c2602453fafd9de43bdaeb1630a8f511ec820f71",
    size_bytes: 986_048_000,
};

const LEGACY_STOPS: &[&str] = &["\n"];
// The qualified deployment contract stops on Qwen's two end tokens. A
// newline is valid command content and must not truncate generation.
const QWEN_STOPS: &[&str] = &["<|endoftext|>", "<|im_end|>"];
const QWEN25_STOP_IDS: &[i32] = &[151_643, 151_645];
const QWEN35_STOP_IDS: &[i32] = &[248_044, 248_046];

const RESEARCH_CAVEAT: &str =
    "one 400-task Linux bash evaluation fold; latency and peak memory were measured in BF16 on an NVIDIA RTX 5090, not the final Mac/Linux GGUF runtime";

const FALLBACK_CATALOG: [ModelEntry; 4] = [
    ModelEntry {
        profile: ModelProfile::Legacy,
        artifact_id: "nl2sh-1.5b-q4-k-m",
        display_name: "Legacy — nl2sh 1.5B",
        summary: "Existing HowTo model retained for upgrades",
        base_repository: "ThorOdinson246/nl2sh-1.5b-Q4_K_M",
        base_revision: Some("36a06980dccf66995c8544aa4e33bf060cb26299"),
        parameter_count_millions: 1_500,
        reference_disk_bytes: LEGACY_ARTIFACT.size_bytes,
        recommended_ram_mib: None,
        benchmark: None,
        production_benchmarks: None,
        runtime_llama_cpp_revision: None,
        prompt_format: PromptFormat::ModelDefault,
        stop_strings: LEGACY_STOPS,
        stop_token_ids: &[],
        license: LicenseMetadata {
            base_spdx: "Apache-2.0",
            adapter_spdx: None,
            distribution_approved: true,
            note: "Published legacy artifact",
        },
        release: Release::Published(ArtifactFiles {
            base: LEGACY_ARTIFACT,
            lora: None,
        }),
    },
    ModelEntry {
        profile: ModelProfile::Compact,
        artifact_id: "howto-qwen25-coder-0.5b",
        display_name: "Compact — Qwen2.5-Coder 0.5B",
        summary: "Smallest download and memory footprint",
        base_repository: "Qwen/Qwen2.5-Coder-0.5B-Instruct",
        base_revision: None,
        parameter_count_millions: 500,
        reference_disk_bytes: 491_400_064,
        recommended_ram_mib: Some(1_536),
        benchmark: Some(Benchmark {
            executable_passes: 255,
            task_count: 400,
            mean_latency_micros: 86_400,
            peak_accelerator_memory_centigb: 114,
            environment: "BF16, NVIDIA RTX 5090, Linux bash evaluation",
            caveat: RESEARCH_CAVEAT,
        }),
        production_benchmarks: None,
        runtime_llama_cpp_revision: None,
        prompt_format: PromptFormat::QwenChatMl,
        stop_strings: QWEN_STOPS,
        stop_token_ids: QWEN25_STOP_IDS,
        license: LicenseMetadata {
            base_spdx: "Apache-2.0",
            adapter_spdx: None,
            distribution_approved: false,
            note: "Base is Apache-2.0; tuned-adapter training-data and redistribution clearance is pending",
        },
        release: Release::Unpublished {
            reason: UNPUBLISHED_REASON,
        },
    },
    ModelEntry {
        profile: ModelProfile::Balanced,
        artifact_id: "howto-qwen35-0.8b",
        display_name: "Balanced — Qwen3.5 0.8B",
        summary: "Recommended quality, speed, and memory balance",
        base_repository: "Qwen/Qwen3.5-0.8B",
        base_revision: None,
        parameter_count_millions: 800,
        reference_disk_bytes: 563_036_064,
        recommended_ram_mib: Some(2_048),
        benchmark: Some(Benchmark {
            executable_passes: 271,
            task_count: 400,
            mean_latency_micros: 180_000,
            peak_accelerator_memory_centigb: 162,
            environment: "BF16, NVIDIA RTX 5090, Linux bash evaluation",
            caveat: RESEARCH_CAVEAT,
        }),
        production_benchmarks: None,
        runtime_llama_cpp_revision: None,
        prompt_format: PromptFormat::Qwen35NoThinking,
        stop_strings: QWEN_STOPS,
        stop_token_ids: QWEN35_STOP_IDS,
        license: LicenseMetadata {
            base_spdx: "Apache-2.0",
            adapter_spdx: None,
            distribution_approved: false,
            note: "Base is Apache-2.0; tuned-adapter training-data and redistribution clearance is pending",
        },
        release: Release::Unpublished {
            reason: UNPUBLISHED_REASON,
        },
    },
    ModelEntry {
        profile: ModelProfile::Accurate,
        artifact_id: "howto-qwen35-2b",
        display_name: "Accurate — Qwen3.5 2B",
        summary: "Best measured command-generation quality",
        base_repository: "Qwen/Qwen3.5-2B",
        base_revision: None,
        parameter_count_millions: 2_000,
        reference_disk_bytes: 1_396_198_496,
        recommended_ram_mib: Some(4_096),
        benchmark: Some(Benchmark {
            executable_passes: 298,
            task_count: 400,
            mean_latency_micros: 181_800,
            peak_accelerator_memory_centigb: 396,
            environment: "BF16, NVIDIA RTX 5090, Linux bash evaluation",
            caveat: RESEARCH_CAVEAT,
        }),
        production_benchmarks: None,
        runtime_llama_cpp_revision: None,
        prompt_format: PromptFormat::Qwen35NoThinking,
        stop_strings: QWEN_STOPS,
        stop_token_ids: QWEN35_STOP_IDS,
        license: LicenseMetadata {
            base_spdx: "Apache-2.0",
            adapter_spdx: None,
            distribution_approved: false,
            note: "Base is Apache-2.0; tuned-adapter training-data and redistribution clearance is pending",
        },
        release: Release::Unpublished {
            reason: UNPUBLISHED_REASON,
        },
    },
];

// Publication is an explicit, reviewable hand-off from howto-modelling. The
// checked-in fragment is `None` until a fully qualified catalog is imported;
// it then contains exactly compact, balanced, and accurate in that order.
const IMPORTED_PROFILE_MODELS: Option<[ModelEntry; 3]> = include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/models/catalog.rs.inc"
));

const fn selected_profile_model(index: usize) -> ModelEntry {
    match IMPORTED_PROFILE_MODELS {
        Some(models) => models[index],
        None => FALLBACK_CATALOG[index + 1],
    }
}

pub static CATALOG: [ModelEntry; 4] = [
    FALLBACK_CATALOG[0],
    selected_profile_model(0),
    selected_profile_model(1),
    selected_profile_model(2),
];

#[must_use]
pub const fn catalog() -> &'static [ModelEntry; 4] {
    &CATALOG
}

pub fn installable_profiles() -> impl Iterator<Item = &'static ModelEntry> {
    CATALOG.iter().filter(|entry| entry.is_installable())
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::{catalog, installable_profiles, ModelProfile, Release, RuntimeTarget};
    use crate::embedded::{ChatMessage, ChatRole};

    #[test]
    fn profile_names_round_trip_through_serde_and_from_str() {
        for profile in ModelProfile::ALL {
            let json = serde_json::to_string(&profile).unwrap();
            assert_eq!(
                serde_json::from_str::<ModelProfile>(&json).unwrap(),
                profile
            );
            assert_eq!(profile.as_str().parse::<ModelProfile>().unwrap(), profile);
        }
    }

    #[test]
    fn profile_publication_is_all_or_nothing() {
        let imported = ModelProfile::Compact.entry().is_installable();
        for profile in ModelProfile::USER_CHOICES {
            let entry = profile.entry();
            assert_eq!(entry.is_installable(), imported);
            assert_eq!(entry.production_benchmarks.is_some(), imported);
            assert_eq!(entry.runtime_llama_cpp_revision.is_some(), imported);
            assert_eq!(entry.license.distribution_approved, imported);
            assert_eq!(entry.artifacts().is_some(), imported);
            assert_eq!(entry.release.unavailable_reason().is_none(), imported);
            if let Some(files) = entry.artifacts() {
                assert!(
                    entry.artifact_id.ends_with(files.base.sha256),
                    "imported artifact IDs must change whenever their bytes change"
                );
            }
        }
        let expected = if imported {
            ModelProfile::ALL.to_vec()
        } else {
            vec![ModelProfile::Legacy]
        };
        assert_eq!(
            installable_profiles()
                .map(|entry| entry.profile)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn published_artifacts_are_immutable_and_well_formed() {
        for entry in catalog() {
            let Release::Published(files) = entry.release else {
                continue;
            };
            for artifact in [Some(files.base), files.lora].into_iter().flatten() {
                assert!(artifact.filename.ends_with(".gguf"));
                assert!(artifact.size_bytes > 1_048_576);
                assert_eq!(artifact.sha256.len(), 64);
                assert!(artifact
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
                assert!(artifact.url.starts_with("https://"));
                assert!(artifact.url.ends_with(artifact.filename));
                for mutable in [
                    "/resolve/main/",
                    "/resolve/master/",
                    "/releases/latest/",
                    "/latest/",
                    "/refs/heads/",
                ] {
                    assert!(!artifact.url.contains(mutable));
                }
            }
        }
    }

    #[test]
    fn benchmark_counts_encode_the_reported_rates() {
        let rates = ModelProfile::USER_CHOICES.map(|profile| {
            profile
                .entry()
                .benchmark
                .expect("new profile has benchmark")
                .pass_rate_percent()
        });
        assert_eq!(rates, [63.75, 67.75, 74.5]);
    }

    #[test]
    fn qwen_profiles_do_not_truncate_commands_at_newlines() {
        for profile in ModelProfile::USER_CHOICES {
            assert_eq!(
                profile.entry().stop_strings,
                &["<|endoftext|>", "<|im_end|>"]
            );
        }
    }

    #[test]
    fn imported_qualification_covers_every_native_runtime_target() {
        for profile in ModelProfile::USER_CHOICES {
            let entry = profile.entry();
            let Some(benchmarks) = entry.production_benchmarks else {
                assert!(entry.runtime_llama_cpp_revision.is_none());
                continue;
            };
            assert_eq!(benchmarks.macos_arm64.target, RuntimeTarget::MacosArm64);
            assert_eq!(benchmarks.macos_x86_64.target, RuntimeTarget::MacosX86_64);
            assert_eq!(benchmarks.linux_arm64.target, RuntimeTarget::LinuxArm64);
            assert_eq!(benchmarks.linux_x86_64.target, RuntimeTarget::LinuxX86_64);
            assert_eq!(
                entry.runtime_llama_cpp_revision,
                Some(super::EMBEDDED_LLAMA_CPP_REVISION)
            );
            for target in RuntimeTarget::ALL {
                let benchmark = benchmarks.for_target(target);
                assert_eq!(
                    benchmark.runtime_llama_cpp_revision,
                    super::EMBEDDED_LLAMA_CPP_REVISION
                );
                assert_eq!(benchmark.quality_report_schema_version, 1);
                assert_eq!(benchmark.performance_report_schema_version, 1);
                for digest in [
                    benchmark.generation_policy_sha256,
                    benchmark.task_corpus_sha256,
                    benchmark.task_set_sha256,
                    benchmark.split_policy_sha256,
                    benchmark.quality_report_sha256,
                    benchmark.performance_report_sha256,
                    benchmark.qualification_report_sha256,
                ] {
                    assert_eq!(digest.len(), 64);
                    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
                }
                for revision in [
                    benchmark.quality_harness_revision,
                    benchmark.performance_harness_revision,
                ] {
                    assert!(matches!(revision.len(), 40 | 64));
                    assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
                }
            }
        }
    }

    #[test]
    fn imported_prompt_hashes_match_the_deployed_renderer() {
        for profile in ModelProfile::USER_CHOICES {
            let entry = profile.entry();
            let Some(benchmarks) = entry.production_benchmarks else {
                continue;
            };
            for target in RuntimeTarget::ALL {
                let platform = target.platform();
                let benchmark = benchmarks.for_target(target);
                let system_prompt = platform.system_prompt(false);
                assert_eq!(
                    hex::encode(Sha256::digest(system_prompt.as_bytes())),
                    benchmark.system_prompt_sha256
                );
                let messages = [
                    ChatMessage::new(ChatRole::System, system_prompt),
                    ChatMessage::new(ChatRole::User, "__HOWTO_USER_REQUEST__"),
                ];
                let rendered = entry
                    .prompt_format
                    .render_explicit(&messages)
                    .expect("published Qwen profiles have an explicit prompt renderer");
                assert_eq!(
                    hex::encode(Sha256::digest(rendered.as_bytes())),
                    benchmark.rendered_prompt_sha256
                );
            }
        }
    }
}
