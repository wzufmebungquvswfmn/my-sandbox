# My Sandbox 用户手册

基于 Rust 的轻量级受限命令执行沙箱，面向人类用户和 Agent 使用。支持命令白名单、超时控制、环境变量隔离、审计日志，并提供 CLI 与 HTTP API。

本项目在 Linux/WSL2 上提供“有限度的隔离执行”能力（namespace + chroot + no_new_privs + 可选 seccomp strict）。Windows 走非隔离路径。

---

## 功能概览

* 命令白名单
* 超时控制
* 环境变量白名单
* 审计记录
* CLI 与 HTTP API
* 扩展能力（本地脚本）
* Linux/WSL2 隔离执行（可选）
* Bench 子命令（延迟统计）

---

## 构建

```bash
cargo build --release
```

产物位于 `target/release/my_sandbox`（Windows 为 `my_sandbox.exe`）。

---

## CLI 使用

### run - 执行命令

```bash
my_sandbox run --cmd <命令> [选项]
```

参数：

* `--cmd` 要执行的命令（必填）
* `--arg` 命令参数（可多次）
* `--cwd` 工作目录
* `--timeout` 超时秒数（默认 10）
* `--env` 环境变量 `KEY=VALUE`（可多次）

示例：

```bash
my_sandbox run --cmd echo --arg hello --arg world
my_sandbox run --cmd python3 --arg script.py --cwd ./workspace --timeout 15
my_sandbox run --cmd node --arg app.js --env MODE=test --env LANG=en_US
my_sandbox run --cmd python3 --arg=-c --arg="print('hi')"
```

### invoke - 调用扩展能力（本地脚本）

```bash
my_sandbox invoke --name <扩展名> [选项]
```

参数：

* `--name` 扩展名（必填）
* `--arg` 额外参数（可多次）
* `--cwd` 覆盖执行目录
* `--timeout` 覆盖超时
* `--env` 额外环境变量

示例：

```bash
my_sandbox invoke --name echo --arg world
```

### bench - 简单基准统计

```bash
my_sandbox bench --cmd echo --arg hi --n 1000
```

输出 `avg/p50/p95/p99` 延迟（毫秒）。

### serve - 启动 HTTP 服务

```bash
my_sandbox serve --addr 127.0.0.1:3000
```

服务支持 Ctrl+C 优雅退出。启用隔离时（`[isolation].enabled = true`）通常需要 `sudo` 启动，例如：

```bash
sudo my_sandbox serve --addr 127.0.0.1:3000
```

---

## HTTP API

### GET /health

```bash
curl http://127.0.0.1:3000/health
```

### POST /execute

```bash
curl -X POST http://127.0.0.1:3000/execute \
  -H "Content-Type: application/json" \
  -d '{
    "command": "echo",
    "args": ["hello"],
    "timeout_secs": 5
  }'
```

### POST /execute/batch

```bash
curl -X POST http://127.0.0.1:3000/execute/batch \
  -H "Content-Type: application/json" \
  -d '[
    {"command":"echo","args":["a"]},
    {"command":"echo","args":["b"]}
  ]'
```

### GET /extensions

```bash
curl http://127.0.0.1:3000/extensions
```

### POST /extensions/{name}/invoke

```bash
curl -X POST http://127.0.0.1:3000/extensions/echo/invoke \
  -H "Content-Type: application/json" \
  -d '{"args":["hello"]}'
```

### GET /executions

```bash
curl http://127.0.0.1:3000/executions
```

### GET /executions/{id}

```bash
curl http://127.0.0.1:3000/executions/<request_id>
```

### GET /metrics

```bash
curl http://127.0.0.1:3000/metrics
```

---

## 策略配置（sandbox.toml）

```toml
allowed_commands = ["echo", "python", "python3", "node", "cargo", "ls", "cat", "pwd", "date", "whoami"]
max_timeout_secs = 30
allowed_env_keys = ["PATH", "HOME", "USER", "LANG", "MODE"]
```

### 扩展能力（本地脚本）

```toml
[extensions.echo]
command = "echo"
args = ["hello-from-extension"]
```

### Linux/WSL2 隔离执行（可选）

```toml
[isolation]
enabled = true
rootfs = "/var/lib/my_sandbox/rootfs"
mount_proc = true
net_namespace = false
seccomp_strict = false
```

说明：

* 仅在 Linux/WSL2 生效，Windows 不启用。
* 通常需要 `sudo` 执行服务端。
* `seccomp_strict = true` 会极其严格，很多命令会直接失败。
* 可用 `my_sandbox init-rootfs` 快速生成最小 rootfs（依赖 busybox，自动创建全量 busybox 命令链接）。
* 启用隔离后，执行的命令必须存在于 rootfs 中（否则会报 “No such file or directory”）。

### WSL2 rootfs 构建

优先用自动方式：

```bash
sudo target/release/my_sandbox init-rootfs
```

可选参数：

* `--rootfs` rootfs 目录（默认取 `sandbox.toml` 中的 `isolation.rootfs`，若未配置则使用 `/var/lib/my_sandbox/rootfs`）
* `--busybox` busybox 路径（默认从 PATH 查找）

说明：`init-rootfs` 会创建全量 busybox 命令软链接，避免白名单命令缺失导致执行失败。

手动方式（需要时）：

1. 创建 rootfs 目录  
   `sudo mkdir -p /var/lib/my_sandbox/rootfs`
2. 安装 busybox（若未安装）  
   `sudo apt-get update`  
   `sudo apt-get install -y busybox`
3. 构建最小 rootfs  
   `sudo mkdir -p /var/lib/my_sandbox/rootfs/{bin,lib,lib64,proc,usr}`  
   `sudo cp /bin/busybox /var/lib/my_sandbox/rootfs/bin/`
4. 拷贝依赖库（按你系统输出为准）  
   `ldd /bin/busybox`  
   复制输出中的依赖库到 rootfs（示例）：  
   `sudo cp /lib/x86_64-linux-gnu/libc.so.6 /var/lib/my_sandbox/rootfs/lib/`  
   `sudo cp /lib64/ld-linux-x86-64.so.2 /var/lib/my_sandbox/rootfs/lib64/`

### 隔离机制说明（简述）

本项目的隔离是“有限度隔离”，基于 Linux 内核能力组合而成，适合受控命令执行场景，并非完整容器级沙箱。核心机制包括：

* `unshare` 创建新的 namespace（`mount/pid/uts/ipc`，可选 `net`）
* `chroot` 切换到 `rootfs` 作为新的根目录
* `no_new_privs` 防止进程提权
* 可选 `seccomp strict`（非常严格）

未覆盖的隔离能力包括（示例）：cgroups 资源限制、user namespace、精细化 syscall 策略、只读根/挂载白名单、网络过滤等。

---

## 执行状态

|状态|含义|
|-|-|
|`succeeded`|执行完成，exit code 0|
|`failed`|执行完成，exit code 非 0|
|`rejected`|被策略拒绝，未执行|
|`timed_out`|超时被终止|

---

## 运行测试

```bash
cargo test
```

当前测试覆盖：白名单拒绝、超时策略、环境变量过滤、扩展调用、并发批量、输出截断、超时 kill 验证。
