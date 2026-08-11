// Portions of this safety policy were adapted from whatisit.
// Copyright 2026 Mukesh Poudel. Licensed under Apache-2.0.
// Modified for HowTo in 2026.

//! Conservative, syntax-aware safety checks for generated shell commands.
//!
//! This module is deliberately a policy gate, not a sandbox. A clean parse and
//! the absence of a finding only mean that no known rule matched. Commands with
//! syntax we cannot understand are never eligible for execution.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use tree_sitter::{Node, Parser};

const MAX_COMMAND_BYTES: usize = 4 * 1024;
const MAX_RECURSION: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    MacOs,
    Linux,
}

impl Platform {
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Risk {
    /// No rule matched. This is not a promise that the command is safe.
    NoKnownRisk,
    Caution,
    Danger,
    /// The command could not be understood well enough to make a decision.
    Unknown,
}

pub type Severity = Risk;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionGate {
    Confirm,
    ConfirmCaution,
    Blocked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Finding {
    pub rule_id: &'static str,
    pub severity: Severity,
    pub message: String,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assessment {
    pub risk: Risk,
    pub findings: Vec<Finding>,
}

impl Assessment {
    #[must_use]
    pub const fn execution_gate(&self) -> ExecutionGate {
        match self.risk {
            Risk::NoKnownRisk => ExecutionGate::Confirm,
            Risk::Caution => ExecutionGate::ConfirmCaution,
            Risk::Danger | Risk::Unknown => ExecutionGate::Blocked,
        }
    }

    #[must_use]
    pub const fn may_execute_after_confirmation(&self) -> bool {
        matches!(
            self.execution_gate(),
            ExecutionGate::Confirm | ExecutionGate::ConfirmCaution
        )
    }

    #[must_use]
    pub fn has_rule(&self, rule_id: &str) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.rule_id == rule_id)
    }
}

#[must_use]
pub fn assess(command: &str) -> Assessment {
    assess_for_platform(command, Platform::current())
}

#[must_use]
pub fn check(command: &str) -> Assessment {
    assess(command)
}

#[must_use]
pub fn check_for_platform(command: &str, platform: Platform) -> Assessment {
    assess_for_platform(command, platform)
}

#[must_use]
pub fn assess_for_platform(command: &str, platform: Platform) -> Assessment {
    assess_inner(command, platform, 0)
}

fn assess_inner(command: &str, platform: Platform, depth: usize) -> Assessment {
    let full_span = SourceSpan {
        start: 0,
        end: command.len(),
    };
    let mut checker = Checker::new(command, platform);

    if command.trim().is_empty() {
        checker.add(
            "syntax.empty",
            Risk::Unknown,
            "the generated command is empty",
            full_span,
        );
        return checker.finish();
    }
    if command.len() > MAX_COMMAND_BYTES {
        checker.add(
            "syntax.too_long",
            Risk::Unknown,
            format!("command exceeds the {MAX_COMMAND_BYTES}-byte safety limit"),
            full_span,
        );
        return checker.finish();
    }
    if command
        .bytes()
        .any(|byte| byte == b'\0' || byte == b'\r' || byte == b'\n' || byte == 0x1b)
    {
        checker.add(
            "syntax.control_character",
            Risk::Unknown,
            "command contains a newline, NUL, carriage return, or escape character",
            full_span,
        );
        return checker.finish();
    }
    if depth >= MAX_RECURSION {
        checker.add(
            "syntax.recursion_limit",
            Risk::Unknown,
            "nested shell evaluation exceeded the safety analysis limit",
            full_span,
        );
        return checker.finish();
    }

    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .is_err()
    {
        checker.add(
            "syntax.parser_unavailable",
            Risk::Unknown,
            "the shell parser could not be initialized",
            full_span,
        );
        return checker.finish();
    }
    let Some(tree) = parser.parse(command, None) else {
        checker.add(
            "syntax.parse_failed",
            Risk::Unknown,
            "the command could not be parsed",
            full_span,
        );
        return checker.finish();
    };
    let root = tree.root_node();
    if root.has_error() || contains_missing_or_error(root) {
        checker.add(
            "syntax.invalid",
            Risk::Unknown,
            "the command contains invalid or incomplete shell syntax",
            full_span,
        );
        return checker.finish();
    }

    checker.inspect_tree(root, depth);
    checker.inspect_whole_command();
    checker.finish()
}

struct Checker<'source> {
    source: &'source str,
    platform: Platform,
    findings: Vec<Finding>,
    seen: HashSet<(&'static str, usize, usize)>,
}

impl<'source> Checker<'source> {
    fn new(source: &'source str, platform: Platform) -> Self {
        Self {
            source,
            platform,
            findings: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn finish(self) -> Assessment {
        let risk = if self
            .findings
            .iter()
            .any(|finding| finding.severity == Risk::Danger)
        {
            Risk::Danger
        } else if self
            .findings
            .iter()
            .any(|finding| finding.severity == Risk::Unknown)
        {
            Risk::Unknown
        } else if self
            .findings
            .iter()
            .any(|finding| finding.severity == Risk::Caution)
        {
            Risk::Caution
        } else {
            Risk::NoKnownRisk
        };
        Assessment {
            risk,
            findings: self.findings,
        }
    }

    fn add(
        &mut self,
        rule_id: &'static str,
        severity: Severity,
        message: impl Into<String>,
        span: SourceSpan,
    ) {
        if self.seen.insert((rule_id, span.start, span.end)) {
            self.findings.push(Finding {
                rule_id,
                severity,
                message: message.into(),
                span,
            });
        }
    }

    fn inspect_tree(&mut self, root: Node<'_>, depth: usize) {
        self.visit(root, depth);
    }

    fn visit(&mut self, node: Node<'_>, depth: usize) {
        let span = span(node);
        match node.kind() {
            "command" => {
                if let Some(invocation) = self.invocation(node) {
                    self.inspect_invocation(&invocation, depth);
                } else {
                    self.add(
                        "syntax.dynamic_command",
                        Risk::Unknown,
                        "the executable name is dynamic and cannot be checked",
                        span,
                    );
                }
            }
            "variable_assignment" => self.inspect_variable_assignment(node),
            "file_redirect" => self.inspect_redirect(node),
            "heredoc_redirect" | "herestring_redirect" => self.add(
                "shell.inline_input",
                Risk::Caution,
                "inline shell input deserves extra review",
                span,
            ),
            "for_statement"
            | "c_style_for_statement"
            | "while_statement"
            | "if_statement"
            | "case_statement"
            | "function_definition"
            | "compound_statement" => self.add(
                "shell.control_flow",
                Risk::Caution,
                "compound shell control flow deserves extra review",
                span,
            ),
            "process_substitution" => self.add(
                "shell.process_substitution",
                Risk::Caution,
                "process substitution can hide additional command execution",
                span,
            ),
            "pipeline" => self.inspect_pipeline(node),
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.visit(child, depth);
        }
    }

    fn invocation(&self, node: Node<'_>) -> Option<Invocation> {
        let name_node = node.child_by_field_name("name")?;
        let name = ShellWord::new(self.text(name_node), span(name_node));
        let mut cursor = node.walk();
        let arguments = node
            .children_by_field_name("argument", &mut cursor)
            .map(|argument| ShellWord::new(self.text(argument), span(argument)))
            .collect();
        Some(Invocation {
            name,
            arguments,
            span: span(node),
        })
    }

    fn inspect_invocation(&mut self, invocation: &Invocation, depth: usize) {
        let Some(name) = invocation.name.literal.as_deref() else {
            self.add(
                "syntax.dynamic_command",
                Risk::Unknown,
                "the executable name contains an expansion",
                invocation.name.span,
            );
            return;
        };
        let executable = executable_basename(name).to_ascii_lowercase();
        self.inspect_platform_compatibility(&executable, invocation);

        match executable.as_str() {
            "sudo" | "doas" => {
                self.add(
                    "privilege.escalation",
                    Risk::Caution,
                    format!("{executable} crosses a privilege boundary"),
                    invocation.span,
                );
                match unwrap_command(&executable, &invocation.arguments) {
                    Some(nested) => self.inspect_invocation(&nested, depth + 1),
                    None => self.add(
                        "privilege.unresolved_command",
                        Risk::Unknown,
                        "the privileged command could not be determined",
                        invocation.span,
                    ),
                }
            }
            "su" => self.inspect_su(invocation, depth),
            "env" => self.inspect_env(invocation, depth),
            "command" if command_is_lookup(&invocation.arguments) => {}
            "nohup" | "nice" | "ionice" | "setsid" | "time" | "command" | "builtin" | "exec"
            | "stdbuf" => match unwrap_command(&executable, &invocation.arguments) {
                Some(nested) => self.inspect_invocation(&nested, depth + 1),
                None if !invocation.arguments.is_empty() => self.add(
                    "shell.unresolved_wrapper",
                    Risk::Unknown,
                    format!("the command wrapped by {executable} could not be determined"),
                    invocation.span,
                ),
                None => {}
            },
            "timeout" | "gtimeout" => self.inspect_wrapped_command(
                &executable,
                parse_timeout_wrapper(invocation),
                invocation,
                depth,
            ),
            "watch" => self.inspect_wrapped_command(
                &executable,
                parse_watch_wrapper(invocation),
                invocation,
                depth,
            ),
            "flock" => self.inspect_wrapped_command(
                &executable,
                parse_flock_wrapper(invocation),
                invocation,
                depth,
            ),
            "chrt" => {
                self.add(
                    "process.scheduling",
                    Risk::Caution,
                    "chrt changes process scheduling policy",
                    invocation.span,
                );
                self.inspect_wrapped_command(
                    &executable,
                    parse_chrt_wrapper(invocation),
                    invocation,
                    depth,
                );
            }
            "taskset" => {
                self.add(
                    "process.affinity",
                    Risk::Caution,
                    "taskset changes process CPU affinity",
                    invocation.span,
                );
                self.inspect_wrapped_command(
                    &executable,
                    parse_taskset_wrapper(invocation),
                    invocation,
                    depth,
                );
            }
            "chroot" => self.inspect_wrapped_command(
                &executable,
                parse_chroot_wrapper(invocation),
                invocation,
                depth,
            ),
            "arch" if self.platform == Platform::MacOs => self.inspect_wrapped_command(
                &executable,
                parse_arch_wrapper(invocation),
                invocation,
                depth,
            ),
            "caffeinate" if self.platform == Platform::MacOs => {
                self.add(
                    "process.power_assertion",
                    Risk::Caution,
                    "caffeinate changes system idle-sleep behavior",
                    invocation.span,
                );
                self.inspect_wrapped_command(
                    &executable,
                    parse_caffeinate_wrapper(invocation),
                    invocation,
                    depth,
                );
            }
            "unshare" | "nsenter" | "systemd-run" | "script" | "parallel" | "at" | "batch"
            | "xterm" | "gnome-terminal" | "konsole" | "screen" | "tmux" => {
                self.inspect_opaque_wrapper(&executable, invocation, depth);
            }
            "busybox" | "toybox" => {
                match invocation_from_words(&invocation.arguments, invocation.span) {
                    Some(nested) => self.inspect_invocation(&nested, depth + 1),
                    None => self.add(
                        "shell.unresolved_dispatcher",
                        Risk::Unknown,
                        format!("the {executable} applet could not be determined"),
                        invocation.span,
                    ),
                }
            }
            "xargs" => {
                self.add(
                    "shell.xargs",
                    Risk::Caution,
                    "xargs constructs command invocations from input",
                    invocation.span,
                );
                if let Some(nested) = unwrap_command("xargs", &invocation.arguments) {
                    self.inspect_invocation(&nested, depth + 1);
                } else if !invocation.arguments.is_empty() {
                    self.add(
                        "shell.unresolved_wrapper",
                        Risk::Unknown,
                        "the command constructed by xargs could not be determined",
                        invocation.span,
                    );
                }
            }
            "sh" | "bash" | "zsh" | "dash" | "ksh" => {
                self.inspect_shell_eval(&executable, invocation, depth);
            }
            "eval" => self.inspect_eval(invocation, depth),
            "trap" => self.inspect_trap(invocation, depth),
            "." | "source" => self.add(
                "shell.unanalysed_script",
                Risk::Unknown,
                "a sourced script cannot be inspected by the safety checker",
                invocation.span,
            ),
            "python" | "python3" | "perl" | "ruby" | "node" | "php" | "osascript" => {
                if is_interpreter_info_query(&executable, &invocation.arguments) {
                    // Explicitly non-executing informational modes are safe to inspect.
                } else if inline_interpreter_reverse_shell(&executable, invocation) {
                    self.add(
                        "network.reverse_shell",
                        Risk::Danger,
                        format!("inline {executable} code resembles a reverse shell"),
                        invocation.span,
                    );
                } else if has_any_literal(&invocation.arguments, &["-c", "-e", "--eval"])
                    || has_short_flag(&invocation.arguments, 'e')
                    || (executable == "perl" && has_short_flag(&invocation.arguments, 'E'))
                    || has_option_prefix(&invocation.arguments, &["--eval=", "--execute="])
                    || (executable == "node"
                        && (has_any_literal(&invocation.arguments, &["-r", "--require"])
                            || has_option_prefix(&invocation.arguments, &["--require=", "-r"])))
                    || (matches!(executable.as_str(), "python" | "python3")
                        && has_short_flag(&invocation.arguments, 'c'))
                {
                    self.add(
                        "shell.unanalysed_code",
                        Risk::Unknown,
                        format!("inline {executable} code cannot be checked as shell syntax"),
                        invocation.span,
                    );
                } else {
                    self.add(
                        "shell.unanalysed_script",
                        Risk::Unknown,
                        format!("input executed by {executable} cannot be inspected"),
                        invocation.span,
                    );
                }
            }
            "awk" | "gawk" | "mawk" | "nawk" => self.inspect_awk(invocation),
            "sed" | "gsed" => self.inspect_sed(invocation),
            "make" | "gmake" => {
                if !has_any_literal(&invocation.arguments, &["--version", "--help", "-h"]) {
                    self.add(
                        "shell.unanalysed_build",
                        Risk::Unknown,
                        "makefiles can execute commands and cannot be inspected by the safety checker",
                        invocation.span,
                    );
                }
            }
            "ssh" => self.inspect_ssh(invocation, depth),
            "rm" => self.inspect_rm(invocation),
            "rmdir" | "unlink" | "shred" => {
                self.inspect_delete_like(&executable, invocation);
            }
            "cp" | "mv" | "install" | "ln" => {
                self.inspect_copy_like(&executable, invocation);
            }
            "truncate" => self.inspect_truncate(invocation),
            "rsync" => self.inspect_rsync(invocation),
            "find" => self.inspect_find(invocation, depth),
            "dd" => self.inspect_dd(invocation),
            "mkfs" | "newfs" | "newfs_apfs" | "newfs_hfs" | "wipefs" | "sfdisk" | "cfdisk"
            | "parted" | "gpart" | "blkdiscard" | "sgdisk" | "cryptsetup" => {
                self.add(
                    "disk.destructive",
                    Risk::Danger,
                    format!("{executable} can overwrite disk or filesystem structures"),
                    invocation.span,
                );
            }
            "fdisk" => self.inspect_fdisk(invocation),
            name if name.starts_with("mkfs.") || name.starts_with("newfs_") => self.add(
                "disk.destructive",
                Risk::Danger,
                format!("{executable} can overwrite a filesystem"),
                invocation.span,
            ),
            "diskutil" => self.inspect_diskutil(invocation),
            "tee" => self.inspect_tee(invocation),
            "chmod" | "chown" | "chgrp" => self.inspect_permissions(&executable, invocation),
            "kill" | "pkill" | "killall" | "xkill" => self.inspect_process_kill(invocation),
            "shutdown" | "reboot" | "halt" | "poweroff" => self.add(
                "system.power",
                Risk::Danger,
                format!("{executable} interrupts the whole system"),
                invocation.span,
            ),
            "crontab" => self.inspect_crontab(invocation),
            "launchctl" => self.inspect_launchctl(invocation),
            "systemctl" => self.inspect_systemctl(invocation),
            "pfctl" | "iptables" | "ip6tables" | "nft" | "ufw" => {
                self.inspect_firewall(&executable, invocation);
            }
            "curl" | "wget" => self.inspect_downloader(&executable, invocation),
            "nc" | "netcat" | "ncat" | "socat" => self.inspect_network_tool(invocation),
            "security" => self.inspect_macos_security(invocation),
            "csrutil" | "spctl" => self.inspect_macos_protection(&executable, invocation),
            "tmutil" => self.inspect_tmutil(invocation),
            "fdesetup" => self.inspect_fdesetup(invocation),
            "nvram" => self.inspect_nvram(invocation),
            "defaults" => self.inspect_defaults(invocation),
            "git" => self.inspect_git(invocation),
            "docker" | "podman" => self.inspect_container_tool(&executable, invocation),
            "kubectl" | "oc" => self.inspect_cluster_tool(&executable, invocation),
            "terraform" | "tofu" => {
                let action = first_positional(&invocation.arguments).unwrap_or_default();
                if action == "destroy" {
                    self.add(
                        "infrastructure.destroy",
                        Risk::Danger,
                        format!("{executable} destroy can remove managed infrastructure"),
                        invocation.span,
                    );
                } else if action == "apply" {
                    self.add(
                        "infrastructure.apply",
                        Risk::Caution,
                        format!("{executable} apply changes managed infrastructure"),
                        invocation.span,
                    );
                }
            }
            "pulumi" => {
                if first_positional(&invocation.arguments).is_some_and(|arg| arg == "destroy") {
                    self.add(
                        "infrastructure.destroy",
                        Risk::Danger,
                        "pulumi destroy removes managed infrastructure",
                        invocation.span,
                    );
                }
            }
            "mysql" | "psql" | "sqlite3" | "mysqladmin" | "redis-cli" | "mongosh" | "mongo"
            | "wp" => self.inspect_database(&executable, invocation),
            "aws" | "gcloud" | "az" | "gh" | "atlas" | "mongocli" => {
                self.inspect_cloud(&executable, invocation);
            }
            "journalctl" => self.inspect_journalctl(invocation),
            "unset" => self.inspect_history_control(&executable, invocation),
            "export" => {
                self.inspect_history_control(&executable, invocation);
                self.inspect_assignment_arguments(invocation);
            }
            "readonly" | "declare" | "typeset" | "local" | "let" => {
                self.inspect_assignment_arguments(invocation);
            }
            "vi" | "vim" | "nvim" | "view" => self.inspect_editor(&executable, invocation),
            "service" => self.inspect_service(invocation),
            "lvremove" | "vgremove" | "pvremove" | "zpool" | "zfs" | "mdadm" | "umount" => {
                self.inspect_storage(&executable, invocation);
            }
            "setenforce" => {
                if first_positional(&invocation.arguments) == Some("0") {
                    self.add(
                        "system.mandatory_access_control",
                        Risk::Danger,
                        "setenforce 0 disables SELinux enforcement",
                        invocation.span,
                    );
                }
            }
            "userdel" | "deluser" | "usermod" => {
                self.inspect_account(&executable, invocation);
            }
            "brew" | "apt" | "apt-get" | "dnf" | "yum" | "pacman" | "zypper" | "npm" | "pnpm"
            | "yarn" | "pip" | "pip3" | "pipx" | "cargo" | "gem" | "go" | "composer" | "bundle" => {
                self.inspect_package_manager(&executable, invocation);
            }
            _ => {}
        }

        self.inspect_sensitive_arguments(invocation);
    }

    fn inspect_rm(&mut self, invocation: &Invocation) {
        let recursive = has_short_flag(&invocation.arguments, 'r')
            || has_short_flag(&invocation.arguments, 'R')
            || has_literal(&invocation.arguments, "--recursive");
        let force = has_short_flag(&invocation.arguments, 'f')
            || has_literal(&invocation.arguments, "--force");
        let targets = positional_arguments(&invocation.arguments);

        if targets.is_empty() {
            self.add(
                "filesystem.delete",
                Risk::Caution,
                "rm permanently removes data",
                invocation.span,
            );
            return;
        }

        let history = targets
            .iter()
            .any(|target| target.literal.as_deref().is_some_and(is_shell_history_path));
        let scoped_cwd_cleanup =
            is_scoped_by_safe_cd(self.source, invocation.span.start, self.platform)
                && targets.iter().all(|target| is_cwd_wide_glob(target));
        let critical = history
            || targets.iter().any(|target| {
                is_critical_or_broad_target(target, self.platform)
                    && !(scoped_cwd_cleanup && is_cwd_wide_glob(target))
                    && (!is_specific_log_file(target) || recursive)
            });
        if critical {
            let qualifier = if force {
                if recursive {
                    "recursive force-delete"
                } else {
                    "force-delete"
                }
            } else if recursive {
                "recursive delete"
            } else {
                "delete"
            };
            self.add(
                "filesystem.critical_recursive_delete",
                Risk::Danger,
                format!("{qualifier} targets a critical, broad, or unresolved path"),
                invocation.span,
            );
        } else {
            self.add(
                "filesystem.delete",
                Risk::Caution,
                if recursive {
                    "recursive deletion permanently removes a directory tree"
                } else {
                    "rm permanently removes data"
                },
                invocation.span,
            );
        }
    }

    fn inspect_delete_like(&mut self, executable: &str, invocation: &Invocation) {
        let targets = positional_arguments(&invocation.arguments);
        let critical = targets
            .iter()
            .any(|target| is_critical_or_broad_target(target, self.platform));
        self.add(
            if critical {
                "filesystem.critical_delete"
            } else {
                "filesystem.delete"
            },
            if critical {
                Risk::Danger
            } else {
                Risk::Caution
            },
            if critical {
                format!("{executable} removes a critical, broad, or unresolved path")
            } else {
                format!("{executable} permanently removes data")
            },
            invocation.span,
        );
    }

    fn inspect_copy_like(&mut self, executable: &str, invocation: &Invocation) {
        let positional = positional_arguments(&invocation.arguments);
        let explicit_target = option_path(
            &invocation.arguments,
            &["-t", "--target-directory"],
            &["--target-directory="],
        );
        let destination =
            explicit_target.or_else(|| positional.last().map(|target| (*target).clone()));
        let critical_destination = destination
            .as_ref()
            .is_some_and(|target| is_critical_or_broad_target(target, self.platform));
        let critical_source = executable == "mv"
            && positional
                .iter()
                .any(|target| is_critical_or_broad_target(target, self.platform));

        self.add(
            if critical_destination || critical_source {
                "filesystem.critical_mutation"
            } else {
                "filesystem.mutation"
            },
            if critical_destination || critical_source {
                Risk::Danger
            } else {
                Risk::Caution
            },
            if critical_destination || critical_source {
                format!("{executable} can replace or move a critical or unresolved path")
            } else {
                format!("{executable} creates, replaces, or moves filesystem entries")
            },
            invocation.span,
        );
    }

    fn inspect_truncate(&mut self, invocation: &Invocation) {
        let target = positional_arguments(&invocation.arguments).last().copied();
        if target.is_some_and(is_documentation_placeholder) {
            self.add(
                "filesystem.dynamic_truncate",
                Risk::Unknown,
                "truncate target is a placeholder and cannot be checked",
                invocation.span,
            );
            return;
        }
        let critical = target.is_some_and(|path| is_critical_or_broad_target(path, self.platform));
        self.add(
            if critical {
                "filesystem.critical_truncate"
            } else {
                "filesystem.truncate"
            },
            if critical {
                Risk::Danger
            } else {
                Risk::Caution
            },
            if critical {
                "truncate can destroy the contents of a critical or unresolved file"
            } else {
                "truncate changes a file's length and may discard data"
            },
            invocation.span,
        );
    }

    fn inspect_rsync(&mut self, invocation: &Invocation) {
        let destination = positional_arguments(&invocation.arguments).last().copied();
        let deletes = has_literal(&invocation.arguments, "--delete")
            || invocation.arguments.iter().any(|argument| {
                argument
                    .literal
                    .as_deref()
                    .is_some_and(|value| value.starts_with("--delete-"))
            });
        let critical =
            destination.is_some_and(|path| is_critical_or_broad_target(path, self.platform));
        self.add(
            if critical {
                "filesystem.critical_sync"
            } else {
                "filesystem.sync"
            },
            if critical {
                Risk::Danger
            } else {
                Risk::Caution
            },
            if critical {
                "rsync can replace or delete data in a critical or unresolved destination"
            } else if deletes {
                "rsync --delete removes destination entries absent from the source"
            } else {
                "rsync changes files at its destination"
            },
            invocation.span,
        );
    }

    fn inspect_find(&mut self, invocation: &Invocation, depth: usize) {
        let roots = find_roots(&invocation.arguments);
        let critical_roots = roots
            .iter()
            .any(|root| is_critical_or_broad_target(root, self.platform));
        let narrowed = has_any_literal(
            &invocation.arguments,
            &["-name", "-iname", "-path", "-ipath", "-regex"],
        );
        let narrowed_log_cleanup = narrowed
            && !roots.is_empty()
            && roots
                .iter()
                .all(|root| root.literal.as_deref().is_some_and(is_log_subtree_path));
        let dangerous_roots =
            critical_roots && !(roots_have_dot(&roots) && narrowed) && !narrowed_log_cleanup;
        if has_literal(&invocation.arguments, "-delete") {
            let severity = if dangerous_roots {
                Risk::Danger
            } else {
                Risk::Caution
            };
            self.add(
                "filesystem.find_delete",
                severity,
                "find -delete permanently removes every match",
                invocation.span,
            );
        }

        for marker in ["-exec", "-execdir", "-ok", "-okdir"] {
            if let Some(index) = literal_position(&invocation.arguments, marker) {
                let nested_words = invocation.arguments[index + 1..]
                    .iter()
                    .take_while(|word| !matches!(word.literal.as_deref(), Some(";") | Some("+")))
                    .cloned()
                    .collect::<Vec<_>>();
                if let Some(nested) = invocation_from_words(&nested_words, invocation.span) {
                    self.add(
                        "shell.find_exec",
                        Risk::Caution,
                        "find executes a command for matching paths",
                        invocation.span,
                    );
                    self.inspect_invocation(&nested, depth + 1);
                    let scoped_permission_walk = roots_have_dot(&roots)
                        && nested.name.literal.as_deref().is_some_and(|name| {
                            matches!(
                                executable_basename(name).to_ascii_lowercase().as_str(),
                                "chmod" | "chown" | "chgrp"
                            )
                        });
                    let executes_shell = nested.name.literal.as_deref().is_some_and(|name| {
                        matches!(
                            executable_basename(name).to_ascii_lowercase().as_str(),
                            "sh" | "bash" | "zsh" | "dash" | "ksh"
                        )
                    });
                    if executes_shell {
                        self.add(
                            "shell.find_shell_escape",
                            Risk::Danger,
                            "find executes a shell for matching paths",
                            invocation.span,
                        );
                    } else if dangerous_roots
                        && invocation_is_mutating(&nested)
                        && !scoped_permission_walk
                    {
                        self.add(
                            "filesystem.find_critical_mutation",
                            Risk::Danger,
                            "find applies a mutating command across a critical or broad tree",
                            invocation.span,
                        );
                    }
                }
            }
        }
    }

    fn inspect_dd(&mut self, invocation: &Invocation) {
        let output = invocation.arguments.iter().find_map(|argument| {
            argument
                .literal
                .as_deref()
                .and_then(|value| value.strip_prefix("of="))
        });
        match output {
            Some(path)
                if is_block_device(path) || is_critical_absolute_path(path, self.platform) =>
            {
                self.add(
                    "disk.raw_write",
                    Risk::Danger,
                    format!("dd writes directly to critical target {path}"),
                    invocation.span,
                );
            }
            Some(_) => self.add(
                "filesystem.overwrite",
                Risk::Caution,
                "dd overwrites its output target without an atomic update",
                invocation.span,
            ),
            None if invocation
                .arguments
                .iter()
                .any(|argument| argument.raw.starts_with("of=")) =>
            {
                self.add(
                    "disk.dynamic_write",
                    Risk::Unknown,
                    "dd has a dynamic output target that cannot be checked",
                    invocation.span,
                )
            }
            None => {}
        }
    }

    fn inspect_diskutil(&mut self, invocation: &Invocation) {
        let dangerous = [
            "eraseDisk",
            "eraseVolume",
            "partitionDisk",
            "apfs",
            "deleteVolume",
            "secureErase",
            "zeroDisk",
            "randomDisk",
        ];
        if invocation.arguments.iter().any(|argument| {
            argument.literal.as_deref().is_some_and(|value| {
                dangerous
                    .iter()
                    .any(|item| value.eq_ignore_ascii_case(item))
            })
        }) {
            self.add(
                "disk.diskutil_destructive",
                Risk::Danger,
                "diskutil operation can erase or repartition a disk",
                invocation.span,
            );
        } else {
            self.add(
                "disk.diskutil",
                Risk::Caution,
                "diskutil can change disks and mounted volumes",
                invocation.span,
            );
        }
    }

    fn inspect_fdisk(&mut self, invocation: &Invocation) {
        let listing_only = !invocation.arguments.is_empty()
            && invocation.arguments.iter().all(|argument| {
                matches!(
                    argument.literal.as_deref(),
                    Some("-l" | "--list" | "-s" | "--getsz" | "-v" | "--version" | "-h" | "--help")
                ) || argument
                    .literal
                    .as_deref()
                    .is_some_and(|value| value.starts_with("/dev/"))
            })
            && has_any_literal(&invocation.arguments, &["-l", "--list", "-s", "--getsz"]);
        if !listing_only {
            self.add(
                "disk.destructive",
                Risk::Danger,
                "fdisk can overwrite disk partition structures",
                invocation.span,
            );
        }
    }

    fn inspect_tee(&mut self, invocation: &Invocation) {
        let appends = has_any_literal(&invocation.arguments, &["-a", "--append"]);
        for target in positional_arguments(&invocation.arguments) {
            match target.literal.as_deref() {
                Some(path)
                    if is_block_device(path)
                        || (is_critical_absolute_path(path, self.platform)
                            && !is_specific_log_path(path)) =>
                {
                    self.add(
                        "filesystem.critical_redirect",
                        Risk::Danger,
                        format!("tee writes to critical path {path}"),
                        target.span,
                    );
                }
                Some(path) if is_persistence_path(path) || is_shell_history_path(path) => self.add(
                    "persistence.redirect",
                    if appends { Risk::Caution } else { Risk::Danger },
                    format!("tee writes to startup or persistence path {path}"),
                    target.span,
                ),
                Some(_) => self.add(
                    "filesystem.write",
                    Risk::Caution,
                    "tee writes command input to a file",
                    target.span,
                ),
                None => self.add(
                    "filesystem.dynamic_redirect",
                    Risk::Unknown,
                    "tee has a dynamic output path that cannot be checked",
                    target.span,
                ),
            }
        }
    }

    fn inspect_permissions(&mut self, executable: &str, invocation: &Invocation) {
        let recursive = has_short_flag(&invocation.arguments, 'R')
            || has_literal(&invocation.arguments, "--recursive");
        let positional = positional_arguments(&invocation.arguments);
        let uses_reference = has_literal(&invocation.arguments, "--reference")
            || has_option_prefix(&invocation.arguments, &["--reference="]);
        let mode_or_owner = positional
            .first()
            .and_then(|argument| argument.literal.as_deref())
            .unwrap_or_default();
        let targets = if uses_reference {
            positional.last().copied().into_iter().collect::<Vec<_>>()
        } else {
            positional.iter().skip(1).copied().collect::<Vec<_>>()
        };
        let critical = targets
            .iter()
            .any(|target| is_critical_or_broad_target(target, self.platform));
        let exposes_private_key = executable == "chmod"
            && chmod_mode_exposes_group_or_other(mode_or_owner)
            && targets.iter().any(|target| {
                target
                    .literal
                    .as_deref()
                    .is_some_and(is_sensitive_private_key_path)
            });
        let setuid = executable == "chmod"
            && (mode_or_owner.contains("u+s")
                || mode_or_owner == "+s"
                || (mode_or_owner.len() == 4
                    && matches!(mode_or_owner.as_bytes().first(), Some(b'2' | b'4' | b'6'))));
        self.add(
            if critical || setuid || exposes_private_key {
                "filesystem.critical_permissions"
            } else {
                "filesystem.permissions"
            },
            if critical || setuid || exposes_private_key {
                Risk::Danger
            } else {
                Risk::Caution
            },
            if setuid {
                "chmod enables a set-user-ID or set-group-ID executable".to_owned()
            } else if exposes_private_key {
                "chmod exposes a private key to group or other users".to_owned()
            } else if recursive && critical {
                format!("{executable} recursively changes a critical or unresolved path")
            } else {
                format!("{executable} changes filesystem permissions or ownership")
            },
            invocation.span,
        );
    }

    fn inspect_process_kill(&mut self, invocation: &Invocation) {
        let catastrophic = (invocation
            .arguments
            .iter()
            .any(|argument| matches!(argument.literal.as_deref(), Some("-1") | Some("1")))
            && (has_literal(&invocation.arguments, "-9")
                || has_literal(&invocation.arguments, "-KILL")
                || has_literal(&invocation.arguments, "--signal=KILL")
                || option_value_is(&invocation.arguments, &["-s", "--signal"], "KILL")
                || has_option_prefix(&invocation.arguments, &["-sKILL"])))
            || invocation
                .arguments
                .iter()
                .any(|argument| argument.literal.as_deref().is_some_and(is_critical_service));
        self.add(
            if catastrophic {
                "process.system_kill"
            } else {
                "process.kill"
            },
            if catastrophic {
                Risk::Danger
            } else {
                Risk::Caution
            },
            if catastrophic {
                "the command can terminate every accessible process or the init process"
            } else {
                "the command terminates one or more processes"
            },
            invocation.span,
        );
    }

    fn inspect_crontab(&mut self, invocation: &Invocation) {
        self.add(
            if has_literal(&invocation.arguments, "-r") {
                "persistence.crontab_remove"
            } else {
                "persistence.crontab"
            },
            if has_literal(&invocation.arguments, "-r") {
                Risk::Danger
            } else {
                Risk::Caution
            },
            if has_literal(&invocation.arguments, "-r") {
                "crontab -r removes all scheduled jobs"
            } else {
                "crontab changes scheduled command execution"
            },
            invocation.span,
        );
    }

    fn inspect_launchctl(&mut self, invocation: &Invocation) {
        let action = first_positional(&invocation.arguments).unwrap_or_default();
        let dangerous = matches!(action, "bootout" | "disable" | "remove" | "unload")
            && invocation.arguments.iter().any(|argument| {
                argument.literal.as_deref().is_some_and(|value| {
                    value.contains("system") || value.contains("LaunchDaemons")
                })
            });
        self.add(
            "system.launchctl",
            if dangerous {
                Risk::Danger
            } else {
                Risk::Caution
            },
            "launchctl changes persistent services",
            invocation.span,
        );
    }

    fn inspect_systemctl(&mut self, invocation: &Invocation) {
        let action = first_positional(&invocation.arguments).unwrap_or_default();
        let critical_service = invocation
            .arguments
            .iter()
            .any(|argument| argument.literal.as_deref().is_some_and(is_critical_service));
        if matches!(
            action,
            "stop"
                | "disable"
                | "mask"
                | "restart"
                | "enable"
                | "daemon-reload"
                | "reboot"
                | "poweroff"
        ) {
            self.add(
                "system.systemctl",
                if matches!(action, "reboot" | "poweroff")
                    || (critical_service && matches!(action, "stop" | "disable" | "mask"))
                {
                    Risk::Danger
                } else {
                    Risk::Caution
                },
                format!("systemctl {action} changes system services or availability"),
                invocation.span,
            );
        }
    }

    fn inspect_firewall(&mut self, executable: &str, invocation: &Invocation) {
        let flush = has_any_literal(
            &invocation.arguments,
            &["-F", "-d", "--flush", "flush", "reset", "disable"],
        );
        let default_drop = invocation.arguments.windows(3).any(|window| {
            window[0]
                .literal
                .as_deref()
                .is_some_and(|value| matches!(value, "-P" | "--policy"))
                && window[1].literal.as_deref().is_some_and(|value| {
                    ["INPUT", "OUTPUT", "FORWARD"]
                        .iter()
                        .any(|chain| value.eq_ignore_ascii_case(chain))
                })
                && window[2].literal.as_deref().is_some_and(|value| {
                    ["DROP", "REJECT"]
                        .iter()
                        .any(|policy| value.eq_ignore_ascii_case(policy))
                })
        });
        self.add(
            "network.firewall",
            if flush || default_drop {
                Risk::Danger
            } else {
                Risk::Caution
            },
            format!("{executable} changes host firewall policy"),
            invocation.span,
        );
    }

    fn inspect_downloader(&mut self, executable: &str, invocation: &Invocation) {
        let (separate, assigned): (&[&str], &[&str]) = if executable == "curl" {
            (&["-o", "--output"], &["--output=", "-o"])
        } else {
            (&["-O", "--output-document"], &["--output-document=", "-O"])
        };
        if let Some(target) = option_path(&invocation.arguments, separate, assigned) {
            let critical = is_critical_or_broad_target(&target, self.platform)
                || target.literal.as_deref().is_some_and(is_persistence_path);
            self.add(
                if critical {
                    "network.critical_download"
                } else {
                    "network.download_write"
                },
                if critical {
                    Risk::Danger
                } else {
                    Risk::Caution
                },
                if critical {
                    "a remote download replaces a critical, persistent, or unresolved path"
                } else {
                    "a remote download writes directly to a local file"
                },
                invocation.span,
            );
        } else if invocation.arguments.iter().any(|argument| {
            argument.raw == "-o"
                || argument.raw == "--output"
                || argument.raw.starts_with("--output=")
                || argument.raw == "--output-document"
                || argument.raw.starts_with("--output-document=")
        }) {
            self.add(
                "network.dynamic_download",
                Risk::Unknown,
                "the download output path cannot be determined",
                invocation.span,
            );
        }
    }

    fn inspect_macos_protection(&mut self, executable: &str, invocation: &Invocation) {
        let disables = invocation.arguments.iter().any(|argument| {
            matches!(
                argument.literal.as_deref(),
                Some("disable" | "--disable" | "--master-disable")
            )
        });
        if disables {
            self.add(
                "system.macos_protection",
                Risk::Danger,
                format!("{executable} disables a macOS security control"),
                invocation.span,
            );
        } else {
            self.add(
                "system.macos_protection",
                Risk::Caution,
                format!("{executable} can change macOS security controls"),
                invocation.span,
            );
        }
    }

    fn inspect_tmutil(&mut self, invocation: &Invocation) {
        let destructive = invocation.arguments.iter().any(|argument| {
            matches!(
                argument.literal.as_deref(),
                Some("disable" | "deletelocalsnapshots" | "delete")
            )
        });
        if destructive {
            self.add(
                "system.macos_backups",
                Risk::Danger,
                "tmutil disables or deletes Time Machine backup protection",
                invocation.span,
            );
        }
    }

    fn inspect_fdesetup(&mut self, invocation: &Invocation) {
        if has_literal(&invocation.arguments, "disable") {
            self.add(
                "system.macos_encryption",
                Risk::Danger,
                "fdesetup disable turns off FileVault disk encryption",
                invocation.span,
            );
        } else if has_any_literal(
            &invocation.arguments,
            &["enable", "add", "remove", "changerecovery"],
        ) {
            self.add(
                "system.macos_encryption",
                Risk::Caution,
                "fdesetup changes FileVault users or recovery settings",
                invocation.span,
            );
        }
    }

    fn inspect_nvram(&mut self, invocation: &Invocation) {
        if has_literal(&invocation.arguments, "-c") {
            self.add(
                "system.macos_nvram",
                Risk::Danger,
                "nvram -c clears all firmware variables",
                invocation.span,
            );
        } else if has_literal(&invocation.arguments, "-d")
            || invocation
                .arguments
                .iter()
                .any(|argument| argument.raw.contains('='))
        {
            self.add(
                "system.macos_nvram",
                Risk::Caution,
                "nvram changes firmware variables",
                invocation.span,
            );
        }
    }

    fn inspect_defaults(&mut self, invocation: &Invocation) {
        if has_literal(&invocation.arguments, "delete") {
            self.add(
                "system.macos_defaults",
                Risk::Danger,
                "defaults delete removes a macOS preference domain or key",
                invocation.span,
            );
        } else if has_any_literal(&invocation.arguments, &["write", "rename"]) {
            self.add(
                "system.macos_defaults",
                Risk::Caution,
                "defaults changes macOS preferences",
                invocation.span,
            );
        }
    }

    fn inspect_network_tool(&mut self, invocation: &Invocation) {
        if has_any_literal(&invocation.arguments, &["-e", "--exec", "exec:"])
            || invocation.arguments.iter().any(|argument| {
                let lower = argument.raw.to_ascii_lowercase();
                lower.contains("/dev/tcp/")
                    || lower.starts_with("exec:")
                    || lower.contains(",exec:")
            })
        {
            self.add(
                "network.reverse_shell",
                Risk::Danger,
                "network tool executes a program over a socket",
                invocation.span,
            );
        } else {
            self.add(
                "network.raw_socket",
                Risk::Caution,
                "raw socket tools can transmit local data or expose a listener",
                invocation.span,
            );
        }
    }

    fn inspect_awk(&mut self, invocation: &Invocation) {
        if has_any_literal(&invocation.arguments, &["-f", "--file"])
            || has_option_prefix(&invocation.arguments, &["-f", "--file="])
        {
            self.add(
                "shell.unanalysed_script",
                Risk::Unknown,
                "an awk program file cannot be inspected by the safety checker",
                invocation.span,
            );
        } else if invocation.arguments.iter().any(|argument| {
            argument.literal.as_deref().is_some_and(|value| {
                let lower = value.to_ascii_lowercase();
                lower.contains("system(")
                    && ["/bin/sh", "/bin/bash", "/bin/zsh"]
                        .iter()
                        .any(|shell| lower.contains(shell))
            })
        }) {
            self.add(
                "shell.awk_shell_escape",
                Risk::Danger,
                "awk system() invokes a shell",
                invocation.span,
            );
        } else if invocation.arguments.iter().any(|argument| {
            argument
                .literal
                .as_deref()
                .is_some_and(|value| value.to_ascii_lowercase().contains("system("))
        }) {
            self.add(
                "shell.unanalysed_code",
                Risk::Unknown,
                "awk system() executes code that cannot be inspected as shell syntax",
                invocation.span,
            );
        }
    }

    fn inspect_sed(&mut self, invocation: &Invocation) {
        if has_any_literal(&invocation.arguments, &["-f", "--file"])
            || has_option_prefix(&invocation.arguments, &["-f", "--file="])
        {
            self.add(
                "shell.unanalysed_script",
                Risk::Unknown,
                "a sed program file cannot be inspected by the safety checker",
                invocation.span,
            );
        } else if invocation.arguments.iter().any(|argument| {
            argument
                .literal
                .as_deref()
                .is_some_and(sed_script_executes_command)
        }) {
            self.add(
                "shell.unanalysed_code",
                Risk::Unknown,
                "sed's execute extension runs code that cannot be inspected safely",
                invocation.span,
            );
        }
    }

    fn inspect_ssh(&mut self, invocation: &Invocation, depth: usize) {
        self.add(
            "network.remote_shell",
            Risk::Caution,
            "ssh opens a remote session or executes a command on another host",
            invocation.span,
        );
        let Some(host_index) = ssh_host_index(&invocation.arguments) else {
            self.add(
                "network.unresolved_remote",
                Risk::Unknown,
                "the ssh destination could not be determined",
                invocation.span,
            );
            return;
        };
        let payload = &invocation.arguments[host_index + 1..];
        if payload.is_empty() {
            return;
        }
        let Some(parts) = payload
            .iter()
            .map(|argument| argument.literal.as_deref())
            .collect::<Option<Vec<_>>>()
        else {
            self.add(
                "shell.dynamic_eval",
                Risk::Unknown,
                "the remote shell command is dynamic and cannot be checked",
                invocation.span,
            );
            return;
        };
        let source = parts.join(" ");
        self.merge_nested(
            &ShellWord {
                raw: source.clone(),
                literal: Some(source),
                span: invocation.span,
            },
            depth,
        );
    }

    fn inspect_macos_security(&mut self, invocation: &Invocation) {
        if invocation.arguments.iter().any(|argument| {
            matches!(
                argument.literal.as_deref(),
                Some(
                    "delete-keychain"
                        | "delete-generic-password"
                        | "delete-internet-password"
                        | "delete-certificate"
                        | "delete-identity"
                )
            )
        }) {
            self.add(
                "credentials.keychain_delete",
                Risk::Danger,
                "security command deletes keychain data or credentials",
                invocation.span,
            );
        } else if has_literal(&invocation.arguments, "-w")
            || has_literal(&invocation.arguments, "-g")
            || has_literal(&invocation.arguments, "find-generic-password")
        {
            self.add(
                "credentials.keychain_read",
                Risk::Caution,
                "security can reveal credentials from the macOS keychain",
                invocation.span,
            );
        }
    }

    fn inspect_git(&mut self, invocation: &Invocation) {
        let action = first_positional(&invocation.arguments).unwrap_or_default();
        if (action == "reset" && has_literal(&invocation.arguments, "--hard"))
            || (action == "clean"
                && (has_short_flag(&invocation.arguments, 'f')
                    || has_literal(&invocation.arguments, "--force")))
            || (action == "reflog" && has_literal(&invocation.arguments, "expire"))
            || (action == "gc"
                && has_option_prefix(&invocation.arguments, &["--prune=now", "--prune=all"]))
        {
            self.add(
                "vcs.destructive",
                Risk::Danger,
                "git operation can permanently discard uncommitted files or changes",
                invocation.span,
            );
        } else if action == "push" && has_any_literal(&invocation.arguments, &["--force", "-f"]) {
            self.add(
                "vcs.force_push",
                Risk::Danger,
                "force-push can replace remote history",
                invocation.span,
            );
        } else if action == "push" && has_literal(&invocation.arguments, "--force-with-lease") {
            self.add(
                "vcs.force_push_lease",
                Risk::Caution,
                "force-with-lease can replace remote history after verifying its expected state",
                invocation.span,
            );
        }
    }

    fn inspect_container_tool(&mut self, executable: &str, invocation: &Invocation) {
        let words = literal_values(&invocation.arguments);
        let text = invocation_text(invocation);
        let volume_delete = words
            .windows(2)
            .any(|pair| matches!(pair, ["volume", "rm"] | ["volume", "prune"]));
        let system_prune = words
            .windows(2)
            .any(|pair| matches!(pair, ["system", "prune"]));
        let removes_container = words.iter().any(|word| matches!(*word, "rm" | "rmi"));
        let dynamic_bulk_remove = removes_container
            && invocation
                .arguments
                .iter()
                .any(|argument| argument.literal.is_none())
            && !container_cleanup_is_filtered(&text);
        if volume_delete
            || (system_prune && has_literal(&invocation.arguments, "--volumes"))
            || dynamic_bulk_remove
        {
            self.add(
                "container.destructive",
                Risk::Danger,
                format!("{executable} operation permanently removes container data"),
                invocation.span,
            );
        } else if system_prune
            || removes_container
            || words.windows(2).any(|pair| pair == ["container", "prune"])
        {
            self.add(
                "container.cleanup",
                Risk::Caution,
                format!("{executable} operation removes containers or cached images"),
                invocation.span,
            );
        }
    }

    fn inspect_cluster_tool(&mut self, executable: &str, invocation: &Invocation) {
        if invocation
            .arguments
            .iter()
            .any(|argument| argument.literal.as_deref() == Some("delete"))
        {
            let broad = has_literal(&invocation.arguments, "--all")
                || invocation
                    .arguments
                    .iter()
                    .any(|argument| argument.literal.is_none())
                || invocation.arguments.iter().any(|argument| {
                    matches!(
                        argument.literal.as_deref(),
                        Some(
                            "namespace"
                                | "namespaces"
                                | "ns"
                                | "pvc"
                                | "persistentvolumeclaim"
                                | "persistentvolumeclaims"
                        )
                    )
                });
            self.add(
                "cluster.delete",
                if broad { Risk::Danger } else { Risk::Caution },
                format!("{executable} delete changes remote cluster state"),
                invocation.span,
            );
        }
    }

    fn inspect_package_manager(&mut self, executable: &str, invocation: &Invocation) {
        let removes = invocation.arguments.iter().any(|argument| {
            matches!(
                argument.literal.as_deref(),
                Some("uninstall" | "remove" | "purge" | "autoremove" | "-R")
            )
        });
        let installs_or_publishes = invocation.arguments.iter().any(|argument| {
            matches!(
                argument.literal.as_deref(),
                Some("install" | "add" | "ci" | "update" | "upgrade" | "publish" | "-S")
            )
        });
        let essential = invocation.arguments.iter().any(|argument| {
            matches!(
                argument.literal.as_deref(),
                Some(
                    "dpkg"
                        | "apt"
                        | "apt-get"
                        | "coreutils"
                        | "glibc"
                        | "libc6"
                        | "systemd"
                        | "bash"
                        | "sudo"
                        | "pacman"
                )
            )
        });
        if removes && essential {
            self.add(
                "package.essential_remove",
                Risk::Danger,
                format!("{executable} removes software required to operate or repair the system"),
                invocation.span,
            );
        } else if removes {
            self.add(
                "package.remove",
                Risk::Caution,
                format!("{executable} removes installed software"),
                invocation.span,
            );
        } else if installs_or_publishes {
            self.add(
                "package.install",
                Risk::Caution,
                format!(
                    "{executable} installs, updates, or publishes code and may run lifecycle scripts"
                ),
                invocation.span,
            );
        }
    }

    fn inspect_database(&mut self, executable: &str, invocation: &Invocation) {
        let text = invocation_text(invocation);
        let danger = text.contains("drop database")
            || text.contains("drop table")
            || text.contains("truncate table")
            || text.contains("flushall")
            || text.contains("flushdb")
            || text.contains("dropdatabase()")
            || (executable == "mysqladmin" && text.split_whitespace().any(|word| word == "drop"))
            || (executable == "wp" && (text.contains("db drop") || text.contains("site empty")));
        if danger {
            self.add(
                "database.destructive",
                Risk::Danger,
                format!("{executable} command can irreversibly remove database data"),
                invocation.span,
            );
        } else if text.contains("delete from") && !text.contains(" where ") {
            self.add(
                "database.unscoped_delete",
                Risk::Caution,
                "database DELETE has no visible WHERE clause",
                invocation.span,
            );
        }
    }

    fn inspect_cloud(&mut self, executable: &str, invocation: &Invocation) {
        let text = invocation_text(invocation);
        let dry_run = text.contains("--dryrun") || text.contains("--dry-run");
        let danger = match executable {
            "aws" => {
                (text.contains("s3 rm") && text.contains("--recursive") && !dry_run)
                    || (text.contains("s3 rb") && text.contains("--force"))
                    || text.contains("delete-table")
                    || text.contains("delete-db-instance")
                    || text.contains("terminate-instances")
            }
            "gcloud" => text.contains("projects delete"),
            "az" => text.contains("group delete"),
            "gh" => text.contains("repo delete"),
            "atlas" | "mongocli" => {
                text.contains("clusters delete") || text.contains("cluster delete")
            }
            _ => false,
        };
        if danger {
            self.add(
                "cloud.destructive",
                Risk::Danger,
                format!("{executable} command removes a broad or high-impact remote resource"),
                invocation.span,
            );
        } else if text
            .split_whitespace()
            .any(|word| matches!(word, "delete" | "remove" | "rm" | "terminate" | "destroy"))
        {
            self.add(
                "cloud.delete",
                Risk::Caution,
                format!("{executable} command deletes remote state"),
                invocation.span,
            );
        }
    }

    fn inspect_journalctl(&mut self, invocation: &Invocation) {
        let text = invocation_text(invocation);
        let aggressive_vacuum = ["--vacuum-time=1s", "--vacuum-size=0", "--rotate"]
            .iter()
            .any(|option| text.split_whitespace().any(|word| word == *option));
        if aggressive_vacuum {
            self.add(
                "logs.destructive_vacuum",
                Risk::Danger,
                "journalctl operation can immediately discard system logs",
                invocation.span,
            );
        }
    }

    fn inspect_history_control(&mut self, executable: &str, invocation: &Invocation) {
        let text = invocation_text(invocation);
        let disables_history = (executable == "unset"
            && text.split_whitespace().any(|word| word == "histfile"))
            || (executable == "export"
                && text
                    .split_whitespace()
                    .any(|word| matches!(word, "histsize=0" | "histfilesize=0")));
        if disables_history {
            self.add(
                "shell.history_disable",
                Risk::Danger,
                "command disables shell command-history recording",
                invocation.span,
            );
        }
    }

    fn inspect_editor(&mut self, executable: &str, invocation: &Invocation) {
        let mut shell_escape = false;
        let mut unanalysed_code = false;
        let mut index = 0;
        let mut after_options = false;
        while index < invocation.arguments.len() {
            let argument = &invocation.arguments[index];
            let Some(value) = argument.literal.as_deref() else {
                if !after_options {
                    unanalysed_code = true;
                }
                index += 1;
                continue;
            };
            if !after_options && value == "--" {
                after_options = true;
                index += 1;
                continue;
            }
            if !after_options && matches!(value, "-S" | "--source" | "-s") {
                unanalysed_code = true;
                index += 2;
                continue;
            }
            if !after_options
                && ((value.starts_with("-S") && value != "-S")
                    || value.starts_with("--source=")
                    || (value.starts_with("-s") && value != "-s"))
            {
                unanalysed_code = true;
                index += 1;
                continue;
            }
            if !after_options && matches!(value, "-u" | "-U") {
                let config = invocation.arguments.get(index + 1);
                if !config
                    .and_then(|config| config.literal.as_deref())
                    .is_some_and(is_builtin_editor_config)
                {
                    unanalysed_code = true;
                }
                index += 2;
                continue;
            }
            if !after_options
                && (value
                    .strip_prefix("-u")
                    .or_else(|| value.strip_prefix("-U")))
                .is_some_and(|config| !config.is_empty() && !is_builtin_editor_config(config))
            {
                unanalysed_code = true;
                index += 1;
                continue;
            }
            if !after_options && executable == "nvim" && value == "-l" {
                unanalysed_code = true;
                index += 2;
                continue;
            }
            if !after_options && executable == "nvim" && value.starts_with("-l") && value != "-l" {
                unanalysed_code = true;
                index += 1;
                continue;
            }
            let payload = if !after_options && matches!(value, "-c" | "--cmd") {
                unanalysed_code = true;
                index += 1;
                invocation
                    .arguments
                    .get(index)
                    .and_then(|payload| payload.literal.as_deref())
            } else if !after_options && value.starts_with("-c") && value != "-c" {
                unanalysed_code = true;
                Some(&value[2..])
            } else if !after_options {
                value.strip_prefix("--cmd=").or_else(|| {
                    value
                        .strip_prefix('+')
                        .filter(|payload| !payload.is_empty())
                })
            } else {
                None
            };
            if let Some(payload) = payload {
                unanalysed_code = true;
                let payload = payload.trim();
                if payload.contains('!')
                    || payload.eq_ignore_ascii_case(":shell")
                    || payload.eq_ignore_ascii_case("shell")
                {
                    shell_escape = true;
                }
            }
            index += 1;
        }
        if shell_escape {
            self.add(
                "privilege.editor_shell_escape",
                Risk::Danger,
                "editor command invokes an interactive shell escape",
                invocation.span,
            );
        } else if unanalysed_code {
            self.add(
                "shell.unanalysed_editor_code",
                Risk::Unknown,
                "editor command or script input can execute behavior the safety checker cannot inspect",
                invocation.span,
            );
        }
    }

    fn inspect_service(&mut self, invocation: &Invocation) {
        let words = literal_values(&invocation.arguments);
        let critical = words.iter().any(|word| is_critical_service(word));
        let mutates = words
            .iter()
            .any(|word| matches!(*word, "stop" | "disable" | "restart" | "start"));
        if mutates {
            self.add(
                "system.service",
                if critical {
                    Risk::Danger
                } else {
                    Risk::Caution
                },
                "service command changes system-service availability",
                invocation.span,
            );
        }
    }

    fn inspect_storage(&mut self, executable: &str, invocation: &Invocation) {
        let text = invocation_text(invocation);
        let danger = matches!(executable, "lvremove" | "vgremove" | "pvremove")
            || (matches!(executable, "zpool" | "zfs") && text.contains("destroy"))
            || (executable == "mdadm" && text.contains("--zero-superblock"))
            || (executable == "umount"
                && (has_literal(&invocation.arguments, "-a")
                    || first_positional(&invocation.arguments) == Some("/")));
        if danger {
            self.add(
                "storage.destructive",
                Risk::Danger,
                format!("{executable} command can tear down storage or filesystems"),
                invocation.span,
            );
        } else if executable == "umount" {
            self.add(
                "storage.unmount",
                Risk::Caution,
                "umount makes a filesystem unavailable",
                invocation.span,
            );
        }
    }

    fn inspect_account(&mut self, executable: &str, invocation: &Invocation) {
        let text = invocation_text(invocation);
        let danger = (executable == "userdel" && has_short_flag(&invocation.arguments, 'r'))
            || (executable == "deluser"
                && (text.contains("--remove-home") || text.contains("--remove-all-files")))
            || (executable == "usermod"
                && ["sudo", "wheel", "admin"]
                    .iter()
                    .any(|group| text.split([',', ' ']).any(|word| word == *group)));
        self.add(
            "account.mutation",
            if danger { Risk::Danger } else { Risk::Caution },
            format!("{executable} changes or removes a user account"),
            invocation.span,
        );
    }

    fn inspect_su(&mut self, invocation: &Invocation, depth: usize) {
        if invocation.arguments.len() == 1
            && invocation.arguments[0]
                .literal
                .as_deref()
                .is_some_and(|value| matches!(value, "--help" | "--version"))
        {
            return;
        }
        self.add(
            "privilege.escalation",
            Risk::Caution,
            "su crosses a privilege boundary",
            invocation.span,
        );
        for (index, argument) in invocation.arguments.iter().enumerate() {
            let Some(value) = argument.literal.as_deref() else {
                continue;
            };
            let payload = if matches!(value, "-c" | "--command") {
                invocation.arguments.get(index + 1).cloned()
            } else {
                value
                    .strip_prefix("--command=")
                    .or_else(|| {
                        value
                            .strip_prefix("-c")
                            .filter(|payload| !payload.is_empty())
                    })
                    .map(|payload| ShellWord {
                        raw: payload.to_owned(),
                        literal: Some(payload.to_owned()),
                        span: argument.span,
                    })
            };
            if let Some(payload) = payload {
                self.merge_nested(&payload, depth);
                return;
            }
        }
        self.add(
            "privilege.unresolved_command",
            Risk::Unknown,
            "su would start an interactive or unresolved privileged shell",
            invocation.span,
        );
    }

    fn inspect_env(&mut self, invocation: &Invocation, depth: usize) {
        let mut index = 0;
        let mut parsing_options = true;
        while let Some(argument) = invocation.arguments.get(index) {
            if parsing_options {
                if let Some(payload) = env_split_string_payload(argument) {
                    let (payload, tail_index) = match payload {
                        Some(payload) => (payload, index + 1),
                        None => {
                            let Some(payload) = invocation.arguments.get(index + 1).cloned() else {
                                self.add(
                                    "shell.unresolved_wrapper",
                                    Risk::Unknown,
                                    "env -S is missing its command string",
                                    invocation.span,
                                );
                                return;
                            };
                            (payload, index + 2)
                        }
                    };
                    self.add(
                        "shell.env_split_string",
                        Risk::Caution,
                        "env -S constructs a command from a string",
                        invocation.span,
                    );
                    let payload = append_shell_words(&payload, &invocation.arguments[tail_index..]);
                    self.merge_nested(&payload, depth);
                    return;
                }

                match argument.literal.as_deref() {
                    Some("--") => {
                        parsing_options = false;
                        index += 1;
                        continue;
                    }
                    Some("--help" | "--version") => return,
                    Some(value) if env_option_takes_separate_value(value) => {
                        if invocation.arguments.get(index + 1).is_none() {
                            self.add(
                                "shell.unresolved_wrapper",
                                Risk::Unknown,
                                format!("env option {value} is missing its value"),
                                invocation.span,
                            );
                            return;
                        }
                        index += 2;
                        continue;
                    }
                    Some(value) if is_env_flag_or_attached_option(value) => {
                        index += 1;
                        continue;
                    }
                    Some(value) if value.starts_with('-') && value != "-" => {
                        self.add(
                            "shell.unresolved_wrapper",
                            Risk::Unknown,
                            format!("env option {value} is not understood by the safety checker"),
                            invocation.span,
                        );
                        return;
                    }
                    None if argument.raw.starts_with('-') => {
                        self.add(
                            "shell.unresolved_wrapper",
                            Risk::Unknown,
                            "a dynamic env option cannot be checked",
                            invocation.span,
                        );
                        return;
                    }
                    _ => {}
                }
            }

            match parsed_assignment_name(argument) {
                ParsedAssignmentName::Known(name) => {
                    self.inspect_environment_name(&name, argument.span);
                    index += 1;
                }
                ParsedAssignmentName::Dynamic => {
                    self.add(
                        "shell.dynamic_environment_assignment",
                        Risk::Unknown,
                        "the environment variable name is dynamic and cannot be checked",
                        argument.span,
                    );
                    index += 1;
                }
                ParsedAssignmentName::NotAssignment => break,
            }
        }

        if let Some(nested) = invocation_from_words(&invocation.arguments[index..], invocation.span)
        {
            self.inspect_invocation(&nested, depth + 1);
        }
    }

    fn inspect_assignment_arguments(&mut self, invocation: &Invocation) {
        for argument in &invocation.arguments {
            match parsed_assignment_name(argument) {
                ParsedAssignmentName::Known(name) => {
                    self.inspect_environment_name(&name, argument.span);
                }
                ParsedAssignmentName::Dynamic => self.add(
                    "shell.dynamic_environment_assignment",
                    Risk::Unknown,
                    "the environment variable name is dynamic and cannot be checked",
                    argument.span,
                ),
                ParsedAssignmentName::NotAssignment => {}
            }
        }
    }

    fn inspect_environment_name(&mut self, name: &str, assignment_span: SourceSpan) {
        if let Some((rule, risk, message)) = risky_environment_name(name) {
            self.add(rule, risk, message, assignment_span);
        }
    }

    fn inspect_wrapped_command(
        &mut self,
        executable: &str,
        wrapped: WrappedCommand,
        invocation: &Invocation,
        depth: usize,
    ) {
        match wrapped {
            WrappedCommand::Direct(nested) => self.inspect_invocation(&nested, depth + 1),
            WrappedCommand::Shell(payload) => {
                self.add(
                    "shell.wrapper_eval",
                    Risk::Caution,
                    format!("{executable} reparses its command through a shell"),
                    invocation.span,
                );
                self.merge_nested(&payload, depth);
            }
            WrappedCommand::None => {}
            WrappedCommand::Unresolved => self.add(
                "shell.unresolved_wrapper",
                Risk::Unknown,
                format!("the command wrapped by {executable} could not be determined"),
                invocation.span,
            ),
        }
    }

    fn inspect_opaque_wrapper(&mut self, executable: &str, invocation: &Invocation, depth: usize) {
        if invocation.arguments.len() == 1
            && invocation.arguments[0]
                .literal
                .as_deref()
                .is_some_and(|value| matches!(value, "--help" | "--version"))
        {
            return;
        }
        self.add(
            "shell.unresolved_wrapper",
            Risk::Unknown,
            format!("{executable} can launch a command using options the safety checker cannot fully resolve"),
            invocation.span,
        );
        if let Some(separator) = literal_position(&invocation.arguments, "--") {
            if let Some(nested) =
                invocation_from_words(&invocation.arguments[separator + 1..], invocation.span)
            {
                self.inspect_invocation(&nested, depth + 1);
            }
        }
    }

    fn inspect_shell_eval(&mut self, executable: &str, invocation: &Invocation, depth: usize) {
        let command_index = invocation.arguments.iter().position(|argument| {
            argument.literal.as_deref().is_some_and(|value| {
                value == "-c"
                    || value == "--command"
                    || (value.starts_with('-')
                        && !value.starts_with("--")
                        && value.chars().skip(1).any(|flag| flag == 'c'))
            })
        });
        let Some(index) = command_index else {
            if !has_any_literal(&invocation.arguments, &["--version", "--help"]) {
                self.add(
                    "shell.unanalysed_script",
                    Risk::Unknown,
                    format!("a script executed by {executable} cannot be inspected"),
                    invocation.span,
                );
            }
            return;
        };
        self.add(
            "shell.nested_eval",
            Risk::Caution,
            format!("{executable} -c starts a nested shell"),
            invocation.span,
        );
        let Some(payload) = invocation.arguments.get(index + 1) else {
            self.add(
                "shell.dynamic_eval",
                Risk::Unknown,
                "nested shell payload is missing",
                invocation.span,
            );
            return;
        };
        self.merge_nested(payload, depth);
    }

    fn inspect_eval(&mut self, invocation: &Invocation, depth: usize) {
        self.add(
            "shell.eval",
            Risk::Caution,
            "eval reparses text as shell syntax",
            invocation.span,
        );
        if invocation.arguments.iter().any(|argument| {
            let lower = argument.raw.to_ascii_lowercase();
            lower.contains("$(curl")
                || lower.contains("$(wget")
                || lower.contains("`curl")
                || lower.contains("`wget")
        }) {
            self.add(
                "network.download_execute",
                Risk::Danger,
                "eval executes text produced by a remote download",
                invocation.span,
            );
        }
        let literals = invocation
            .arguments
            .iter()
            .map(|argument| argument.literal.as_deref())
            .collect::<Option<Vec<_>>>();
        if let Some(parts) = literals {
            let joined = parts.join(" ");
            let lower = joined.to_ascii_lowercase();
            if (lower.contains("$(curl")
                || lower.contains("$(wget")
                || lower.contains("`curl")
                || lower.contains("`wget"))
                && (contains_command_word(&lower, "curl") || contains_command_word(&lower, "wget"))
            {
                self.add(
                    "network.download_execute",
                    Risk::Danger,
                    "eval executes text produced by a remote download",
                    invocation.span,
                );
            }
            let payload = ShellWord {
                raw: joined.clone(),
                literal: Some(joined),
                span: invocation.span,
            };
            self.merge_nested(&payload, depth);
        } else {
            self.add(
                "shell.dynamic_eval",
                Risk::Unknown,
                "dynamic eval input cannot be checked",
                invocation.span,
            );
        }
    }

    fn inspect_trap(&mut self, invocation: &Invocation, depth: usize) {
        let mut arguments = invocation.arguments.as_slice();
        if arguments.is_empty()
            || arguments
                .first()
                .is_some_and(|argument| matches!(argument.literal.as_deref(), Some("-p" | "-l")))
        {
            return;
        }
        if arguments
            .first()
            .is_some_and(|argument| argument.literal.as_deref() == Some("--"))
        {
            arguments = &arguments[1..];
        }
        let Some(action) = arguments.first() else {
            self.add(
                "shell.dynamic_trap",
                Risk::Unknown,
                "trap action is missing",
                invocation.span,
            );
            return;
        };
        if matches!(action.literal.as_deref(), Some("" | "-")) {
            return;
        }
        self.add(
            "shell.trap",
            Risk::Caution,
            "trap schedules shell code to run when a signal or shell event occurs",
            invocation.span,
        );
        self.merge_nested(action, depth);
    }

    fn merge_nested(&mut self, payload: &ShellWord, depth: usize) {
        let Some(source) = payload.literal.as_deref() else {
            self.add(
                "shell.dynamic_eval",
                Risk::Unknown,
                "dynamic nested shell input cannot be checked",
                payload.span,
            );
            return;
        };
        let nested = assess_inner(source, self.platform, depth + 1);
        for finding in nested.findings {
            self.add(
                finding.rule_id,
                finding.severity,
                format!("nested command: {}", finding.message),
                payload.span,
            );
        }
    }

    fn inspect_sensitive_arguments(&mut self, invocation: &Invocation) {
        if invocation.arguments.iter().any(|argument| {
            argument
                .literal
                .as_deref()
                .is_some_and(is_sensitive_private_path)
        }) {
            self.add(
                "credentials.sensitive_file",
                Risk::Caution,
                "command references a private key or credential file",
                invocation.span,
            );
        }
    }

    fn inspect_variable_assignment(&mut self, node: Node<'_>) {
        if let Some((rule, risk, message)) = risky_environment_assignment(self.text(node)) {
            self.add(rule, risk, message, span(node));
        }
    }

    fn inspect_redirect(&mut self, node: Node<'_>) {
        let raw = self.text(node);
        if !is_write_redirect(raw) {
            return;
        }
        let node_span = span(node);
        if contains_angle_bracket_placeholder(self.source) {
            self.add(
                "syntax.placeholder",
                Risk::Unknown,
                "command contains a documentation placeholder and cannot be executed as written",
                node_span,
            );
            return;
        }
        let Some(destination) = node.child_by_field_name("destination") else {
            self.add(
                "filesystem.dynamic_redirect",
                Risk::Unknown,
                "write redirection target could not be determined",
                node_span,
            );
            return;
        };
        let target = ShellWord::new(self.text(destination), span(destination));
        match target.literal.as_deref() {
            Some(path)
                if is_block_device(path)
                    || (is_critical_absolute_path(path, self.platform)
                        && (!is_specific_log_path(path) || !is_append_redirect(raw))) =>
            {
                self.add(
                    "filesystem.critical_redirect",
                    Risk::Danger,
                    format!("write redirection targets critical path {path}"),
                    node_span,
                );
            }
            Some(path) if is_persistence_path(path) || is_shell_history_path(path) => self.add(
                "persistence.redirect",
                if is_append_redirect(raw) {
                    Risk::Caution
                } else {
                    Risk::Danger
                },
                format!("write redirection modifies startup or persistence path {path}"),
                node_span,
            ),
            Some(path) => self.add(
                "filesystem.overwrite",
                Risk::Caution,
                format!("write redirection creates or changes {path}"),
                node_span,
            ),
            None => self.add(
                "filesystem.dynamic_redirect",
                Risk::Unknown,
                "write redirection has a dynamic target that cannot be checked",
                node_span,
            ),
        }
    }

    fn inspect_platform_compatibility(&mut self, executable: &str, invocation: &Invocation) {
        let incompatible = match self.platform {
            Platform::MacOs => matches!(
                executable,
                "apt"
                    | "apt-get"
                    | "dnf"
                    | "yum"
                    | "pacman"
                    | "zypper"
                    | "systemctl"
                    | "service"
                    | "ip"
                    | "ss"
                    | "fuser"
                    | "xdg-open"
            ),
            Platform::Linux => matches!(
                executable,
                "launchctl"
                    | "diskutil"
                    | "pbcopy"
                    | "pbpaste"
                    | "osascript"
                    | "mdfind"
                    | "defaults"
                    | "security"
            ),
        };
        if incompatible {
            self.add(
                match self.platform {
                    Platform::MacOs => "compat.macos.command",
                    Platform::Linux => "compat.linux.command",
                },
                Risk::Caution,
                format!("{executable} is not a standard command on this platform"),
                invocation.span,
            );
        }

        let literals = literal_values(&invocation.arguments);
        let incompatible_flag = match self.platform {
            Platform::MacOs => match executable {
                "stat" => literals
                    .iter()
                    .any(|arg| *arg == "-c" || arg.starts_with("--format")),
                "date" => literals
                    .iter()
                    .any(|arg| *arg == "-d" || arg.starts_with("--date")),
                "find" => literals.contains(&"-printf"),
                "grep" => literals
                    .iter()
                    .any(|arg| *arg == "-P" || arg.starts_with("--perl")),
                "readlink" => literals.contains(&"-f"),
                "xargs" => literals
                    .iter()
                    .any(|arg| *arg == "-r" || *arg == "--no-run-if-empty"),
                "sed" => sed_uses_gnu_in_place(&invocation.arguments),
                _ => false,
            },
            Platform::Linux => match executable {
                "date" => literals.iter().any(|arg| arg.starts_with("-v")),
                "sed" => sed_uses_bsd_empty_suffix(&invocation.arguments),
                _ => false,
            },
        };
        if incompatible_flag {
            self.add(
                match self.platform {
                    Platform::MacOs => "compat.macos.flag",
                    Platform::Linux => "compat.linux.flag",
                },
                Risk::Caution,
                "command uses an option with different BSD/GNU behavior on this platform",
                invocation.span,
            );
        }
    }

    fn inspect_whole_command(&mut self) {
        let lower = self.source.to_ascii_lowercase();
        let structural = strip_quoted_shell_literals(self.source).to_ascii_lowercase();
        let compact_structure = structural
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect::<String>();
        let span = SourceSpan {
            start: 0,
            end: self.source.len(),
        };
        if structural.contains("/dev/tcp/")
            || (structural.contains("bash -i")
                && (structural.contains(" nc ") || structural.contains("socat")))
        {
            self.add(
                "network.reverse_shell",
                Risk::Danger,
                "command resembles a reverse shell",
                span,
            );
        }
        if compact_structure.contains(":(){:|:&};:") {
            self.add(
                "process.fork_bomb",
                Risk::Danger,
                "command is a fork bomb",
                span,
            );
        }
        let trimmed = lower.trim();
        if trimmed == "unset histfile"
            || trimmed == "export histsize=0"
            || trimmed == "export histfilesize=0"
        {
            self.add(
                "shell.history_disable",
                Risk::Danger,
                "command disables shell command-history recording",
                span,
            );
        }
        if structural.contains("mkfifo")
            && ["nc", "ncat", "netcat", "socat"]
                .iter()
                .any(|tool| contains_command_word(&structural, tool))
            && (structural.contains("/bin/sh") || structural.contains("/bin/bash"))
        {
            self.add(
                "network.reverse_shell",
                Risk::Danger,
                "named pipe is connected to a shell and network socket",
                span,
            );
        }
        if contains_sensitive_private_path(&lower)
            && [
                "curl", "wget", "scp", "sftp", "rsync", "nc", "ncat", "netcat", "socat", "mail",
            ]
            .iter()
            .any(|sink| contains_command_word(&lower, sink))
        {
            self.add(
                "credentials.exfiltration",
                Risk::Danger,
                "a private key or credential file is combined with an outbound network sink",
                span,
            );
        }
        if downloaded_artifact_is_executed(&structural)
            || ((structural.contains("<(curl") || structural.contains("<(wget"))
                && ["sh", "bash", "zsh", "python", "python3", "perl", "ruby"]
                    .iter()
                    .any(|interpreter| contains_command_word(&structural, interpreter)))
        {
            self.add(
                "network.download_execute",
                Risk::Danger,
                "the command downloads content and executes it in the same shell expression",
                span,
            );
        }
    }

    fn inspect_pipeline(&mut self, node: Node<'_>) {
        let lower = self.text(node).to_ascii_lowercase();
        let node_span = span(node);
        if pipeline_to_shell(&lower, &["curl", "wget"])
            || pipeline_to_shell(&lower, &["base64", "openssl"])
            || pipeline_to_interpreter(&lower, &["curl", "wget"])
        {
            self.add(
                "network.pipe_to_shell",
                Risk::Danger,
                "downloaded or decoded content is piped directly into a shell",
                node_span,
            );
        }
        if find_pipeline_feeds_broad_delete(&lower) {
            self.add(
                "filesystem.pipeline_broad_delete",
                Risk::Danger,
                "find sends a critical or broad tree into a deleting command",
                node_span,
            );
        }
    }

    fn text(&self, node: Node<'_>) -> &'source str {
        self.source.get(node.byte_range()).unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
struct ShellWord {
    raw: String,
    literal: Option<String>,
    span: SourceSpan,
}

impl ShellWord {
    fn new(raw: &str, span: SourceSpan) -> Self {
        Self {
            raw: raw.to_owned(),
            literal: decode_literal_word(raw),
            span,
        }
    }
}

#[derive(Clone, Debug)]
struct Invocation {
    name: ShellWord,
    arguments: Vec<ShellWord>,
    span: SourceSpan,
}

enum WrappedCommand {
    Direct(Invocation),
    Shell(ShellWord),
    None,
    Unresolved,
}

enum ParsedAssignmentName {
    Known(String),
    Dynamic,
    NotAssignment,
}

fn span(node: Node<'_>) -> SourceSpan {
    SourceSpan {
        start: node.start_byte(),
        end: node.end_byte(),
    }
}

fn contains_missing_or_error(node: Node<'_>) -> bool {
    if node.is_error() || node.is_missing() {
        return true;
    }
    let mut cursor = node.walk();
    let contains_invalid_child = node.children(&mut cursor).any(contains_missing_or_error);
    contains_invalid_child
}

fn decode_literal_word(raw: &str) -> Option<String> {
    #[derive(Clone, Copy)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut quote = Quote::None;
    let mut chars = raw.chars();
    let mut decoded = String::with_capacity(raw.len());
    while let Some(character) = chars.next() {
        match quote {
            Quote::None => match character {
                '\'' => quote = Quote::Single,
                '"' => quote = Quote::Double,
                '\\' => decoded.push(chars.next()?),
                '$' | '`' | '*' | '?' | '[' => return None,
                _ => decoded.push(character),
            },
            Quote::Single => {
                if character == '\'' {
                    quote = Quote::None;
                } else {
                    decoded.push(character);
                }
            }
            Quote::Double => match character {
                '"' => quote = Quote::None,
                '$' | '`' => return None,
                '\\' => decoded.push(chars.next()?),
                _ => decoded.push(character),
            },
        }
    }
    if matches!(quote, Quote::None) {
        Some(decoded)
    } else {
        None
    }
}

fn executable_basename(value: &str) -> &str {
    value.rsplit('/').next().unwrap_or(value)
}

fn risky_environment_assignment(value: &str) -> Option<(&'static str, Risk, &'static str)> {
    let (name, _) = value.split_once('=')?;
    risky_environment_name(name)
}

fn risky_environment_name(name: &str) -> Option<(&'static str, Risk, &'static str)> {
    let name = name.trim_matches(|character| matches!(character, '\'' | '"'));
    let name = name.strip_suffix('+').unwrap_or(name);
    if matches!(
        name,
        "BASH_ENV"
            | "ENV"
            | "LD_PRELOAD"
            | "LD_LIBRARY_PATH"
            | "DYLD_INSERT_LIBRARIES"
            | "DYLD_LIBRARY_PATH"
            | "DYLD_FRAMEWORK_PATH"
            | "DYLD_FORCE_FLAT_NAMESPACE"
            | "PYTHONPATH"
            | "PYTHONHOME"
            | "PYTHONSTARTUP"
            | "NODE_OPTIONS"
            | "RUBYOPT"
            | "RUBYLIB"
            | "PERL5OPT"
            | "PERL5LIB"
            | "JAVA_TOOL_OPTIONS"
            | "JDK_JAVA_OPTIONS"
            | "_JAVA_OPTIONS"
            | "LESSOPEN"
            | "LESSCLOSE"
    ) {
        Some((
            "shell.environment_injection",
            Risk::Danger,
            "environment assignment can inject startup code or alter dynamic loading",
        ))
    } else if matches!(
        name,
        "PATH"
            | "IFS"
            | "CDPATH"
            | "GLOBIGNORE"
            | "BASHOPTS"
            | "SHELLOPTS"
            | "ZDOTDIR"
            | "PROMPT_COMMAND"
    ) {
        Some((
            "shell.environment_resolution",
            Risk::Unknown,
            "environment assignment can change command or shell resolution",
        ))
    } else {
        None
    }
}

fn parsed_assignment_name(word: &ShellWord) -> ParsedAssignmentName {
    if let Some(value) = word.literal.as_deref() {
        return value
            .split_once('=')
            .map_or(ParsedAssignmentName::NotAssignment, |(name, _)| {
                if name.is_empty() {
                    ParsedAssignmentName::NotAssignment
                } else {
                    ParsedAssignmentName::Known(name.to_owned())
                }
            });
    }

    let Some((raw_name, _)) = word.raw.split_once('=') else {
        return ParsedAssignmentName::NotAssignment;
    };
    let raw_name = raw_name.trim_matches(|character| matches!(character, '\'' | '"'));
    if raw_name.is_empty() {
        ParsedAssignmentName::NotAssignment
    } else if let Some(name) = decode_literal_word(raw_name) {
        ParsedAssignmentName::Known(name)
    } else {
        ParsedAssignmentName::Dynamic
    }
}

fn env_split_string_payload(argument: &ShellWord) -> Option<Option<ShellWord>> {
    if matches!(argument.literal.as_deref(), Some("-S" | "--split-string")) {
        return Some(None);
    }

    if let Some(value) = argument.literal.as_deref() {
        let payload = value.strip_prefix("--split-string=").or_else(|| {
            value
                .strip_prefix("-S")
                .filter(|payload| !payload.is_empty())
        })?;
        return Some(Some(ShellWord {
            raw: payload.to_owned(),
            literal: Some(payload.to_owned()),
            span: argument.span,
        }));
    }

    let payload = argument.raw.strip_prefix("--split-string=").or_else(|| {
        argument
            .raw
            .strip_prefix("-S")
            .filter(|payload| !payload.is_empty())
    })?;
    Some(Some(ShellWord::new(payload, argument.span)))
}

fn append_shell_words(payload: &ShellWord, words: &[ShellWord]) -> ShellWord {
    let Some(mut source) = payload.literal.clone() else {
        return payload.clone();
    };
    for word in words {
        source.push(' ');
        source.push_str(&word.raw);
    }
    ShellWord {
        raw: source.clone(),
        literal: Some(source),
        span: SourceSpan {
            start: payload.span.start,
            end: words.last().map_or(payload.span.end, |word| word.span.end),
        },
    }
}

fn env_option_takes_separate_value(value: &str) -> bool {
    matches!(
        value,
        "-u" | "--unset" | "-C" | "--chdir" | "-a" | "--argv0"
    )
}

fn is_env_flag_or_attached_option(value: &str) -> bool {
    matches!(
        value,
        "-" | "-i"
            | "--ignore-environment"
            | "-0"
            | "--null"
            | "-v"
            | "--debug"
            | "--block-signal"
            | "--default-signal"
            | "--ignore-signal"
    ) || (value.starts_with('-')
        && !value.starts_with("--")
        && value
            .chars()
            .skip(1)
            .all(|flag| matches!(flag, 'i' | '0' | 'v')))
        || assigned_option(
            value,
            &[
                "--unset",
                "--chdir",
                "--argv0",
                "--block-signal",
                "--default-signal",
                "--ignore-signal",
            ],
            &["-u", "-C", "-a"],
        )
}

fn command_is_lookup(arguments: &[ShellWord]) -> bool {
    let mut lookup = false;
    for argument in arguments {
        let Some(value) = argument.literal.as_deref() else {
            return lookup;
        };
        if value == "--" {
            continue;
        }
        if value.starts_with('-') && value != "-" {
            if value.starts_with("--") {
                return false;
            }
            let flags = value.chars().skip(1).collect::<Vec<_>>();
            if flags.is_empty() || !flags.iter().all(|flag| matches!(flag, 'p' | 'v' | 'V')) {
                return false;
            }
            lookup |= flags.iter().any(|flag| matches!(flag, 'v' | 'V'));
            continue;
        }
        return lookup;
    }
    lookup
}

fn is_builtin_editor_config(value: &str) -> bool {
    matches!(
        value.to_ascii_uppercase().as_str(),
        "NONE" | "NORC" | "DEFAULTS"
    ) || value == "/dev/null"
}

fn literal_values(arguments: &[ShellWord]) -> Vec<&str> {
    arguments
        .iter()
        .filter_map(|argument| argument.literal.as_deref())
        .collect()
}

fn invocation_text(invocation: &Invocation) -> String {
    invocation
        .arguments
        .iter()
        .map(|argument| argument.literal.as_deref().unwrap_or(&argument.raw))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn is_interpreter_info_query(executable: &str, arguments: &[ShellWord]) -> bool {
    if arguments.len() != 1 {
        return false;
    }
    let Some(argument) = arguments[0].literal.as_deref() else {
        return false;
    };
    match executable {
        "osascript" => matches!(argument, "-h" | "--help"),
        "python" | "python3" => matches!(argument, "-V" | "--version" | "-h" | "--help"),
        "perl" | "ruby" | "node" | "php" => {
            matches!(argument, "-v" | "-V" | "--version" | "-h" | "--help")
        }
        _ => false,
    }
}

fn inline_interpreter_reverse_shell(executable: &str, invocation: &Invocation) -> bool {
    if !matches!(
        executable,
        "python" | "python3" | "perl" | "ruby" | "php" | "node"
    ) {
        return false;
    }
    let text = invocation_text(invocation);
    (text.contains("socket") && (text.contains("connect(") || text.contains("create_connection")))
        && (text.contains("dup2")
            || text.contains("pty.spawn")
            || text.contains("/bin/sh")
            || text.contains("/bin/bash"))
}

fn container_cleanup_is_filtered(text: &str) -> bool {
    text.contains("status=exited")
        || text.contains("status%3dexited")
        || text.contains("dangling=true")
        || text.contains("dangling%3dtrue")
        || (text.contains("grep") && text.contains("<none>"))
}

fn is_critical_service(value: &str) -> bool {
    let normalized = value.trim_end_matches(".service").to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "ssh" | "sshd" | "firewalld" | "network" | "networking"
    )
}

fn has_literal(arguments: &[ShellWord], expected: &str) -> bool {
    arguments
        .iter()
        .any(|argument| argument.literal.as_deref() == Some(expected))
}

fn has_any_literal(arguments: &[ShellWord], expected: &[&str]) -> bool {
    arguments.iter().any(|argument| {
        argument
            .literal
            .as_deref()
            .is_some_and(|value| expected.contains(&value))
    })
}

fn has_option_prefix(arguments: &[ShellWord], prefixes: &[&str]) -> bool {
    arguments.iter().any(|argument| {
        prefixes
            .iter()
            .any(|prefix| argument.raw.starts_with(prefix))
    })
}

fn literal_position(arguments: &[ShellWord], expected: &str) -> Option<usize> {
    arguments
        .iter()
        .position(|argument| argument.literal.as_deref() == Some(expected))
}

fn has_short_flag(arguments: &[ShellWord], expected: char) -> bool {
    arguments.iter().any(|argument| {
        argument.literal.as_deref().is_some_and(|value| {
            value.starts_with('-')
                && !value.starts_with("--")
                && value.chars().skip(1).any(|flag| flag == expected)
        })
    })
}

fn positional_arguments(arguments: &[ShellWord]) -> Vec<&ShellWord> {
    let mut after_double_dash = false;
    arguments
        .iter()
        .filter(|argument| {
            let Some(value) = argument.literal.as_deref() else {
                return true;
            };
            if after_double_dash {
                return true;
            }
            if value == "--" {
                after_double_dash = true;
                return false;
            }
            !value.starts_with('-')
        })
        .collect()
}

fn first_positional(arguments: &[ShellWord]) -> Option<&str> {
    positional_arguments(arguments)
        .into_iter()
        .find_map(|argument| argument.literal.as_deref())
}

fn option_path(
    arguments: &[ShellWord],
    separate_options: &[&str],
    assigned_prefixes: &[&str],
) -> Option<ShellWord> {
    for (index, argument) in arguments.iter().enumerate() {
        if argument
            .literal
            .as_deref()
            .is_some_and(|value| separate_options.contains(&value))
        {
            return arguments.get(index + 1).cloned();
        }
        for prefix in assigned_prefixes {
            if let Some(value) = argument.raw.strip_prefix(prefix) {
                return Some(ShellWord::new(value, argument.span));
            }
        }
    }
    None
}

fn option_value_is(arguments: &[ShellWord], options: &[&str], expected: &str) -> bool {
    arguments.iter().enumerate().any(|(index, argument)| {
        argument
            .literal
            .as_deref()
            .is_some_and(|value| options.contains(&value))
            && arguments
                .get(index + 1)
                .and_then(|value| value.literal.as_deref())
                == Some(expected)
    })
}

fn invocation_from_words(words: &[ShellWord], fallback_span: SourceSpan) -> Option<Invocation> {
    let (name, arguments) = words.split_first()?;
    Some(Invocation {
        name: name.clone(),
        arguments: arguments.to_vec(),
        span: SourceSpan {
            start: name.span.start,
            end: words.last().map_or(fallback_span.end, |word| word.span.end),
        },
    })
}

fn invocation_is_mutating(invocation: &Invocation) -> bool {
    invocation.name.literal.as_deref().is_some_and(|name| {
        matches!(
            executable_basename(name).to_ascii_lowercase().as_str(),
            "rm" | "rmdir"
                | "unlink"
                | "shred"
                | "mv"
                | "cp"
                | "install"
                | "ln"
                | "truncate"
                | "chmod"
                | "chown"
                | "chgrp"
                | "dd"
                | "tee"
        )
    })
}

fn direct_wrapped_command(invocation: &Invocation, index: usize) -> WrappedCommand {
    invocation_from_words(&invocation.arguments[index..], invocation.span)
        .map_or(WrappedCommand::Unresolved, WrappedCommand::Direct)
}

fn shell_wrapped_command(invocation: &Invocation, index: usize) -> WrappedCommand {
    let words = &invocation.arguments[index..];
    if words.is_empty() {
        return WrappedCommand::Unresolved;
    }
    let Some(parts) = words
        .iter()
        .map(|word| word.literal.as_deref())
        .collect::<Option<Vec<_>>>()
    else {
        return WrappedCommand::Unresolved;
    };
    let joined = parts.join(" ");
    WrappedCommand::Shell(ShellWord {
        raw: joined.clone(),
        literal: Some(joined),
        span: SourceSpan {
            start: words[0].span.start,
            end: words
                .last()
                .map_or(invocation.span.end, |word| word.span.end),
        },
    })
}

fn assigned_option(value: &str, long: &[&str], short: &[&str]) -> bool {
    long.iter().any(|option| {
        value
            .strip_prefix(option)
            .is_some_and(|rest| rest.starts_with('='))
    }) || short
        .iter()
        .any(|option| value.starts_with(option) && value.len() > option.len())
}

fn parse_timeout_wrapper(invocation: &Invocation) -> WrappedCommand {
    let mut index = 0;
    while let Some(argument) = invocation.arguments.get(index) {
        let Some(value) = argument.literal.as_deref() else {
            return WrappedCommand::Unresolved;
        };
        if value == "--" {
            index += 1;
            break;
        }
        if !value.starts_with('-') || value == "-" {
            break;
        }
        if matches!(value, "--help" | "--version") {
            return WrappedCommand::None;
        }
        if matches!(value, "-k" | "--kill-after" | "-s" | "--signal") {
            if invocation.arguments.get(index + 1).is_none() {
                return WrappedCommand::Unresolved;
            }
            index += 2;
            continue;
        }
        if assigned_option(value, &["--kill-after", "--signal"], &["-k", "-s"])
            || matches!(
                value,
                "-f" | "--foreground" | "-p" | "--preserve-status" | "-v" | "--verbose"
            )
            || (value.starts_with('-')
                && !value.starts_with("--")
                && value[1..]
                    .chars()
                    .all(|flag| matches!(flag, 'f' | 'p' | 'v')))
        {
            index += 1;
            continue;
        }
        return WrappedCommand::Unresolved;
    }
    if invocation.arguments.get(index).is_none() {
        return WrappedCommand::Unresolved;
    }
    index += 1; // duration
    direct_wrapped_command(invocation, index)
}

fn parse_watch_wrapper(invocation: &Invocation) -> WrappedCommand {
    let mut index = 0;
    let mut direct = false;
    while let Some(argument) = invocation.arguments.get(index) {
        let Some(value) = argument.literal.as_deref() else {
            return WrappedCommand::Unresolved;
        };
        if value == "--" {
            index += 1;
            break;
        }
        if !value.starts_with('-') || value == "-" {
            break;
        }
        if matches!(value, "-h" | "--help" | "-v" | "--version") {
            return WrappedCommand::None;
        }
        if matches!(
            value,
            "-n" | "--interval" | "-q" | "--equexit" | "-s" | "--shotsdir"
        ) {
            if invocation.arguments.get(index + 1).is_none() {
                return WrappedCommand::Unresolved;
            }
            index += 2;
            continue;
        }
        if assigned_option(
            value,
            &["--interval", "--equexit", "--shotsdir", "--differences"],
            &["-n", "-q", "-s"],
        ) {
            index += 1;
            continue;
        }
        if matches!(value, "-x" | "--exec") {
            direct = true;
            index += 1;
            continue;
        }
        if matches!(
            value,
            "-b" | "--beep"
                | "-d"
                | "--differences"
                | "-e"
                | "--errexit"
                | "-f"
                | "--no-wrap"
                | "-g"
                | "--chgexit"
                | "-p"
                | "--precise"
                | "-r"
                | "--no-rerun"
                | "-t"
                | "--no-title"
                | "-w"
                | "--no-linewrap"
        ) {
            index += 1;
            continue;
        }
        return WrappedCommand::Unresolved;
    }
    if direct {
        direct_wrapped_command(invocation, index)
    } else {
        shell_wrapped_command(invocation, index)
    }
}

fn inline_shell_option(argument: &ShellWord) -> Option<Option<ShellWord>> {
    let value = argument.literal.as_deref()?;
    for prefix in ["--command=", "-c"] {
        if let Some(payload) = value
            .strip_prefix(prefix)
            .filter(|payload| !payload.is_empty())
        {
            return Some(Some(ShellWord {
                raw: payload.to_owned(),
                literal: Some(payload.to_owned()),
                span: argument.span,
            }));
        }
    }
    matches!(value, "-c" | "--command").then_some(None)
}

fn parse_flock_wrapper(invocation: &Invocation) -> WrappedCommand {
    let mut index = 0;
    while let Some(argument) = invocation.arguments.get(index) {
        let Some(value) = argument.literal.as_deref() else {
            return WrappedCommand::Unresolved;
        };
        if let Some(payload) = inline_shell_option(argument) {
            return payload.map_or_else(
                || {
                    invocation
                        .arguments
                        .get(index + 1)
                        .cloned()
                        .map_or(WrappedCommand::Unresolved, WrappedCommand::Shell)
                },
                WrappedCommand::Shell,
            );
        }
        if value == "--" {
            index += 1;
            break;
        }
        if !value.starts_with('-') || value == "-" {
            break;
        }
        if matches!(value, "-h" | "--help" | "-V" | "--version") {
            return WrappedCommand::None;
        }
        if matches!(value, "-E" | "--conflict-exit-code" | "-w" | "--timeout") {
            if invocation.arguments.get(index + 1).is_none() {
                return WrappedCommand::Unresolved;
            }
            index += 2;
            continue;
        }
        if assigned_option(value, &["--conflict-exit-code", "--timeout"], &["-E", "-w"])
            || matches!(
                value,
                "-s" | "--shared"
                    | "-x"
                    | "--exclusive"
                    | "-u"
                    | "--unlock"
                    | "-n"
                    | "--nonblock"
                    | "-o"
                    | "--close"
                    | "-F"
                    | "--no-fork"
                    | "--verbose"
            )
        {
            index += 1;
            continue;
        }
        return WrappedCommand::Unresolved;
    }
    let Some(lock) = invocation.arguments.get(index) else {
        return WrappedCommand::Unresolved;
    };
    index += 1;
    if let Some(argument) = invocation.arguments.get(index) {
        if let Some(payload) = inline_shell_option(argument) {
            return payload.map_or_else(
                || {
                    invocation
                        .arguments
                        .get(index + 1)
                        .cloned()
                        .map_or(WrappedCommand::Unresolved, WrappedCommand::Shell)
                },
                WrappedCommand::Shell,
            );
        }
        return direct_wrapped_command(invocation, index);
    }
    if lock
        .literal
        .as_deref()
        .is_some_and(|value| value.parse::<i32>().is_ok())
    {
        WrappedCommand::None
    } else {
        WrappedCommand::Unresolved
    }
}

fn parse_chrt_wrapper(invocation: &Invocation) -> WrappedCommand {
    let mut index = 0;
    let mut pid_mode = false;
    while let Some(argument) = invocation.arguments.get(index) {
        let Some(value) = argument.literal.as_deref() else {
            return WrappedCommand::Unresolved;
        };
        if value == "--" {
            index += 1;
            break;
        }
        if !value.starts_with('-') || value == "-" {
            break;
        }
        if matches!(value, "-h" | "--help" | "-V" | "--version" | "-m" | "--max") {
            return WrappedCommand::None;
        }
        if matches!(
            value,
            "-T" | "--sched-runtime" | "-D" | "--sched-deadline" | "-P" | "--sched-period"
        ) {
            if invocation.arguments.get(index + 1).is_none() {
                return WrappedCommand::Unresolved;
            }
            index += 2;
            continue;
        }
        if assigned_option(
            value,
            &["--sched-runtime", "--sched-deadline", "--sched-period"],
            &["-T", "-D", "-P"],
        ) {
            index += 1;
            continue;
        }
        if matches!(value, "-p" | "--pid") {
            pid_mode = true;
            index += 1;
            continue;
        }
        if matches!(
            value,
            "-a" | "--all-tasks"
                | "-b"
                | "--batch"
                | "-d"
                | "--deadline"
                | "-f"
                | "--fifo"
                | "-i"
                | "--idle"
                | "-o"
                | "--other"
                | "-r"
                | "--rr"
                | "-R"
                | "--reset-on-fork"
                | "-v"
                | "--verbose"
        ) {
            index += 1;
            continue;
        }
        return WrappedCommand::Unresolved;
    }
    if pid_mode {
        return WrappedCommand::None;
    }
    if invocation.arguments.get(index).is_none() {
        return WrappedCommand::Unresolved;
    }
    index += 1; // scheduling priority
    direct_wrapped_command(invocation, index)
}

fn parse_taskset_wrapper(invocation: &Invocation) -> WrappedCommand {
    let mut index = 0;
    let mut pid_mode = false;
    while let Some(argument) = invocation.arguments.get(index) {
        let Some(value) = argument.literal.as_deref() else {
            return WrappedCommand::Unresolved;
        };
        if value == "--" {
            index += 1;
            break;
        }
        if !value.starts_with('-') || value == "-" {
            break;
        }
        if matches!(value, "-h" | "--help" | "-V" | "--version") {
            return WrappedCommand::None;
        }
        if value.starts_with('-') && !value.starts_with("--") {
            let flags = value[1..].chars().collect::<Vec<_>>();
            if !flags.is_empty() && flags.iter().all(|flag| matches!(flag, 'a' | 'c' | 'p')) {
                pid_mode |= flags.contains(&'p');
                index += 1;
                continue;
            }
        }
        if matches!(value, "--all-tasks" | "--cpu-list") {
            index += 1;
            continue;
        }
        if value == "--pid" {
            pid_mode = true;
            index += 1;
            continue;
        }
        return WrappedCommand::Unresolved;
    }
    if pid_mode {
        return WrappedCommand::None;
    }
    if invocation.arguments.get(index).is_none() {
        return WrappedCommand::Unresolved;
    }
    index += 1; // CPU mask or list
    direct_wrapped_command(invocation, index)
}

fn parse_chroot_wrapper(invocation: &Invocation) -> WrappedCommand {
    let mut index = 0;
    while let Some(argument) = invocation.arguments.get(index) {
        let Some(value) = argument.literal.as_deref() else {
            return WrappedCommand::Unresolved;
        };
        if value == "--" {
            index += 1;
            break;
        }
        if !value.starts_with('-') || value == "-" {
            break;
        }
        if matches!(value, "--help" | "--version") {
            return WrappedCommand::None;
        }
        if matches!(value, "--userspec" | "--groups") {
            if invocation.arguments.get(index + 1).is_none() {
                return WrappedCommand::Unresolved;
            }
            index += 2;
            continue;
        }
        if assigned_option(value, &["--userspec", "--groups"], &[]) || value == "--skip-chdir" {
            index += 1;
            continue;
        }
        return WrappedCommand::Unresolved;
    }
    if invocation.arguments.get(index).is_none() {
        return WrappedCommand::Unresolved;
    }
    index += 1; // new root
    direct_wrapped_command(invocation, index)
}

fn parse_arch_wrapper(invocation: &Invocation) -> WrappedCommand {
    let mut index = 0;
    while let Some(argument) = invocation.arguments.get(index) {
        let Some(value) = argument.literal.as_deref() else {
            return WrappedCommand::Unresolved;
        };
        if value == "--" {
            index += 1;
            break;
        }
        if !value.starts_with('-') || value == "-" {
            break;
        }
        if matches!(value, "-h" | "--help") {
            return WrappedCommand::None;
        }
        if matches!(value, "-arch" | "-d" | "-e") {
            if invocation.arguments.get(index + 1).is_none() {
                return WrappedCommand::Unresolved;
            }
            index += 2;
            continue;
        }
        if matches!(
            value,
            "-32" | "-64" | "-c" | "-arm64" | "-arm64e" | "-x86_64" | "-i386" | "-ppc" | "-ppc64"
        ) {
            index += 1;
            continue;
        }
        return WrappedCommand::Unresolved;
    }
    if invocation.arguments.get(index).is_none() {
        WrappedCommand::None
    } else {
        direct_wrapped_command(invocation, index)
    }
}

fn parse_caffeinate_wrapper(invocation: &Invocation) -> WrappedCommand {
    let mut index = 0;
    while let Some(argument) = invocation.arguments.get(index) {
        let Some(value) = argument.literal.as_deref() else {
            return WrappedCommand::Unresolved;
        };
        if value == "--" {
            index += 1;
            break;
        }
        if !value.starts_with('-') || value == "-" {
            break;
        }
        if matches!(value, "-h" | "--help") {
            return WrappedCommand::None;
        }
        if matches!(value, "-t" | "-w") {
            if invocation.arguments.get(index + 1).is_none() {
                return WrappedCommand::Unresolved;
            }
            index += 2;
            continue;
        }
        if assigned_option(value, &[], &["-t", "-w"])
            || (value.starts_with('-')
                && !value.starts_with("--")
                && value[1..]
                    .chars()
                    .all(|flag| matches!(flag, 'd' | 'i' | 'm' | 's' | 'u')))
        {
            index += 1;
            continue;
        }
        return WrappedCommand::Unresolved;
    }
    if invocation.arguments.get(index).is_none() {
        WrappedCommand::None
    } else {
        direct_wrapped_command(invocation, index)
    }
}

fn unwrap_command(wrapper: &str, arguments: &[ShellWord]) -> Option<Invocation> {
    let mut index = 0;
    let mut option_value_pending = false;
    while index < arguments.len() {
        let value = arguments[index].literal.as_deref()?;
        if option_value_pending {
            option_value_pending = false;
            index += 1;
            continue;
        }
        if value == "--" {
            index += 1;
            break;
        }
        if !value.starts_with('-') || value == "-" {
            break;
        }
        option_value_pending = wrapper_option_takes_value(wrapper, value);
        index += 1;
    }
    invocation_from_words(&arguments[index..], SourceSpan { start: 0, end: 0 })
}

fn wrapper_option_takes_value(wrapper: &str, option: &str) -> bool {
    match wrapper {
        "sudo" => matches!(
            option,
            "-u" | "--user"
                | "-g"
                | "--group"
                | "-h"
                | "--host"
                | "-p"
                | "--prompt"
                | "-C"
                | "--close-from"
                | "-T"
                | "--command-timeout"
                | "-D"
                | "--chdir"
                | "-R"
                | "--chroot"
                | "-r"
                | "--role"
                | "-t"
                | "--type"
        ),
        "doas" => matches!(option, "-u" | "-C" | "-a"),
        "su" => matches!(
            option,
            "-c" | "--command" | "-s" | "--shell" | "-g" | "--group"
        ),
        "nice" => option == "-n" || option == "--adjustment",
        "ionice" => matches!(option, "-c" | "--class" | "-n" | "--classdata"),
        "chrt" => matches!(option, "-p" | "--pid" | "-T" | "--sched-runtime"),
        "xargs" => matches!(
            option,
            "-E" | "-I"
                | "-L"
                | "-n"
                | "-P"
                | "-s"
                | "--eof"
                | "--replace"
                | "--max-lines"
                | "--max-args"
                | "--max-procs"
                | "--max-chars"
                | "-a"
                | "--arg-file"
                | "-d"
                | "--delimiter"
                | "-J"
                | "-R"
                | "-S"
                | "--process-slot-var"
        ),
        "time" => matches!(option, "-f" | "--format" | "-o" | "--output"),
        "stdbuf" => matches!(
            option,
            "-i" | "--input" | "-o" | "--output" | "-e" | "--error"
        ),
        "exec" => option == "-a",
        _ => false,
    }
}

fn ssh_host_index(arguments: &[ShellWord]) -> Option<usize> {
    let mut takes_value = false;
    let mut after_options = false;
    for (index, argument) in arguments.iter().enumerate() {
        let value = argument.literal.as_deref()?;
        if takes_value {
            takes_value = false;
            continue;
        }
        if after_options {
            return Some(index);
        }
        if value == "--" {
            after_options = true;
            continue;
        }
        if value.starts_with('-') && value != "-" {
            takes_value = matches!(
                value,
                "-B" | "-b"
                    | "-c"
                    | "-D"
                    | "-E"
                    | "-e"
                    | "-F"
                    | "-I"
                    | "-i"
                    | "-J"
                    | "-L"
                    | "-l"
                    | "-m"
                    | "-O"
                    | "-o"
                    | "-P"
                    | "-p"
                    | "-Q"
                    | "-R"
                    | "-S"
                    | "-W"
                    | "-w"
            );
            continue;
        }
        return Some(index);
    }
    None
}

fn find_roots(arguments: &[ShellWord]) -> Vec<&ShellWord> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.literal.as_deref() {
            Some("-H" | "-L" | "-P") => index += 1,
            Some("--") => {
                index += 1;
                break;
            }
            Some("-D") => index += 2,
            Some(value) if value.starts_with("-O") => index += 1,
            _ => break,
        }
    }
    arguments[index..]
        .iter()
        .take_while(|argument| {
            argument
                .literal
                .as_deref()
                .is_none_or(|value| !value.starts_with('-') && value != "!")
        })
        .collect()
}

fn roots_have_dot(roots: &[&ShellWord]) -> bool {
    roots.iter().any(|root| {
        root.literal
            .as_deref()
            .is_some_and(|value| value.trim_end_matches('/') == ".")
    })
}

fn is_critical_or_broad_target(target: &ShellWord, platform: Platform) -> bool {
    if is_unquoted_named_home_root_or_glob(&target.raw) {
        return true;
    }
    let Some(value) = target.literal.as_deref() else {
        return is_broad_dynamic_target(&target.raw);
    };
    value == "~"
        || value == "~/"
        || matches!(value, "~+" | "~+/" | "~-" | "~-/")
        || value == "."
        || value.trim_end_matches('/') == "."
        || value == ".."
        || value.starts_with("~root")
        || value.starts_with("~/..")
        || value.starts_with("/path/to/")
        || (target.raw != "{}" && (target.raw.contains('{') || target.raw.contains('}')))
        || lexical_path_escapes_cwd(value)
        || is_broad_root_directory(value, platform)
        || is_user_home_root(value, platform)
        || is_critical_absolute_path(value, platform)
        || dirs::home_dir().is_some_and(|home| {
            lexical_normalize(value).is_some_and(|path| paths_equal(&path, &home, platform))
        })
}

fn is_unquoted_named_home_root_or_glob(raw: &str) -> bool {
    let Some(remainder) = raw.strip_prefix('~') else {
        return false;
    };
    let (username, suffix) = remainder
        .split_once('/')
        .map_or((remainder, None), |(username, suffix)| {
            (username, Some(suffix))
        });
    if username.is_empty()
        || matches!(username, "+" | "-")
        || !username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return false;
    }
    let Some(suffix) = suffix else {
        return true;
    };
    let mut depth = 0_usize;
    for component in Path::new(suffix).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if depth == 0 => return true,
            Component::ParentDir => depth -= 1,
            Component::Normal(part) => {
                let part = part.to_string_lossy();
                if depth == 0
                    && (part.starts_with('*')
                        || part.starts_with('?')
                        || part.starts_with('[')
                        || part.starts_with(".*"))
                {
                    return true;
                }
                depth += 1;
            }
            Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    depth == 0
}

fn is_critical_absolute_path(value: &str, platform: Platform) -> bool {
    let Some(path) = lexical_normalize(value) else {
        return false;
    };
    if !path.is_absolute() {
        return false;
    }
    if is_temporary_path(&path, platform) {
        return false;
    }
    if path == Path::new("/") {
        return true;
    }
    let common = [
        Path::new("/bin"),
        Path::new("/sbin"),
        Path::new("/etc"),
        Path::new("/usr"),
        Path::new("/var"),
        Path::new("/dev"),
        Path::new("/lib"),
        Path::new("/lib64"),
    ];
    let platform_paths: &[&Path] = match platform {
        Platform::MacOs => &[
            Path::new("/System"),
            Path::new("/Library"),
            Path::new("/Applications"),
            Path::new("/Volumes"),
            Path::new("/private"),
            Path::new("/private/etc"),
            Path::new("/private/var"),
            Path::new("/opt/homebrew"),
        ],
        Platform::Linux => &[
            Path::new("/boot"),
            Path::new("/root"),
            Path::new("/proc"),
            Path::new("/sys"),
            Path::new("/run"),
        ],
    };
    common
        .iter()
        .chain(platform_paths.iter())
        .any(|critical| path_equals_or_descends_from(&path, critical, platform))
}

fn is_user_home_root(value: &str, platform: Platform) -> bool {
    let Some(path) = lexical_normalize(value) else {
        return false;
    };
    path.parent().is_some_and(|parent| {
        paths_equal(parent, Path::new("/Users"), platform)
            || paths_equal(parent, Path::new("/home"), platform)
    })
}

fn is_specific_log_path(value: &str) -> bool {
    let Some(path) = lexical_normalize(value) else {
        return false;
    };
    [Path::new("/var/log"), Path::new("/private/var/log")]
        .iter()
        .any(|root| path.starts_with(root) && path != *root)
}

fn is_log_subtree_path(value: &str) -> bool {
    let Some(path) = lexical_normalize(value) else {
        return false;
    };
    [Path::new("/var/log"), Path::new("/private/var/log")]
        .iter()
        .any(|root| path == *root || path.starts_with(root))
}

fn is_specific_log_file(target: &ShellWord) -> bool {
    target.literal.as_deref().is_some_and(is_specific_log_path)
}

fn is_shell_history_path(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "/.bash_history",
        "/.zsh_history",
        "/.sh_history",
        "/.history",
        "/.local/share/fish/fish_history",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

fn is_sensitive_private_key_path(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    is_sensitive_private_path(&lower)
        || lower.ends_with("/.ssh/private_key")
        || lower.ends_with("/.ssh/identity")
}

fn chmod_mode_exposes_group_or_other(mode: &str) -> bool {
    let numeric = mode.trim_start_matches('0');
    if (3..=4).contains(&mode.len()) && mode.chars().all(|character| matches!(character, '0'..='7'))
    {
        return u16::from_str_radix(mode, 8).is_ok_and(|permissions| permissions & 0o077 != 0);
    }
    if numeric.is_empty() {
        return false;
    }
    let lower = mode.to_ascii_lowercase();
    lower.contains("a+r")
        || lower.contains("a+w")
        || lower.contains("a+x")
        || lower.contains("g+r")
        || lower.contains("g+w")
        || lower.contains("g+x")
        || lower.contains("o+r")
        || lower.contains("o+w")
        || lower.contains("o+x")
        || lower.contains("go=")
}

fn is_documentation_placeholder(target: &ShellWord) -> bool {
    let raw = target
        .raw
        .trim_matches(|character| matches!(character, '\'' | '"'));
    raw.starts_with("{{") && raw.ends_with("}}") && raw.len() > 4
}

fn contains_angle_bracket_placeholder(source: &str) -> bool {
    let Some(open) = source.find('<') else {
        return false;
    };
    let Some(relative_close) = source[open + 1..].find('>') else {
        return false;
    };
    let contents = &source[open + 1..open + 1 + relative_close];
    !contents.is_empty()
        && contents.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '/')
        })
}

fn is_cwd_wide_glob(target: &ShellWord) -> bool {
    matches!(
        target
            .raw
            .trim_matches(|character| matches!(character, '\'' | '"')),
        "*" | "./*"
    )
}

fn is_scoped_by_safe_cd(source: &str, command_start: usize, platform: Platform) -> bool {
    let Some(prefix) = source.get(..command_start) else {
        return false;
    };
    let Some(before_guard) = prefix.trim_end().strip_suffix("&&") else {
        return false;
    };
    let candidate = before_guard
        .trim_end()
        .rsplit([';', '|', '&'])
        .next()
        .unwrap_or_default()
        .trim();
    let words = candidate.split_ascii_whitespace().collect::<Vec<_>>();
    let raw_target = match words.as_slice() {
        ["cd", target] => *target,
        ["cd", "--", target] => *target,
        _ => return false,
    };
    let Some(target) = decode_literal_word(raw_target) else {
        return false;
    };
    let Some(path) = lexical_normalize(&target) else {
        return false;
    };
    if path.is_absolute() {
        return is_temporary_path(&path, platform)
            && ![
                Path::new("/tmp"),
                Path::new("/private/tmp"),
                Path::new("/var/tmp"),
                Path::new("/private/var/tmp"),
            ]
            .iter()
            .any(|root| paths_equal(&path, root, platform));
    }
    target != "."
        && target != "./"
        && !target.starts_with('~')
        && !target.contains('*')
        && !target.contains('?')
        && !lexical_path_escapes_cwd(&target)
}

fn is_broad_root_directory(value: &str, platform: Platform) -> bool {
    let Some(path) = lexical_normalize(value) else {
        return false;
    };
    let platform_roots: &[&Path] = match platform {
        Platform::MacOs => &[Path::new("/Users")],
        Platform::Linux => &[Path::new("/home")],
    };
    paths_equal(&path, Path::new("/opt"), platform)
        || platform_roots
            .iter()
            .any(|root| paths_equal(&path, root, platform))
}

fn is_temporary_path(path: &Path, platform: Platform) -> bool {
    [
        Path::new("/tmp"),
        Path::new("/private/tmp"),
        Path::new("/var/tmp"),
        Path::new("/private/var/tmp"),
        Path::new("/private/var/folders"),
    ]
    .iter()
    .any(|temporary| path_equals_or_descends_from(path, temporary, platform))
}

fn paths_equal(path: &Path, expected: &Path, platform: Platform) -> bool {
    if platform == Platform::MacOs {
        path.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&expected.as_os_str().to_string_lossy())
    } else {
        path == expected
    }
}

