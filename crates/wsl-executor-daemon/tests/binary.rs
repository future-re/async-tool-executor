#![cfg(unix)]
use executor_protocol::{
    ClientMessage, DEFAULT_MAX_FRAME_SIZE, ExecutionRequest, PROTOCOL_VERSION, ServerMessage,
    read_frame, write_frame,
};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// End-to-end check against the real daemon binary: spawn it as a child
/// process, drive a full protocol session over piped stdio, and confirm the
/// executor closure works outside an in-memory duplex.
#[tokio::test]
async fn daemon_binary_serves_a_protocol_session_over_stdio() {
    let test_home = std::env::temp_dir().join(format!("ate-daemon-test-{}", std::process::id()));
    let plugin_dir = test_home.join(".local/share/ate/plugins/com.example.echo");
    std::fs::create_dir_all(&plugin_dir).expect("create test plugin directory");
    let example =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/echo-plugin");
    std::fs::copy(example.join("tool.json"), plugin_dir.join("tool.json"))
        .expect("copy plugin manifest");
    std::fs::copy(example.join("plugin.py"), plugin_dir.join("plugin.py"))
        .expect("copy plugin executable");
    let workspace = test_home.join("work");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let config_path = test_home.join("config.json");
    std::fs::write(
        &config_path,
        serde_json::json!({"workspace_root": test_home}).to_string(),
    )
    .expect("write test config");

    let mut child = Command::new(env!("CARGO_BIN_EXE_wsl-executor-daemon"))
        .arg("--config")
        .arg(&config_path)
        .env("HOME", &test_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn daemon binary");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    let result = async {
        write_frame(
            &mut stdin,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                token: "stdio".into(),
                workspace: workspace.to_string_lossy().into_owned(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            read_frame(&mut stdout, DEFAULT_MAX_FRAME_SIZE)
                .await
                .unwrap(),
            Some(ServerMessage::HelloAck { .. })
        ));

        write_frame(
            &mut stdin,
            &ClientMessage::ListTools {
                request_id: "binary-tools".into(),
            },
        )
        .await
        .unwrap();
        let tools = match read_frame(&mut stdout, DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap()
            .unwrap()
        {
            ServerMessage::Tools { tools, .. } => tools,
            unexpected => panic!("unexpected daemon response: {unexpected:?}"),
        };
        assert!(tools.iter().any(|tool| tool.name == "example_echo"));

        write_frame(
            &mut stdin,
            &ClientMessage::Execute {
                request: ExecutionRequest {
                    id: "plugin-round-trip".into(),
                    tool: "example_echo".into(),
                    arguments: serde_json::json!({"value": "hello-plugin"}),
                },
                timeout_ms: Some(2_000),
            },
        )
        .await
        .unwrap();
        let plugin_result = loop {
            match read_frame(&mut stdout, DEFAULT_MAX_FRAME_SIZE)
                .await
                .unwrap()
                .unwrap()
            {
                ServerMessage::Completed { result } => break result,
                ServerMessage::Accepted { .. } | ServerMessage::Started { .. } => {}
                unexpected => panic!("unexpected daemon response: {unexpected:?}"),
            }
        };
        assert!(!plugin_result.is_error);
        assert_eq!(plugin_result.content, "hello-plugin");

        write_frame(
            &mut stdin,
            &ClientMessage::Execute {
                request: ExecutionRequest {
                    id: "binary-round-trip".into(),
                    tool: "shell".into(),
                    arguments: serde_json::json!({"argv": ["echo", "hello-daemon"]}),
                },
                timeout_ms: Some(2_000),
            },
        )
        .await
        .unwrap();

        let result = loop {
            match read_frame(&mut stdout, DEFAULT_MAX_FRAME_SIZE)
                .await
                .unwrap()
                .unwrap()
            {
                ServerMessage::Completed { result } => break result,
                ServerMessage::Accepted { .. } | ServerMessage::Started { .. } => {}
                unexpected => panic!("unexpected daemon response: {unexpected:?}"),
            }
        };
        assert!(!result.is_error);
        assert_eq!(result.content["stdout"], "hello-daemon\n");

        write_frame(&mut stdin, &ClientMessage::Shutdown)
            .await
            .unwrap();
        assert!(matches!(
            read_frame(&mut stdout, DEFAULT_MAX_FRAME_SIZE)
                .await
                .unwrap(),
            Some(ServerMessage::ShutdownAck)
        ));
        Result::<(), String>::Ok(())
    }
    .await;

    if let Err(error) = result {
        let mut stderr_text = String::new();
        let _ = stderr.read_to_string(&mut stderr_text).await;
        panic!("{error}; daemon stderr: {stderr_text}");
    }

    let status = child.wait().await.expect("wait for daemon");
    assert!(status.success(), "daemon exited with {status}");
    let _ = std::fs::remove_dir_all(&test_home);
    let _ = stdin.flush().await;
}
