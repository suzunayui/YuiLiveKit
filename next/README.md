# YuiLiveKit（作り直し中）

Rust + Tauri で作り直している次の YuiLiveKit です。仕様は [docs/rewrite-spec.md](../docs/rewrite-spec.md) を参照してください。

## 構成

| 場所 | 内容 |
|---|---|
| `src-tauri/src/lib.rs` 以下 | 本体（ウィンドウなしでテストできる部分） |
| `src-tauri/src/youtube.rs` | YouTube のチャット取得と解析 |
| `src-tauri/src/twitch.rs` | Twitch のチャット取得と解析（匿名、読み取りのみ） |
| `src-tauri/src/db.rs` | SQLite への保存、視聴者の識別、以前の履歴の取り込み |
| `src-tauri/src/core.rs` | 取得の開始と停止、画面と OBS 表示への通知 |
| `src-tauri/src/server.rs` | OBS 向けのサーバー（`/overlay`、`/overlay/ws`） |
| `src-tauri/src/main.rs` | Tauri のウィンドウとコマンド |
| `ui/` | 設定画面 |
| `overlay/` | OBS に表示するページ |
| `obs-plugin/` | ソースを自動で追加する OBS プラグイン |

## 開発

Rust、Node.js、Microsoft Edge（テストで使用）が必要です。

```powershell
npm install
npx tauri dev     # 開発用に起動
npm test          # Rust のテスト、OBS 表示と設定画面のテスト
npm run test:perf # 50万件の履歴での速度（K9）
npx tauri build   # src-tauri/target/release/bundle/nsis にインストーラーを作成
```

`src-tauri/examples/` には確認用のプログラムがあります。

- `overlay_demo`: OBS 表示のサーバーを、標準入力から流したコメントで動かす（テストで使用）
- `chat_probe`: 実際の YouTube や Twitch のチャットを読み、件数だけを表示する
- `import_probe`: 以前の `comments.db` を一時的な DB に取り込み、件数だけを表示する（元の DB は読み取り専用で開く）

## OBS プラグイン

Visual Studio の C++ ツールと、OBS 33.0.1 のインストールが必要です。初回は OBS のヘッダーを取得します。

```powershell
obs-plugin\build.cmd
```

`obs-plugin/build/yui-live-kit.dll` ができます。プラグインのソースは GPL-2.0-or-later です。
