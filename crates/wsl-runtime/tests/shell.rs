use executor_core::{ExecutionOptions, ExecutorConfig, ToolExecutor, ToolRegistry};
use executor_protocol::ExecutionRequest;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wsl_runtime::{
    CommandOutput, CommandSpec, NativeShell, NativeShellConfig, ResourceLimits, Shell, ShellTool,
};

fn request(id: impl Into<String>, tool: &str) -> ExecutionRequest {
    ExecutionRequest {
        id: id.into(),
        tool: tool.into(),
        arguments: serde_json::json!({}),
    }
}

fn config(limit: usize) -> ExecutorConfig {
    ExecutorConfig {
        concurrency_limit: limit,
        ..Default::default()
    }
}

fn native_shell() -> NativeShell {
    NativeShell::new(NativeShellConfig {
        base_env: HashMap::from([("PATH".into(), "/usr/bin:/bin:/usr/local/bin".into())]),
        ..NativeShellConfig::default()
    })
}

async fn run_shell(
    shell: Arc<dyn Shell>,
    argv: Vec<&str>,
    limits: ResourceLimits,
) -> CommandOutput {
    shell
        .run(
            CommandSpec {
                argv: argv.into_iter().map(str::to_string).collect(),
                cwd: None,
                env: HashMap::new(),
                limits,
            },
            CancellationToken::new(),
        )
        .await
        .expect("shell run should not fail")
}

#[tokio::test]
async fn native_shell_captures_stdout_and_exit_code() {
    let output = run_shell(
        Arc::new(native_shell()),
        vec!["echo", "hello"],
        ResourceLimits::default(),
    )
    .await;
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "hello\n");
    assert!(!output.terminated);
}

#[tokio::test]
async fn native_shell_structures_exit_code_and_stderr() {
    let output = run_shell(
        Arc::new(native_shell()),
        vec!["/bin/sh", "-c", "echo err >&2; exit 7"],
        ResourceLimits::default(),
    )
    .await;
    assert_eq!(output.exit_code, Some(7));
    assert_eq!(output.stderr, "err\n");
}

#[tokio::test]
async fn native_shell_runs_in_the_requested_working_directory() {
    let dir = std::env::temp_dir().join(format!("ate-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let shell = Arc::new(native_shell());
    let output = shell
        .run(
            CommandSpec {
                argv: vec!["pwd".into()],
                cwd: Some(dir.clone()),
                env: HashMap::new(),
                limits: ResourceLimits::default(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(output.stdout.trim(), dir.to_str().unwrap());
}

#[tokio::test]
async fn native_shell_applies_base_env_and_spec_overrides() {
    let shell = Arc::new(native_shell());
    let output = shell
        .run(
            CommandSpec {
                argv: vec!["/bin/sh".into(), "-c".into(), "printf '%s' \"$FOO\"".into()],
                cwd: None,
                env: HashMap::from([("FOO".into(), "override".into())]),
                limits: ResourceLimits::default(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(output.stdout, "override");
}

#[tokio::test]
async fn native_shell_applies_resource_limits() {
    let shell = Arc::new(native_shell());
    let output = shell
        .run(
            CommandSpec {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf '%s %s' \"$(ulimit -v)\" \"$(ulimit -t)\"".into(),
                ],
                cwd: None,
                env: HashMap::new(),
                limits: ResourceLimits {
                    memory_bytes: Some(64 * 1024 * 1024),
                    cpu_seconds: Some(5),
                    ..ResourceLimits::default()
                },
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(output.stdout, "65536 5");
}

#[tokio::test]
async fn native_shell_cancellation_terminates_the_process_tree() {
    let cancellation = CancellationToken::new();
    let shell = Arc::new(native_shell());
    let shell = Arc::clone(&shell);
    let token = cancellation.clone();
    let task = tokio::spawn(async move {
        shell
            .run(
                CommandSpec {
                    argv: vec!["/bin/sh".into(), "-c".into(), "sleep 100 & wait".into()],
                    cwd: None,
                    env: HashMap::new(),
                    limits: ResourceLimits::default(),
                },
                token,
            )
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancellation.cancel();
    let output = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("process tree should terminate promptly")
        .unwrap();
    assert!(output.terminated);
}

#[tokio::test]
async fn native_shell_truncates_overflowing_stdout() {
    let output = run_shell(
        Arc::new(native_shell()),
        vec!["/bin/sh", "-c", "seq 1 100000"],
        ResourceLimits {
            max_stdout: 1024,
            ..ResourceLimits::default()
        },
    )
    .await;
    assert!(output.truncated);
    assert_eq!(output.exit_code, Some(0));
}

#[tokio::test]
async fn shell_tool_returns_structured_results_through_the_executor() {
    let mut tools = ToolRegistry::new();
    tools.register(ShellTool::new(native_shell()));
    let executor = ToolExecutor::new(tools, config(2));

    let result = executor
        .execute(
            ExecutionRequest {
                arguments: json!({"argv": ["echo", "hello"]}),
                ..request("s1", "shell")
            },
            ExecutionOptions::new(),
        )
        .await;
    assert!(!result.is_error);
    assert_eq!(result.content["exit_code"], 0);
    assert_eq!(result.content["stdout"], "hello\n");
}

#[tokio::test]
async fn shell_tool_nonzero_exit_is_a_result_not_an_error() {
    let mut tools = ToolRegistry::new();
    tools.register(ShellTool::new(native_shell()));
    let executor = ToolExecutor::new(tools, config(2));

    let result = executor
        .execute(
            ExecutionRequest {
                arguments: json!({"argv": ["/bin/sh", "-c", "exit 3"]}),
                ..request("s2", "shell")
            },
            ExecutionOptions::new(),
        )
        .await;
    assert!(!result.is_error);
    assert_eq!(result.content["exit_code"], 3);
}

#[tokio::test]
async fn shell_tool_spawn_failure_is_an_execution_error() {
    let mut tools = ToolRegistry::new();
    tools.register(ShellTool::new(native_shell()));
    let executor = ToolExecutor::new(tools, config(2));

    let result = executor
        .execute(
            ExecutionRequest {
                arguments: json!({"argv": ["/no/such/program"]}),
                ..request("s3", "shell")
            },
            ExecutionOptions::new(),
        )
        .await;
    assert!(result.is_error);
    assert_eq!(result.content["error"]["kind"], "execution_error");
}

#[tokio::test]
async fn executor_timeout_kills_the_spawned_process_tree() {
    let mut tools = ToolRegistry::new();
    tools.register(ShellTool::new(native_shell()));
    let executor = ToolExecutor::new(tools, config(1));

    let pid_file = std::env::temp_dir().join(format!("ate-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pid_file);
    let script = format!(
        "echo $$ > {pid}; sleep 300 & echo $! >> {pid}; wait",
        pid = pid_file.display()
    );

    let result = executor
        .execute(
            ExecutionRequest {
                arguments: json!({"argv": ["/bin/sh", "-c", script]}),
                ..request("t1", "shell")
            },
            ExecutionOptions::new().with_deadline(Instant::now() + Duration::from_millis(400)),
        )
        .await;
    assert!(result.is_error);
    assert_eq!(result.content["error"]["kind"], "timed_out");

    let pids = std::fs::read_to_string(&pid_file).expect("pid file should exist");
    let _ = std::fs::remove_file(&pid_file);
    for pid in pids.lines() {
        let pid: i32 = pid.trim().parse().expect("valid pid");
        let gone = async {
            for _ in 0..40 {
                if unsafe { libc::kill(pid, 0) } != 0 {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            false
        };
        assert!(gone.await, "process {pid} survived the deadline");
    }
}
