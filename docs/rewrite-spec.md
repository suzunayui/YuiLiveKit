# YuiLiveKit 作り直し仕様書（Rust + Tauri）

- 作成日: 2026-11-08
- 状態: 確定。M0〜M4 完了、M5 は公開の準備まで（2026-11-08、結果は 13〜15 章）
- 対象: YuiLiveKit（Electron 版）と YuiAIAssistant を統合し、Rust + Tauri で作り直す

## 1. 目的と方針

### 1.1 目的

- 配信中に使う機能を、1つの軽いアプリにまとめる。
- OBS に追加するソースを1つにし、OBS WebSocket とそのパスワードを不要にする。
- YouTube と Twitch のチャットを1回だけ取得し、表示、読み上げ、AI 返答、翻訳で共有する。

### 1.2 方針

- **1から作り直す。** Electron 版のコード、設定、データ形式との互換性は持たない。旧版はアンインストールしてから新版を入れる。
- **挙動は引き継ぐ。** Electron 版で直した不具合を再発させないため、「8. 引き継ぐ挙動」を受け入れ条件にする。
- **Python の処理は流用してよい。** 字幕と翻訳のワーカー（`subtitles/backend/`、`translation/backend/`）は、標準入出力の JSON Lines で動くため、そのまま呼び出せる。
- **段階的に作る。** 「10. マイルストーン」の M1 だけで配信に使える状態を目指す。完成までは Electron 版を不具合修正だけで使い続ける。

### 1.3 対象外

- Windows x64 以外（macOS、Linux、32bit）
- Electron 版と YuiAIAssistant の設定、データ、OBS ソースの移行（ただしコメント履歴は一度だけ取り込む。「11. 決定事項」を参照）
- 仮想カメラ出力
- OBS WebSocket による連携
- 同じプラットフォームの複数配信の同時取得（YouTube 1つと Twitch 1つの同時取得は対象）
- Live Subtitle Assistant など、ほかのアプリのモデルや設定の再利用

## 2. 全体構成

```
┌──────────────── YuiLiveKit（Tauri） ────────────────┐
│ WebView2 の設定画面（HTML/JS、Node.js なし）          │
│        │ Tauri コマンドとイベントのみ                 │
│ Rust コア                                            │
│  ├ チャット取得（YouTube / Twitch）                    │
│  ├ SQLite（rusqlite）                                 │
│  ├ 配信用 HTTP サーバー（127.0.0.1）  ──────────┐      │
│  ├ 読み上げ（VOICEVOX へ HTTP）                │      │
│  ├ AI 返答（OpenAI API へ HTTPS）              │      │
│  └ Python ワーカー管理（字幕、翻訳）            │      │
└───────────────────────────────────────────────┼──────┘
                                                │ HTTP + WebSocket
OBS ── ブラウザソース1つ（/overlay） ◀──────────┘
  └ YuiLiveKit プラグイン（ソースを自動で追加するだけ）
```

| 部品 | 技術 | 備考 |
|---|---|---|
| アプリ本体 | Rust、Tauri 2 | 配布は NSIS、ユーザー単位のインストール |
| 設定画面 | HTML/CSS/JS（フレームワークは任意） | Node.js の API は使わない |
| DB | SQLite（rusqlite、同梱ビルド） | ネイティブモジュールの作り直しが不要 |
| 配信用サーバー | Rust の HTTP サーバー（例: axum） | 127.0.0.1 のみで待ち受け |
| 字幕 | 既存の Python ワーカー（faster-whisper） | 必要なときだけ導入 |
| 翻訳 | 既存の Python ワーカー + llama.cpp（TranslateGemma） | 必要なときだけ導入 |
| 読み上げ | VOICEVOX（別途インストール） | `http://127.0.0.1:50021` |
| AI 返答 | OpenAI API | API キーはユーザーが用意 |
| OBS プラグイン | C、libobs と obs-frontend-api のみ（Qt なし） | GPL-2.0-or-later |

## 3. 画面

設定画面はタブで分ける。各機能は個別にオンオフでき、オフの機能は通信もプロセスも動かさない。

| タブ | 内容 |
|---|---|
| ホーム | 取得対象の入力（YouTube、Twitch）、開始と停止、状態表示、各機能のオンオフ |
| コメント | コメント一覧、検索、初見と前回参加の表示、ブロックの操作 |
| スパチャ・メンバー | 現在の配信の支援一覧、既読と未読、コピー |
| 参加管理 | 待ち一覧、完了、削除、並べ替え |
| 配置 | OBS 画面と同じ比率のプレビューで、表示部品の配置と見た目を決める |
| 読み上げ | VOICEVOX の話者、速度、音量、名前の読み上げ、読み方、置き換え、NG ワード |
| AI 返答 | API キー、性格、トリガーキーワード、話者、上限 |
| 絵文字 | 出現した絵文字の一覧、読み方、効果音と音量 |
| 字幕・翻訳 | 機能の導入、マイク、デバイス、モデル、字幕の見た目 |
| OBS | プラグインの導入、ソースの追加、URL のコピー |

## 4. 機能要件

### 4.1 チャット取得

