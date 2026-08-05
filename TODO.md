# TOD

## 高优先级

- [ ] 将参数校验提前到获取许可之前，避免无效请求占用并发配额并阻塞队列（`crates/executor-core/src/scheduler.rs` → `invocation.rs`）
- [ ] 给 `ToolRegistry` 增加 `list()`/`all()` 方法，枚举全部工具及其 schema，供 LLM tool-calling 使用

## 中优先级

- [ ] 为 `ExecutionObserver` 增加 `Finished`/`Failed` 终态事件，补全执行生命周期观测
- [ ] 为 blocking worker 增加数量上限，防止 `spawn_blocking` 任务无限泄漏（`crates/executor-core/src/tool.rs` `run_blocking`）
- [ ] 补充 README 与 license，明确 `spawn_blocking` 取消是协作式、无法强杀 OS 线程的警告

## 低优先级

- [ ] 清理小问题：`ExecutionResult` 独立 error 字段 / `ToolExecutor::new` 原地修改 config / 文档化 `execute_all` 取消粒度
