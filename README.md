# YuiLiveKit

配信で使う道具をまとめた Windows アプリです。YouTube と Twitch のチャットを取得し、OBS には、コメント、ニコニコ風、参加管理、おはようカウンター、字幕を、部品ごとのブラウザソースで表示します。読み上げ（VOICEVOX）、AI 返答（OpenAI）、外国語コメントの日本語訳もできます。

仕様は [docs/rewrite-spec.md](docs/rewrite-spec.md) にあります。2026.1108.2 から Rust + Tauri で作り直しました（以前の Electron 版は `v2026.1108.1` までのタグにあります）。

## 使い方

1. 以前の YuiLiveKit（Electron 版、または YouTubeOverlay）を使っていた場合は、Windows の設定からアンインストールします。データは残るので、あとで履歴を取り込めます。
2. `YuiLiveKit-Setup-<バージョン>.exe` をインストールします（YuiLauncher からも入れられます）。
3. 「OBS 表示」タブで、使う部品の「⠿ ドラッグ」を OBS にドラッグして追加します（コメント、ニコニコ風、字幕、参加管理、おはようのうち、使うものだけ）。同じタブの「OBS プラグインを導入」を押しておくと、アプリより先に OBS を起動しても表示が自動で読み込み直され、OBS の「ツール」メニューからも部品を追加できます。
4. 画面の一番上に YouTube（@ハンドル、チャンネル ID、動画 ID、URL）か Twitch のチャンネルを入れて「開始」を押します。チャンネルを入れた場合は、配信がまだ始まっていなくても押しておけます。配信が始まるまで待ち、終わったあとは次の配信を待ちます。
5. 設定を変えない日は、YuiLiveKit を開かずに使えます。「OBS 表示」タブの「OBS と一緒に動かす」で、OBS の起動に合わせて画面なしで起動し、前回の接続先で取得を始めるようにできます（どちらも最初はオフ、プラグインが必要）。動いているあいだはタスクトレイのアイコンから画面を開けます。
6. 部品の位置、大きさ、重なり順は OBS で決めます。文字の大きさや色は「OBS 表示」タブの「見た目」で決めます。

| グループ | タブ | できること |
|---|---|---|
| （一番上） | | 取得の開始と停止、YouTube と Twitch の入力、状態 |
| 配信中に見る | コメント | コメント一覧（初見、前回の表示）、名前の読み方、ブロック、以前の履歴の取り込み（一度だけ） |
| | スパチャ・メンバー | 支援の一覧、既読と未読、未読のコピー |
| | 参加・おはよう | 「参加希望」の待ち一覧、おはようカウンター、参加・辞退・おはようのキーワード |
| 声と字幕 | 読み上げ・AI返答 | 読み上げ、AI 返答（OpenAI）、VOICEVOX エンジンの導入、名前の読み方とブロック、絵文字の読み方と効果音 |
| | 字幕・翻訳 | 追加機能の導入、音声字幕、コメント翻訳 |
| OBS | OBS 表示 | プラグインの導入、部品ごとのドラッグと URL、部品の文字の大きさや色 |

一度決めれば触らない設定（キーワード、置き換えと NG ワード、絵文字など）は、折りたたんであります。見出しを押すと開き、開閉は次に起動したときも保たれます。

左のタブは、グループの中でドラッグして並べ替えられます。使わないタブは、左の列の一番下の「タブの表示」で隠せます。

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
| `obs-plugin/` | 表示の読み込み直し、部品の追加、アプリの起動と終了をする OBS プラグイン（C、GPL-2.0-or-later） |
| `src-tauri/examples/` | 確認用のプログラム（下記） |

確認用のプログラム:

- `overlay_demo`: OBS 表示のサーバーを、標準入力から流したデータで動かす（テストで使用）
- `chat_probe`: 実際の YouTube や Twitch のチャットを読み、件数だけを表示する
- `import_probe`: 以前の `comments.db` を一時的な DB に取り込み、件数だけを表示する
- `media_probe`: 導入済みの追加機能で、実際の字幕ワーカーと翻訳を一度動かす
- `voicevox_probe`: 実際の VOICEVOX エンジンを指定したフォルダに導入し、別のポートで起動して、話者の一覧と音声の作成を確かめる（約 1.9 GB をダウンロード）

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
5. YuiLauncher は最新のリリースを見に行くので、登録の変更は要りません。