**入力**
- YouTube: 動画 ID、`@ハンドル`、チャンネル ID、YouTube の URL（`watch?v=`、`youtu.be/`、`/live/`、`/shorts/`、`/@ハンドル`、`/channel/`、`/c/`、`/user/`、`live_chat?v=`）。`https://` は省略できる。
- Twitch: チャンネル名、`twitch.tv/チャンネル名` の URL。
- 解釈できない入力は、取得を始める前に理由を表示する。

**YouTube**
- 非公式の InnerTube API（`get_live_chat`）で取得する。「上位のチャット」ではなく「すべてのチャット」に切り替える。
- 取得する種類: 通常コメント、スーパーチャット、スーパーステッカー、メンバー加入とマイルストーン、メンバーシップギフトの購入と受け取り。
- 投稿者は表示名とチャンネル ID の両方を保存する。
- 削除アクション（`removeChatItemAction`、`markChatItemAsDeletedAction`、`removeChatItemByAuthorAction`、`markChatItemsByAuthorAsDeletedAction`）に追従する。**フィールド名は記憶と Electron 版の実装に基づくため、M1 で実際の応答を記録して確認する。**
- 応答に続きの continuation がない状態が3回続いたら、配信終了として取得を止め、画面に知らせる。1回だけなら通信の乱れとして続ける。
- 通信エラーは再接続を続け、状態を「再接続中」と表示する。

**Twitch**
- 匿名の IRC（`irc.chat.twitch.tv:6697`、TLS、`justinfan` ニック）で読み取り専用で接続する。YuiAIAssistant と同じ方式。
- `twitch.tv/tags` と `twitch.tv/commands` を要求し、`CLEARMSG`（1件削除）と `CLEARCHAT`（ユーザーの全削除、またはチャット全消去）に追従する。YuiAIAssistant は未対応だったため新規に作る。
- Bits は支援として扱い、スパチャ一覧に出す。

**共通**
- 取得したメッセージは、プラットフォームによらない1つの形式に変換して保存し、各機能へ配る。
- 取得はアプリ起動中に1回だけ行い、表示、読み上げ、AI 返答、翻訳は同じデータを使う。

### 4.2 保存

- SQLite に保存する。スキーマは「6. データ」。
- 削除されたメッセージは行を残して削除済みにする。同じメッセージが再送されても表示に戻さない。
- 初回起動時などに、Electron 版の `comments.db` から履歴を一度だけ取り込める。元の DB は読み取り専用で開き、変更しない。取り込んだ履歴は初見判定と前回参加の表示に使う。
- 表示や一覧の問い合わせは、履歴が数十万件でも 20ms 以内に返す（インデックス必須）。DB 処理は UI と配信用サーバーを止めない。

### 4.3 視聴者の識別

- 視聴者は「プラットフォーム + プラットフォームのユーザー ID」で識別する。表示名は識別に使わない。
- 同じ表示名でも ID が違えば別人、表示名が変わっても ID が同じなら同一人物として扱う。
- 初見判定と「前回の配信から何回ぶりか」は、この識別に基づく。

### 4.4 OBS 向け表示（ブラウザソース1つ）

- OBS に追加するのは `http://127.0.0.1:<ポート>/overlay` の1つだけ。基準の大きさは 1920×1080 とし、ソースの大きさに合わせて拡大縮小する。
- 表示部品: 通常コメント、ニコニコ風、字幕（日本語と英語）、参加管理、おはようカウンター、AI 返答の表示（任意）。
- 部品の位置、大きさ、表示と非表示、見た目は「配置」タブで決める。
- 配置は名前を付けて複数保存でき、`/overlay?layout=<名前>` で選べる。OBS のシーンごとに別の配置を使える。
- 表示ページは WebSocket（または SSE）で更新を受け取る。ポーリングはしない。アプリが再起動しても自動で再接続し、配置の変更はすぐ反映する。OBS 側の再読み込みは不要にする。
- アプリが停止しているときは何も表示しない（エラー画面を出さない）。
- 部品ごとの URL は作らない（必要になったら `?only=<部品>` を追加で検討する）。

**通常コメント**: 今の配信のコメントだけを表示する（OBS を再読み込みしても前回の配信のコメントは出さない）。新しい順に積み上げ、件数の上限を超えたら古いものから消す。スパチャは YouTube の色を使い、色がないときは円の金額帯で色を決める。翻訳があれば原文の下に追記する。

**ニコニコ風**: 右から左へ流す。絵文字だけの短いコメントは落下演出にする（Electron 版の挙動）。流れている途中でも、削除されたら消す。

**字幕**: 黒背景、白文字、角丸、白枠。中央、左、右寄せ。表示秒数と文字サイズを設定できる。一部の語の伏字（Electron 版と同じ対象）を選べる。

### 4.5 参加管理

- キーワード（初期値「参加希望」）を含む通常コメントで参加を受け付ける。
- 待っている人の重複した参加希望は、受け付けずに記録だけ残す。その人の最初の希望が完了や削除になっても、記録した希望は戻さない。
- 完了した人が新しくコメントすれば、また参加できる。
- 完了、削除、全クリア、完了分のクリア、並べ替えができる。待ち一覧は配信ごとに分ける。
- 判定はコアの1か所だけで行う。設定画面は結果を表示するだけにする（Electron 版では画面側にも判定があり、削除した人が再読み込みで戻った）。

