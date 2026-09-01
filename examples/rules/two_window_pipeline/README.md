# 两窗口 Pipeline

这个场景检查的是“同一源 IP 对多个目标主机发起连续失败登录”的异常。它不是
单点失败登录告警，而是先识别同一 `sip` 到同一 `dip` 的失败登录突发，再检查
同一 `sip` 是否在更长时间窗口内对多个目标出现这种突发。

安全语义上，这类行为通常对应横向暴力破解、账号探测、密码喷洒的局部形态，
或自动化登录尝试在内网目标间扩散。

这个示例演示一条经过内部中间窗口的两阶段规则链：

1. `auth_events` 从 `auth` stream 接收原始登录事件。
2. 第一段 `match<sip,dip:5m>` 检测同一源 IP 对同一目标 IP 的连续失败登录。
3. `|>` pipeline 操作符把第一段结果写入运行时生成的内部窗口。
4. 最后一段 `match<sip:10m>` 从内部窗口读取 `_in`，当同一源 IP 对多个目标产生失败登录突发时，输出 `security_alerts`。

内部窗口由 runtime 根据 pipeline stage 自动生成，不会发送到 sink；只有最终
`security_alerts` 的 `yield` 会被分发。

```wfl
rule two_window_pipeline_alert {
    events { e : auth_events && e.result == "failed" }
    match<sip,dip:5m> {
        on event { failures: e | count >= 3; }
    }
    |> match<sip:10m> {
        on event { targets: _in.dip | distinct | count >= 2; }
    } -> score(avg(_in.failures) + 40.0)
    entity(ip, _in.sip)
    yield security_alerts (
        sip = _in.sip,
        alert_type = "two_window_pipeline",
        detail = "failed login bursts across multiple targets"
    )
}
```

运行确定性的 replay 检查：

```bash
./run.sh
```

示例保留了 `wfusion.toml` topology，用来展示 source、内部 pipeline window
和最终 sink 的接线关系。当前可执行检查使用 `wfl replay`，因为一次性 file
receiver 在 runtime 模式下可能先于两个 pipeline stage 完全 drain 就结束。
