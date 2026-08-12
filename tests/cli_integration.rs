use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use howto::config::{self, Config};
use howto::paths::Paths;
use howto::{setup, shell};
use sha2::{Digest, Sha256};

fn stage_pending(paths: &Paths, session: &str, command: &str) {
    let session_hash = hex::encode(Sha256::digest(session.as_bytes()));
    let path = paths
        .runtime_dir
        .join(format!("pending-{session_hash}.json"));
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "created_at": created_at,
            "session": session,
            "command": command,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn fake_server(socket: &Path, commands: &[&str]) -> (String, thread::JoinHandle<()>) {
    let listener = UnixListener::bind(socket).unwrap();
    let commands = commands.iter().map(ToString::to_string).collect::<Vec<_>>();
    let handle = thread::spawn(move || {
        for command in commands {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert_eq!(request["n"], 1, "llama-server accepts only n=1");
            let body = serde_json::json!({
                "choices": [{
                    "message": {"role": "assistant", "content": command},
                    "finish_reason": "stop"
                }]
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            stream.flush().unwrap();
        }
    });
    (format!("unix://{}", socket.display()), handle)
}

fn fake_health_server(socket: &Path) -> (String, thread::JoinHandle<()>) {
    let listener = UnixListener::bind(socket).unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = [0_u8; 4_096];
        let length = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..length]).starts_with("GET /health "));
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        )
        .unwrap();
        stream.flush().unwrap();
    });
    (format!("unix://{}", socket.display()), handle)
}

fn fake_models_fallback_server(socket: &Path) -> (String, thread::JoinHandle<()>) {
    let listener = UnixListener::bind(socket).unwrap();
    let handle = thread::spawn(move || {
        for (route, status) in [("/health", "404 Not Found"), ("/v1/models", "200 OK")] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 4_096];
            let length = stream.read(&mut request).unwrap();
            assert!(
                String::from_utf8_lossy(&request[..length]).starts_with(&format!("GET {route} "))
            );
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
            stream.flush().unwrap();
        }
    });
    (format!("unix://{}", socket.display()), handle)
}

fn read_request(stream: &mut UnixStream) -> serde_json::Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4_096];
    let mut expected = None;
    let mut body_start = None;
    loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "client closed before sending a complete request");
        request.extend_from_slice(&buffer[..read]);
        if expected.is_none() {
            if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                expected = Some(header_end + 4 + content_length);
                body_start = Some(header_end + 4);
            }
        }
        if expected.is_some_and(|length| request.len() >= length) {
            break;
        }
    }
    assert!(String::from_utf8_lossy(&request).starts_with("POST /v1/chat/completions"));
    serde_json::from_slice(&request[body_start.unwrap()..expected.unwrap()]).unwrap()
}

