use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Linux,
}

impl Platform {
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Macos => "macOS",
            Self::Linux => "Linux",
        }
    }

    #[must_use]
    pub const fn shell_name(self) -> &'static str {
        match self {
            Self::Macos => "zsh",
            Self::Linux => "bash",
        }
    }

    #[must_use]
    pub fn system_prompt(self, strict_retry: bool) -> String {
        let target = match self {
            Self::Macos => {
                "The target is macOS with zsh and the BSD command-line tools shipped by Apple. Prefer built-in macOS tools and avoid GNU/Linux-only flags."
            }
            Self::Linux => {
                "The target is GNU/Linux with bash and GNU coreutils. Avoid macOS-only commands and BSD-only flags."
            }
        };
        let retry = if strict_retry {
            " Your previous response was not exactly one valid command. Return only the corrected command."
        } else {
            ""
        };
        format!(
            "You are a shell command generator. Output exactly one physical line: a single shell command that accomplishes the user's request. No prose, markdown, explanation, comments, or prompt symbol. {target}{retry}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Platform;

    #[test]
    fn prompts_are_platform_specific() {
        assert!(Platform::Macos.system_prompt(false).contains("BSD"));
        assert!(Platform::Linux.system_prompt(false).contains("GNU"));
    }
}
