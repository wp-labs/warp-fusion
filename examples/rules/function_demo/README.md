# Function Demo

演示 WFL 函数在真实输出中的用法：

- `sha1_n(@__wfu_id, 8)`：对 wfusion 生成的输出 ID 取 8 位 SHA1。
- `join(...)`：按参数顺序直接拼接，不加分隔符、不转义、不 trim、不改大小写。
- `join_by(sep, ...)`：按参数顺序拼接，并在字段之间插入显式分隔符。

运行：

```bash
wfl test rules/function_demo.wfl --schemas "schemas/*.wfs"
wfusion batch --config wfusion.toml --work-dir .
```

输出文件：

```text
data/out_dat/alerts.ndjson
```
