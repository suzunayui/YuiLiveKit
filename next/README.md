# YuiLiveKit（Rust + Tauri 版）

配信で使う道具をまとめた Windows アプリです。YouTube と Twitch のチャットを取得し、OBS にはブラウザソース1つで、コメント、ニコニコ風、参加管理、おはようカウンター、字幕を表示します。読み上げ（VOICEVOX）、AI 返答（OpenAI）、外国語コメントの日本語訳もできます。

仕様は [docs/rewrite-spec.md](../docs/rewrite-spec.md) にあります。

## 使い方

1. 以前の YuiLiveKit（Electron 版、または YouTubeOverlay）を使っていた場合は、Windows の設定からアンインストールします。データは残るので、あとで履歴を取り込めます。
2. `YuiLiveKit-Setup-<バージョン>.exe` をインストールします（YuiLauncher からも入れられます）。
3. 「OBS」タブの「OBS プラグインを導入」を押し、OBS を起動します。今のシーンに `YuiLiveKit` のソースが一度だけ追加されます。プラグインを入れない場合は、URL `http://127.0.0.1:5000/overlay` のブラウザソース（1920×1080）を追加します。
4. 「ホーム」で YouTube（@ハンドル、チャンネル ID、動画 ID、URL）か Twitch のチャンネルを入れて「チャット取得開始」を押します。
5. 表示する部品と位置は「配置」タブで決めます。OBS のシーンごとに配置を変えるときは、`?layout=<名前>` を付けた URL のソースを使います。

| タブ | できること |
|---|---|
| ホーム | 取得の開始と停止、おはようカウンター、以前の履歴の取り込み（一度だけ） |
| コメント | コメント一覧（初見、前回の表示）、名前の読み方、ブロック |
| スパチャ・メンバー | 支援の一覧、既読と未読、未読のコピー |
| 参加管理 | 「参加希望」の待ち一覧、完了、削除、並べ替え |
| 読み上げ | VOICEVOX の話者、速さ、音量、置き換え、NG ワード |
| AI返答 | OpenAI の API キー（資格情報マネージャーに保存）、トリガー、性格、上限 |
| 絵文字 | 絵文字の読み方と効果音 |
| 字幕・翻訳 | 追加機能の導入、音声字幕、コメント翻訳 |
| 配置 | OBS 表示の部品の位置、大きさ、見た目 |
| OBS | プラグインの導入、URL のコピー |

データは `%APPDATA%\com.suzunayui.yuilivekit` に保存します。

## 開発

Rust、Node.js、Microsoft Edge（テストで使用）が必要です。

```powershell
npm install
npx tauri dev     # 開発用に起動
npm test          # Rust のテスト、OBS 表示と設定画面のテスト
npm run test:perf # 50万件の履歴での速度（K9）
```

| 場所 | 内容 |
|---|---|
| `src-tauri/src/` | 本体。`main.rs` だけが Tauri のウィンドウとコマンドで、ほかはウィンドウなしでテストできる |
| `ui/` | 設定画面（素の JavaScript） |
| `overlay/` | OBS に表示するページ |
| `python/` | 字幕と翻訳の Python ワーカー（Electron 版のものをそのまま使用）と、導入する依存の一覧 |
| `obs-plugin/` | ソースを自動で追加する OBS プラグイン（C、GPL-2.0-or-later） |
| `src-tauri/examples/` | 確認用のプログラム（下記） |

確認用のプログラム:

- `overlay_demo`: OBS 表示のサーバーを、標準入力から流したデータで動かす（テストで使用）
- `chat_probe`: 実際の YouTube や Twitch のチャットを読み、件数だけを表示する
- `import_probe`: 以前の `comments.db` を一時的な DB に取り込み、件数だけを表示する
- `media_probe`: 導入済みの追加機能で、実際の字幕ワーカーと翻訳を一度動かす

テストのために環境変数で場所を変えられます: `YUI_LIVE_KIT_PORT`（OBS 表示のポート）、`YUI_LIVE_KIT_RUNTIME_DIR`（追加機能）、`YUI_LIVE_KIT_MODEL_DIR`（モデル）、`YUI_LIVE_KIT_OBS_PLUGIN_DIR`（プラグインの導入先）。

### OBS プラグイン

Visual Studio の C++ ツールと、OBS 33.0.1 のインストールが必要です。

```powershell
obs-plugin\build.cmd
```

できた `obs-plugin/build/yui-live-kit.dll` を `obs-plugin/prebuilt/` にコピーしてからアプリをビルドすると、アプリの「OBS プラグインを導入」で入るようになります。

## 公開の手順

1. `src-tauri/tauri.conf.json` と `src-tauri/Cargo.toml` のバージョンを、公開する日の `2026.1108.3` のような形にそろえます。公開済みのバージョンより大きくします。
2. `npm test` が通ることを確認します。
3. `npm run release` で `release/YuiLiveKit-Setup-<バージョン>.exe` と `release/SHA256SUMS.txt` を作ります。
4. GitHub Releases に `v<バージョン>` のタグで、この2つのファイルを公開します。
5. YuiLauncher の `yuilivekit-tauri` ブランチ（登録を Tauri 版のインストーラーに変える変更）を `main` に入れて、YuiLauncher を公開します。

Electron 版から切り替えるときは、このフォルダの中身をリポジトリの最上位に移し、Electron 版のファイルを消してから `main` に入れます。