fn run_with_response(arguments: &[&str], responses: &[&str]) -> Output {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::under(directory.path().to_path_buf());
    paths.create().unwrap();
    let (server_url, handle) = fake_server(&directory.path().join("provider.sock"), responses);
    let configuration = Config {
        server_url: Some(server_url),
        ..Config::default()
    };
    config::save(&paths.config_file(), &configuration).unwrap();
    setup::save(&paths, &setup::Receipt::new(None)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(arguments)
        .env("HOWTO_HOME", directory.path())
        .env_remove("HOWTO_MODEL")
        .env_remove("HOWTO_LLAMA_SERVER")
        .env_remove("HOWTO_API_KEY")
        .output()
        .unwrap();
    handle.join().unwrap();
    output
}

#[test]
fn generates_through_replaceable_provider_boundary() {
    let output = run_with_response(&["free", "port", "8080"], &["lsof -ti :8080 | xargs kill"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "lsof -ti :8080 | xargs kill\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("CAUTION"));
}

#[test]
fn query_requires_setup_before_contacting_a_provider() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::under(directory.path().to_path_buf());
    paths.create().unwrap();
    config::save(
        &paths.config_file(),
        &Config {
            server_url: Some(format!(
                "unix://{}",
                directory
                    .path()
                    .join("provider-that-must-not-be-contacted.sock")
                    .display()
            )),
            ..Config::default()
        },
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["show", "current", "directory"])
        .env("HOWTO_HOME", directory.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("run `howto setup` first"));
}

#[test]
fn setup_with_a_configured_provider_skips_the_local_model() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::under(directory.path().to_path_buf());
    paths.create().unwrap();
    config::save(
        &paths.config_file(),
        &Config {
            server_url: Some("https://provider.example".into()),
            ..Config::default()
        },
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["setup", "--yes", "--no-shell"])
        .env("HOWTO_HOME", directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(setup::load(&paths).unwrap().is_some());
    assert!(std::fs::read_dir(paths.models_dir())
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn setup_manages_zsh_integration_idempotently() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let state = directory.path().join("state");
    std::fs::create_dir_all(&home).unwrap();
    let paths = Paths::under(state.clone());
    paths.create().unwrap();
    config::save(
        &paths.config_file(),
        &Config {
            server_url: Some("https://provider.example".into()),
            ..Config::default()
        },
    )
    .unwrap();

    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_howto"))
            .args(["setup", "--yes", "--shell", "zsh"])
            .env("HOWTO_HOME", &state)
            .env("HOME", &home)
            .env("ZDOTDIR", &home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let startup = std::fs::read_to_string(home.join(".zshrc")).unwrap();
    assert_eq!(startup.matches(">>> HowTo shell integration").count(), 1);
    assert!(paths.shell_dir().join("howto.zsh").is_file());

    let moved_zdotdir = home.join("new-zdotdir");
    std::fs::create_dir_all(&moved_zdotdir).unwrap();
    let migrated = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["setup", "--yes", "--shell", "zsh"])
        .env("HOWTO_HOME", &state)
        .env("HOME", &home)
        .env("ZDOTDIR", &moved_zdotdir)
        .output()
        .unwrap();
    assert!(
        migrated.status.success(),
        "{}",
        String::from_utf8_lossy(&migrated.stderr)
    );
    assert!(!std::fs::read_to_string(home.join(".zshrc"))
        .unwrap()
        .contains("HowTo shell integration"));
    let receipt = setup::load(&paths).unwrap().unwrap();
    assert_eq!(receipt.shell.as_deref(), Some("zsh"));
    assert_eq!(
        receipt.shell_startup_files,
        vec![moved_zdotdir.join(".zshrc")]
    );

    let status = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["shell", "status", "--json"])
        .env("HOWTO_HOME", &state)
        .env("HOME", &home)
        .env("ZDOTDIR", &moved_zdotdir)
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["configured"], true);
    assert_eq!(status["active_in_this_shell"], false);

    let third_zdotdir = home.join("third-zdotdir");
    std::fs::create_dir_all(&third_zdotdir).unwrap();
    let disabled = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["shell", "disable", "--shell", "zsh"])
        .env("HOWTO_HOME", &state)
        .env("HOME", &home)
        .env("ZDOTDIR", &third_zdotdir)
        .output()
        .unwrap();
    assert!(
        disabled.status.success(),
        "{}",
        String::from_utf8_lossy(&disabled.stderr)
    );
    assert!(!std::fs::read_to_string(moved_zdotdir.join(".zshrc"))
        .unwrap()
        .contains("HowTo shell integration"));
    assert!(!paths.shell_dir().join("howto.zsh").exists());
    assert_eq!(setup::load(&paths).unwrap().unwrap().shell, None);
}

#[test]
fn automatic_setup_skips_an_unsupported_login_shell() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let state = directory.path().join("state");
    std::fs::create_dir_all(&home).unwrap();
    let paths = Paths::under(state.clone());
    paths.create().unwrap();
    config::save(
        &paths.config_file(),
        &Config {
            server_url: Some("https://provider.example".into()),
            ..Config::default()
        },
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["setup", "--yes"])
        .env("HOWTO_HOME", &state)
        .env("HOME", &home)
        .env("SHELL", "/usr/bin/nu")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("was skipped"));
    assert_eq!(setup::load(&paths).unwrap().unwrap().shell, None);
}

