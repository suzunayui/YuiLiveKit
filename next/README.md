# YuiLiveKit（作り直し中）

Rust + Tauri で作り直している次の YuiLiveKit です。仕様は [docs/rewrite-spec.md](../docs/rewrite-spec.md) を参照してください。

## アプリ

Rust と Node.js が必要です。

```powershell
npm install
npx tauri dev     # 開発用に起動
npx tauri build   # src-tauri/target/release/bundle/nsis にインストーラーを作成
```

## OBS プラグイン

Visual Studio の C++ ツールと、OBS 33.0.1 のインストールが必要です。初回は OBS のヘッダーを取得します。

```powershell
obs-plugin\build.cmd
```

`obs-plugin/build/yui-live-kit.dll` ができます。プラグインのソースは GPL-2.0-or-later です。