### 4.6 おはようカウンター

- 配信ごとに、キーワードを含むコメントをした人数を数える。1人1回まで。
- 手動の増減とリセットができる。リセットより前のコメントは数えない。
- キーワードは一覧で持ち、設定で編集できる。部分一致で「おはなし」などを数えないよう、初期値は「おはよ」「おはー」「おっは」などにする。

### 4.7 読み上げ（VOICEVOX）

- VOICEVOX が起動していれば話者一覧を取得し、なければ画面に案内を出して定期的に確認する。
- 読み上げの対象は、アプリで取得を始めた後のコメントだけ。過去の履歴は読まない。
- ブロックした視聴者と、NG ワードを含むコメントは読まない。
- 読み上げる前に、置き換えルールと絵文字の読み方を適用し、読み方のない絵文字は除く。
- 名前を読むかどうかを選べる。視聴者ごとに読み方（あだ名）を登録できる。
- 待ち行列で順番に読む。読む直前にも削除、ブロック、NG を確認し、該当すれば読まない。
- 待ち行列の上限を設け、あふれたら古いものから捨てる。停止したら待ち行列を空にする。
- 話者、速度、音量は、コメント用と AI 返答用で別に設定できる。

### 4.8 絵文字の効果音

- 配信中に出現した絵文字を一覧に記録する。
- 絵文字ごとに読み方、効果音ファイル（wav、mp3、ogg、m4a、10MB まで）、音量を設定できる。
- 効果音を設定した絵文字を含むコメントで音を鳴らす。

### 4.9 AI 返答（OpenAI）

- トリガーキーワード（最大50件）を含むコメントにだけ返答する。
- 視聴者のコメントはそのままプロンプトになるため、次を守る。
  - 指示文（システムプロンプト）は固定し、コメントは引用として別に渡す。コメントの長さには上限を設ける。
  - ブロック、NG ワード、削除済みのコメントには返答しない。返答文にも NG ワードを適用し、含まれていれば読まない。
  - 1件ずつ処理し、間隔と1時間あたりの上限を設ける（料金と連投の対策）。
- 返答は AI 返答用の話者で読み上げる。OBS 表示への表示は任意。
- モデル名は設定できるようにする（コードに固定しない）。
- API キーが未設定なら、OpenAI へは一切通信しない。

### 4.10 字幕

- マイク、認識デバイス（CPU / GPU）、モデル（base、small、medium、large-v3）、音声検出のしきい値を選べる。
- 「チャット取得開始で字幕も始める」を選べる（初期値オン）。オンのときは、一緒に始めた字幕をチャットの停止で一緒に止める。字幕だけの開始と停止もできる。
- 録音や字幕を保存しない。

### 4.11 翻訳

- 日本語以外のコメントを、TranslateGemma 12B（Q4_K_M、llama.cpp）でローカル翻訳し、原文の下に追記する。
- 翻訳はコメントの表示を待たせない。対象は新しいコメントだけで、待ちは最大30件。あふれたら古いものを捨てる。
- 日本語のコメントと絵文字だけのコメントは翻訳しない。

### 4.12 追加機能の導入

- 字幕と翻訳に必要なもの（Python 3.13 の embeddable、pip、ライブラリ、llama.cpp、CUDA ライブラリ）は、ボタンを押したときだけ導入する。初期インストーラーには含めない。
- ダウンロードするファイルは、すべて固定した SHA-256 で検証する。ライブラリはバージョンを固定する。
- 導入先はアプリのデータフォルダ内に置き、システムへはインストールしない。
- Python の検索パス（`python313._pth`）は絶対パスを含むため、起動のたびに現在の場所で書き直す。
- 導入中に失敗したら、同じボタンで再試行できる。字幕と翻訳の実行中は導入できない。

### 4.13 OBS プラグイン

**導入**
- 「OBS」タブのボタンで、`%ProgramData%\obs-studio\plugins\yui-live-kit\bin\64bit\yui-live-kit.dll` に配置する（OBS 33 の形式。導入の手順は YuiVoiceChanger と同じ）。この場所で読み込まれるかは、M2 の導入ボタンで実際の OBS を使って確認する。
- OBS の起動中は、終了するよう案内する。書き込めなければ、管理者の確認を出す。
- 配置後にファイルのハッシュを確認する。ポータブル版 OBS は対象外とし、URL のコピーで対応する。

**動作**
- OBS の起動完了後、今のシーンコレクションに `YuiLiveKit` という名前のソースがなければ、今のシーンにブラウザソースを追加する。
- 自動で追加するのは、シーンコレクションごとに1回だけ。ユーザーが消したソースは自動で戻さない。
- 「ツール」メニューに「YuiLiveKit を今のシーンに追加」を置き、手動でも追加できるようにする。
- 接続先の URL は、アプリが書き出す設定ファイルから読む（ポートを変えてもプラグインの作り直しが不要になる）。
- OBS の C API と frontend API だけを使い、Qt には依存しない。YuiVoiceChanger のプラグインとは別のプラグインにする。

