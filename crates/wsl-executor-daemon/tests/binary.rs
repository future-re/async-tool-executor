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
    let config_path =
        std::env::temp_dir().join(format!("ate-daemon-test-{}.json", std::process::id()));
    let workspace = std::env::temp_dir();
    std::fs::write(
        &config_path,
        serde_json::json!({"workspace_root": workspace}).to_string(),
    )
    .expect("write test config");

    let mut child = Command::new(env!("CARGO_BIN_EXE_wsl-executor-daemon"))
        .arg("--config")
        .arg(&config_path)
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
    let _ = std::fs::remove_file(&config_path);
    let _ = stdin.flush().await;
}