#[cfg(unix)]
#[test]
fn noninteractive_automatic_setup_skips_a_startup_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let state = directory.path().join("state");
    std::fs::create_dir_all(&home).unwrap();
    let zshrc_target = home.join("managed-zshrc");
    std::fs::write(&zshrc_target, "user configuration\n").unwrap();
    symlink(&zshrc_target, home.join(".zshrc")).unwrap();
    let paths = Paths::under(state.clone());
    paths.create().unwrap();
    config::save(
        &paths.config_file(),
        &Config {
            server_url: Some("https://provider.example".into()),
            ..Config::default()
        },
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["setup", "--yes"])
        .env("HOWTO_HOME", &state)
        .env("HOME", &home)
        .env("ZDOTDIR", &home)
        .env("SHELL", "/bin/zsh")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains(&home.join(".zshrc").display().to_string()));
    assert!(stderr.contains(" -> "));
    assert!(stderr.contains(&zshrc_target.canonicalize().unwrap().display().to_string()));
    assert!(stderr.contains("requires an interactive terminal"));
    assert!(!stderr.contains("BEGIN HOWTO MANAGED BLOCK"));
    assert!(stderr.contains("howto setup --shell zsh"));
    assert!(stderr.contains("HowTo setup is complete."));

    let receipt = setup::load(&paths).unwrap().unwrap();
    assert_eq!(receipt.shell, None);
    assert!(receipt.shell_startup_files.is_empty());
    assert_eq!(
        std::fs::read_link(home.join(".zshrc")).unwrap(),
        zshrc_target
    );
    assert_eq!(
        std::fs::read_to_string(home.join("managed-zshrc")).unwrap(),
        "user configuration\n"
    );
    assert!(!paths.shell_dir().join("howto.zsh").exists());
}

#[cfg(unix)]
#[test]
fn noninteractive_shell_enable_refuses_a_startup_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let state = directory.path().join("state");
    std::fs::create_dir_all(&home).unwrap();
    let zshrc_target = home.join("managed-zshrc");
    std::fs::write(&zshrc_target, "user configuration\n").unwrap();
    symlink(&zshrc_target, home.join(".zshrc")).unwrap();
    let paths = Paths::under(state.clone());
    paths.create().unwrap();
    setup::save(&paths, &setup::Receipt::new(None)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["shell", "enable", "--shell", "zsh"])
        .env("HOWTO_HOME", &state)
        .env("HOME", &home)
        .env("ZDOTDIR", &home)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("requires an interactive terminal"));
    assert!(stderr.contains(&home.join(".zshrc").display().to_string()));
    assert!(stderr.contains(" -> "));
    assert!(stderr.contains(&zshrc_target.canonicalize().unwrap().display().to_string()));
    assert!(!stderr.contains("BEGIN HOWTO MANAGED BLOCK"));
    assert_eq!(setup::load(&paths).unwrap().unwrap().shell, None);
    assert_eq!(
        std::fs::read_to_string(home.join("managed-zshrc")).unwrap(),
        "user configuration\n"
    );
    assert!(!paths.shell_dir().join("howto.zsh").exists());
}

#[test]
fn shell_take_is_one_shot() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::under(directory.path().to_path_buf());
    paths.create().unwrap();
    setup::save(&paths, &setup::Receipt::new(Some("zsh"))).unwrap();
    stage_pending(&paths, "zsh-integration-test", "printf '%s\\n' hello");

    let first = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["shell", "take"])
        .env("HOWTO_HOME", directory.path())
        .env(shell::SESSION_ENV, "zsh-integration-test")
        .output()
        .unwrap();
    assert!(first.status.success());
    assert_eq!(first.stdout, b"printf '%s\\n' hello\n");

    let second = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["shell", "take"])
        .env("HOWTO_HOME", directory.path())
        .env(shell::SESSION_ENV, "zsh-integration-test")
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(second.stdout.is_empty());

    stage_pending(&paths, "zsh-integration-test", "pwd");
    setup::save(&paths, &setup::Receipt::new(None)).unwrap();
    let disabled = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["shell", "take"])
        .env("HOWTO_HOME", directory.path())
        .env(shell::SESSION_ENV, "zsh-integration-test")
        .output()
        .unwrap();
    assert_eq!(disabled.status.code(), Some(1));
    assert!(disabled.stdout.is_empty());
}

