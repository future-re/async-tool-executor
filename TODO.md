# TODO

当前状态：已完成 Cargo Workspace 拆分，以及 `Windows Client → Protocol → WSL Daemon → Executor Core → Linux Process` 的最小闭环。后续优先把它从“可运行原型”推进到“安全、可靠、可部署的 Runtime”。

## P0：建立可信的 WSL 执行边界

- [ ] 引入服务端 `SandboxPolicy`，集中定义允许的工作区根目录、基础环境变量、可执行程序、资源上限和网络策略；客户端请求只能收紧策略，不能放宽服务端限制。
- [ ] 对 `cwd` 执行 `canonicalize` 和工作区根目录校验，拒绝路径穿越、符号链接逃逸、Windows 挂载盘及未授权目录。
- [ ] 过滤危险或敏感环境变量，校验变量名称，并明确 `PATH`、`HOME`、`TMPDIR`、代理变量及凭证变量的继承规则。
- [ ] 修正 `ResourceLimits` 合并语义，按照 `min(requested, policy_max)` 计算有效限制；统一处理 stdout/stderr 上限，避免请求覆盖守护进程默认值。
- [ ] 在 `rlimit` 之外增加 cgroup v2 限制，对整个进程树约束 CPU、内存和 PID 数，并在任务结束后删除对应 cgroup。
- [ ] 明确沙箱强度和威胁模型；按需要增加低权限用户、mount/PID/network namespace、只读挂载及 seccomp，避免把当前进程级限制描述为完整安全沙箱。

## P0：补齐 Host/Guest 可用链路

- [ ] 为 `windows-agent-client` 实现完整事件分发，向调用方暴露 `Accepted`、`Started`、`Progress`、`CancelAcknowledged`、`Tools` 和连接状态；当前客户端只消费终态结果。
- [ ] 为工具发现、取消和其他控制请求增加 correlation ID，支持同一连接上的并发控制请求及准确响应匹配。
- [ ] 增加 stdout/stderr 分块流式事件、单调递增 sequence 和最终截断信息，避免长任务只能在结束后返回全部输出。
- [ ] 将 daemon 的无界响应队列改成有界队列，并定义背压、慢客户端和输出丢弃策略，防止输出过快导致内存无限增长。
- [ ] 增加握手、启动和无响应超时；writer/reader 任一方向断开时立即通知另一方向、失败所有 pending request 并清理 WSL 子进程。
- [ ] Windows client 退出时先发送 `Shutdown` 并等待确认，超时后再强制终止 `wsl.exe`；同时收集 daemon stderr 用于启动失败诊断。
- [ ] 增加 WSL 发行版发现、daemon 路径配置、版本兼容检查与安装/升级流程，避免依赖预先手工部署二进制。
- [ ] 在真实 Windows + WSL2 环境验证并编译 `windows-agent-client`；增加 Windows CI 和跨边界端到端测试。

## P1：完善执行内核

- [ ] 将 JSON Schema 和工具自定义参数校验提前到获取并发许可之前，避免无效请求占用配额或阻塞独占任务（`crates/executor-core/src/scheduler.rs`、`invocation.rs`）。
- [ ] 为 `ExecutionObserver` 增加 `Queued`、`Finished`、`Failed`、`Cancelled` 等事件，形成完整且只产生一次终态的生命周期。
- [ ] 为 `spawn_blocking` worker 增加独立并发上限，防止同步任务超时后继续运行并耗尽 Tokio blocking pool。
- [ ] 明确 `execute_all` 的取消粒度：批次级 token、单任务 token，以及某个任务失败时是否影响同批其他任务。
- [ ] 将 `ExecutionResult` 的成功内容与结构化错误拆开，避免依赖 `is_error + content["error"]` 的隐式约定。
- [ ] 避免 `ToolExecutor::new` 原地修改配置；拆分 core、daemon 和 sandbox 配置，并在启动时完成校验。
- [ ] 补齐 daemon 终态任务清理和 `JoinError` 诊断，确保任务 panic、writer 失败及 shutdown 都不会静默丢失状态。

## P1：测试与质量保障

- [ ] 增加 daemon 取消、重复 execution ID、协议版本不匹配、客户端断连、shutdown 期间仍有任务等集成测试。
- [ ] 增加 malformed frame、超大 frame、无效 JSON、半包和提前 EOF 测试；考虑对 framing 与消息状态机进行 fuzz/property testing。
- [ ] 增加路径逃逸、符号链接、环境变量注入、资源上限不可放宽及 fork bomb 防护测试。
- [ ] 增加高并发和慢消费者压力测试，验证公平性、背压、内存上限以及进程树回收。
- [ ] 配置 CI：`fmt`、`clippy -D warnings`、workspace tests、Linux/Windows 构建和真实 WSL smoke test。

## P2：可观测性、文档与发布

- [ ] 为 daemon 增加结构化 tracing、请求耗时、排队时间、活跃任务数、取消数和资源终止原因等指标；日志只能写 stderr，不能污染 stdout 协议流。
- [ ] 为协议错误、传输错误、工具错误、非零退出、信号终止、超时和策略拒绝建立稳定错误码。
- [ ] 补充 README：部署方式、协议示例、安全边界、配置说明和故障排查；明确 `spawn_blocking` 只能协作式取消，无法强杀 OS 线程。
- [ ] 添加实际 `LICENSE` 文件，并补充版本策略、changelog 和发布产物构建流程。

## 已完成

- [x] 拆分 `executor-protocol`、`executor-core`、`wsl-runtime`、`wsl-executor-daemon` 和 `windows-agent-client`。
- [x] 实现版本握手、length-prefixed JSON framing、执行、取消、工具发现和优雅关闭消息。
- [x] 实现 `ToolRegistry::list()`，返回稳定排序的工具及 schema。
- [x] 实现并发限制、独占工具、超时、取消、Panic 隔离和 Observer 基础接口。
- [x] 实现 Linux 子进程 stdout/stderr 捕获、`rlimit`、进程组终止和输出截断。
- [x] 增加 core、protocol、WSL runtime 及 daemon 内存通道端到端测试。
