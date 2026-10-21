---
description: 检查并升级 OMP 至最新版本（或指定版本），校验增量补丁、契约符号、运行回归测试、更新文档并总结升级内容
argumentHint: "[target-version]"
---

你正在执行当前仓库的 OMP 升级与契约维护工作流。
如果用户传入了参数 `$ARGUMENTS`，则将 `$ARGUMENTS` 作为目标版本；否则升级至上游最新可用版本。

请严格按以下步骤依次执行并保证证据链完整：

### 1. 检查版本与升级前基线
1. 执行 `integrations/herdr-tps/.runtime/omp update --check` 检查上游最新版本。
2. 记录当前版本（`integrations/herdr-tps/.runtime/omp --version`）与当前二进制 SHA256（`sha256sum integrations/herdr-tps/.runtime/omp`）。
3. 记录当前 bundle 内核心符号计数基线（通过 `grep -ao` 统计 `getWithCredential`, `markReached`, `onGeneration`, `appendResetBoundary`, `appendEntry`, `session_shutdown`, `session_start`, `message_update`, `message_end`, `TokenRateMeter`, `willContinue`, `awaitingAsyncWork`）。

### 2. 执行升级并注意增量补丁
1. 运行 `integrations/herdr-tps/.runtime/omp update` 执行升级。
2. 观察升级输出，记录：
   - 是否命中 Stencil 增量二进制补丁机制（下载 `omp-linux-x64.patch`，由 HDiffPatch + zstd 就地打补丁并校验 SHA256）。
   - 补丁包体积大小与目标完整二进制体积，计算带宽节省比例。
   - 目标生成的二进制 SHA256 校验和。
3. 检查升级后版本：
   - `integrations/herdr-tps/.runtime/omp --version`
   - `node integrations/herdr-tps/omp-with-tps.mjs --version`
   - 确认版本一致且与目标版本匹配。

### 3. 接缝符号与核心契约核验
1. 再次通过 `grep -ao` 统计上述 12 个核心符号在新二进制中的出现频次，与基线进行对比。
2. 检查 `packages/ai/src/auth/` 的选号逻辑、`rotation.ts`、`TokenRateMeter` 算法等关键契约是否保持兼容。
3. 评估是否有 upstream breaking changes，确认本地包装层（`lib/omp-api-key-observer.mjs` 等）是否受到影响。

### 4. 运行全套回归验证
执行以下命令并确保全部通过：
1. `npm run herdr:tps:test`：运行 TPS 模块与本机 OMP 运行时契约测试（确保 155+ 测试全部 pass）。
2. `cargo test --offline --locked`：运行 ai-monitor 与 herdr-usage 单元/集成测试。
3. `npm run herdr:tps:install -- --dry-run`：确认 `config`、`shell`、`links` 状态均为“已就绪”。
4. `node integrations/herdr-tps/share-omp-daemons.mjs --dry-run`：确认守护进程共享状态为 `already-shared`。

### 5. 抓取 Release Notes 与升级内容分析
1. 使用 `read` 工具读取上游 release 页面：`https://github.com/can1357/oh-my-pi/releases/tag/v<VERSION>`（如跨越多个小版本，需一并获取对比）。
2. 结构化梳理新版本的改动内容，重点分类：
   - 模型适配、推理网关与结构化输出（Auth Gateway, Providers, JSON Schema, 图像预算等）
   - 任务规划、工具审批与子代理控制（审批三态、Prewalk、/seance、Profile 标识等）
   - 工具链、LSP 与系统性能优化（LSP 超时、语法检测、内存回收、Grep 修复等）
   - TUI 终端与排版显示（Markdown 实时滚动、SIXEL/SVG 单元格适配、输入防抖等）

### 6. 更新工程文档与适配引用
1. 在 `integrations/herdr-tps/OMP-SESSION-ROUTING.md` 中新增该版本的升级章节（参考已有 18.11.3 / 18.11.1 章节格式），记录 compare 链接、发布日期、详细改动点、符号计数变动以及增量补丁 SHA256 与测试结果；更新文档开头的版本引用。
2. 在 `integrations/herdr-tps/MAINTENANCE.md`、`integrations/herdr-tps/README.md`、`integrations/herdr-tps/lib/omp-api-key-observer.mjs` 中同步更新 OMP 版本号。
3. 运行 `git diff --check` 确保格式无误。

### 7. 输出升级总结报告
向用户输出完整的升级报告，包含：
- 升级前后版本对比与目标二进制 SHA256。
- 增量升级补丁详情（补丁大小、全量大小、节省带宽）。
- 核心契约与符号对比、测试验证结果。
- 该版本的系统性升级内容总结。