### 4.14 アプリの更新と配布

- NSIS のインストーラーと `SHA256SUMS.txt` を GitHub Releases で公開する。バージョンは `2026.1108.1` の形式。
- YuiLauncher の登録を新しいインストーラーに合わせて更新する。レジストリキーは `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\YuiLiveKit`、実行ファイルは `YuiLiveKit.exe`、インストール先は `%LOCALAPPDATA%\YuiLiveKit`（M0 で確認）。
- Tauri が出力する `YuiLiveKit_<バージョン>_x64-setup.exe` は、公開時に `YuiLiveKit-Setup-<バージョン>.exe` に名前を変えて `SHA256SUMS.txt` を作る（YuiLauncher の `scripts/package-release.cjs` と同じ方法）。
- 二重起動はさせず、2つ目の起動では既存のウィンドウを前面に出す。
- 配信用サーバーのポートが使えないときは、理由を表示し、ポートを変更できるようにする。

## 5. 非機能要件

### 5.1 性能

- 待機時（取得停止、字幕と翻訳オフ）の、プロセス全体（WebView2 を含む）のプライベートメモリを 200MB 以下にする。M0 の雛形は 166MB、Electron 45 版は 212MB だった。機能を足しても雛形から大きく増やさない。
- インストーラーは 10MB 以下にする（M0 の雛形は 1.2MB、Electron 45 版は 133MB）。
- OBS 表示はポーリングせず、更新があったときだけ送る。
- 長時間配信（8時間）でメモリが増え続けないこと。

### 5.2 セキュリティ

- **設定画面:** Node.js の API を持たない WebView にし、Tauri の capabilities で許可したコマンドだけを呼べるようにする。CSP を設定し、外部スクリプトは読み込まない。
- **表示:** コメント、名前、翻訳、AI 返答は文字列として表示し、HTML として解釈しない（`innerHTML` を使わない）。画像は YouTube と Twitch の画像ホストからのみ読み込む。
- **配信用サーバー:** 127.0.0.1 のみで待ち受け、`Host` ヘッダーを確認する（DNS リバインディング対策）。OBS 表示に必要な読み取り専用の API だけを公開し、操作は Tauri コマンドで行う。
- **秘密情報:** OpenAI の API キーは Windows の資格情報マネージャー（または DPAPI）に保存し、平文のファイルや WebView の保存領域に置かない。ログに出さない。
- **外部ファイル:** ダウンロードは SHA-256 で検証する。効果音ファイルは種類と大きさを確認する。

### 5.3 プライバシー

- 録音、字幕、翻訳結果を保存しない（翻訳はメモリ内だけ）。
- OpenAI に送るのは、トリガーに一致したコメントと性格の設定だけ。

### 5.4 信頼性

- YouTube の仕様変更で解析に失敗しても、アプリは落とさずに状態を表示する。
- Python ワーカー、VOICEVOX、OpenAI の失敗は、その機能だけを止め、ほかの機能は動かし続ける。
- 導入した Python が壊れていても、アプリの画面は開く（Electron 版では開かなくなる不具合があった）。

## 6. データ

### 6.1 SQLite スキーマ（案）

```sql
CREATE TABLE streams (
  id INTEGER PRIMARY KEY,
  platform TEXT NOT NULL,              -- 'youtube' | 'twitch'
  external_id TEXT NOT NULL,           -- YouTube の動画 ID、Twitch のチャンネル名と開始日時
  title TEXT,
  first_seen_ms INTEGER NOT NULL,
  UNIQUE (platform, external_id)
);

CREATE TABLE viewers (
  id INTEGER PRIMARY KEY,
  platform TEXT NOT NULL,
  platform_user_id TEXT NOT NULL,      -- YouTube のチャンネル ID、Twitch のユーザー ID
  display_name TEXT NOT NULL,          -- 最新の表示名
  icon_url TEXT,
  first_seen_ms INTEGER NOT NULL,
  UNIQUE (platform, platform_user_id)
);

CREATE TABLE messages (
  id INTEGER PRIMARY KEY,
  stream_id INTEGER NOT NULL REFERENCES streams(id),
  viewer_id INTEGER REFERENCES viewers(id),
  platform_message_id TEXT NOT NULL,
  author_name TEXT NOT NULL,           -- 送信時の表示名（M1 で追加）
  icon_url TEXT,                       -- 送信時のアイコン（M1 で追加）
  kind TEXT NOT NULL,                  -- text, paid, sticker, membership, gift_purchase, gift_redeem, bits
  text TEXT NOT NULL,
  parts_json TEXT NOT NULL,            -- テキスト、絵文字、スタンプ
  amount_micros INTEGER,               -- 金額 × 1,000,000（$4.99 なら 4990000）
  currency TEXT,                       -- 'JPY' など。不明なら NULL
  amount_text TEXT,                    -- 表示用の原文
  colors_json TEXT,
  badges_json TEXT,                    -- メンバー、モデレーター、配信者
  created_ms INTEGER NOT NULL,
  deleted_ms INTEGER,
  UNIQUE (stream_id, platform_message_id)
);

CREATE INDEX idx_messages_stream_time ON messages(stream_id, created_ms);
CREATE INDEX idx_messages_viewer_time ON messages(viewer_id, created_ms);
CREATE INDEX idx_messages_deleted ON messages(deleted_ms) WHERE deleted_ms IS NOT NULL;

CREATE TABLE participants (
  id INTEGER PRIMARY KEY,
  stream_id INTEGER NOT NULL REFERENCES streams(id),
  message_id INTEGER NOT NULL UNIQUE REFERENCES messages(id),
  viewer_id INTEGER REFERENCES viewers(id),
  status TEXT NOT NULL,                -- waiting, done, removed, duplicate
  queue_order INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL
);

CREATE TABLE ohayo (
  stream_id INTEGER PRIMARY KEY REFERENCES streams(id),
  adjustment INTEGER NOT NULL DEFAULT 0,
  reset_ms INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE support_read (
  message_id INTEGER PRIMARY KEY REFERENCES messages(id),
  read_ms INTEGER NOT NULL
);

CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);
```