fn path_equals_or_descends_from(path: &Path, root: &Path, platform: Platform) -> bool {
    if platform == Platform::MacOs {
        let path = PathBuf::from(path.as_os_str().to_string_lossy().to_ascii_lowercase());
        let root = PathBuf::from(root.as_os_str().to_string_lossy().to_ascii_lowercase());
        path == root || path.starts_with(root)
    } else {
        path == root || path.starts_with(root)
    }
}

fn is_broad_dynamic_target(raw: &str) -> bool {
    let value = raw.trim_matches(|character| matches!(character, '\'' | '"'));
    if value.contains('$') || value.contains('`') || value.contains('?') || value.contains('[') {
        return true;
    }
    if !value.contains('*') {
        return true;
    }
    matches!(value, "*" | "./*" | "../*" | "~/*" | "/*")
        || value.starts_with("**")
        || value == "*/"
        || value.starts_with("~/**")
        || value.starts_with('/')
        || value.starts_with("../")
        || value.starts_with("~/../")
}

fn lexical_normalize(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => return None,
            Component::RootDir => output.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() && !path.is_absolute() {
                    output.push("..");
                }
            }
            Component::Normal(part) => output.push(part),
        }
    }
    Some(output)
}

fn lexical_path_escapes_cwd(value: &str) -> bool {
    if Path::new(value).is_absolute() {
        return false;
    }
    let mut depth = 0_i32;
    for component in Path::new(value).components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn is_block_device(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("/dev/disk")
        || lower.starts_with("/dev/rdisk")
        || lower.starts_with("/dev/sd")
        || lower.starts_with("/dev/hd")
        || lower.starts_with("/dev/nvme")
        || matches!(lower.as_str(), "/dev/mem" | "/dev/kmem")
}

fn is_persistence_path(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "/launchagents/",
        "/launchdaemons/",
        "/systemd/system/",
        "/cron.d/",
        "/authorized_keys",
        "/.zshenv",
        "/.zshrc",
        "/.bashrc",
        "/.bash_profile",
        "/.bash_logout",
        "/.profile",
        "/.config/fish/config.fish",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_sensitive_private_path(value: &str) -> bool {
    let lower = value
        .trim_start_matches('@')
        .trim_matches(|character| matches!(character, '\'' | '"'))
        .to_ascii_lowercase();
    if lower.ends_with(".pub") || lower.contains("known_hosts") {
        return false;
    }
    lower.contains("/.ssh/id_")
        || lower.contains("/.gnupg/")
        || lower.contains("/.aws/credentials")
        || lower.contains("/.config/gcloud/credentials")
        || lower.contains("/.git-credentials")
        || lower.ends_with("/.netrc")
        || lower == "/etc/shadow"
        || lower.ends_with(".pem")
}

fn contains_sensitive_private_path(lower: &str) -> bool {
    lower
        .split(|character: char| {
            character.is_ascii_whitespace() || matches!(character, '|' | '&' | ';' | ',')
        })
        .any(is_sensitive_private_path)
}

fn is_write_redirect(raw: &str) -> bool {
    let compact = raw.trim_start_matches(|character: char| character.is_ascii_digit());
    compact.starts_with('>')
        || compact.starts_with("&>")
        || compact.contains(" >")
        || compact.contains(" &>")
}

fn strip_quoted_shell_literals(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut quote = None;
    let mut escaped = false;
    for character in source.chars() {
        if escaped {
            output.push(if quote.is_some() { ' ' } else { character });
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            output.push(if quote.is_some() { ' ' } else { character });
            escaped = true;
            continue;
        }
        match (quote, character) {
            (None, '\'' | '"') => {
                quote = Some(character);
                output.push(' ');
            }
            (Some(expected), actual) if expected == actual => {
                quote = None;
                output.push(' ');
            }
            (Some(_), _) => output.push(' '),
            (None, _) => output.push(character),
        }
    }
    output
}

fn is_append_redirect(raw: &str) -> bool {
    raw.contains(">>")
}

fn pipeline_to_shell(lower: &str, producers: &[&str]) -> bool {
    let shell_names = ["sh", "bash", "zsh", "dash", "ksh"];
    let parts = lower.split('|').map(str::trim).collect::<Vec<_>>();
    parts.windows(2).any(|window| {
        producers
            .iter()
            .any(|producer| contains_command_word(window[0], producer))
            && shell_names
                .iter()
                .any(|shell| contains_command_word(window[1], shell))
    })
}

fn pipeline_to_interpreter(lower: &str, producers: &[&str]) -> bool {
    let parts = lower.split('|').map(str::trim).collect::<Vec<_>>();
    parts.windows(2).any(|window| {
        producers
            .iter()
            .any(|producer| contains_command_word(window[0], producer))
            && interpreter_consumes_stdin(window[1])
    })
}

fn interpreter_consumes_stdin(command: &str) -> bool {
    let words = command.split_ascii_whitespace().collect::<Vec<_>>();
    ["python", "python3", "perl", "ruby", "php", "node"]
        .iter()
        .any(|interpreter| {
            words
                .iter()
                .position(|word| {
                    executable_basename(
                        word.trim_matches(|character| matches!(character, '\'' | '"')),
                    ) == *interpreter
                })
                .is_some_and(|index| {
                    words.get(index + 1).is_none()
                        || words.get(index + 1).is_some_and(|word| *word == "-")
                })
        })
}

fn find_pipeline_feeds_broad_delete(lower: &str) -> bool {
    let parts = lower.split('|').map(str::trim).collect::<Vec<_>>();
    parts.windows(2).any(|window| {
        contains_command_word(window[0], "find")
            && [
                "find / ",
                "find /etc",
                "find /usr",
                "find /var",
                "find /system",
            ]
            .iter()
            .any(|prefix| window[0].starts_with(prefix))
            && contains_command_word(window[1], "xargs")
            && ["rm", "unlink", "shred", "truncate"]
                .iter()
                .any(|command| contains_command_word(window[1], command))
    })
}

fn downloaded_artifact_is_executed(lower: &str) -> bool {
    let Some((target, tail)) = download_target_and_tail(lower) else {
        return false;
    };
    let direct = [format!("&& {target}"), format!("; {target}")];
    if direct.iter().any(|pattern| tail.contains(pattern)) {
        return true;
    }
    if ["exec", "command", "nohup"]
        .iter()
        .any(|wrapper| tail.contains(&format!("{wrapper} {target}")))
    {
        return true;
    }
    [
        "sh", "bash", "zsh", "dash", "python", "python3", "perl", "ruby", "node",
    ]
    .iter()
    .any(|interpreter| {
        tail.contains(&format!("{interpreter} {target}"))
            || tail.contains(&format!("{interpreter} < {target}"))
    })
}

fn download_target_and_tail(lower: &str) -> Option<(String, &str)> {
    if !contains_command_word(lower, "curl") && !contains_command_word(lower, "wget") {
        return None;
    }
    for marker in [
        " --output=",
        " --output ",
        " --output-document=",
        " --output-document ",
        " -o ",
        " -o",
        " > ",
        " >",
    ] {
        let Some(marker_start) = lower.find(marker) else {
            continue;
        };
        let mut start = marker_start + marker.len();
        let bytes = lower.as_bytes();
        if marker == " >" && bytes.get(start) == Some(&b'>') {
            continue;
        }
        let quote = bytes
            .get(start)
            .copied()
            .filter(|byte| matches!(byte, b'\'' | b'"'));
        if quote.is_some() {
            start += 1;
        }
        let end = lower[start..]
            .char_indices()
            .find_map(|(offset, character)| {
                if quote.is_some_and(|quote| character == char::from(quote))
                    || (quote.is_none() && character.is_ascii_whitespace())
                {
                    Some(start + offset)
                } else {
                    None
                }
            })
            .unwrap_or(lower.len());
        let target = lower[start..end].trim();
        if !target.is_empty() {
            return Some((target.to_owned(), &lower[end..]));
        }
    }
    None
}

fn contains_command_word(value: &str, word: &str) -> bool {
    value
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
        })
        .any(|part| part == word)
}

fn sed_uses_gnu_in_place(arguments: &[ShellWord]) -> bool {
    let Some(index) = literal_position(arguments, "-i") else {
        return false;
    };
    arguments
        .get(index + 1)
        .and_then(|argument| argument.literal.as_deref())
        .is_some_and(|next| !next.is_empty())
}

fn sed_uses_bsd_empty_suffix(arguments: &[ShellWord]) -> bool {
    let Some(index) = literal_position(arguments, "-i") else {
        return false;
    };
    arguments
        .get(index + 1)
        .and_then(|argument| argument.literal.as_deref())
        == Some("")
}

fn sed_script_executes_command(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower == "e"
        || lower.starts_with("e ")
        || lower.contains(";e ")
        || lower.contains("; e ")
        || (lower.starts_with('s')
            && lower
                .rsplit_once('/')
                .is_some_and(|(_, flags)| flags.contains('e')))
}