#[test]
fn quiet_mode_refuses_dangerous_output() {
    let output = run_with_response(&["--quiet", "delete", "root"], &["rm -rf /"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("quiet output is blocked"));
}

#[test]
fn json_output_has_stable_risk_shape() {
    let output = run_with_response(&["--json", "show", "current", "directory"], &["pwd"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["commands"][0]["command"], "pwd");
    assert_eq!(value["commands"][0]["risk"], "NO_KNOWN_RISK");
    assert_eq!(value["provider"], "configured");
}

#[test]
fn retries_once_when_model_returns_multiple_lines() {
    let output = run_with_response(
        &["show", "current", "directory"],
        &["Here is the command:\npwd\nIt prints the directory.", "pwd"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "pwd\n");
}

#[test]
fn alternatives_use_separate_single_choice_requests() {
    let output = run_with_response(
        &["--json", "-n", "4", "inspect", "this", "directory"],
        &["pwd", "ls -la", "du -sh .", "find . -maxdepth 1 -type f"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["commands"].as_array().unwrap().len(), 4);
}

#[test]
fn doctor_probes_a_configured_provider() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::under(directory.path().to_path_buf());
    paths.create().unwrap();
    let (server_url, handle) = fake_health_server(&directory.path().join("provider.sock"));
    config::save(
        &paths.config_file(),
        &Config {
            server_url: Some(server_url),
            ..Config::default()
        },
    )
    .unwrap();
    setup::save(&paths, &setup::Receipt::new(None)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["doctor", "--json"])
        .env("HOWTO_HOME", directory.path())
        .env_remove("HOWTO_API_KEY")
        .output()
        .unwrap();
    handle.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ready"], true);
    assert_eq!(value["setup_complete"], true);
    assert_eq!(value["provider_healthy"], true);
}

#[test]
fn doctor_rejects_an_unreachable_configured_provider() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::under(directory.path().to_path_buf());
    paths.create().unwrap();
    config::save(
        &paths.config_file(),
        &Config {
            server_url: Some(format!(
                "unix://{}",
                directory.path().join("missing.sock").display()
            )),
            ..Config::default()
        },
    )
    .unwrap();
    setup::save(&paths, &setup::Receipt::new(None)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["doctor", "--json"])
        .env("HOWTO_HOME", directory.path())
        .env_remove("HOWTO_API_KEY")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ready"], false);
    assert_eq!(value["provider_healthy"], false);
}

#[test]
fn doctor_falls_back_to_the_openai_models_route() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::under(directory.path().to_path_buf());
    paths.create().unwrap();
    let (server_url, handle) = fake_models_fallback_server(&directory.path().join("provider.sock"));
    config::save(
        &paths.config_file(),
        &Config {
            server_url: Some(server_url),
            ..Config::default()
        },
    )
    .unwrap();
    setup::save(&paths, &setup::Receipt::new(None)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["doctor", "--json"])
        .env("HOWTO_HOME", directory.path())
        .env_remove("HOWTO_API_KEY")
        .output()
        .unwrap();
    handle.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["provider_healthy"], true);
    assert_eq!(value["setup_complete"], true);
}

#[test]
fn local_doctor_rejects_an_unrelated_executable_and_custom_manifest_is_null() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::under(directory.path().to_path_buf());
    paths.create().unwrap();
    let model = directory.path().join("custom.gguf");
    let mut file = std::fs::File::create(&model).unwrap();
    file.set_len(1_048_577).unwrap();
    file.write_all(b"GGUF").unwrap();
    drop(file);
    config::save(
        &paths.config_file(),
        &Config {
            model_path: Some(model),
            llama_server_path: Some("/usr/bin/true".into()),
            ..Config::default()
        },
    )
    .unwrap();
    setup::save(&paths, &setup::Receipt::new(None)).unwrap();

    let doctor = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["doctor", "--json"])
        .env("HOWTO_HOME", directory.path())
        .env_remove("HOWTO_LLAMA_SERVER")
        .output()
        .unwrap();
    assert_eq!(doctor.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(value["ready"], false);
    assert_eq!(value["llama_server_path"], serde_json::Value::Null);
    assert_eq!(value["llama_server_usable"], serde_json::Value::Null);

    let status = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["model", "--json"])
        .env("HOWTO_HOME", directory.path())
        .output()
        .unwrap();
    assert!(status.status.success());
    let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["installed"], true);
    assert_eq!(value["manifest"], serde_json::Value::Null);
}

#[test]
fn relative_xdg_runtime_directory_is_rejected() {
    let output = Command::new(env!("CARGO_BIN_EXE_howto"))
        .args(["config", "path"])
        .env_remove("HOWTO_HOME")
        .env("XDG_RUNTIME_DIR", "relative-run-directory")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("XDG_RUNTIME_DIR must be an absolute path"));
}