- 参加管理は `message_id` を一意にすることで、同じコメントを二度受け付けない。
- 視聴者ごとの読み方、ブロック、絵文字の設定は `settings` に置くか、必要に応じて表を分ける（実装時に決める）。
- 金額帯の色分けは `currency = 'JPY'` のときだけ金額を使い、それ以外は YouTube の色を使う。

### 6.2 保存場所

- アプリのデータは Tauri のアプリデータフォルダ（`%APPDATA%\<identifier>`）に置く。
- モデルと追加機能も同じフォルダの下に置く。アンインストール時に消すかどうかは選べるようにする。

## 7. 配信用サーバーの API（案）

| メソッドとパス | 用途 |
|---|---|
| `GET /overlay` | OBS 表示のページ。`?layout=<名前>` で配置を選ぶ |
| `GET /overlay/assets/*` | 表示ページの JS、CSS、フォント |
| `GET /overlay/ws` | 表示の更新（初期状態、新しいメッセージ、削除、配置、字幕、参加、おはよう） |
| `GET /health` | プラグインとテスト用の生存確認 |

操作（参加の完了、配置の保存など）はすべて Tauri コマンドで行い、HTTP では公開しない。

## 8. 引き継ぐ挙動（受け入れ条件）

Electron 版で見つけて直した問題。作り直しでも再発しないことをテストで確認する。

| ID | 挙動 |
|---|---|
| K1 | YouTube で削除されたコメントと、BAN された人のコメントは、通常コメント、ニコニコ風、コメント一覧、読み上げの待ちから消える |
| K2 | 削除されたコメントが再送されても、表示に戻らない |
| K3 | 待っている人の重複した参加希望は、最初の希望が完了や削除になっても、待ち一覧に戻らない |
| K4 | 全クリアの後に、既存のコメントを読み直しても参加者が戻らない。画面を再読み込みしても戻らない |
| K5 | 同じ表示名の別人は別の視聴者、改名した人は同じ視聴者として扱う |
| K6 | 続きのない応答が3回続いたら取得を止めて知らせる。1回だけなら続ける |
| K7 | コメント、名前、翻訳、スパチャの文字に含まれる HTML は、文字として表示する |
| K8 | 外貨のスパチャ（`$5.00`、`CA$5.00`、`CN¥50` など）を円として扱わない |
| K9 | 履歴が数十万件あっても、表示と一覧の更新が遅くならない |
| K10 | 二重起動しない。ポートが使えないときは理由を表示する |
| K11 | 導入した Python が壊れていても、アプリの画面は開く |
| K12 | Python の検索パスは、データフォルダを移動しても正しい場所を指す |
| K13 | 翻訳は新しいコメントだけを対象にし、待ちが30件を超えたら古いものを捨てる |
| K14 | 動画やチャンネルの URL を貼って開始できる。探せない URL は理由を表示する |
| K15 | 読み上げの直前にも、削除、ブロック、NG を確認する |

## 9. テスト方針

- **Rust の単体テスト:** 入力の解釈、YouTube と Twitch の応答の変換、削除の適用、参加管理、おはよう、金額の解釈、DB の問い合わせ。
- **記録した応答による試験:** 実際の YouTube と Twitch の応答を記録し、個人情報を取り除いてテストデータにする。削除アクションと配信終了の応答は、M1 で必ず記録する。
- **表示ページの試験:** 配信用サーバーを起動し、Playwright（Chromium）で `/overlay` を開いて、表示、削除、配置の反映、再接続を確認する。OBS のブラウザも Chromium 系なので、同じ確認で代用できる範囲が広い。
- **設定画面の試験:** Tauri の WebDriver（tauri-driver）による試験を検討する。難しければ、画面の処理を Tauri コマンドの呼び出し側と分けて、JS の単体テストにする。
- **性能の試験:** 50万件の履歴を入れた DB で、表示と一覧の問い合わせ時間を測る（K9）。
- **手動の確認（リリースごと）:** OBS プラグインの導入と自動追加、実際の配信での取得と削除と終了、VOICEVOX の読み上げ、OpenAI の返答、マイクでの字幕、GPU での翻訳。
- **CI:** GitHub Actions の Windows で、Rust のテストと表示ページの試験を実行する。

