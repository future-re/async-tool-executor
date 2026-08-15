#![cfg(unix)]
use executor_core::{ExecutorConfig, ToolExecutor, ToolRegistry};
use executor_protocol::{
    ClientMessage, DEFAULT_MAX_FRAME_SIZE, ExecutionRequest, PROTOCOL_VERSION, ServerMessage,
    read_frame, write_frame,
};
use executor_tools::register_core_tools;
use std::collections::HashMap;
use std::sync::Arc;
use wsl_executor_daemon::serve;
use wsl_runtime::{NativeShell, NativeShellConfig, ShellTool};

#[tokio::test]
async fn tool_discovery_reports_base_tools_plus_shell() {
    let cwd = std::env::current_dir().unwrap();
    let shell = NativeShell::new(NativeShellConfig {
        base_env: HashMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        cwd: Some(cwd.clone()),
        ..NativeShellConfig::default()
    });
    let mut registry = ToolRegistry::new();
    register_core_tools(&mut registry).unwrap();
    registry.register(ShellTool::new(shell)).unwrap();
    let tools = registry.list();
    let names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();

    let mut expected = vec!["Read", "Write", "Edit", "Glob", "Grep", "WebFetch", "shell"];
    expected.sort_unstable();
    let mut got = names.clone();
    got.sort_unstable();
    assert_eq!(got, expected, "registered tools should be discoverable");

    let executor = ToolExecutor::new(
        registry.clone(),
        ExecutorConfig {
            cwd,
            env: Arc::new(HashMap::new()),
            concurrency_limit: 2,
            ..Default::default()
        },
    );

    let (client, server) = tokio::io::duplex(64 * 1024);
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    let daemon = tokio::spawn(serve(server_read, server_write, executor, tools));

    write_frame(
        &mut client_write,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame(&mut client_read, DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap(),
        Some(ServerMessage::HelloAck { .. })
    ));

    write_frame(
        &mut client_write,
        &ClientMessage::ListTools {
            request_id: "tools-1".into(),
        },
    )
    .await
    .unwrap();
    let tools = match read_frame(&mut client_read, DEFAULT_MAX_FRAME_SIZE)
        .await
        .unwrap()
        .unwrap()
    {
        ServerMessage::Tools { request_id, tools } => {
            assert_eq!(request_id, "tools-1");
            tools
        }
        unexpected => panic!("unexpected daemon response: {unexpected:?}"),
    };
    assert_eq!(
        tools.len(),
        expected.len(),
        "daemon should serve every registered tool"
    );

    write_frame(&mut client_write, &ClientMessage::Shutdown)
        .await
        .unwrap();
    assert!(matches!(
        read_frame(&mut client_read, DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap(),
        Some(ServerMessage::ShutdownAck)
    ));
    daemon.await.unwrap().unwrap();
}

#[tokio::test]
async fn protocol_request_reaches_the_wsl_executor_and_returns_a_result() {
    let cwd = std::env::current_dir().unwrap();
    let shell = NativeShell::new(NativeShellConfig {
        base_env: HashMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        cwd: Some(cwd.clone()),
        ..NativeShellConfig::default()
    });
    let mut registry = ToolRegistry::new();
    registry.register(ShellTool::new(shell)).unwrap();
    let tools = registry.list();
    let executor = ToolExecutor::new(
        registry,
        ExecutorConfig {
            cwd,
            env: Arc::new(HashMap::new()),
            concurrency_limit: 2,
            ..Default::default()
        },
    );

    let (client, server) = tokio::io::duplex(64 * 1024);
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    let daemon = tokio::spawn(serve(server_read, server_write, executor, tools));

    write_frame(
        &mut client_write,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame(&mut client_read, DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap(),
        Some(ServerMessage::HelloAck { .. })
    ));

    write_frame(
        &mut client_write,
        &ClientMessage::Execute {
            request: ExecutionRequest {
                id: "end-to-end".into(),
                tool: "shell".into(),
                arguments: serde_json::json!({"argv": ["echo", "from-wsl"]}),
            },
            timeout_ms: Some(2_000),
        },
    )
    .await
    .unwrap();

    let result = loop {
        match read_frame(&mut client_read, DEFAULT_MAX_FRAME_SIZE)
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
    assert_eq!(result.content["stdout"], "from-wsl\n");

    write_frame(&mut client_write, &ClientMessage::Shutdown)
        .await
        .unwrap();
    assert!(matches!(
        read_frame(&mut client_read, DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap(),
        Some(ServerMessage::ShutdownAck)
    ));
    daemon.await.unwrap().unwrap();
}
