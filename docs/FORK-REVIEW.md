# Fork 维护与功能审查 Runbook

本仓库是 `mizorewww/course2md` 的 fork（origin = `sleepinlava/course2md`）。
fork 的审查与单仓库不同：除了"代码写得好不好"，还要回答"我们相对上游改了什么、
上游的新变更会不会破坏我们的补丁"。本文是可复用的审查流程。

## 前置：一次性的基础设施

```bash
git remote add upstream https://github.com/mizorewww/course2md.git
git fetch upstream
```

## 三种审查场景

### 场景 A：上游同步前（incoming review）——上游要进来什么

```bash
git fetch upstream
git rev-list --left-right --count main...upstream/main   # 分叉量化：本地独有 x / 上游新 y
git log main..upstream/main --oneline                     # 上游新提交清单
git diff main...upstream/main --stat                      # 影响面
```

重点核对：上游改动是否触及我们补丁的同一批文件（冲突预判）。
我们的补丁地图（截至 2026-09）：`deps.rs`、`net.rs`（新）；`runtime.rs`、
`pipeline.rs`、`main.rs`、`settings.rs`、`doctor.rs`（改）；本地独有功能 =
依赖链自动安装。上游若改 `pipeline.rs` 预检段或 `runtime.rs::which`，同步时必须人工核对。

### 场景 B：本地补丁审查（outgoing review）——我们改了什么

固定点永远是 merge-base（三点 diff），而不是上游 HEAD：

```bash
git diff upstream/main...main        # 我们的净变更
git log upstream/main..main --oneline
```

双轴并行审查（各自独立出报告，禁止合并排序——两轴会互相掩盖）：

- **Standards 轴**：是否符合仓库约定。本仓库无正式标准文档，实际约定 =
  `docs/DESIGN.md` 架构意图 + 模块头中文"为什么"注释 + 测试同文件
  `#[cfg(test)]` + `i18n::tr` 文案 + anyhow/context 错误 + 依赖最小化；
  叠加 Fowler 坏味道基线（Mysterious Name / Duplicated Code / Feature Envy /
  Data Clumps / Primitive Obsession / Repeated Switches / Shotgun Surgery /
  Divergent Change / Speculative Generality / Message Chains / Middle Man /
  Refused Bequest），全部为判断性提示，仓库约定优先。
- **Spec 轴**：实现是否忠实于承诺。spec 来源 = CHANGELOG 未发布条目 +
  readme 用户可见承诺 + 新模块头部的设计不变量声明。核对三类问题：
  承诺了没做 / 做了没承诺（scope creep）/ 做了但做法与承诺矛盾。
  **spec 引用必须逐条quote原文**；评审员无法自证的事实（如"E2E 已验证"）
  由 supervisor 补跑核实。

执行方式：两个并行 subagent（reviewer），各自 400 字内报告，
supervisor 汇总时逐条核实"待证"论断后再接受。

### 场景 C：功能审查（dynamic）——跑起来对不对

静态 diff 审查替代不了执行。每次功能合入后按矩阵跑一遍（`docs/` 下
BENCHMARKS.md 的方法论适用于性能项）：

| 功能 | 最小验证 | 状态 |
| :--- | :--- | :--- |
| 本地视频 → 字幕路径转换 | 合成视频 + .srt，`--transcript-source subtitle` | ✅ 每轮 |
| 依赖自动安装冷启动 | 纯净 PATH + 空私有目录 `setup --yes --all` | ✅ 每轮 |
| llama 变体切换 | `--provider cpu` ↔ gpu，确认 stamp 变化 + 旧变体清零 | ✅ 每轮 |
| 升级路径 | 篡改 stamp sha → `↻ update available` → 重装恢复 | ✅ 每轮 |
| 单元/集成测试 | `cargo test --lib` + `--features integration` | ✅ CI |
| ASR 全链路（gpu/npu/api） | 需要模型/硬件/密钥，随环境可用性抽测 | ⚠️ 抽测 |
| CoreML（macOS） | 仅 macOS arm64 环境 | ⚠️ 发布前 |

## 已知 fork 卫生问题

- CHANGELOG `[未发布]` 混有上游 1.2.0 自带条目（LLM 重试/并发）——
  上游合并带入，非我们所作；发布时需拆分归属。
- `git rev-list --left-right --count main...upstream/main` 应保持
  "本地少而精"；本地补丁超过 ~10 个提交时应考虑向上游提 PR 回流。