## 10. マイルストーン

| 段階 | 内容 | 完了の条件 |
|---|---|---|
| M0 試作（完了） | Tauri の雛形、OBS プラグインの試作、ビルドと配布の流れ | プラグインで OBS にソースが自動で追加される。Tauri の NSIS のレジストリキーを確認した。待機時のメモリを測った |
| M1 取得と表示（完了） | YouTube と Twitch の取得、DB、履歴の取り込み、配信用サーバー、通常コメントの表示、コメント一覧 | 実際の配信で取得から OBS 表示まで動く。K1、K2、K5〜K10、K14 を満たす |
| M2 配信の道具（完了） | ニコニコ風、参加管理、おはよう、スパチャ一覧、配置の編集、プラグインの導入ボタン | K3、K4 を満たす。配置を2つ作り、シーンごとに切り替えられる |
| M3 読み上げと AI（完了） | VOICEVOX、絵文字の読み方と効果音、AI 返答、API キーの保存 | K15 を満たす。YuiAIAssistant を使わずに配信できる |
| M4 字幕と翻訳（完了） | 追加機能の導入、字幕、翻訳 | K11〜K13 を満たす。マイクと GPU で実際に動く |
| M5 公開（準備まで） | インストーラー、`SHA256SUMS.txt`、YuiLauncher の登録変更、README | YuiLauncher から入れて更新できる。Electron 版と YuiAIAssistant の利用をやめられる |

M1 から M4 の間は、配信では Electron 版を使い続ける。Electron 版は不具合修正だけにする。

## 11. 決定事項

2026-11-08 に決定。アプリの識別子と設定画面のフレームワークは、M0 の結果を見て同日に決めた。

| 項目 | 選択肢 | 決定 |
|---|---|---|
| コメント履歴の取り込み | 取り込まない / Electron 版の `comments.db` を一度だけ読み込む | 一度だけ読み込む（初見判定のため）。読み込みは読み取り専用で、元の DB は変更しない。YuiAIAssistant の DB は取り込まない |
| 前回の配信のコメント | 表示する / 今の配信だけ表示する | 今の配信だけ表示する |
| おはようのキーワード | 「おは」の部分一致 / 「おはよ」「おはー」「おっは」などの一覧 | 一覧にして、設定で編集できるようにする |
| AI 返答の提供元 | OpenAI のみ / ローカル LLM も選べる | OpenAI のみ。翻訳用の llama.cpp を流用できるかは M5 以降に検討 |
| OBS 表示のフォント | Google Fonts を読み込む / アプリに同梱する | 同梱する（外部への通信をなくす）。M1 でライセンスを確認する |
| Twitch の Bits | 支援として扱う / 通常コメント扱い | 支援として扱い、スパチャ一覧に出す |
| アプリの識別子 | `com.suzunayui.yuilivekit` / 新しい識別子 | `com.suzunayui.yuilivekit`。データ、レジストリキー、インストール先のどれも Electron 版と重ならないことを M0 で確認した |
| 配信用サーバーのポート | 5000 固定 / 空いているポートを選ぶ | 初期値 5000、使えなければ変更できる |
| 設定画面のフレームワーク | 素の JS / 軽量なフレームワーク | 素の JS（ES modules）。YuiLauncher と同じ。画面が複雑になったら再検討する |

## 12. 参考にする既存のコード

| 内容 | 場所 |
|---|---|
| YouTube の取得と解析、削除と配信終了の処理 | YuiLiveKit（Electron 版）`youtubeChat.js` |
| 入力の解釈（URL を含む） | YuiLiveKit `improve/small-fixes` ブランチの `youtubeChat.js` の `parseChatTarget`（`main` には未反映） |
| 参加管理の判定（チャンネル ID 対応） | YuiLiveKit `improve/small-fixes` ブランチの `participantQueue.js`（`main` には未反映） |
| 字幕と翻訳のワーカー | YuiLiveKit `subtitles/backend/`、`translation/backend/` |
| 追加機能の導入（Python、llama.cpp、ハッシュ固定） | YuiLiveKit `runtime/manager.js` |
| 字幕の伏字 | YuiLiveKit `subtitles/profanity.js` |
| Twitch の取得 | YuiAIAssistant `twitchChat.js` |
| 読み上げ、AI 返答、絵文字の効果音 | YuiAIAssistant `renderer.js`、`main.js` |
| OBS プラグインの導入手順 | YuiVoiceChanger `electron/obs-plugin-install.cjs` |
| Tauri の構成と WebView との接続 | YuiLauncher `tauri-prototype` ブランチの `src-tauri/` |

## 13. M0 の結果（2026-11-08）

試作は `next/` にある（Tauri の雛形は `next/src-tauri/`、OBS プラグインは `next/obs-plugin/`）。

### 13.1 OBS プラグイン

