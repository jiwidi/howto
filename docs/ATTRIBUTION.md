# Attribution and third-party notices

This document records the third-party work used by HowTo and the notices that
must travel with a source release, binary archive, Homebrew bottle, or other
redistribution. It complements [LICENSE](../LICENSE) and [NOTICE](../NOTICE); it
does not replace the license text for any component.

The inventory reflects the project as audited on 2026-08-10. Update it whenever
the model, inference runtime, copied upstream code, or dependency set changes.

## Component inventory

| Component | How HowTo uses it | License | Required treatment |
|---|---|---|---|
| [whatisit](https://github.com/ThorOdinson246/whatisit-nl2sh) | Reference design and adapted implementation or regression-test material | Apache-2.0 | Keep the Apache-2.0 license and relevant upstream notices; identify modified files when source is carried over. |
| [nl2sh-1.5b-Q4_K_M](https://huggingface.co/ThorOdinson246/nl2sh-1.5b-Q4_K_M) | Default local command-generation weights | Apache-2.0, as declared by the model author | Ship the Apache-2.0 license and model/provenance notices when the weights are bundled or redistributed. |
| [Qwen2.5-Coder-1.5B-Instruct](https://huggingface.co/Qwen/Qwen2.5-Coder-1.5B-Instruct) | Base model from which nl2sh-1.5b was fine-tuned | Apache-2.0; Copyright 2024 Alibaba Cloud | Retain the base-model copyright and Apache-2.0 terms. |
| [tldr-pages](https://github.com/tldr-pages/tldr) page content | Reported source of 23.1% of the fine-tuning rows | CC-BY-4.0; Copyright 2014-present the tldr-pages team and contributors | Credit the project and contributors, link the material and license, and state how the material was transformed. Do not imply endorsement. |
| [llama.cpp](https://github.com/ggml-org/llama.cpp) | Local GGUF inference runtime | MIT; Copyright 2023-2026 The ggml authors | A distribution that bundles the runtime must carry its copyright and MIT permission notice. |
| [Locked Rust packages](../THIRD_PARTY_LICENSES.txt) | Libraries compiled into or used to build HowTo | Package-specific; generated from `Cargo.lock` | Ship and keep the generated dependency license/notice bundle current with the lockfile. |

HowTo is independent of these projects. Their names are used only to describe
origin and compatibility; no endorsement is implied.

## Reference implementation: Apache-2.0

The reference source and its safety regression cases are licensed under the
[Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0). When HowTo
copies or closely adapts that material, Apache-2.0 section 4 requires the
distribution to:

1. provide recipients a copy of the Apache-2.0 license;
2. carry prominent change notices in modified source files;
3. retain relevant copyright, patent, trademark, and attribution notices; and
4. reproduce the relevant contents of the upstream `NOTICE` in a readable
   notice, source file, documentation, or normal third-party-notice display.

HowTo satisfies the project-level parts through [LICENSE](../LICENSE),
[NOTICE](../NOTICE), and this file. A source file copied from whatisit should
also contain a short notice such as `Modified for HowTo in 2026` rather than
relying on this project-level document alone.

The upstream copyright notice is:

> whatisit — Copyright 2026 Mukesh Poudel

## Default model and base model

The default model is a Q4_K_M GGUF made from a LoRA fine-tune of
Qwen2.5-Coder-1.5B-Instruct. Both the fine-tuned model metadata and the Qwen
base-model repository declare Apache-2.0. The Hugging Face fine-tune repository
contains its model card and GGUF file but no standalone `LICENSE` file, so a
HowTo distribution must not assume that downloading the GGUF also downloaded
the required license text.

The default artifact was independently checked against the host metadata on
2026-08-10:

| Field | Verified value |
|---|---|
| Immutable downloader revision | [`36a06980dccf66995c8544aa4e33bf060cb26299`](https://huggingface.co/ThorOdinson246/nl2sh-1.5b-Q4_K_M/tree/36a06980dccf66995c8544aa4e33bf060cb26299) |
| Later repository audit revision | [`b98a7eb2301790ce8cf18da1ce582914f8ab63da`](https://huggingface.co/ThorOdinson246/nl2sh-1.5b-Q4_K_M/tree/b98a7eb2301790ce8cf18da1ce582914f8ab63da) |
| Filename | `nl2sh-1.5b-Q4_K_M.gguf` |
| Size | 986,048,000 bytes |
| SHA-256 | `6f8a17a11129a31074c944f4c2602453fafd9de43bdaeb1630a8f511ec820f71` |

Both listed revisions expose the same verified filename, byte length, and
SHA-256. HowTo's built-in downloader uses the first immutable revision rather
than `resolve/main` and still verifies the size and digest before installation.

The reference project also offers an optional 3B model. HowTo does not use it
by default. If it is added, the artifact verified on 2026-08-10 was revision
[`77c35e8a114d1007069316bd19e5f6477786ac05`](https://huggingface.co/ThorOdinson246/nl2sh-3b-Q4_K_M/tree/77c35e8a114d1007069316bd19e5f6477786ac05),
1,929,902,464 bytes, with SHA-256
`e36f4e610276e2197f02be7b47bd871f47e29fde8d74b0387c90a5939130d884`.
Its base model is Qwen2.5-Coder-3B-Instruct and its model metadata also declares
Apache-2.0.

## Training-data attribution and provenance limits

The model author reports a 125,770-row training pool with these measured row
shares:

| Source | Share | Reported license |
|---|---:|---|
| Fig autocomplete specifications | 32.8% | MIT |
| tldr-pages page content | 23.1% | CC-BY-4.0 |
| NL2SH-ALFA training split | 18.0% | MIT |
| cli-commands-explained | 11.8% | CC0-1.0, declared but not independently verified |
| command-generation | 7.3% | Apache-2.0, declared but not independently verified |
| git-instruction | 7.1% | MIT, declared but not independently verified |

The author also reports that 5.67% is verbatim NL2Bash material arriving
through NL2SH-ALFA and that the relevant `data/bash` corpus is MIT-licensed.
Warp workflows are reported as unused.

The reference repository expressly omits its training and evaluation pipeline,
and three source-license claims are marked unverified by the model author.
HowTo therefore preserves those qualifications and does not present this
table as an independent provenance audit.

### CC-BY-4.0 notice for tldr-pages

The tldr-pages team licenses page content under
[Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/);
only its `scripts/` directory is MIT-licensed. Appropriate credit is:

> Includes material from the tldr-pages team and contributors,
> <https://github.com/tldr-pages/tldr>, licensed under CC-BY-4.0.

The upstream model author transformed tldr-derived command examples into
fine-tuning instruction pairs and then into model weights. HowTo uses or
redistributes those resulting weights without changing the weights. HowTo's
packaging, prompt, inference client, and safety layer are separate modifications
and do not imply endorsement by tldr-pages or its contributors.

## llama.cpp MIT notice

The following notice must accompany any HowTo artifact that bundles a
llama.cpp executable or substantial portion of llama.cpp. When Homebrew installs
llama.cpp as a separate dependency, its own keg carries the license, but keeping
this attribution in HowTo's documentation remains useful.

```text
MIT License

Copyright (c) 2023-2026 The ggml authors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Release checklist

Before publishing a source archive, GitHub release, or Homebrew bottle:

- include `LICENSE`, `NOTICE`, this document, and
  `THIRD_PARTY_LICENSES.txt` in installed and release documentation;
- pin and verify the exact model revision, byte size, and SHA-256;
- include the model and Qwen notices if the model is present in the artifact;
- include the full llama.cpp MIT notice if the runtime is present;
- preserve change notices on source or tests adapted from whatisit;
- regenerate and review `THIRD_PARTY_LICENSES.txt` from the exact `Cargo.lock`
  and run the freshness check;
- update this document if a model or runtime is replaced; and
- keep the upstream qualifications around unverified training-data licenses.

This inventory is a practical compliance record, not legal advice.
