from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "import-model-catalog.py"
SPEC = importlib.util.spec_from_file_location("import_model_catalog", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
IMPORTER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = IMPORTER
SPEC.loader.exec_module(IMPORTER)


def _sha(character: str) -> str:
    return character * 64


def _revision(character: str) -> str:
    return character * 40


def _digest(label: str) -> str:
    return hashlib.sha256(label.encode()).hexdigest()


def _model(tier: str, index: int) -> dict[str, object]:
    policy = IMPORTER.POLICIES[tier]
    artifact_sha = _sha(str(index + 1))
    artifact_file = f"howto-{policy.model_id}-q4_k_m.gguf"
    adapter_revision = _revision(chr(ord("a") + index))
    reference = {
        "executable_pass_at_1": float(policy.reference_pass_at_1),
        "mean_latency_ms": float(policy.reference_latency_ms),
        "peak_gpu_gb": float(policy.reference_peak_gpu_gb),
        "tasks": 400,
        "environment": "Transformers BF16, RTX 5090",
        "provenance": {
            "repository_revision": adapter_revision,
            "evaluation": {
                "path": "reports/evaluation.json",
                "sha256": _sha(chr(ord("4") + index)),
            },
            "latency": {
                "path": "reports/latency.json",
                "sha256": _sha(chr(ord("7") + index)),
            },
        },
    }
    quality_evaluations = []
    performance_evaluations = []
    for target_index, target in enumerate(IMPORTER.PRODUCTION_TARGETS):
        target_os = target.split("-", 1)[0]
        quality_evaluations.append(
            {
                "target": target,
                "policy_id": IMPORTER.PROMPT_POLICY_IDS[target_os],
                "system_prompt_sha256": IMPORTER.SYSTEM_PROMPT_SHA256[target_os],
                "rendered_prompt_sha256": (
                    policy.rendered_production_prompt_sha256[
                        0 if target_os == "macos" else 1
                    ]
                ),
                "runtime_llama_cpp_revision": (
                    IMPORTER.EXPECTED_RUNTIME_LLAMA_CPP_REVISION
                ),
                "generation_policy_id": IMPORTER.GENERATION_POLICY_ID,
                "generation_policy_sha256": policy.generation_policy_sha256,
                "executable_pass_at_1": float(policy.reference_pass_at_1)
                + target_index * 0.01,
                "task_count": 400,
                "task_corpus_sha256": IMPORTER.QUALIFICATION_TASK_CORPUS_SHA256,
                "task_set_sha256": _digest(f"{tier}-held-out-task-set"),
                "split_policy_id": IMPORTER.QUALIFICATION_SPLIT_POLICY_ID,
                "split_policy_sha256": IMPORTER.QUALIFICATION_SPLIT_POLICY_SHA256,
                "harness_revision": _digest(f"{tier}-quality-harness"),
                "report_schema_version": IMPORTER.QUALITY_REPORT_SCHEMA_VERSION,
                "report_sha256": _digest(f"{tier}-{target}-quality"),
            }
        )
        performance_evaluations.append(
            {
                "target": target,
                "runtime_llama_cpp_revision": (
                    IMPORTER.EXPECTED_RUNTIME_LLAMA_CPP_REVISION
                ),
                "generation_policy_id": IMPORTER.GENERATION_POLICY_ID,
                "generation_policy_sha256": policy.generation_policy_sha256,
                "mean_latency_ms": 45.25 + index + target_index * 10,
                "peak_rss_bytes": 900_000_000 + index + target_index * 100_000_000,
                "platform": f"{target} fixture warm end-to-end",
                "harness_revision": _digest(f"{tier}-performance-harness"),
                "report_schema_version": IMPORTER.PERFORMANCE_REPORT_SCHEMA_VERSION,
                "report_sha256": _digest(f"{tier}-{target}-performance"),
            }
        )
    return {
        "id": policy.model_id,
        "tier": tier,
        "display_name": policy.display_name,
        "parameters_billions": float(policy.parameters_billions),
        "base_model_id": policy.base_model_id,
        "base_revision": _revision(chr(ord("d") + index)),
        "training_base_revision_provenance": {
            "source": "release_attestation",
            "evidence": f"qualification fixture {tier}",
        },
        "distribution_rights_evidence": f"rights fixture {tier}",
        "export_quantization": "Q4_K_M",
        "artifact_kind": "merged_model",
        "artifact_file": artifact_file,
        "artifact_size_bytes": 600_000_000 + index,
        "artifact_sha256": artifact_sha,
        "artifact_url": (
            f"https://downloads.example.invalid/{artifact_sha}/{artifact_file}"
        ),
        "installable": True,
        "recommended_ram_bytes": policy.minimum_ram_bytes,
        "reference_bf16_metrics": reference,
        "production_artifact_metrics": {
            "executable_pass_at_1": float(policy.reference_pass_at_1),
            "mean_latency_ms": 75.25 + index,
            "peak_rss_bytes": 1_200_000_000 + index,
            "platform": IMPORTER.PRODUCTION_AGGREGATE_PLATFORM,
            "qualification_report_sha256": _sha(chr(ord("a") + index)),
        },
        "production_quality_evaluations": quality_evaluations,
        "production_performance_evaluations": performance_evaluations,
        "prompt_format_id": policy.prompt_format_id,
        "rendered_prefix_sha256": policy.rendered_prefix_sha256,
        "stop_tokens": [
            {"token": token, "id": token_id}
            for token, token_id in zip(
                IMPORTER.STOP_TOKENS, policy.expected_stop_ids
            )
        ],
        "chat_template_sha256": _sha(chr(ord("d") + index)),
        "adapter_revision": adapter_revision,
        "llama_cpp_revision": _revision(chr(ord("1") + index)),
        "runtime_llama_cpp_revision": IMPORTER.EXPECTED_RUNTIME_LLAMA_CPP_REVISION,
        "manifest_sha256": _sha(chr(ord("a") + index)),
    }


def _catalog() -> dict[str, object]:
    return {
        "schema_version": 1,
        "qualified": True,
        "models": [
            _model(tier, index)
            for index, tier in enumerate(IMPORTER.MODEL_ORDER)
        ],
    }


def _load_fixture(root: Path, catalog: dict[str, object]):
    path = root / "model-catalog.json"
    path.write_text(json.dumps(catalog), encoding="utf-8")
    return path, IMPORTER.load_catalog(path)


class ImportCatalogTests(unittest.TestCase):
    def test_valid_catalog_renders_all_installable_profiles_deterministically(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            _, loaded = _load_fixture(Path(directory), _catalog())
            models = IMPORTER.validate_catalog(loaded)
            first = IMPORTER.render_catalog(
                models, IMPORTER.semantic_catalog_sha256(loaded)
            )

            _, loaded_again = _load_fixture(Path(directory), _catalog())
            second = IMPORTER.render_catalog(
                IMPORTER.validate_catalog(loaded_again),
                IMPORTER.semantic_catalog_sha256(loaded_again),
            )

            changed_catalog = _catalog()
            changed_catalog["models"][0]["distribution_rights_evidence"] += " amended"
            _, changed = _load_fixture(Path(directory), changed_catalog)
            changed_output = IMPORTER.render_catalog(
                IMPORTER.validate_catalog(changed),
                IMPORTER.semantic_catalog_sha256(changed),
            )

        self.assertEqual(first, second)
        self.assertNotEqual(first, changed_output)
        rendered = first.decode()
        self.assertEqual(rendered.count("release: Release::Published"), 3)
        self.assertEqual(rendered.count("production_benchmarks: Some"), 3)
        self.assertEqual(rendered.count("target: RuntimeTarget::"), 12)
        self.assertIn("peak_rss_bytes: 900_000_000", rendered)
        self.assertIn("recommended_ram_bytes: 1_610_612_736", rendered)
        self.assertIn("mean_latency_ms: 45.25", rendered)
        self.assertIn(
            f'artifact_id: "howto-qwen25-coder-0.5b-{_sha("1")}"', rendered
        )
        self.assertIn("generation_policy_sha256:", rendered)
        self.assertIn("quality_harness_revision:", rendered)
        self.assertIn(
            "runtime_llama_cpp_revision: Some(EMBEDDED_LLAMA_CPP_REVISION)",
            rendered,
        )
        self.assertNotIn("runtime_lora", rendered)

    def test_cli_writes_atomically_and_check_detects_drift(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            input_path, _ = _load_fixture(root, _catalog())
            output = root / "catalog.rs.inc"
            command = [sys.executable, str(SCRIPT), str(input_path), "--output", str(output)]

            created = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertEqual(created.returncode, 0, created.stderr)
            expected = output.read_bytes()

            checked = subprocess.run(
                [*command, "--check"], capture_output=True, text=True, check=False
            )
            self.assertEqual(checked.returncode, 0, checked.stderr)

            output.write_text("drift\n")
            drifted = subprocess.run(
                [*command, "--check"], capture_output=True, text=True, check=False
            )
            self.assertEqual(drifted.returncode, 1)
            self.assertIn("generated catalog drift", drifted.stderr)
            self.assertEqual(output.read_text(), "drift\n")

            repaired = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertEqual(repaired.returncode, 0, repaired.stderr)
            self.assertEqual(output.read_bytes(), expected)

            invalid = _catalog()
            invalid["qualified"] = False
            input_path.write_text(json.dumps(invalid))
            refused = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertEqual(refused.returncode, 1)
            self.assertEqual(output.read_bytes(), expected)

    def test_refuses_unqualified_mutable_incomplete_and_contract_drift(self) -> None:
        cases = []

        unqualified = _catalog()
        unqualified["qualified"] = False
        cases.append(("unqualified", unqualified))

        wrong_order = _catalog()
        wrong_order["models"][0], wrong_order["models"][1] = (
            wrong_order["models"][1],
            wrong_order["models"][0],
        )
        cases.append(("order", wrong_order))

        mutable = _catalog()
        model = mutable["models"][0]
        model["artifact_url"] = (
            "https://huggingface.co/example/model/resolve/main/"
            f"{model['artifact_sha256']}/{model['artifact_file']}"
        )
        cases.append(("mutable URL", mutable))

        traversal = _catalog()
        traversal["models"][1]["artifact_file"] = "../model.gguf"
        cases.append(("basename", traversal))

        incomplete = _catalog()
        del incomplete["models"][2]["distribution_rights_evidence"]
        cases.append(("incomplete", incomplete))

        invalid_checksum = _catalog()
        invalid_checksum["models"][0]["artifact_sha256"] = "A" * 64
        cases.append(("checksum", invalid_checksum))

        invalid_size = _catalog()
        invalid_size["models"][0]["artifact_size_bytes"] = 1
        cases.append(("size", invalid_size))

        invalid_ram = _catalog()
        invalid_ram["models"][1]["recommended_ram_bytes"] += 256 * 1024 * 1024
        cases.append(("RAM derivation", invalid_ram))

        ambiguous_latency = _catalog()
        ambiguous_latency["models"][0]["production_artifact_metrics"][
            "platform"
        ] = "macOS-arm64 fixture"
        cases.append(("latency methodology", ambiguous_latency))

        wrong_runtime = _catalog()
        wrong_runtime["models"][0]["runtime_llama_cpp_revision"] = _revision("0")
        cases.append(("embedded runtime revision", wrong_runtime))

        wrong_record_runtime = _catalog()
        wrong_record_runtime["models"][1]["production_performance_evaluations"][
            0
        ]["runtime_llama_cpp_revision"] = _revision("0")
        cases.append(("performance runtime revision", wrong_record_runtime))

        missing_platform = _catalog()
        missing_platform["models"][2]["production_quality_evaluations"].pop()
        cases.append(("both quality platforms", missing_platform))

        production_prompt_drift = _catalog()
        production_prompt_drift["models"][0]["production_quality_evaluations"][
            0
        ]["rendered_prompt_sha256"] = _sha("0")
        cases.append(("production prompt", production_prompt_drift))

        generation_policy_drift = _catalog()
        generation_policy_drift["models"][0]["production_quality_evaluations"][
            0
        ]["generation_policy_sha256"] = _sha("0")
        cases.append(("generation policy", generation_policy_drift))

        task_corpus_drift = _catalog()
        task_corpus_drift["models"][0]["production_quality_evaluations"][0][
            "task_corpus_sha256"
        ] = _sha("0")
        cases.append(("task corpus", task_corpus_drift))

        task_set_drift = _catalog()
        task_set_drift["models"][0]["production_quality_evaluations"][0][
            "task_set_sha256"
        ] = _sha("0")
        cases.append(("held-out task set", task_set_drift))

        split_policy_drift = _catalog()
        split_policy_drift["models"][0]["production_quality_evaluations"][0][
            "split_policy_id"
        ] = "howto.group-kfold.fold-1.v1"
        cases.append(("split policy", split_policy_drift))

        quality_harness_drift = _catalog()
        quality_harness_drift["models"][0]["production_quality_evaluations"][0][
            "harness_revision"
        ] = _revision("0")
        cases.append(("quality harness consistency", quality_harness_drift))

        quality_schema_drift = _catalog()
        quality_schema_drift["models"][0]["production_quality_evaluations"][0][
            "report_schema_version"
        ] = 2
        cases.append(("quality report schema", quality_schema_drift))

        performance_policy_drift = _catalog()
        performance_policy_drift["models"][1][
            "production_performance_evaluations"
        ][0]["generation_policy_id"] = "howto.embedded.command-generation.other.v1"
        cases.append(("performance generation policy", performance_policy_drift))

        performance_harness_drift = _catalog()
        performance_harness_drift["models"][1][
            "production_performance_evaluations"
        ][0]["harness_revision"] = _revision("0")
        cases.append(("performance harness consistency", performance_harness_drift))

        performance_schema_drift = _catalog()
        performance_schema_drift["models"][1][
            "production_performance_evaluations"
        ][0]["report_schema_version"] = True
        cases.append(("performance report schema", performance_schema_drift))

        aggregate_drift = _catalog()
        aggregate_drift["models"][1]["production_artifact_metrics"][
            "mean_latency_ms"
        ] += 1
        cases.append(("platform aggregate", aggregate_drift))

        incomplete_provenance = _catalog()
        del incomplete_provenance["models"][1]["reference_bf16_metrics"][
            "provenance"
        ]["evaluation"]["sha256"]
        cases.append(("provenance", incomplete_provenance))

        unpinned_url = _catalog()
        unpinned_url["models"][2]["artifact_url"] = (
            "https://downloads.example.invalid/releases/stable/"
            + unpinned_url["models"][2]["artifact_file"]
        )
        cases.append(("unpinned URL", unpinned_url))

        prompt_drift = _catalog()
        prompt_drift["models"][0]["prompt_format_id"] = "model_default"
        cases.append(("prompt", prompt_drift))

        stop_drift = _catalog()
        stop_drift["models"][1]["stop_tokens"][1]["id"] += 1
        cases.append(("stop", stop_drift))

        numeric_boolean = _catalog()
        numeric_boolean["models"][0]["installable"] = 1
        cases.append(("boolean", numeric_boolean))

        duplicate_artifact = _catalog()
        duplicate_artifact["models"][2]["artifact_sha256"] = duplicate_artifact[
            "models"
        ][1]["artifact_sha256"]
        duplicate_artifact["models"][2]["artifact_url"] = duplicate_artifact[
            "models"
        ][2]["artifact_url"].replace(
            _sha("3"), duplicate_artifact["models"][2]["artifact_sha256"]
        )
        cases.append(("unique", duplicate_artifact))

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for label, catalog in cases:
                with self.subTest(label=label):
                    _, loaded = _load_fixture(root, copy.deepcopy(catalog))
                    with self.assertRaises(IMPORTER.CatalogError):
                        IMPORTER.validate_catalog(loaded)

    def test_refuses_duplicate_json_keys_and_preserves_existing_output(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            input_path = root / "duplicate.json"
            input_path.write_text(
                '{"schema_version":1,"schema_version":1,"qualified":true,"models":[]}'
            )
            with self.assertRaisesRegex(IMPORTER.CatalogError, "duplicate JSON"):
                IMPORTER.load_catalog(input_path)

    def test_cli_refuses_to_follow_an_output_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            input_path, _ = _load_fixture(root, _catalog())
            target = root / "target.rs.inc"
            target.write_text("keep me\n")
            output = root / "catalog.rs.inc"
            output.symlink_to(target)

            result = subprocess.run(
                [sys.executable, str(SCRIPT), str(input_path), "--output", str(output)],
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(result.returncode, 1)
            self.assertIn("symlink output", result.stderr)
            self.assertTrue(output.is_symlink())
            self.assertEqual(target.read_text(), "keep me\n")

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO support is required")
    def test_cli_refuses_a_non_regular_output_without_blocking(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            input_path, _ = _load_fixture(root, _catalog())
            output = root / "catalog.rs.inc"
            os.mkfifo(output)

            result = subprocess.run(
                [sys.executable, str(SCRIPT), str(input_path), "--output", str(output)],
                capture_output=True,
                text=True,
                check=False,
                timeout=5,
            )

            self.assertEqual(result.returncode, 1)
            self.assertIn("non-regular output", result.stderr)
            self.assertTrue(output.exists())


if __name__ == "__main__":
    unittest.main()