- OBS 33.0.1 のヘッダーと、インストール済みの `obs.dll`、`obs-frontend-api.dll` から作った import library でビルドできた。Qt は不要。DLL は 12.8KB。
- 本番の OBS を汚さないよう、OBS をコピーしたポータブル版（`portable_mode.txt`）で試した。プラグインは `obs-plugins\64bit\` に置いた。
- 結果:
  - 1回目の起動で、今のシーンに `YuiLiveKit` のブラウザソースが追加された。URL、1920×1080、画面全体に合わせる配置（境界ボックス）が保存された。
  - 2回目の起動では、追加済みとして何もしなかった。
  - ソースを消してから起動しても、自動では戻らなかった。
- 未確認: 「ツール」メニューからの手動追加、シーンコレクションの切り替え、`%ProgramData%` への配置で読み込まれること、OBS が実際に `/overlay` を読み込んだこと。

### 13.2 Tauri の雛形

- 設定画面と配信用サーバー（axum、127.0.0.1:5000）を持つ。`Host` ヘッダーが `127.0.0.1` と `localhost` 以外なら 403 を返すことを確認した。
- 起動時に `%APPDATA%\com.suzunayui.yuilivekit\obs-source.json` を書き出し、プラグインはここから URL を読む。
- rustc 1.99.1 では、`lto = true`（fat LTO）でコンパイラがアクセス違反で落ちたため、`lto = "thin"` にした。

### 13.3 インストーラー

- インストーラーは 1.2MB、実行ファイルは 4MB（Electron 45 版はインストーラー 133MB、実行ファイル 250MB）。
- レジストリキーは `Uninstall\YuiLiveKit`、インストール先は `%LOCALAPPDATA%\YuiLiveKit`。`InstallLocation` は引用符付きで書かれるが、YuiLauncher（Tauri 版）は引用符を外して読むので問題ない。
- インストーラーの実行（インストールとアンインストール）はまだ試していない。

### 13.4 メモリ（待機時、ウィンドウ表示、各1回の測定）

| | プロセス | ワーキングセット | プライベート |
|---|---|---|---|
| Tauri 雛形 | 7（本体と WebView2 が6つ） | 368MB | 166MB |
| Electron 45 版 | 4 | 389MB | 212MB |

- 差は約2割で、思ったより小さい。WebView2 も Chromium 系のため、待機時のメモリの大半は画面の表示にかかる。
- Electron 版は設定画面が大きく、雛形の画面は小さいため、同じ条件の比較ではない。機能を足した後にもう一度測る。
- 作り直しで大きく減るのは、主にインストーラーと実行ファイルの大きさ。

## 14. M1 の結果（2026-11-08）

実装は `next/src-tauri/src/`（本体はライブラリ、Tauri の部分は `main.rs` だけ）、OBS 表示は `next/overlay/`、設定画面は `next/ui/` にある。

### 14.1 受け入れ条件

| ID | 確認方法 | 結果 |
|---|---|---|
| K1 | Rust の単体テスト、OBS 表示と設定画面のテスト（Edge）、実際の Twitch 配信 | 合格。xQc の配信で、モデレーションによる削除1件が反映された |
| K2 | Rust の単体テスト、OBS 表示のテスト | 合格 |
| K5 | Rust の単体テスト（改名、同名の別人、取り込んだ履歴との結び付け） | 合格 |
| K6 | Rust の単体テスト（3回続いたら停止、1回なら続行） | 合格。実際の配信終了では未確認 |
| K7 | OBS 表示と設定画面のテスト（名前と本文の HTML、許可外の画像ホスト） | 合格 |
| K8 | Rust の単体テスト、OBS 表示のテスト（`$100.00` が円の金額帯にならない） | 合格 |
| K9 | 50万件、200配信、2万人の DB（リリースビルド） | 合格。最新50件の取得 1ms、履歴付きの保存 1ms |
| K10 | 実際のアプリ（デバッグビルド） | 合格。2つ目の起動は自分で終了し、ポートが使用中なら理由を表示して起動は続ける |
| K14 | Rust の単体テスト、実際の YouTube 配信（`@LofiGirl` と URL） | 合格 |

### 14.2 実際の配信での確認

`examples/chat_probe` で、件数だけを数えた（名前や本文は表示していない）。

- YouTube: Lofi Girl（`@LofiGirl`、`https://www.youtube.com/@LofiGirl/live`）、ウェザーニュース（`@weathernews`）で、40秒に約80件を取得した。すべてにチャンネル ID があった。
- Twitch: xQc、Kai Cenat、zackrawrr、summit1g で、25秒に36〜158件を取得した。すべてにユーザー ID があった。
- スーパーチャット、メンバーシップ、YouTube の削除アクションは、実際の配信ではまだ届いていない。解析は記憶と Electron 版に基づくテストデータでのみ確認している。

### 14.3 履歴の取り込み

- この PC の Electron 版の履歴（`%APPDATA%\YuiLiveKit\comments.db`、11MB）を、使い捨ての DB に取り込んだ。72配信、435人、19,664件を0.4秒で取り込み、元のファイルのハッシュは変わらなかった。

### 14.4 見つかった問題と対応

