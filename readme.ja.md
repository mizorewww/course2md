<div align="center">

<img src="desktop/assets/128x128@2x.png" width="96" alt="course2md アプリアイコン">

# course2md

**講義動画を、読んで、検索して、保存できる図解ノートに変換します。**

YouTube や Bilibili のリンクを貼り付けるか、ローカルの動画をドロップしてください。course2md はスライドが切り替わるたびにスクリーンショットを撮り、その下に話された内容の文字起こしを添えて、結果をお使いのコンピューター上のライブラリに保存します。

[![Latest release](https://img.shields.io/github/v/release/mizorewww/course2md?label=download)](https://github.com/mizorewww/course2md/releases/latest)
[![License: MIT](https://img.shields.io/github/license/mizorewww/course2md)](LICENSE)
![macOS · Windows · Linux](https://img.shields.io/badge/desktop-macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-blue)

[English](readme.md) · [中文](readme.zh.md) · **日本語** · [ダウンロード](#インストール) · [Wiki](https://github.com/mizorewww/course2md/wiki)

</div>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/en/reader-dark.png">
  <img src="docs/images/en/reader-light.png" alt="要約、アウトライン、組版された数式を含む講義ノートを表示している course2md のリーダー">
</picture>

## course2md を選ぶ理由

- **講義の流れどおりのノート。** スライドごとに 1 枚のスクリーンショットと、その下に段落に整えた対応する文字起こしを配置し、スクリーンショットと一緒に、どこでも開ける普通の `course.md` として保存します。必要なときはいつでも Web ページ、.zip、JSON としてエクスポートできます。
- **初期設定でプライバシーを保護。** 音声認識はお使いのコンピューター上で行われます。Apple Silicon ではそのまま動作し、その他のシステムではグラフィックカード、CPU、または Intel Core Ultra の NPU を使用します。音声がコンピューターの外に送られるのは、クラウドサービスを選んだ場合だけです。
- **字幕を優先。** 動画にすでに字幕がある場合はそれをそのまま使うため、音声認識は不要です。
- **AI はオプション。** OpenAI 互換の任意のサービス、ローカルの Ollama サーバー、または ChatGPT のサブスクリプションを使って、校正、要約、2 か国語のノート、数式の組版を行えます。有効にするまではオフです。
- **4 つの言語。** English、简体中文、繁體中文、日本語に対応し、システムの言語に従います。

## アプリの紹介

### 動画を追加する

スタートページである **ホーム** にリンクを貼り付けるか、ファイルをドラッグしてください（course2md のロゴをクリックするとここに戻れます）。course2md はまず動画を読み込むので、変換を始める前にタイトル、長さ、処理オプションを確認できます。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/en/home-dark.png">
  <img src="docs/images/en/home-light.png" alt="プレビューされた動画とその処理オプションが表示されたホームページ">
</picture>

### 読む、探す、コピーする

リーダーは要約と要点から開き、ノートの横にセクションのアウトラインを表示し、ノート全体からテキストを検索できます。数式は組版され、文章をコピーすると、各数式の LaTeX を含む元のテキストが得られます。

### すべてのスライドを見る

**スクリーンショット** ビューでは、講義全体をひと目で確認できます。任意のスライドを開くと、大きく表示してズームやパンを行い、その時点で話されていた内容を読むことができます。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/en/screenshots-dark.png">
  <img src="docs/images/en/screenshots-light.png" alt="講義スライドがグリッド表示されたスクリーンショットビュー">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/en/viewer-dark.png">
  <img src="docs/images/en/viewer-light.png" alt="拡大されたスライドと、それが表示されていたときに話された文字起こし">
</picture>

### ライブラリを管理する

ノートはライブラリに保存され、入れ子のフォルダー、ドラッグ＆ドロップ、サブフォルダーまで含めた検索が使えます。複数のライブラリの場所を持つことができ、削除したノートはゴミ箱（Windows と Linux ではごみ箱）に移動します。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/en/library-dark.png">
  <img src="docs/images/en/library-light.png" alt="フォルダーツリーとノートカードが表示されたライブラリ">
</picture>

### バックグラウンドで処理する

変換はタスクとして実行され、一時停止、再開、並べ替えができます。各ステップは進捗を表示し、モデルのダウンロードではサイズ、速度、残り時間が表示されます。一部が失敗した場合は、完了した部分を読み、残りだけをやり直せます。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/en/tasks-dark.png">
  <img src="docs/images/en/tasks-light.png" alt="実行中の変換とその進捗が表示されたタスクページ">
</picture>

### 自分好みにする

**設定 → 外観** で、10 種類のテーマから選び、文字サイズを調整し、動きを減らすことができます。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/en/settings-dark.png">
  <img src="docs/images/en/settings-light.png" alt="テーマのプレビューが表示された外観設定">
</picture>

## インストール

デスクトップアプリには変換エンジンと、それが実行する動画ツール（`ffmpeg`、`ffprobe`、`yt-dlp`）が含まれているため、以下のダウンロードはほかに何もインストールせずに動作します。アプリを使うのにコマンドラインツールは必要ありません。

### macOS（Apple Silicon、macOS 15 以降）

[Homebrew](https://brew.sh) を使うと、アプリと一緒に `yt-dlp` と `aria2`（モデルのダウンロードを高速化）がインストールされます。

```sh
brew install --cask mizorewww/tap/course2md-gui
```

その後、**アプリケーション** から **course2md** を開いてください。Homebrew を使わない場合は、[DMG をダウンロード](https://github.com/mizorewww/course2md/releases/latest/download/course2md-gui-macos-arm64.dmg)して **course2md** をアプリケーションにドラッグしてください。詳しくは [macOS ガイド](https://github.com/mizorewww/course2md/wiki/GUI-Installation#macos-apple-silicon)をご覧ください。

### Windows（x64）

1. [ポータブル ZIP をダウンロード](https://github.com/mizorewww/course2md/releases/latest/download/course2md-desktop-windows-AMD64.zip)し、**すべて展開** して恒久的なフォルダーに置いてください。`ffmpeg`、`yt-dlp`、`aria2`、`uv`（Intel NPU での認識）、および GPU（Vulkan）と CPU での認識用の `llama-server` が含まれています。GPU と CPU での認識には [Microsoft Visual C++ ランタイム](https://aka.ms/vs/17/release/vc_redist.x64.exe)も必要です。ない場合は **チェックを実行** がそれを知らせ、インストーラーへのリンクを表示します。
2. 展開したフォルダーから **course2md-desktop.exe** を開き、ほかのファイルはその隣に置いたままにしてください。

### Arch Linux / CachyOS（x86_64）

```sh
yay -S course2md-gui-bin
```

（または `paru -S course2md-gui-bin`）。これでアプリ、システムの動画ツール、グラフィックライブラリがインストールされます。GPU には動作する Vulkan ドライバーが必要です。[Linux ガイド](https://github.com/mizorewww/course2md/wiki/GUI-Installation#arch-linux--cachyos)をご覧ください。

### Ubuntu とその他の Linux（x86_64）

[tar.gz をダウンロード](https://github.com/mizorewww/course2md/releases/latest/download/course2md-desktop-linux-x86_64.tar.gz)し、[Linux ガイド](https://github.com/mizorewww/course2md/wiki/GUI-Installation#other-linux-x64-distributions)に従ってグラフィックライブラリをインストールしてから、展開して `./course2md-desktop` を実行してください。Ubuntu 24.04 でビルドされており、`ffmpeg`、`yt-dlp`、`uv`、`llama-server` が含まれています（GPU での認識にはお使いの Vulkan ドライバーを使用します）。

Intel Mac と Linux ARM64 にはまだデスクトップアプリがありません。[コマンドラインツール](#コマンドラインツール)をお使いください。

## はじめに

1. **course2md** を開いてセットアップに従ってください。音声の認識方法を選び、必要に応じて AI サービスと Bilibili を接続し、音声モデル（1〜2.4 GB、初回のみ）をダウンロードします。
2. 足りないものがある場合は、**設定 → アプリ → チェックを実行** でインストールが必要なツールを確認できます。
3. **ホーム** でリンクを貼り付けるか動画をドロップし、プレビューを確認して **変換を開始** をクリックします。完成したノートは **ノートライブラリ** に表示されます。

ノートはデフォルトのライブラリに保存されます。ほかのフォルダーは **設定 → ストレージ** で追加できます。インターフェースはシステムの言語に従います。**設定 → 外観 → 表示言語** で変更できます。詳しくは[初回起動ガイド](https://github.com/mizorewww/course2md/wiki/GUI-Installation#first-launch)をご覧ください。

## よくある質問

**変換時に `ffmpeg` が見つからないと表示されます。** ダウンロードしたアプリは `course2md-desktop` の隣に専用の `ffmpeg` を持っています。パッケージ全体を展開するか、再インストールしてください。CLI と Arch のパッケージはシステムのものを使用するので、インストールしてください（macOS では `brew install ffmpeg`、Windows では `winget install --id Gyan.FFmpeg -e`、Linux ではお使いのパッケージマネージャー）。**設定 → アプリ → チェックを実行**（CLI では `course2md doctor`）で、各ツールがどこから来ているかを確認できます。

**Bilibili の動画が失敗する、または低画質になります。** **設定 → サービスとアカウント → Bilibiliアカウント** で QR コードをスキャンしてログインしてください。

**モデルのダウンロードはどのくらいの大きさで、どこに保存されますか？** Apple Silicon では約 1〜2.3 GB（`~/Library/Caches/qwen3-speech/`）、それ以外では 2.4 GB（`~/.cache/course2md/models/`）です。中国本土では国内のミラー（ModelScope / hf-mirror.com）からダウンロードし、それ以外の地域ではこれらがフォールバックになります。ダウンロードは中断後に再開でき、GPU/CPU 用のモデルは SHA-256 で検証されます。

**1.7 からアップグレードしました。ノートはどこにありますか？** 1.x で作成したノートは `course.md` / `course.html` として元の場所に残っており、引き続き任意のエディターやブラウザーで開けますが、2.0 のライブラリには 2.0 で作成したノートだけが表示されます。ダウンロード済みのモデル、Bilibili のログイン、CLI の `config.toml` は引き続き使えます。認識サービスと AI サービスはセットアップの手順で再度設定してください。

その他の回答は[トラブルシューティング](https://github.com/mizorewww/course2md/wiki/Troubleshooting)をご覧ください。

## コマンドラインツール

ターミナルやスクリプトのほうがお好みですか？ `course2md` CLI でも同じ変換ができます。デスクトップアプリと並べてインストールでき、両者は Bilibili と ChatGPT のログイン、ダウンロード済みの音声モデルを共有し、設定はそれぞれ別に保持します。

```sh
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash   # macOS / Linux, into ~/bin
brew install mizorewww/tap/course2md                                                      # or Homebrew
yay -S course2md-bin                                                                      # or Arch
```

`ffmpeg` が必要です。オンラインのリンクには `yt-dlp` も必要で（ない場合はスクリプトがインストールします）、`aria2` があるとモデルを複数の接続でダウンロードします。Apple 以外のシステムでローカル認識を行うには `llama-server` が必要です。`course2md doctor` ですべてを確認できます。

```sh
course2md ./lecture.mp4                        # a local file
course2md https://www.youtube.com/watch?v=…    # or a YouTube / Bilibili link
course2md ./lecture.mp4 -o ./notes             # choose where notes go
```

初回の実行ではいくつかのセットアップの質問があります。ノートは出力フォルダーの下に `platform/title/ID/` として保存されます。

| コマンド | 用途 |
| --- | --- |
| `course2md --login bilibili` / `--logout bilibili` | QR コードで Bilibili にログイン / 保存されたログインを削除 |
| `course2md --login ollama` / `--login codex` | ローカルの Ollama サーバーを使用 / Codex のログイン経由で ChatGPT のサブスクリプションを使用 |
| `course2md doctor` | ツール、音声バックエンド、設定をチェック |
| `course2md models list` / `models prepare` | ダウンロード済みのモデルを確認 / モデルを事前にダウンロード |
| `course2md llm setup` / `llm status` / `llm disable` | AI 校正を設定、確認、またはオフにする |
| `course2md summarize ./notes` | 既存のノートに AI 要約を追加 |
| `course2md config init` / `config show` | 設定テンプレートを作成 / 設定を表示 |

CLI の設定は `~/.config/course2md/config.toml`（Windows では `%APPDATA%\course2md\config.toml`）にあり、オプションはそれより優先されます。`course2md remove` は CLI の AI サービス設定と ChatGPT のログインを削除します。`--lang en|zh-Hans|zh-Hant|ja`（または `COURSE2MD_LANG`）で言語を設定します。`--json` の出力はスクリプト向けに簡体字中国語のままです。`<command> --help` と [CLI ガイド](https://github.com/mizorewww/course2md/wiki/CLI-Guide)をご覧ください。

## ヘルプとコントリビュート

[ドキュメント（Wiki）](https://github.com/mizorewww/course2md/wiki) · [アップグレードとアンインストール](https://github.com/mizorewww/course2md/wiki/GUI-Installation#upgrade-and-uninstall) · [トラブルシューティング](https://github.com/mizorewww/course2md/wiki/Troubleshooting) · [問題を報告](https://github.com/mizorewww/course2md/issues) · [リリースノート](https://github.com/mizorewww/course2md/releases)

開発者向け：[コントリビュート](AGENTS.md) · [デスクトップ開発](desktop/README.md) · [エンジンのアーキテクチャ](docs/DESIGN.md) · [ベンチマーク](docs/BENCHMARKS.md) · [リリース](docs/PACKAGING.md) · [変更履歴](CHANGELOG.md)

course2md は [MIT license](LICENSE) のもとで公開されています。
