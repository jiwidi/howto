#!/usr/bin/env python3
"""Import a qualified howto-modelling catalog into HowTo's built-in catalog.

This command is intentionally download-free. It treats the modelling catalog
as an untrusted release boundary, validates its complete schema and immutable
artifact references, and emits the Rust fragment compiled into HowTo.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from decimal import Decimal
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import tempfile
from typing import Any, NoReturn, Sequence
from urllib.parse import unquote, urlsplit


SCHEMA_VERSION = 1
MODEL_ORDER = ("compact", "balanced", "accurate")
REPO_ROOT = Path(__file__).resolve().parents[1]
OUTPUT_PATH = REPO_ROOT / "models" / "catalog.rs.inc"
RUST_CATALOG_SOURCE = REPO_ROOT / "src" / "model_catalog.rs"
EXPECTED_RUNTIME_LLAMA_CPP_REVISION = "5f55650a78f92aff4d48d671423e888fac0469ff"
MAX_INPUT_BYTES = 4 * 1024 * 1024
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
REVISION_RE = re.compile(r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")
FILENAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*\.gguf$")
SAFE_PLATFORM_RE = re.compile(r"^[ -~]{1,200}$")
RAM_QUANTUM = 256 * 1024 * 1024
MIB = 1024 * 1024
MAX_RECOMMENDED_RAM_BYTES = (2**32 - 1) * MIB
STOP_TOKENS = ("<|endoftext|>", "<|im_end|>")
GENERATION_POLICY_ID = "howto.embedded.command-generation.deterministic.v1"
QUALIFICATION_TASK_CORPUS_SHA256 = (
    "2457970a5bc708ffe642cf62ceb430e80c0493c78ab40d7caa4f71acd44fe204"
)
QUALIFICATION_SPLIT_POLICY_ID = "howto.group-kfold.fold-0.v1"
QUALIFICATION_SPLIT_POLICY_SHA256 = (
    "d23e9d22519f8d0aee184be49f179010e6db78d37889486f1a713e114367a684"
)
QUALITY_REPORT_SCHEMA_VERSION = 1
PERFORMANCE_REPORT_SCHEMA_VERSION = 1
PRODUCTION_TARGETS = (
    "macos-arm64",
    "macos-x86_64",
    "linux-arm64",
    "linux-x86_64",
)
PRODUCTION_AGGREGATE_PLATFORM = (
    "macos+linux arm64+x86_64 warm end-to-end worst-case aggregate"
)
PROMPT_POLICY_IDS = {
    "macos": "howto.command-generation.macos.non-retry.v1",
    "linux": "howto.command-generation.linux.non-retry.v1",
}
SYSTEM_PROMPT_SHA256 = {
    "macos": "a31c2650c55e6aad3c54d81fb7430601ee4dfd5a3f023ca31ae43e8c6d733ab8",
    "linux": "d17f6a13075035601f90dbd8d0943da39ae8fb5b328a4d7dee2b27e33b789cc9",
}

ROOT_KEYS = frozenset(("schema_version", "qualified", "models"))
MODEL_KEYS = frozenset(
    (
        "id",
        "tier",
        "display_name",
        "parameters_billions",
        "base_model_id",
        "base_revision",
        "training_base_revision_provenance",
        "distribution_rights_evidence",
        "export_quantization",
        "artifact_kind",
        "artifact_file",
        "artifact_size_bytes",
        "artifact_sha256",
        "artifact_url",
        "installable",
        "recommended_ram_bytes",
        "reference_bf16_metrics",
        "production_artifact_metrics",
        "production_quality_evaluations",
        "production_performance_evaluations",
        "prompt_format_id",
        "rendered_prefix_sha256",
        "stop_tokens",
        "chat_template_sha256",
        "adapter_revision",
        "llama_cpp_revision",
        "runtime_llama_cpp_revision",
        "manifest_sha256",
    )
)
REFERENCE_KEYS = frozenset(
    (
        "executable_pass_at_1",
        "mean_latency_ms",
        "peak_gpu_gb",
        "tasks",
        "environment",
        "provenance",
    )
)
PRODUCTION_KEYS = frozenset(
    (
        "executable_pass_at_1",
        "mean_latency_ms",
        "peak_rss_bytes",
        "platform",
        "qualification_report_sha256",
    )
)
QUALITY_KEYS = frozenset(
    (
        "target",
        "policy_id",
        "system_prompt_sha256",
        "rendered_prompt_sha256",
        "runtime_llama_cpp_revision",
        "generation_policy_id",
        "generation_policy_sha256",
        "executable_pass_at_1",
        "task_count",
        "task_corpus_sha256",
        "task_set_sha256",
        "split_policy_id",
        "split_policy_sha256",
        "harness_revision",
        "report_schema_version",
        "report_sha256",
    )
)
PERFORMANCE_KEYS = frozenset(
    (
        "target",
        "runtime_llama_cpp_revision",
        "generation_policy_id",
        "generation_policy_sha256",
        "mean_latency_ms",
        "peak_rss_bytes",
        "platform",
        "harness_revision",
        "report_schema_version",
        "report_sha256",
    )
)


class CatalogError(ValueError):
    """A safe refusal at the modelling-to-runtime publication boundary."""


@dataclass(frozen=True)
class TierPolicy:
    tier: str
    model_id: str
    display_name: str
    summary: str
    base_model_id: str
    parameters_billions: Decimal
    reference_pass_at_1: Decimal
    reference_latency_ms: Decimal
    reference_peak_gpu_gb: Decimal
    minimum_ram_bytes: int
    prompt_format_id: str
    prompt_format_variant: str
    rendered_prefix_sha256: str
    stop_ids_constant: str
    expected_stop_ids: tuple[int, int]
    rendered_production_prompt_sha256: tuple[str, str]
    generation_policy_sha256: str


POLICIES = {
    "compact": TierPolicy(
        tier="compact",
        model_id="qwen25-coder-0.5b",
        display_name="Compact — Qwen2.5 Coder 0.5B",
        summary="Smallest download and memory footprint",
        base_model_id="Qwen/Qwen2.5-Coder-0.5B-Instruct",
        parameters_billions=Decimal("0.5"),
        reference_pass_at_1=Decimal("0.6375"),
        reference_latency_ms=Decimal("86.4"),
        reference_peak_gpu_gb=Decimal("1.14"),
        minimum_ram_bytes=1_610_612_736,
        prompt_format_id="qwen_chatml_no_thinking",
        prompt_format_variant="PromptFormat::QwenChatMl",
        rendered_prefix_sha256=(
            "3c8662b24dbbc7cbf131e39538fa52fca4f848250a36088c64b62008dc655487"
        ),
        stop_ids_constant="QWEN25_STOP_IDS",
        expected_stop_ids=(151_643, 151_645),
        rendered_production_prompt_sha256=(
            "0690fa0611e4c849adabffbb82cd58c664589b6e63fe25c99177d43709c8b805",
            "f33c6f6813dd35e64fe88bac19f64da5136e96afdd0c777b9e8b22503ea19cd1",
        ),
        generation_policy_sha256=(
            "bf5cc2fc03580a33d0a71f19153a0f52ed35715adcddb1277e2326b77afbf35e"
        ),
    ),
    "balanced": TierPolicy(
        tier="balanced",
        model_id="qwen35-0.8b",
        display_name="Balanced — Qwen3.5 0.8B",
        summary="Recommended quality, speed, and memory balance",
        base_model_id="Qwen/Qwen3.5-0.8B",
        parameters_billions=Decimal("0.8"),
        reference_pass_at_1=Decimal("0.6775"),
        reference_latency_ms=Decimal("180.0"),
        reference_peak_gpu_gb=Decimal("1.62"),
        minimum_ram_bytes=2_147_483_648,
        prompt_format_id="qwen35_no_thinking",
        prompt_format_variant="PromptFormat::Qwen35NoThinking",
        rendered_prefix_sha256=(
            "85faff74b0cd134fac958e43cea0b514bb50b3170e9c95d2d0c355d578c77577"
        ),
        stop_ids_constant="QWEN35_STOP_IDS",
        expected_stop_ids=(248_044, 248_046),
        rendered_production_prompt_sha256=(
            "1ecf0eaf474f317d71648840c12295786f49612b6b3e266f2d5c2d1c0478045f",
            "fcd1737e211052f735b11045b503646c702567030b41ca263f18e51a54b54309",
        ),
        generation_policy_sha256=(
            "65b4d051714ee98d3c1e405af9e84b77230637e188fbe66695135ff0673a4660"
        ),
    ),
    "accurate": TierPolicy(
        tier="accurate",
        model_id="qwen35-2b",
        display_name="Accurate — Qwen3.5 2B",
        summary="Best measured command-generation quality",
        base_model_id="Qwen/Qwen3.5-2B",
        parameters_billions=Decimal("2.0"),
        reference_pass_at_1=Decimal("0.745"),
        reference_latency_ms=Decimal("181.8"),
        reference_peak_gpu_gb=Decimal("3.96"),
        minimum_ram_bytes=4_294_967_296,
        prompt_format_id="qwen35_no_thinking",
        prompt_format_variant="PromptFormat::Qwen35NoThinking",
        rendered_prefix_sha256=(
            "85faff74b0cd134fac958e43cea0b514bb50b3170e9c95d2d0c355d578c77577"
        ),
        stop_ids_constant="QWEN35_STOP_IDS",
        expected_stop_ids=(248_044, 248_046),
        rendered_production_prompt_sha256=(
            "1ecf0eaf474f317d71648840c12295786f49612b6b3e266f2d5c2d1c0478045f",
            "fcd1737e211052f735b11045b503646c702567030b41ca263f18e51a54b54309",
        ),
        generation_policy_sha256=(
            "65b4d051714ee98d3c1e405af9e84b77230637e188fbe66695135ff0673a4660"
        ),
    ),
}


def fail(message: str) -> NoReturn:
    raise CatalogError(message)


def reject_json_constant(value: str) -> NoReturn:
    fail(f"non-finite JSON number is forbidden: {value}")


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            fail(f"duplicate JSON object key: {key!r}")
        result[key] = value
    return result


def load_catalog(path: Path) -> dict[str, Any]:
    try:
        size = path.stat().st_size
        if size > MAX_INPUT_BYTES:
            fail(f"catalog is larger than {MAX_INPUT_BYTES} bytes")
        payload = path.read_bytes()
        text = payload.decode("utf-8")
        value = json.loads(
            text,
            parse_float=Decimal,
            parse_constant=reject_json_constant,
            object_pairs_hook=unique_object,
        )
    except CatalogError:
        raise
    except (OSError, UnicodeDecodeError, ValueError) as error:
        fail(f"cannot read catalog {path}: {error}")
    if not isinstance(value, dict):
        fail("catalog root must be a JSON object")
    return value


def semantic_catalog_sha256(value: Any) -> str:
    """Hash every parsed field without depending on JSON whitespace."""

    def normalize(item: Any) -> Any:
        if isinstance(item, Decimal):
            return {"$decimal": format(item.normalize(), "f")}
        if isinstance(item, dict):
            return {key: normalize(child) for key, child in item.items()}
        if isinstance(item, list):
            return [normalize(child) for child in item]
        if item is None or isinstance(item, (bool, int, str)):
            return item
        fail(f"catalog contains unsupported value type: {type(item).__name__}")

    canonical = json.dumps(
        normalize(value),
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def embedded_runtime_revision() -> str:
    """Read the runtime revision from the Rust source of truth."""

    try:
        source = RUST_CATALOG_SOURCE.read_text(encoding="utf-8")
    except OSError as error:
        fail(f"cannot read Rust runtime revision source {RUST_CATALOG_SOURCE}: {error}")
    matches = re.findall(
        r'pub const EMBEDDED_LLAMA_CPP_REVISION: &str\s*=\s*"([0-9a-f]{40})";',
        source,
    )
    if len(matches) != 1:
        fail("Rust source must define exactly one embedded llama.cpp revision")
    revision = matches[0]
    if revision != EXPECTED_RUNTIME_LLAMA_CPP_REVISION:
        fail(
            "Rust embedded llama.cpp revision changed without updating the qualified "
            f"runtime contract: expected {EXPECTED_RUNTIME_LLAMA_CPP_REVISION}, got {revision}"
        )
    return revision


def require_exact_keys(value: Any, expected: frozenset[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail(f"{label} must be an object")
    actual = frozenset(value)
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        fail(f"{label} fields are incomplete or unknown; missing={missing}, extra={extra}")
    return value


def require_string(value: Any, label: str, *, max_length: int = 2_000) -> str:
    if not isinstance(value, str) or not value.strip():
        fail(f"{label} must be a non-empty string")
    if len(value) > max_length or any(ord(character) < 32 for character in value):
        fail(f"{label} contains unsupported control characters or is too long")
    return value


def require_int(
    value: Any,
    label: str,
    *,
    minimum: int = 0,
    maximum: int | None = None,
) -> int:
    if (
        isinstance(value, bool)
        or not isinstance(value, int)
        or value < minimum
        or (maximum is not None and value > maximum)
    ):
        range_text = f">= {minimum}" if maximum is None else f"in [{minimum}, {maximum}]"
        fail(f"{label} must be an integer {range_text}")
    return value


def require_number(value: Any, label: str, *, positive: bool = True) -> Decimal:
    if isinstance(value, bool) or not isinstance(value, (int, Decimal)):
        fail(f"{label} must be a number")
    number = Decimal(value)
    if not number.is_finite() or (positive and number <= 0):
        fail(f"{label} must be a finite positive number")
    return number


def require_bounded_decimal(
    value: Any, label: str, *, maximum: Decimal, decimal_places: int = 6
) -> Decimal:
    number = require_number(value, label)
    if number > maximum or number.as_tuple().exponent < -decimal_places:
        fail(
            f"{label} must be <= {maximum} with at most {decimal_places} decimal places"
        )
    return number


def require_sha256(value: Any, label: str) -> str:
    if not isinstance(value, str) or SHA256_RE.fullmatch(value) is None:
        fail(f"{label} must be a lowercase 64-hex SHA-256")
    return value


def require_revision(value: Any, label: str) -> str:
    if not isinstance(value, str) or REVISION_RE.fullmatch(value) is None:
        fail(f"{label} must be a full lowercase 40- or 64-hex revision")
    return value


def validate_provenance(model: dict[str, Any], label: str) -> None:
    provenance = model["training_base_revision_provenance"]
    if not isinstance(provenance, dict):
        fail(f"{label}.training_base_revision_provenance must be an object")
    source = provenance.get("source")
    expected = {"source"} if source == "training_metadata" else {"source", "evidence"}
    if source not in ("training_metadata", "release_attestation"):
        fail(f"{label} has unsupported training-base provenance source")
    require_exact_keys(provenance, frozenset(expected), f"{label} training provenance")
    if source == "release_attestation":
        require_string(provenance["evidence"], f"{label} training provenance evidence")
    require_string(
        model["distribution_rights_evidence"],
        f"{label}.distribution_rights_evidence",
    )


def validate_reference_metrics(
    value: Any, policy: TierPolicy, adapter_revision: str, label: str
) -> dict[str, Any]:
    metrics = require_exact_keys(value, REFERENCE_KEYS, f"{label} reference metrics")
    expected_numbers = {
        "executable_pass_at_1": policy.reference_pass_at_1,
        "mean_latency_ms": policy.reference_latency_ms,
        "peak_gpu_gb": policy.reference_peak_gpu_gb,
    }
    for field, expected in expected_numbers.items():
        actual = require_number(metrics[field], f"{label} reference {field}")
        if actual != expected:
            fail(f"{label} reference {field} must be {expected}, got {actual}")
    if require_int(
        metrics["tasks"], f"{label} reference tasks", minimum=1, maximum=65_535
    ) != 400:
        fail(f"{label} reference task count must be 400")
    require_string(metrics["environment"], f"{label} reference environment")

    provenance = require_exact_keys(
        metrics["provenance"],
        frozenset(("repository_revision", "evaluation", "latency")),
        f"{label} metrics provenance",
    )
    revision = require_revision(
        provenance["repository_revision"], f"{label} metrics repository revision"
    )
    if revision != adapter_revision:
        fail(f"{label} metrics provenance must match adapter_revision")
    for name, expected_path in (
        ("evaluation", "reports/evaluation.json"),
        ("latency", "reports/latency.json"),
    ):
        report = require_exact_keys(
            provenance[name], frozenset(("path", "sha256")), f"{label} {name} report"
        )
        if report["path"] != expected_path:
            fail(f"{label} {name} report path must be {expected_path!r}")
        require_sha256(report["sha256"], f"{label} {name} report checksum")
    return metrics


def validate_production_metrics(value: Any, policy: TierPolicy, label: str) -> dict[str, Any]:
    metrics = require_exact_keys(value, PRODUCTION_KEYS, f"{label} production metrics")
    pass_at_1 = require_bounded_decimal(
        metrics["executable_pass_at_1"],
        f"{label} production executable_pass_at_1",
        maximum=Decimal("1"),
    )
    if pass_at_1 < policy.reference_pass_at_1 - Decimal("0.03"):
        fail(f"{label} production executable_pass_at_1 is outside its quality gate")
    require_bounded_decimal(
        metrics["mean_latency_ms"],
        f"{label} production mean_latency_ms",
        maximum=Decimal("3600000"),
    )
    require_int(
        metrics["peak_rss_bytes"],
        f"{label} production peak_rss_bytes",
        minimum=1,
        maximum=2**64 - 1,
    )
    platform = require_string(metrics["platform"], f"{label} production platform")
    if platform != PRODUCTION_AGGREGATE_PLATFORM:
        fail(
            f"{label} production platform must be the exact macOS+Linux aggregate label"
        )
    require_sha256(
        metrics["qualification_report_sha256"],
        f"{label} production qualification report checksum",
    )
    return metrics


def validate_quality_evaluations(
    value: Any, policy: TierPolicy, runtime_revision: str, label: str
) -> dict[str, dict[str, Any]]:
    if not isinstance(value, list) or len(value) != len(PRODUCTION_TARGETS):
        fail(f"{label} quality evaluations must contain all four runtime targets")
    targets = [record.get("target") if isinstance(record, dict) else None for record in value]
    if targets != list(PRODUCTION_TARGETS):
        fail(f"{label} quality evaluation order must be exactly {PRODUCTION_TARGETS}")

    validated: dict[str, dict[str, Any]] = {}
    for index, target in enumerate(PRODUCTION_TARGETS):
        target_os = target.split("-", 1)[0]
        rendered_hash = policy.rendered_production_prompt_sha256[
            0 if target_os == "macos" else 1
        ]
        record = require_exact_keys(
            value[index], QUALITY_KEYS, f"{label} {target} quality evaluation"
        )
        expected_strings = {
            "target": target,
            "policy_id": PROMPT_POLICY_IDS[target_os],
            "system_prompt_sha256": SYSTEM_PROMPT_SHA256[target_os],
            "rendered_prompt_sha256": rendered_hash,
            "runtime_llama_cpp_revision": runtime_revision,
            "generation_policy_id": GENERATION_POLICY_ID,
            "generation_policy_sha256": policy.generation_policy_sha256,
            "task_corpus_sha256": QUALIFICATION_TASK_CORPUS_SHA256,
            "split_policy_id": QUALIFICATION_SPLIT_POLICY_ID,
            "split_policy_sha256": QUALIFICATION_SPLIT_POLICY_SHA256,
        }
        for field, expected in expected_strings.items():
            if record[field] != expected:
                fail(f"{label} {target} quality {field} does not match runtime policy")
        pass_at_1 = require_bounded_decimal(
            record["executable_pass_at_1"],
            f"{label} {target} executable_pass_at_1",
            maximum=Decimal("1"),
        )
        if pass_at_1 < policy.reference_pass_at_1 - Decimal("0.03"):
            fail(f"{label} {target} executable_pass_at_1 is below its quality gate")
        if require_int(
            record["task_count"],
            f"{label} {target} quality task_count",
            minimum=1,
            maximum=65_535,
        ) != 400:
            fail(f"{label} {target} quality evaluation must contain 400 tasks")
        require_sha256(record["task_set_sha256"], f"{label} {target} task set")
        require_revision(
            record["harness_revision"], f"{label} {target} quality harness revision"
        )
        if require_int(
            record["report_schema_version"],
            f"{label} {target} quality report schema version",
            minimum=1,
        ) != QUALITY_REPORT_SCHEMA_VERSION:
            fail(
                f"{label} {target} quality report schema version must be "
                f"{QUALITY_REPORT_SCHEMA_VERSION}"
            )
        require_sha256(record["report_sha256"], f"{label} {target} quality report")
        validated[target] = record
    if len({record["report_sha256"] for record in validated.values()}) != 4:
        fail(f"{label} quality reports must have unique checksums")
    if len({record["task_set_sha256"] for record in validated.values()}) != 1:
        fail(f"{label} quality targets must use the same held-out task set")
    if len({record["harness_revision"] for record in validated.values()}) != 1:
        fail(f"{label} quality targets must use the same harness revision")
    return validated


def validate_performance_evaluations(
    value: Any, policy: TierPolicy, runtime_revision: str, label: str
) -> dict[str, dict[str, Any]]:
    if not isinstance(value, list) or len(value) != len(PRODUCTION_TARGETS):
        fail(f"{label} performance evaluations must contain all four runtime targets")
    targets = [record.get("target") if isinstance(record, dict) else None for record in value]
    if targets != list(PRODUCTION_TARGETS):
        fail(f"{label} performance evaluation order must be exactly {PRODUCTION_TARGETS}")

    validated: dict[str, dict[str, Any]] = {}
    for index, target in enumerate(PRODUCTION_TARGETS):
        record = require_exact_keys(
            value[index], PERFORMANCE_KEYS, f"{label} {target} performance evaluation"
        )
        if record["target"] != target:
            fail(f"{label} performance target must be {target}")
        if record["runtime_llama_cpp_revision"] != runtime_revision:
            fail(f"{label} {target} performance used a different llama.cpp runtime")
        if record["generation_policy_id"] != GENERATION_POLICY_ID:
            fail(f"{label} {target} performance used a different generation policy")
        if record["generation_policy_sha256"] != policy.generation_policy_sha256:
            fail(f"{label} {target} performance generation policy hash is incorrect")
        require_bounded_decimal(
            record["mean_latency_ms"],
            f"{label} {target} mean_latency_ms",
            maximum=Decimal("3600000"),
        )
        require_int(
            record["peak_rss_bytes"],
            f"{label} {target} peak_rss_bytes",
            minimum=1,
            maximum=2**64 - 1,
        )
        platform = require_string(record["platform"], f"{label} {target} platform")
        if SAFE_PLATFORM_RE.fullmatch(platform) is None:
            fail(f"{label} {target} performance platform must be printable ASCII")
        measurement = platform.casefold()
        if target not in measurement or "warm" not in measurement or not any(
            marker in measurement for marker in ("end-to-end", "end to end", "e2e")
        ):
            fail(
                f"{label} {target} platform must identify its target and a warm "
                "end-to-end measurement"
            )
        require_revision(
            record["harness_revision"],
            f"{label} {target} performance harness revision",
        )
        if require_int(
            record["report_schema_version"],
            f"{label} {target} performance report schema version",
            minimum=1,
        ) != PERFORMANCE_REPORT_SCHEMA_VERSION:
            fail(
                f"{label} {target} performance report schema version must be "
                f"{PERFORMANCE_REPORT_SCHEMA_VERSION}"
            )
        require_sha256(record["report_sha256"], f"{label} {target} performance report")
        validated[target] = record
    if len({record["report_sha256"] for record in validated.values()}) != 4:
        fail(f"{label} performance reports must have unique checksums")
    if len({record["harness_revision"] for record in validated.values()}) != 1:
        fail(f"{label} performance targets must use the same harness revision")
    return validated


def validate_production_aggregate(
    aggregate: dict[str, Any],
    quality: dict[str, dict[str, Any]],
    performance: dict[str, dict[str, Any]],
    label: str,
) -> None:
    expected_pass = min(
        Decimal(record["executable_pass_at_1"]) for record in quality.values()
    )
    expected_latency = max(
        Decimal(record["mean_latency_ms"]) for record in performance.values()
    )
    expected_rss = max(record["peak_rss_bytes"] for record in performance.values())
    if Decimal(aggregate["executable_pass_at_1"]) != expected_pass:
        fail(f"{label} aggregate pass@1 must be the worst platform result")
    if Decimal(aggregate["mean_latency_ms"]) != expected_latency:
        fail(f"{label} aggregate latency must be the worst platform result")
    if aggregate["peak_rss_bytes"] != expected_rss:
        fail(f"{label} aggregate peak RSS must be the worst platform result")


def validate_artifact_url(
    value: Any,
    *,
    filename: str,
    artifact_sha256: str,
    immutable_revisions: tuple[str, ...],
    label: str,
) -> str:
    url = require_string(value, f"{label}.artifact_url")
    try:
        parsed = urlsplit(url)
        hostname = parsed.hostname
        _ = parsed.port
    except ValueError:
        fail(f"{label}.artifact_url is malformed")
    if (
        parsed.scheme != "https"
        or not hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
    ):
        fail(f"{label}.artifact_url must be a credential-free HTTPS URL without query or fragment")
    try:
        decoded_path = unquote(parsed.path, errors="strict")
    except UnicodeDecodeError:
        fail(f"{label}.artifact_url contains invalid path encoding")
    path = PurePosixPath(decoded_path)
    if any(part in (".", "..") for part in decoded_path.split("/")):
        fail(f"{label}.artifact_url must not contain path traversal")
    if path.name != filename or not decoded_path.endswith(f"/{filename}"):
        fail(f"{label}.artifact_url basename must exactly match artifact_file")
    lowered = decoded_path.lower()
    mutable_markers = (
        "/resolve/main/",
        "/resolve/master/",
        "/raw/main/",
        "/raw/master/",
        "/releases/latest/",
        "/latest/",
        "/refs/heads/",
    )
    if any(marker in lowered for marker in mutable_markers):
        fail(f"{label}.artifact_url contains a mutable branch or latest-release reference")
    path_parts = set(path.parts)
    immutable_ids = {artifact_sha256, *immutable_revisions}
    if path_parts.isdisjoint(immutable_ids):
        fail(
            f"{label}.artifact_url must contain the artifact SHA-256 or an immutable "
            "40-/64-hex revision as a complete path component"
        )
    return url


def validate_model(
    model: Any, policy: TierPolicy, index: int, runtime_revision: str
) -> dict[str, Any]:
    label = f"models[{index}] ({policy.tier})"
    model = dict(require_exact_keys(model, MODEL_KEYS, label))
    exact_values = {
        "tier": policy.tier,
        "id": policy.model_id,
        "display_name": policy.display_name,
        "base_model_id": policy.base_model_id,
        "export_quantization": "Q4_K_M",
        "artifact_kind": "merged_model",
        "prompt_format_id": policy.prompt_format_id,
        "rendered_prefix_sha256": policy.rendered_prefix_sha256,
    }
    for field, expected in exact_values.items():
        if model[field] != expected:
            fail(f"{label}.{field} must be {expected!r}, got {model[field]!r}")
    parameters = require_number(
        model["parameters_billions"], f"{label}.parameters_billions"
    )
    if parameters != policy.parameters_billions:
        fail(f"{label}.parameters_billions does not match the deployment tier")
    if model["installable"] is not True:
        fail(f"{label}.installable must be true")

    base_revision = require_revision(model["base_revision"], f"{label}.base_revision")
    adapter_revision = require_revision(
        model["adapter_revision"], f"{label}.adapter_revision"
    )
    llama_revision = require_revision(
        model["llama_cpp_revision"], f"{label}.llama_cpp_revision"
    )
    if model["runtime_llama_cpp_revision"] != runtime_revision:
        fail(f"{label}.runtime_llama_cpp_revision does not match the embedded runtime")
    validate_provenance(model, label)

    filename = require_string(model["artifact_file"], f"{label}.artifact_file")
    if FILENAME_RE.fullmatch(filename) is None or PurePosixPath(filename).name != filename:
        fail(f"{label}.artifact_file must be a safe .gguf basename")
    size = require_int(
        model["artifact_size_bytes"],
        f"{label}.artifact_size_bytes",
        minimum=1_048_577,
        maximum=2**64 - 1,
    )
    artifact_sha = require_sha256(model["artifact_sha256"], f"{label}.artifact_sha256")
    model["artifact_url"] = validate_artifact_url(
        model["artifact_url"],
        filename=filename,
        artifact_sha256=artifact_sha,
        immutable_revisions=(base_revision, adapter_revision, llama_revision),
        label=label,
    )
    recommended_ram = require_int(
        model["recommended_ram_bytes"],
        f"{label}.recommended_ram_bytes",
        minimum=1,
        maximum=MAX_RECOMMENDED_RAM_BYTES,
    )
    if recommended_ram % RAM_QUANTUM != 0:
        fail(f"{label}.recommended_ram_bytes must be rounded to 256 MiB")
    if recommended_ram < policy.minimum_ram_bytes:
        fail(f"{label}.recommended_ram_bytes is below the tier minimum")

    require_sha256(model["chat_template_sha256"], f"{label}.chat_template_sha256")
    require_sha256(model["manifest_sha256"], f"{label}.manifest_sha256")
    require_sha256(model["rendered_prefix_sha256"], f"{label}.rendered_prefix_sha256")
    reference = validate_reference_metrics(
        model["reference_bf16_metrics"], policy, adapter_revision, label
    )
    quality = validate_quality_evaluations(
        model["production_quality_evaluations"], policy, runtime_revision, label
    )
    performance = validate_performance_evaluations(
        model["production_performance_evaluations"], policy, runtime_revision, label
    )
    production = validate_production_metrics(
        model["production_artifact_metrics"], policy, label
    )
    validate_production_aggregate(production, quality, performance, label)
    expected_ram = platform_recommended_ram(policy, production["peak_rss_bytes"])
    if recommended_ram != expected_ram:
        fail(
            f"{label}.recommended_ram_bytes must be {expected_ram}: the larger of "
            "the tier minimum and measured peak RSS plus 25% headroom, rounded to 256 MiB"
        )

    stops = model["stop_tokens"]
    if not isinstance(stops, list) or len(stops) != 2:
        fail(f"{label}.stop_tokens must contain exactly two entries")
    actual_stops: list[tuple[str, int]] = []
    for stop_index, stop in enumerate(stops):
        stop = require_exact_keys(
            stop, frozenset(("token", "id")), f"{label}.stop_tokens[{stop_index}]"
        )
        token = require_string(stop["token"], f"{label} stop token")
        token_id = require_int(
            stop["id"], f"{label} stop token id", minimum=0, maximum=2**31 - 1
        )
        actual_stops.append((token, token_id))
    expected_stops = list(zip(STOP_TOKENS, policy.expected_stop_ids))
    if actual_stops != expected_stops:
        fail(f"{label}.stop_tokens do not match the pinned Qwen tokenizer contract")
    if len(set(actual_stops)) != 2:
        fail(f"{label}.stop_tokens must be unique")

    # Retain normalized values used by the renderer.
    model["_policy"] = policy
    model["_reference"] = reference
    model["_production"] = production
    model["_quality"] = quality
    model["_performance"] = performance
    model["_runtime_revision"] = runtime_revision
    model["_artifact_size"] = size
    model["_recommended_ram"] = recommended_ram
    return model


def validate_catalog(value: dict[str, Any]) -> list[dict[str, Any]]:
    root = require_exact_keys(value, ROOT_KEYS, "catalog")
    if require_int(root["schema_version"], "catalog.schema_version", minimum=1) != SCHEMA_VERSION:
        fail(f"catalog.schema_version must be exactly {SCHEMA_VERSION}")
    if root["qualified"] is not True:
        fail("catalog.qualified must be true")
    models = root["models"]
    if not isinstance(models, list) or len(models) != len(MODEL_ORDER):
        fail(f"catalog.models must contain exactly {MODEL_ORDER}")
    actual_order = [model.get("tier") if isinstance(model, dict) else None for model in models]
    if actual_order != list(MODEL_ORDER):
        fail(f"catalog model order must be exactly {MODEL_ORDER}, got {actual_order}")
    runtime_revision = embedded_runtime_revision()
    validated = [
        validate_model(model, POLICIES[tier], index, runtime_revision)
        for index, (tier, model) in enumerate(zip(MODEL_ORDER, models))
    ]
    unique_fields = (
        "id",
        "display_name",
        "base_model_id",
        "artifact_file",
        "artifact_sha256",
        "artifact_url",
        "manifest_sha256",
    )
    for field in unique_fields:
        values = [model[field] for model in validated]
        if len(set(values)) != len(values):
            fail(f"catalog models must have unique {field} values")
    report_hashes = [
        model["production_artifact_metrics"]["qualification_report_sha256"]
        for model in validated
    ]
    if len(set(report_hashes)) != len(report_hashes):
        fail("catalog models must have unique qualification report checksums")
    platform_report_hashes = [
        model[field][index]["report_sha256"]
        for model in validated
        for field in (
            "production_quality_evaluations",
            "production_performance_evaluations",
        )
        for index in range(len(PRODUCTION_TARGETS))
    ]
    if len(set(platform_report_hashes)) != len(platform_report_hashes):
        fail("catalog platform qualification reports must have unique checksums")
    return validated


def rust_string(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def rust_float(value: Any) -> str:
    decimal = Decimal(value)
    rendered = format(decimal, "f")
    if "." not in rendered:
        rendered += ".0"
    return rendered


def platform_recommended_ram(policy: TierPolicy, peak_rss_bytes: int) -> int:
    peak_with_headroom = (peak_rss_bytes * 5 + 3) // 4
    rounded_peak = (
        (peak_with_headroom + RAM_QUANTUM - 1) // RAM_QUANTUM
    ) * RAM_QUANTUM
    return max(policy.minimum_ram_bytes, rounded_peak)


def render_production_benchmark(model: dict[str, Any], target: str) -> str:
    policy: TierPolicy = model["_policy"]
    quality = model["_quality"][target]
    performance = model["_performance"][target]
    aggregate = model["_production"]
    target_variant = {
        "macos-arm64": "MacosArm64",
        "macos-x86_64": "MacosX86_64",
        "linux-arm64": "LinuxArm64",
        "linux-x86_64": "LinuxX86_64",
    }[target]
    recommended_ram = platform_recommended_ram(policy, performance["peak_rss_bytes"])
    qualification_report = rust_string(aggregate["qualification_report_sha256"])
    return f"""ProductionBenchmark {{
                target: RuntimeTarget::{target_variant},
                executable_pass_at_1: {rust_float(quality['executable_pass_at_1'])},
                task_count: {quality['task_count']:_},
                mean_latency_ms: {rust_float(performance['mean_latency_ms'])},
                peak_rss_bytes: {performance['peak_rss_bytes']:_},
                recommended_ram_bytes: {recommended_ram:_},
                platform: {rust_string(performance['platform'])},
                prompt_policy_id: {rust_string(quality['policy_id'])},
                system_prompt_sha256: {rust_string(quality['system_prompt_sha256'])},
                rendered_prompt_sha256: {rust_string(quality['rendered_prompt_sha256'])},
                generation_policy_id: {rust_string(quality['generation_policy_id'])},
                generation_policy_sha256: {rust_string(quality['generation_policy_sha256'])},
                task_corpus_sha256: {rust_string(quality['task_corpus_sha256'])},
                task_set_sha256: {rust_string(quality['task_set_sha256'])},
                split_policy_id: {rust_string(quality['split_policy_id'])},
                split_policy_sha256: {rust_string(quality['split_policy_sha256'])},
                quality_harness_revision: {rust_string(quality['harness_revision'])},
                performance_harness_revision: {rust_string(performance['harness_revision'])},
                quality_report_schema_version: {quality['report_schema_version']},
                performance_report_schema_version: {performance['report_schema_version']},
                quality_report_sha256: {rust_string(quality['report_sha256'])},
                performance_report_sha256: {rust_string(performance['report_sha256'])},
                qualification_report_sha256: {qualification_report},
                runtime_llama_cpp_revision: EMBEDDED_LLAMA_CPP_REVISION,
            }}"""


def render_model(model: dict[str, Any]) -> str:
    policy: TierPolicy = model["_policy"]
    reference: dict[str, Any] = model["_reference"]
    tasks = reference["tasks"]
    passes = int(Decimal(reference["executable_pass_at_1"]) * tasks)
    latency_micros = int(Decimal(reference["mean_latency_ms"]) * 1_000)
    peak_centigb = int(Decimal(reference["peak_gpu_gb"]) * 100)
    parameter_millions = int(Decimal(model["parameters_billions"]) * 1_000)
    recommended_ram = model["_recommended_ram"]
    recommended_ram_mib = (recommended_ram + MIB - 1) // MIB
    profile_variant = f"ModelProfile::{policy.tier.title()}"
    return f"""    ModelEntry {{
        profile: {profile_variant},
        artifact_id: {rust_string('howto-' + policy.model_id + '-' + model['artifact_sha256'])},
        display_name: {rust_string(model['display_name'])},
        summary: {rust_string(policy.summary)},
        base_repository: {rust_string(model['base_model_id'])},
        base_revision: Some({rust_string(model['base_revision'])}),
        parameter_count_millions: {parameter_millions:_},
        reference_disk_bytes: {model['_artifact_size']:_},
        recommended_ram_mib: Some({recommended_ram_mib:_}),
        benchmark: Some(Benchmark {{
            executable_passes: {passes:_},
            task_count: {tasks:_},
            mean_latency_micros: {latency_micros:_},
            peak_accelerator_memory_centigb: {peak_centigb:_},
            environment: {rust_string(reference['environment'])},
            caveat: RESEARCH_CAVEAT,
        }}),
        production_benchmarks: Some(ProductionBenchmarks {{
            macos_arm64: {render_production_benchmark(model, 'macos-arm64')},
            macos_x86_64: {render_production_benchmark(model, 'macos-x86_64')},
            linux_arm64: {render_production_benchmark(model, 'linux-arm64')},
            linux_x86_64: {render_production_benchmark(model, 'linux-x86_64')},
        }}),
        runtime_llama_cpp_revision: Some(EMBEDDED_LLAMA_CPP_REVISION),
        prompt_format: {policy.prompt_format_variant},
        stop_strings: QWEN_STOPS,
        stop_token_ids: {policy.stop_ids_constant},
        license: LicenseMetadata {{
            base_spdx: "Apache-2.0",
            adapter_spdx: None,
            distribution_approved: true,
            note: "Distribution rights approved by the qualified release catalog",
        }},
        release: Release::Published(ArtifactFiles {{
            base: Artifact {{
                filename: {rust_string(model['artifact_file'])},
                url: {rust_string(model['artifact_url'])},
                sha256: {rust_string(model['artifact_sha256'])},
                size_bytes: {model['_artifact_size']:_},
            }},
            lora: None,
        }}),
    }}"""


def render_catalog(models: list[dict[str, Any]], source_sha256: str) -> bytes:
    require_sha256(source_sha256, "semantic catalog checksum")
    entries = ",\n".join(render_model(model) for model in models)
    text = (
        "// Generated by scripts/import-model-catalog.py. Do not edit by hand.\n"
        "// Source contract: howto-modelling qualified model catalog schema 1.\n"
        f"// Semantic catalog SHA-256: {source_sha256}\n"
        "Some([\n"
        f"{entries},\n"
        "])\n"
    )
    return text.encode("utf-8")


def normalized_output_path(path: Path) -> Path:
    """Resolve the parent while deliberately preserving the final path component."""

    absolute = path.absolute()
    if absolute.name in ("", ".", ".."):
        fail(f"catalog output must name a file: {path}")
    return absolute.parent.resolve() / absolute.name


def validate_output_leaf(path: Path) -> None:
    """Use lstat semantics and accept only an absent or regular output leaf."""

    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return
    if stat.S_ISLNK(metadata.st_mode):
        fail(f"refusing to use symlink output: {path}")
    if not stat.S_ISREG(metadata.st_mode):
        fail(f"refusing to use non-regular output: {path}")


def fsync_directory(path: Path) -> None:
    """Persist the directory entry after an atomic replacement."""

    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    descriptor = os.open(path, flags)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_write(path: Path, payload: bytes) -> None:
    path = normalized_output_path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    validate_output_leaf(path)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
        fsync_directory(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


def parse_arguments(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="qualified model-catalog.json")
    parser.add_argument(
        "--output",
        type=Path,
        default=OUTPUT_PATH,
        help=f"generated Rust fragment (default: {OUTPUT_PATH})",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="validate input and fail if the checked-in fragment differs",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parse_arguments(argv)
    try:
        catalog = load_catalog(arguments.input)
        source_sha256 = semantic_catalog_sha256(catalog)
        models = validate_catalog(catalog)
        payload = render_catalog(models, source_sha256)
        output = normalized_output_path(arguments.output)
        validate_output_leaf(output)
        if arguments.check:
            try:
                current = output.read_bytes()
            except OSError as error:
                fail(f"cannot read generated catalog {output}: {error}")
            if current != payload:
                fail(
                    f"generated catalog drift: run {Path(__file__).name} "
                    f"{arguments.input} --output {arguments.output}"
                )
        else:
            try:
                current = output.read_bytes() if output.exists() else None
            except OSError as error:
                fail(f"cannot read generated catalog {output}: {error}")
            if current != payload:
                atomic_write(arguments.output, payload)
    except (CatalogError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"validated qualified model catalog: {arguments.input}")
    action = "checked" if arguments.check else "generated"
    print(f"{action} Rust catalog: {arguments.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