- **ポートの二重使用:** Windows では tokio の `TcpListener::bind` が `SO_REUSEADDR` を付けるため、ほかのアプリが同じポートを使っていても起動できてしまう可能性があった。標準ライブラリで確保してから tokio に渡すようにした（`server::bind`）。ほかのアプリが `SO_REUSEADDR` で後から同じポートを奪うことまでは防いでいない（`SO_EXCLUSIVEADDRUSE` は M2 以降で検討）。
- **状態の通知:** YouTube は取得のたびに「取得中」を通知していたため、変わったときだけ通知するようにした。
- **WebSocket の Origin:** ほかのページから `/overlay/ws` に接続できないよう、Origin を確認する。Origin がない接続（OBS 以外のツールなど）は許可している。

### 14.5 未確認のこと

- 設定画面は、Tauri の代わりの模擬で動かしたテストだけで、実際のウィンドウでの操作は確認していない。
- OBS の実物での表示（M0 のプラグインで追加したソースに、コメントが出ること）。
- 実際の配信終了の検知（K6）と、YouTube の削除アクションの実データ。

### 14.6 そのほか

- 開発用の起動（`tauri dev`）とインストール版は、同じデータフォルダ（`%APPDATA%\com.suzunayui.yuilivekit`）を使う。
- インストーラーは 2.3MB、実行ファイルは 6.9MB（M0 は 1.2MB と 4MB）。
- この PC の Rust には rustfmt と clippy が入っていないため、整形と静的解析はしていない。

## 15. M2〜M5 の結果（2026-11-08）

### 15.1 受け入れ条件

| ID | 確認方法 | 結果 |
|---|---|---|
| K3 | Rust の単体テスト、OBS 表示のテスト | 合格。待っている人の重複は `duplicate` として記録し、後から並ばない |
| K4 | Rust の単体テスト | 合格。全クリアのあとで同じコメントが届いても戻らない。設定画面は本体の一覧を表示するだけにした |
| K11 | Rust の単体テスト（起動できないワーカー）、実際のアプリ | 合格。ワーカーの起動に失敗しても、理由を表示して画面は開く |
| K12 | Rust の単体テスト、導入済み環境のコピー | 合格。コピー先で検索パスが書き直され、実際の翻訳と字幕のワーカーが動いた |
| K13 | Rust の単体テスト、模擬ワーカー | 合格。新しい外国語コメントだけを訳し、待ちが30件を超えると古いものを捨てる |
| K15 | 模擬の VOICEVOX と再生の記録 | 合格。順番待ちの間に削除、ブロック、NG ワードの追加があったものは読まない |

K1〜K15 はすべて、少なくとも自動テストで確認した。

### 15.2 段階ごとの内容

- **M2:** ニコニコ風、参加管理、おはよう、スパチャ一覧、配置の編集（名前付き、`?layout=`）、OBS プラグインの導入ボタン（`%ProgramData%` に置き、書き込めないときだけ管理者の確認）。
- **M3:** VOICEVOX の読み上げ、絵文字の読み方と効果音、AI 返答。再生と順番待ちは本体側で行う。API キーは Windows の資格情報マネージャーに保存し、画面には返さない。AI への指示文は固定し、コメントは引用として別に渡す。
- **M4:** 追加機能の導入（Electron 版の移植、SHA-256 を固定）、音声字幕、コメント翻訳。ワーカーはプロセスごと終了する。
- **M5:** 公開用のスクリプト（`npm run release`）、README、YuiLauncher の登録の変更（`yuilivekit-tauri` ブランチ、未マージ）。

### 15.3 実物での確認

- 翻訳: 導入済みの環境のコピーで、TranslateGemma が GPU（RTX 5070 Ti）で8秒で準備でき、英文を正しく訳した。日本語は訳さなかった。
- 字幕: 実際のワーカーが起動し、マイク4つと GPU 1つを見つけた。マイクの音声からの字幕は試していない。
- メモリ: すべての機能を入れた状態で、待機時のプライベートメモリは 178MB（目標 200MB 以下）。インストーラーは 4.9MB。

### 15.4 見つかった問題と対応

- ルートの `.gitignore` の `dist` に当たり、アプリに組み込むプラグインの DLL がコミットされていなかった。置き場所を `obs-plugin/prebuilt` にした。
- 配置を追加したとき、新しい配置が選ばれなかった（設定画面の不具合）。
- ポータブル版の OBS は `%ProgramData%` のプラグインを読まないため、プラグインの置き場所は普段の OBS でしか確認できない。普段の OBS には同じ形式で Spout2 のプラグインが入っている。

### 15.5 未確認のこと（公開前に実機で確認する）

- 実際のウィンドウでの操作全般（設定画面は模擬の Tauri でテストした）。
- OBS プラグインを `%ProgramData%` に入れたときの読み込みと自動追加、管理者の確認からの導入。
- VOICEVOX での実際の読み上げと、効果音の再生。
- OpenAI の実際の返答（模擬サーバーでのみ確認）。
- マイクの音声からの字幕と、追加機能の新規導入（ダウンロードから）。
- 実際の配信での、スパチャ、メンバーシップ、YouTube の削除、配信終了の検知。
- インストーラーでのインストールと、YuiLauncher からのインストールと更新。

### 15.6 公開と切り替え

公開と `main` への切り替えは、15.5 を実機で確認してから行う。手順は `next/README.md` の「公開の手順」にまとめた。YuiLauncher の登録の変更は、Tauri 版を公開したあとにマージする。
