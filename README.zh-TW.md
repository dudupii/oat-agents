<p align="center">
  <a href="README.md">English</a> · <b>繁體中文</b>
</p>

<p align="center">
  <img src="docs/assets/hero.svg" alt="oat-agents 的即時檢視：整個團隊在同一個畫面上，一個 meta-agent 和它派出去的 member agent，每個都有自己的 branch、worktree 和 tmux session，透過 Run inbox 寫信給 meta-agent。按一個鍵就進到某個 agent 自己的 session 裡打字，再按一個鍵回到團隊。" width="100%">
</p>

<h1 align="center">oat-agents</h1>

<p align="center">
  <b>一個畫面看整個團隊，一個鍵接手任何一個 agent。</b><br>
  一個 meta-agent 只帶一層 member agent。每個 member 都是原生的 Claude Code 或 Codex session，
  各在自己的 branch 上。你在同一個終端機裡看著整個團隊，隨時可以進去任何一個。
</p>

<p align="center">
  <a href="#為什麼">為什麼</a> ·
  <a href="#用最精簡的結構做到三件事">三件事</a> ·
  <a href="#一個-run-怎麼跑">一個 Run 怎麼跑</a> ·
  <a href="#用-plugin-就能制定你的團隊與規則">用 Plugin 制定團隊與規則</a> ·
  <a href="#執行環境與隔離">執行環境與隔離</a> ·
  <a href="#五個指令上手">開始使用</a>
</p>

