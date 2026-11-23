# Connection latency and QUIC / 连接耗时与 QUIC

## 中文

2026-11-23 的实机日志反复显示 QUIC 等待约 15 秒后失败，而随后的 TCP + TLS
在 17–23 毫秒内完成连接。无认证探测确认 UDP 上的 TLS 握手成功，QUIC 证书与
同一端口的 TCP 证书一致，因此此次故障不是 UDP 完全被封锁。

根因是 Network.framework / Quinn 的流分配互通：

- 原客户端将 `NWConnectionGroup` 的 QUIC 方向设为双向，连接组预留了隐式流 0；
  后续显式创建的控制流实际编号为 4。
- Quinn 将更低编号的双向流视为已打开，主机 `accept_bi()` 先返回流 0。
  主机在空流 0 上等待控制前缀，客户端在流 4 上等待 HelloAck，直到超时。
- 将**连接组的预留流**设为单向，同时保留**控制与 Bulk 流**为双向，控制流即为 0。
  无需改变 Windows 主机或线上协议。

隔离 Quinn 服务端复现了旧路径在流 0 等待前缀超时；修复后的生产客户端约 8 毫秒
完成本机 Hello / HelloAck 和 Bulk 绑定。互通测试还验证两条服务端主动打开的视频流
可达（使用合成终止帧，非视频解码性能测试）。实机未认证探测也从 15 秒停滞变为立即
响应/关闭，但完整实机认证、实际窗口播放仍需用户更新客户端后验证。

### 超时与回退

- 自动模式仍优先尝试 QUIC；总握手预算为 2.5 秒（带备用地址的首选主机为 1.8 秒），
  包括 QUIC/TLS、控制流、HelloAck 和 Bulk 绑定，而非只等待连接组 `.ready`。
- 显式 QUIC 的总预算为 8 秒（带备用地址的首选主机为 3 秒）。
- TCP 的总预算为 8 秒（带备用地址的首选主机为 2.5 秒），包含 TLS、HelloAck 和
  媒体通道绑定。实际网络调度可能带来少量延迟。
- 超时/取消会关闭所有正在建立的通道，解除等待中的 Network.framework 回调。
- 可恢复 QUIC 失败后，自动模式回退 TCP，并在进程内按地址、端口、证书记住 120 秒；
  期间重连直接走 TCP。显式 QUIC 可立即重试；QUIC 成功清除此记录。
- 认证拒绝、证书缺失/不匹配、权限拒绝、用户取消不能通过回退绕过。
- 日志记录握手阶段、耗时和 QUIC 控制流编号；不记录口令、会话令牌或应用内容。

本修复没有实现活动会话的 TCP → QUIC 迁移。此功能需要两端增加绑定已有认证会话的
协议；直接再发一次 Hello 会与单控制会话限制冲突。修复流分配后应先实测 QUIC
连接与播放，再决定是否引入迁移复杂度。

### 验证

`mac-client/scripts/run_tests.sh` 运行常规测试，包括无 HelloAck 的真实 TCP 回环服务端、
取消、超时、认证保护和重试冷却。`mac-client/scripts/run_quic_tests.sh` 使用 Rust Cargo
构建仅监听 `127.0.0.1` 的 Quinn 合成服务端并运行完整 Swift 测试（含互通测试），
结束自动清理服务端。可通过 `CARGO` / `CARGO_HOME` / `RUSTC` 使用已有工具链。
互通测试不连接用户主机、不读取真实口令、不控制窗口或访问剪贴板。

## English

Physical-host logs showed roughly 15 seconds of QUIC waiting followed by a successful
TCP/TLS connection in 17–23 ms. Credential-free probes completed QUIC TLS with the same
certificate as TCP, excluding a blanket UDP block for this reproduction.

The bidirectional Network.framework connection group reserves an implicit stream 0.
The explicitly created Control stream was actually stream 4. Quinn accepts the implied
lower-numbered stream 0 first, so the host waits for a preface on 0 while the client waits
for HelloAck on 4. Reserve a **unidirectional group stream**, keeping explicit Control and
Bulk streams **bidirectional**. Control then uses stream 0. No host/protocol update is needed.

A loopback Quinn peer reproduced the original empty-stream timeout. The fixed production
client completed Hello/HelloAck and Bulk binding in about 8 ms locally. The interoperability
test also receives two server-initiated video streams using synthetic terminal frames; it
is not a decoding benchmark. A credential-free physical-host probe no longer stalled;
full authenticated physical-host playback remains a user verification step.

One deadline now covers the entire handshake, including sends, HelloAck and channel binding.
Auto QUIC budgets are 2.5 seconds (1.8 for a primary with a fallback address); explicit QUIC
budgets are 8 / 3 seconds; TCP budgets are 8 / 2.5 seconds. Expiry and task cancellation close
pending channels to unblock Network.framework callbacks. Recoverable QUIC failures trigger
an in-memory 120-second endpoint/port/certificate cooldown, during which auto reconnects use
TCP directly. Explicit QUIC retries immediately; success clears cooldown. Certificate/auth
failures, permission denial and cancellation never permit a security-bypassing fallback.
Logs contain stage names, elapsed times and stream IDs, not credentials or application data.

This patch does not migrate an active TCP session to QUIC. Such migration requires a
same-authenticated-session binding protocol on both ends; a second Hello currently conflicts
with the single-control-session lease. Evaluate the repaired QUIC path before adding migration.

Run `mac-client/scripts/run_quic_tests.sh` for the full Swift suite against a temporary
loopback-only Quinn fixture. The script builds it using Cargo, starts it with an ephemeral
port, and cleans it up on exit. Normal `run_tests.sh` skips only the optional Quinn test.