> 本文件是 [README.md](README.md) 的繁體中文版，內容若有出入以英文版為準。meta-agent、member agent、
> Run、Dispatch、Run inbox 等詞保留英文，意思見文末〈[會看到的詞](#會看到的詞)〉。圖片和連結的文件都是英文。

---

## 為什麼

現在的 coding agent 大多能把工作分給 subagent 去做。分給一兩個還好；同一個變更要是分給五個，就很難跟得上了。

<p align="center">
  <img src="docs/assets/one-session.svg" alt="左邊：很多 agent 擠在同一個 session 裡，每個 subagent 都寫進同一個捲動紀錄，看不出誰卡住，也沒辦法進去任何一個。右邊：同一個團隊在 oat-agents 裡，一個 agent 一列、每列都看得到狀態，每個都是自己的 session、在自己的 branch 上，按 enter 就能接手任何一個。" width="100%">
</p>

帶一隊 coding agent，會碰到兩個問題。

第一個是資訊沒辦法好好被呈現、被接收。在同一個 session 裡，所有 subagent 的輸出都擠進同一個捲動紀錄。你看不出哪一個卡住了；某一個出錯時，你也沒辦法直接跟它說話，只能請上層的 agent 幫你轉達。你需要看得清楚每個 agent 的脈絡。

第二個來自最直覺的解法。如果要 agent 自己替你做資訊紀錄，那就是變更之外的額外工作；它替你寫的每一筆紀錄，都是沒花在變更上的注意力，這會造成認知負荷。agent 應該只做事，沒有額外負荷。

所以 oat-agents 要用最少的結構，同時做到三件事。

## 用最精簡的結構做到三件事

### 結構簡單

團隊只有兩層：一個 meta-agent 負責拆目標、分派工作，以及它派出去的 member agent。團隊裡有哪些角色、工作怎麼拆、每個角色載入哪些 skill，都寫在 plugin 裡。要改團隊的做法，從來不需要改 oat-agents。見〈[用 Plugin 就能制定你的團隊與規則](#用-plugin-就能制定你的團隊與規則)〉。

### Agent 沒有額外負荷

member agent 就是原生的 Claude Code 或 Codex，用的是它自己的互動介面。除了變更本身，它只需要在做完時寄報告給 meta-agent，或在卡住時提問。其餘交給 oat-agents：每個 agent 有自己的 git worktree 和 branch，角色需要時再給一個 pod 跑 build 和測試。見〈[執行環境與隔離](#執行環境與隔離)〉。

### 人看得見脈絡

即時檢視只呈現資訊流：一個 agent 一列，看得到它的狀態、diff、timeline 和 log，整個團隊在同一個畫面上。要動手時，選一個 agent 按 `enter`，你就是在它真正的 tmux session 裡打字，跟你自己打開它沒有兩樣。按 `ctrl+]` 回到團隊。見〈[一個 Run 怎麼跑](#一個-run-怎麼跑)〉。

## 一個 Run 怎麼跑

<p align="center">
  <img src="docs/assets/tui-demo.svg" alt="oat-agents tui 的動畫重播：meta-agent 分派給一個 planner、兩個跑在不同 backend 上的 worker 和一個 reviewer；審查沒過，修正交給一個新的 member agent，meta-agent 問了操作者一個問題，最後由新的 reviewer 確認修正通過。" width="100%">
</p>

這是 `oat-agents tui`。畫面中的 Run 是示意，但版面、符號和按鍵都是真的。

1. 你用一個目標（goal）開始 Run：`oat-agents meta fire --prompt "…"`。寫你要的結果就好，步驟不用寫。
2. meta-agent 用 `role fire` 派出 member agent：一個 planner、兩個 worker、一個 reviewer，或是你的 plugin 定義的任何角色。程式碼它自己不寫。
3. member 透過 Run inbox 寫信給 meta-agent。做完的寄報告（`dispatch done`），卡住的提問（`dispatch ask`）並等回覆。inbox 只有 meta-agent 會讀。
4. meta-agent 遇到光看目標決定不了的事，它那一列會出現 🙋。選它、按 `enter`，打字回答就行。其他時候它不會來問你。
5. 審查沒過時，修正會交給一個*新的* member agent。失敗的那次嘗試還留著它那一列、它的報告，以及它在 workflow log 裡的紀錄。迴圈有上限：一輪輪做到審查的 findings 收完為止；一直沒過的工作會交還給你，不會無限打轉。
6. meta-agent 用 `meta finish` 結束 Run，並印出收據。成果在這個 Run 的 `oat/<run>/…` branch 上，由你審查、合併。

要站多近由你決定。可以留在高處：看 roster、打開 meta-agent 的 checklist（`ctrl+l`），或直接問 meta-agent 進度如何。也可以下到任何一個 member，看它的 diff，直接跟它一起做。

## 用 Plugin 就能制定你的團隊與規則

<p align="center">
  <img src="docs/assets/architecture.svg" alt="plugin 是一個放 Markdown 和 TOML 的目錄：core/oat-meta-instruction.md 說明 meta-agent 怎麼拆分工作、何時要問你，roles/ 底下的每個目錄就是一種 member agent。核心對每個 plugin 都用同樣的方式啟動這個團隊：worktree、tmux session、Run inbox、workflow log、pod 和即時檢視。" width="100%">
</p>

你的團隊怎麼工作，寫在 plugin 裡。plugin 就是一個放 Markdown 和 TOML 的目錄，它決定有哪些角色、每個角色收到什麼指示、跑在哪個模型上，以及 meta-agent 該怎麼拆分工作、何時該問你。核心只提供機制，其他一概不管（ADR-0001、ADR-0004）。

範例 plugin 內建 `planner`、`worker` 和 `reviewer`，讓 oat-agents 第一天就能做點有用的事。你的 plugin 可以留著它們、換掉它們，或加上 `tester`、`security-auditor`、一次跑四個的 `migrator`。完全沒有 planner 的團隊也可以。

新增一個角色，就是一個放了兩個檔案的目錄：

```text
team/
├── oat-plugin.toml
├── core/
│   └── oat-meta-instruction.md          # 告訴 meta-agent 什麼時候該用你的角色
└── roles/
    └── security-auditor/
        ├── role.toml
        └── instructions.md
```

```toml
# team/roles/security-auditor/role.toml
description = "Audits a change for security issues and reports findings with evidence."
start = "existing"          # 在被審查的變更所在的 worktree 裡執行
exec_environment = false
max_concurrent = 1

[model.claude]
model = "opus"
```

```markdown
<!-- team/roles/security-auditor/instructions.md -->
# Security auditor

You start in the worktree of the change under audit. Look for injection, authorization gaps
and leaked secrets. Report each finding with the file, the line, and why it is exploitable.
```

把它釘選在範例團隊旁邊（或單獨使用），信任一次，下一個 Run 就能用：

```sh
oat-agents init --force --embedded --path team my-team
oat-agents plugin list --repo .          # 會為每個 plugin 印出確切的 trust 指令
```

| 想改什麼…                                             | 在哪裡改                                                              |
|------------------------------------------------------|----------------------------------------------------------------------|
| 有哪些角色、各自做什麼                                  | plugin 裡的 `roles/<role>/role.toml` 與 `instructions.md`              |
| meta-agent 怎麼拆分工作、何時該問、何時該放棄            | plugin 裡的 `core/oat-meta-instruction.md`                             |
| 角色可以載入的可重用知識                                 | `skills/<skill>/SKILL.md`，並列在該角色的 `skills = [...]` 中           |
| 每個角色的模型、推理強度或 backend                        | `role.toml` 裡的 `[model.claude]`／`[model.codex]`，或在各 repository 的 `.oat/roles.toml` 覆寫 |
| 同一個角色最多同時跑幾個                                 | `max_concurrent`，可在各 repository 覆寫                                |
| 在多個 repository 之間共用同一個團隊                     | 把 plugin 發佈成 git repo，再用 `init --git <url> <commit\|latest> <name>` 釘選 |

所有 TOML 檔都會拒絕未知欄位，所以打錯字會在載入時就失敗，而不是被默默忽略。完整格式請見
[`docs/plugins.md`](docs/plugins.md)。

## 執行環境與隔離

`oat-agents` 是單一的 Rust 執行檔，背後沒有伺服器、資料庫或 daemon。一個 Run 就是一些 branch、tmux session，以及 `~/.local/state/oat-agents/` 底下的純文字檔。workflow log 也在裡面，是一份只會往後附加的 JSONL 檔，可以直接 `tail -f`、`jq` 或 `grep`。

每個 member agent 都有一個在自己 branch（`oat/<run>/<name>`）上的 worktree，以及 oat-agents 私有 tmux server 上的一個 session。`meta fire` 和 `role fire` 會印出 attach 進去的指令（`tmux -L oat attach -t oat_<…>`）。

設了 `exec_environment = true` 的角色，會在用你的 `.devcontainer/` 建出來的 pod 裡（目前支援 Kubernetes）跑建置和測試指令，前提是這個 Run 有 execution profile。agent 本身和其他所有東西都留在你的機器上。即時檢視會在每個 agent 旁邊顯示 `pod:…`；要了 pod 卻沒拿到的角色，會以黃色顯示 `HOST (no pod)`。如果執行環境跑不了某項檢查，角色協定要求 agent 回報為*未驗證*，不能當成通過。

agent 是在沒人看著的情況下執行的。沒有人在旁邊一步步核准，所以 oat-agents 啟動 agent CLI 時會關掉它的權限確認。worktree 只是把每個 agent 的工作分開，並不會限制 agent 能碰到什麼，所以請只在你放心交給 agent 的機器上、用你放心交給它的憑證來跑 oat-agents。

目前支援的 backend 是 Claude Code 和 Codex。每個角色可以各選一個，所以同一個團隊可以混用。

### 無人值守的 session 與 backend 自帶的 daemon

oat-agents 本身沒有 daemon——但 agent CLI 可能有自己的。Codex 會自行管理一個 app-server daemon；當 CLI 和這個 daemon 對 feature settings 的認知不一致時，新開的 session 會先跳出一個編號選單對話框，游標停在 Cancel 上。Run 裡沒有人會去回答它，member 自己也過不了這一關；放著不管，Dispatch 就會永遠停在那裡。只要 daemon 套用了 CLI 要求的設置，對話框就會消失：在對話框上選一次「Restart with these settings」會套用並持久化該設置（daemon 會重啟），`codex update` 或把舊的 managed daemon 砍掉也行；之後 session 乾淨啟動，完全不需要任何 flag。

在那之前，透傳變數可以把 CLI 這一側的偏好帶進每一次啟動：

```sh
OAT_CODEX_ARGS="--disable api_key_model_discovery" oat-agents meta fire …
```

值以空白切分；引號不會被特殊處理。`OAT_CLAUDE_ARGS` 對 Claude Code 的作用相同。要注意：flag 表達的是 CLI 要什麼——daemon 還抱著不同設置的期間，對話框仍會出現，而那正是下面這張網要接住的。

作為任何互動式 backend 彈出對話框時的安全網，逾時的 `run wait` 會把每個還在跑、畫面停在選單上的 Dispatch 以 `dialog_ms` 回報出來；超過五分鐘就會升級：workflow log 記一筆 `needs_human`，Run inbox 多一則 `escalation` 訊息。不會有人替你把 session 砍掉或結案——由 meta-agent（或看著 log 的你）決定。

## 五個指令上手

<p align="center">
  <img src="docs/assets/quickstart.svg" alt="一個終端機動畫：install.sh、oat-agents init、plugin trust、meta fire，接著是即時檢視。" width="100%">
</p>

**你需要：** `git`、`tmux`、Rust 工具鏈（[rustup.rs](https://rustup.rs)，1.85 以上，用來建置），以及至少一個已安裝並登入的 agent CLI：Claude Code（預設）或 Codex。

**1. 一行指令安裝。** 不必先 clone，這支腳本會把原始碼抓到暫存目錄、建置、把 `oat-agents` 放進 `~/.local/bin`、安裝 `oat-agents-cli` skill（讓你自己的 coding agent 能幫你解說並操作 oat-agents），最後刪除暫存目錄。

```sh
curl -fsSL https://raw.githubusercontent.com/Noopher-AI/oat-agents/main/install.sh | sh
```

<details>
<summary>其他安裝方式</summary>

```sh
# 在 `sh -s --` 之後加選項：換個安裝目錄、指定 tag 或 commit、只裝執行檔
curl -fsSL https://raw.githubusercontent.com/Noopher-AI/oat-agents/main/install.sh \
  | sh -s -- --bin-dir /usr/local/bin --ref <tag-or-commit> --skip-skill

# 只用 cargo（只裝執行檔，裝到 ~/.cargo/bin）
cargo install --locked --git https://github.com/Noopher-AI/oat-agents

# 從 clone 下來的原始碼安裝
git clone https://github.com/Noopher-AI/oat-agents && cd oat-agents && ./install.sh
```

</details>

**2. 指向一個 repository。** 不加任何旗標時，`init` 會釘選內建的範例團隊。`--local` 會把設定放在 `.git/` 底下，讓你的 `git status` 保持乾淨。

```sh
cd ~/src/your-project
oat-agents init --local
```

**3. 在這台機器上信任這個 plugin 一次。** 它的指示會引導你的 agent，所以 oat-agents 不會執行你沒核准過的 plugin。

```sh
oat-agents plugin trust embedded --name example
```

**4. 用一個目標開始一個 Run。** 寫你要的結果就好，不用寫步驟。目標比較長時用 `--input-file`，想讓 meta-agent 跑在 Codex 上就加 `--agent codex`。

```sh
oat-agents meta fire --name rate-limit-login \
  --prompt "Rate-limit the login endpoint: 5 attempts a minute per account, with tests."
```

**5. 看著團隊工作。**

```sh
oat-agents tui
```

`↑↓` 選擇 agent，`←→` 在它的 `live`、`diff`、`timeline`、`log` 分頁之間切換。`enter` 直接在選取的 session 裡打字，`ctrl+]` 把按鍵控制交回來，`ctrl+l` 開啟 checklist，`ctrl+\` 開啟主控台。

> **提示：** 問問你自己的 coding agent「how do I use oat-agents?」。`install.sh` 安裝的 `oat-agents-cli` skill 會帶它一步步啟動、觀察並接手一個 Run。

### 選用：在 pod 裡執行檢查

每台機器設定一次 execution profile，有需要的角色就會在 pod 裡執行建置與測試指令：

```sh
oat-agents init --repo . --exec-profile local --exec-context <kube-context> --exec-namespace <namespace>
$EDITOR ~/.oat/exec.toml           # 補完 [image] 區段；每個欄位都已列出並附上註解
oat-agents env doctor --profile local
oat-agents env image  --profile local
```

`meta fire` 和 `role fire` 會回報指令在哪裡執行，並在要求 pod 的角色將在主機上執行時發出警告。詳見 [`docs/exec-environments.md`](docs/exec-environments.md)。

## 會看到的詞

| 詞 | 意思 |
|---|---|
| **Goal（目標）** | 你想完成的事，寫給 meta-agent 看。它描述結果，而不是步驟。 |
| **Run** | 一個目標的一次執行，從 `meta fire` 到 `meta finish`。 |
| **Meta-agent** | 帶領一個 Run 的 agent：拆分目標、派出 member agent、讀它們的信。它跑的是核心角色 `oat-meta`。 |
| **Member agent** | meta-agent 為某一塊工作派出的 agent，扮演你的 plugin 裡的某個角色。 |
| **Dispatch** | 某個角色的一次啟動：一個 session、一個 worktree、一次結算。重試就是一個新的 Dispatch。 |
| **Run inbox** | member agent 寄給 meta-agent 的信：報告和問題。只有 meta-agent 會讀。 |
| **Workflow log** | 這個 Run 只會附加寫入的紀錄。你和即時檢視讀的都是它。 |

完整詞彙表（包括我們刻意避免使用的詞）在 [`.dev_docs/CONTEXT.md`](.dev_docs/CONTEXT.md)，以英文撰寫。

## 延伸閱讀

以下文件皆以英文撰寫。

- [`docs/plugins.md`](docs/plugins.md)：如何撰寫 plugin，包含磁碟上的格式與完整範例
- [`docs/exec-environments.md`](docs/exec-environments.md)：pod，從設定 profile、為 Run 選擇 profile 到查看執行位置
- [`.dev_docs/adr/`](.dev_docs/adr/)：架構決策紀錄
- [`AGENTS.md`](AGENTS.md)：在這個 repository 工作的規則，包含驗證指令

## 參與貢獻

歡迎提出 issue 與 pull request。請從 [`CONTRIBUTING.md`](CONTRIBUTING.md) 開始；安全性漏洞請依 [`SECURITY.md`](SECURITY.md) 的說明私下回報。所有參與者都應遵守[行為準則](CODE_OF_CONDUCT.md)。

## 授權

[MIT](LICENSE)。
