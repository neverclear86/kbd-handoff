# kbd-handoff

kbd-handoff は、Linux に接続したキーボードを、同じ LAN 上の Windows PC からの要求に応じて Windows へ転送するツールである。
Linux と Windows を並べて使うとき、Linux 側のキーボードで Windows も操作できる。

実装は途中段階であり、実機での動作確認は済んでいない。
確認できていない範囲は「既知の制限」に書いた。

## 概要

切り替えは Windows 側から要求する。
1回目の要求でキーボードを Windows へ渡し、2回目の要求で Linux へ戻す。
マウスカーソルを画面の端へ出す操作は、切り替えのきっかけに使わない。

成果物は2つの実行ファイルである。

- **kbd-handoff-sender**：Linux 側の常駐プロセス。Windows からの要求を待ち、要求が来たらキーボードを占有して、キーイベントを Windows へ送る。次の要求で占有を解く。
- **kbd-handoff-receiver**：Windows 側の実行ファイル。常駐しない。`toggle` で切り替えを要求し、キーボードを受け取るときだけ受信用のプロセス（`session`）を起動する。このプロセスは、届いたキーイベントを Windows に入力し、キーボードが Linux に戻ると終了する。常駐させる `run` も選べる。

転送の対象はキーボードだけである。
マウスは対象にしていない。

要求を出す道具（マクロデバイスやランチャーなど）の設定は、このリポジトリの範囲に含めない。
`kbd-handoff-receiver toggle` を実行できる道具であれば、要求を出せる。
Linux 側を systemd のユーザーサービスとして登録する見本は、`contrib/systemd/` に置いている（「運用」を参照）。

## 仕組み

- Windows 側から Linux 側へ TCP で接続する。Windows のファイアウォールで受信ポートを開ける必要はない。
- `kbd-handoff-receiver toggle` は、受信プロセスを経由せず、Linux 側へ直接接続して切り替えを要求する。受信セッションがなければ、受信用のプロセスを起動する。そのプロセスが自分の接続で切り替えを要求し、キーボードを受け取る。
- Linux 側は evdev でキーボードを読み、`EVIOCGRAB` で占有する。占有中はコンポジターにキーが届かないので、Super キーを含むショートカットも Windows に届く。
- Windows 側は `SendInput` にスキャンコードを渡してキーを入力する。
- 通信は Noise Protocol Framework（Rust 実装の `snow`、パターンは `Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s`）で認証し、暗号化する。事前共有鍵を知らない相手とはハンドシェイクが失敗し、接続が切れる。暗号の処理はすべて `snow` に任せている。

通信のプロトコルの詳細は「決定事項」にある文書にまとめた。

## 動作環境

- Linux 側：evdev に対応した Linux。コンポジターの内側ではなく evdev で入力デバイスを読むので、コンポジターの種類には依存しない設計である。実際に動かして確認した環境は、まだない。
- Windows 側：x86_64 の Windows。対応する Windows のバージョンは確認していない。
- 2台は、TCP で到達できるネットワークに置く。
- ビルドには、edition 2024 と let chains を使うため、Rust 1.88 以上が必要である。

## ビルド

### Linux 側

```sh
cargo build --release -p kbd-handoff-sender
```

実行ファイルは `target/release/kbd-handoff-sender` に出力される。
`-p kbd-handoff-sender` を付けて、送信側だけをビルドする。
ワークスペース全体をまとめてビルドすると、受信側が使う tokio の機能が送信側にも入り、常駐時のメモリが少し増える。

メモリをさらに減らしたいときは、musl ターゲットで静的リンクする。

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release -p kbd-handoff-sender --target x86_64-unknown-linux-musl
```

実行ファイルは `target/x86_64-unknown-linux-musl/release/kbd-handoff-sender` に出力される。

### Windows 側

Windows 用の実行ファイルは、Linux 上でクロスビルドできる。
ターゲットは `x86_64-pc-windows-gnu` である。

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu -p kbd-handoff-receiver
```

リンカーに mingw-w64 が必要である。
Arch Linux では、`mingw-w64-gcc` パッケージを入れる。
ほかの配布形態では、mingw-w64 の gcc を含むパッケージを入れ、必要なら Cargo の設定でリンカーを指定する。

実行ファイルは `target/x86_64-pc-windows-gnu/release/kbd-handoff-receiver.exe` に出力される。

Linux 上で mingw-w64 の gcc を使い、リンクまで通ることを確認している。
ビルドが通らない場合は、リンカーの設定を疑う。

`kbd-handoff-receiver.exe` は GUI サブシステムの実行ファイルとしてビルドする。
ランチャーやマクロデバイスから起動したときに、コンソールウィンドウが開かないようにするためである。
出力先は「ログ」に書いた。

`kbd-handoff-receiver` は Linux でもビルドして実行できる。
Linux では、受け取ったキーイベントを入力せずにログへ出す。

### ビルド済みの実行ファイル

`v` で始まるタグを push すると、GitHub Actions が Linux 用と Windows 用の実行ファイルをビルドし、GitHub の Releases に置く。
Linux 用は glibc 版と musl の静的リンク版の2種類である。

### テスト

```sh
cargo test --workspace
```

送信側と受信側の状態機械は、実際の入力デバイスと `SendInput` の代わりに、メモリ上の偽の実装でテストしている。

## 設定

設定ファイルは TOML で、Linux 側と Windows 側で別々に用意する。
設定ファイルには事前共有鍵が入る。
実際の設定ファイル（`sender.toml`、`receiver.toml`）は `.gitignore` の対象であり、リポジトリには含めない。
このリポジトリには、値を伏せた見本として `sender.example.toml` と `receiver.example.toml` を置いている。
どちらの設定ファイルも、未知のキーはエラーになる。

### 事前共有鍵の生成

Linux 側で生成し、同じ値を両方の設定ファイルに書く。

```sh
kbd-handoff-sender gen-psk
```

32 バイトの乱数を、16 進数 64 桁で標準出力に出力する。
全バイトが 0 の鍵は拒否される。
鍵を共有する経路には、第三者に見られない手段を選ぶ。

### Linux 側

既定のパスは `$XDG_CONFIG_HOME/kbd-handoff/sender.toml` である。
`XDG_CONFIG_HOME` が未設定か空のときは、`~/.config/kbd-handoff/sender.toml` を使う。
別の場所に置くときは、`run` に `--config PATH` を渡す。

```sh
mkdir -p ~/.config/kbd-handoff
cp sender.example.toml ~/.config/kbd-handoff/sender.toml
chmod 600 ~/.config/kbd-handoff/sender.toml
```

`chmod` は、事前共有鍵を他のユーザーに読ませないための操作である。
プログラムは権限を検査しない。

| キー | 必須 | 既定値 | 内容 |
| --- | --- | --- | --- |
| `listen` | いいえ | `0.0.0.0:47820` | 待ち受けるアドレスとポート |
| `psk` | はい | なし | 事前共有鍵（16 進数 64 桁） |
| `device_name` | はい | なし | 占有する入力デバイスの名前 |
| `emergency_release` | いいえ | `["KEY_LEFTCTRL", "KEY_LEFTALT", "KEY_LEFTSHIFT", "KEY_BACKSPACE"]` | 緊急の解放キー（evdev の `KEY_*` 名の一覧） |

`listen` の既定値は、すべてのネットワークインターフェースで待ち受ける。
特定のインターフェースに限るときは、そのアドレスを書く。

### 入力デバイスの名前

`device_name` に書く名前は、次のコマンドで調べる。

```sh
kbd-handoff-sender list-devices
```

`KEY_A` と `KEY_ENTER` を報告する入力デバイスの名前を、1行に1つ出力する。
同じ名前で `KEY_A` を報告するデバイスが複数あるときは、名前のあとにタブを挟んで、その数を `(2 devices share this name; ambiguous)` の形で添える。
この形で出た名前は、`device_name` に使えない。
読む権限のないデバイスは一覧に出ない。
一覧が空のときは、権限の不足を疑う（次節）。

名前は完全一致で照合する。
`/dev/input/eventN` の番号は変わりうるので、占有を始めるたびに名前から探す。
物理キーボードは、同じ名前のデバイスを複数出すことがある（メディアキー用など）。
同じ名前のデバイスが複数あるときは、そのうち `KEY_A` を報告するものが1つだけなら、それを使う。
`KEY_A` を報告するものが複数ある場合と、1つもない場合は、どれを占有すべきか決められないので、エラーにして占有しない。
エラーのときは、1つのキーボードにしかない名前を指定する。

keyd を使っている環境では、`device_name` に `keyd virtual keyboard` を指定する。
keyd は物理キーボードを自分で占有するので、物理デバイスは読めない。
keyd が出力する仮想キーボードを読むと、keyd による再配置が Windows での入力にもそのまま反映される。

### Windows 側

既定のパスは `%APPDATA%\kbd-handoff\receiver.toml` である。
別の場所に置くときは、`run` と `toggle` に `--config PATH` を渡す。
Windows 以外では、Linux 側と同様に `$XDG_CONFIG_HOME/kbd-handoff/receiver.toml`（または `~/.config/kbd-handoff/receiver.toml`）を使う。

```powershell
mkdir $env:APPDATA\kbd-handoff
copy receiver.example.toml $env:APPDATA\kbd-handoff\receiver.toml
```

| キー | 必須 | 既定値 | 内容 |
| --- | --- | --- | --- |
| `server` | はい | なし | Linux 側の `host:port`。例は `192.0.2.10:47820` |
| `psk` | はい | なし | Linux 側と同じ事前共有鍵 |

見本の `psk` はわざと不正な値にしてあり、置き換えないと読み込みに失敗する。

## 入力デバイスを読む権限

一般ユーザーは、既定では `/dev/input/event*` を読めない（`root:input` の 660）。
`kbd-handoff-sender` を一般ユーザーで動かすには、読む権限を与える設定が必要であり、その設定には管理者権限（sudo）が要る。

与え方は3つの候補があり、どれを採用するかは決まっていない。

- **`input` グループへの追加**：設定が最も単純である。ただし、そのユーザーの全プロセスが全入力デバイスを読めるようになる。ユーザー権限で動く任意のプログラムが、パスワードを含むキー入力を読める。
- **対象デバイスに限った udev ルール**：読める範囲を `device_name` のデバイスに絞れる。ルールにはデバイス名を書くので、環境ごとに書き換えが要る。
- **システムサービスとしての実行**：入力デバイスを読む権限を、サービスのユーザーに閉じ込められる。サービスの登録はこのリポジトリの範囲外である。

udev ルールの例を示す。

```udev
# /etc/udev/rules.d/70-kbd-handoff.rules
SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="<デバイス名>", TAG+="uaccess"
```

`<デバイス名>` は、`device_name` と同じ名前に置き換える。
`TAG+="uaccess"` は、現在ログインしているユーザーのシートに、デバイスへのアクセス権を与える。
このタグは systemd の `73-seat-late.rules` が処理するので、ルールのファイル名は 73 より小さい番号にする。
グループで与えるときは、`GROUP="<グループ名>", MODE="0660"` を使う。
ルールを書いたら、次のコマンドで読み込ませる。

```sh
sudo udevadm control --reload
sudo udevadm trigger --subsystem-match=input
```

## 運用

Linux 側の送信プロセスだけを常駐させる。
Windows 側は常駐させず、切り替えのたびに `toggle` を実行する。

### Linux 側を systemd のユーザーサービスにする場合

見本のユニットファイルを `contrib/systemd/kbd-handoff-sender.service` に置いている。
実行ファイルを `~/.local/bin` に置き、ユニットファイルを複製して有効にする。

```sh
cargo build --release -p kbd-handoff-sender
install -Dm755 target/release/kbd-handoff-sender ~/.local/bin/kbd-handoff-sender
install -Dm644 contrib/systemd/kbd-handoff-sender.service ~/.config/systemd/user/kbd-handoff-sender.service
systemctl --user daemon-reload
systemctl --user enable --now kbd-handoff-sender.service
```

設定ファイルは既定のパス（`~/.config/kbd-handoff/sender.toml`）から読む。
別のパスを使うときは、`systemctl --user edit kbd-handoff-sender.service` で `ExecStart` を上書きする。
ユーザーサービスは、そのユーザーがログインしている間だけ動く。
ログは `journalctl --user -u kbd-handoff-sender` で読む。

ユニットは、異常終了したときに再起動する（`Restart=on-failure`）。
また、権限を絞る設定（`NoNewPrivileges`、`ProtectSystem=strict`、`ProtectHome=read-only`、`PrivateTmp` など）を入れている。
これらの設定は `/dev` と TCP/IP の通信を制限しない。
入力デバイスを読む権限は、サービスではなくユーザーに与える（前節）。

### Windows 側を常駐させない場合

ランチャーやマクロデバイスには、次のコマンドを登録する。

```powershell
kbd-handoff-receiver.exe toggle --config C:\path\to\receiver.toml
```

`--config` を省くと、既定のパス（`%APPDATA%\kbd-handoff\receiver.toml`）を使う。

1回目の実行では、受信セッションがないので、`toggle` は `session` をバックグラウンドで起動して終了する。
起動された `session` は、Linux 側へ接続して自分で切り替えを要求し、キーボードを受け取る。
2回目の実行では、Linux 側が占有を解き、`session` に解放を伝える。
`session` は押されたままのキーをすべて離して終了する。
緊急の解放キーで占有が解けたとき、接続が切れたとき、生存確認が途絶えたときも、`session` はキーを離して終了する。
再接続はしない。

`session` は、`toggle` を実行したプロセスから切り離して起動する。
コンソールを持たず、新しいプロセスグループに属する。
ランチャーがジョブオブジェクトで子プロセスをまとめて終了させる場合に備えて、ジョブの外で起動することを試み、許されなければジョブの中で起動する。

### Windows 側を常駐させる場合

`run` で常駐させることもできる。
`run` は接続を保ち、切れたら再接続する。
この場合、`toggle` は常駐中の受信セッションに対して切り替えを要求し、`session` を起動しない。
ただし、`run` が再接続を待っている間に `toggle` を実行すると、受信セッションがないので `session` を起動する。
そのあと `run` が再接続すると、新しいセッションが `session` を置き換え、占有は解ける。
常駐の登録（スタートアップへの登録など）は、このリポジトリの範囲に含めない。

## 使い方

### 起動

```sh
kbd-handoff-sender run [--config PATH]
```

```powershell
kbd-handoff-receiver run [--config PATH] [--dry-run]
kbd-handoff-receiver session [--config PATH] [--dry-run]
```

`run` は常駐する受信プロセスである。
接続が切れても、最初は 1 秒、最大 10 秒まで間隔を延ばして再接続を繰り返す。
接続が確立したあとに切れたときは、間隔を 1 秒から数え直す。
起動の順序は問わない。

`session` は、1回だけの受信セッションである。
通常は `toggle` が起動するので、手で実行する必要はない。
接続して切り替えを要求し、キーボードが Linux に戻るか接続が切れると終了する。
終了コードは、解放を受けて終わったときは 0、Linux 側が占有しなかったときは 3（`failed`）か 2（`no-session`）、その他の誤りでは 1 である。

送信プロセスが受け付ける受信セッションは、同時に1つだけである。
新しいセッションが来ると、古いセッションを切る。
占有中にセッションが置き換わったときは、占有を解く。

`--dry-run` は、キーを入力せずにログへ出す。
Windows 以外では、このオプションを付けなくても常にログへ出す。
このログにはキーコードが含まれるので、パスワードを入力する場面では使わない。
Windows でログがファイルに出る場合（「ログ」を参照）、キーコードはファイルに残る。

### 切り替えの要求

Windows 側で次のコマンドを実行する。

```powershell
kbd-handoff-receiver toggle [--config PATH]
```

占有していなければ占有を始め、占有していれば解く。
結果を標準出力に出力し、終了コードで知らせる。

| 出力 | 終了コード | 意味 |
| --- | --- | --- |
| `grabbed` | 0 | 常駐中の受信セッションへキーボードを渡した |
| `released` | 0 | キーボードを Linux へ戻した |
| `session-started` | 0 | 受信セッションがなかったので、`session` を起動した |
| `failed` | 3 | 占有に失敗した（デバイスが見つからない、権限がない、キーが離されなかった、など） |

`session-started` は、`session` を起動できたことだけを示す。
占有に成功したかどうかは、`toggle` の終了コードには表れない。
`session` が占有に失敗したときは、その旨をログに出して終了する。

接続の失敗や設定ファイルの誤りなど、その他の誤りでは、終了コード 1 でエラーメッセージを標準エラー出力に出力する。
コマンドの全体は 10 秒で打ち切る。

占有を始める前に、全キーが離されるまで待つ。
3 秒待っても離されなければ、占有をあきらめて `failed` を返す。
キーを押したままコマンドを実行する道具を使うときは、この待ちに注意する。

`toggle` は、受信プロセスとは別に、Linux 側へ直接接続する。
そのため、受信プロセスが動いていなくても実行できる。

### 緊急の解放キー

Windows 側が応答しないときは、Linux 側のキーボードで解放キーを同時に押す。
既定値は、左 Ctrl、左 Alt、左 Shift、Backspace の同時押しである（`KEY_LEFTCTRL`、`KEY_LEFTALT`、`KEY_LEFTSHIFT`、`KEY_BACKSPACE`）。
この判定は送信側だけで行うので、受信側が応答しなくても占有を解いて、キーボードを Linux に返す。
判定は占有中にだけ働く。

変更するには、Linux 側の設定ファイルで `emergency_release` に evdev の `KEY_*` 名を並べる。

```toml
emergency_release = ["KEY_LEFTCTRL", "KEY_LEFTALT", "KEY_LEFTSHIFT", "KEY_BACKSPACE"]
```

キー名は大文字小文字を区別する。
空の一覧と未知のキー名は、起動時にエラーになる。
解放キーは、通常の入力と衝突しない組み合わせを選ぶ。

### ログ

`kbd-handoff-sender` と、Windows 以外で動かす `kbd-handoff-receiver` は、ログを標準エラー出力に出力する。
ログの水準は環境変数 `RUST_LOG` で指定する。
未設定のときは `info` である。
`kbd-handoff-sender` は、水準を表す1語（`off`、`error`、`warn`、`info`、`debug`、`trace`）だけを受け付ける。
`kbd_handoff_sender=debug` のようなモジュールごとの指定は受け付けず、警告を出して `info` で動く。
`kbd-handoff-receiver` は、モジュールごとの指定も受け付ける。

```sh
RUST_LOG=debug kbd-handoff-sender run
```

Windows の `kbd-handoff-receiver.exe` は GUI サブシステムの実行ファイルなので、起動時にコンソールを持たない。
出力先は次の順で決める。

1. 親プロセスが標準エラー出力をファイルかパイプへリダイレクトしていれば、そこへ出力する。
2. 親プロセスがコンソールを持っていれば（コマンドプロンプトや PowerShell から実行した場合）、そのコンソールに接続して出力する。
3. どちらでもなければ（ランチャーやマクロデバイスから起動した場合）、`%LOCALAPPDATA%\kbd-handoff\receiver.log` に追記する。

`session` は常に 3 のファイルへ出力する。
`toggle` を実行したコンソールに接続すると、そのウィンドウを閉じたときに `session` が終了するためである。
ファイルの各行には、プロセス ID を `receiver{pid=…}` の形で付ける。
`toggle` の結果とエラーも、ファイルへ出力する場合はログに記録する。
ファイルが 1 MiB に達すると、`receiver.log.old` に名前を変えて新しいファイルに切り替える（古い `receiver.log.old` は上書きする）。

コマンドプロンプトと PowerShell は、GUI サブシステムの実行ファイルの終了を待たずに次のプロンプトを出す。
出力はプロンプトのあとに表示され、終了コードも取れない。
終了を待って終了コードを得るには、コマンドプロンプトでは `start /wait`、PowerShell では `Start-Process -Wait -PassThru` を使う。

## ファイアウォール

Linux 側でファイアウォールを使う環境では、待ち受けポート（既定は 47820/TCP）を開ける。
`listen` を変えたときは、そのポートを開ける。
Windows 側は、Linux 側へ接続するだけなので、受信ポートを開ける必要はない。

例として、ufw で `192.0.2.0/24` からの接続だけを許可する。

```sh
sudo ufw allow from 192.0.2.0/24 to any port 47820 proto tcp
```

アドレスは、実際の LAN のものに置き換える。

## 安全性

### キーボードを失わない仕組み

次の場合に、送信プロセスは占有を解いてキーボードを Linux に返す。

- 受信セッションの接続が切れたとき
- 3 秒間なにも届かず、生存確認が途絶えたとき
- 送信キューが詰まるなど、受信側へ送れなくなったとき
- 入力デバイスの読み取りでエラーが起きたとき
- 新しい受信セッションが古いセッションを置き換えたとき
- 緊急の解放キーが押されたとき
- `SIGINT` または `SIGTERM` を受けて終了するとき

プロセスが異常終了した場合は、ファイルディスクリプターが閉じられ、カーネルが占有を解く。

生存確認では、受信側は 1 秒ごとに `Ping` を送り、送信側は 1 秒間なにも送らなかったときに `Ping` を送る。
どちらの側も、3 秒間なにも届かなければ接続が切れたとみなす。

### 押しっぱなしの防止

キーが押された状態で占有を始めると、コンポジターに解放イベントが届かず、Linux 側でキーが押されたままになる。
これを避けるため、占有を始めるときは全キーが離された状態まで待つ。
占有を始める前にキューに溜まっていたイベントは、Windows へ送らずに捨てる。

占有を解くとき、受信プロセス（`session` と `run`）は、押されたままのキーをすべて Windows 側で離す。
接続が切れたときと、受信プロセスが終了の通知を受けて終了するときも同じである。
終了の通知は、`Ctrl+C`、`Ctrl+Break`、コンソールウィンドウを閉じる操作である。
これらはコンソールに接続したプロセスにだけ届く。
`toggle` が起動した `session` はコンソールを持たないので、これらの通知を受けない。
ログオフとシャットダウンの通知も待ち受けているが、`user32.dll` を読み込むプロセス（`SendInput` を使う受信プロセスが該当する）には届かないと Windows の文書にあり、実機では確認していない。

自動リピート（evdev の value 2）は、Windows 側でキーを押す入力として送り直す。
`SendInput` で押したキーは、Windows 側で自動リピートしないためである。

### 認証と暗号化

転送するキー入力にはパスワードが含まれうる。
そのため、事前共有鍵による認証と暗号化を必須にしている。
暗号は実績のあるライブラリ（`snow`）に任せ、自作していない。
`snow` は C のコードを含まない構成で使っている。

Linux 側が待ち受けるポートには、鍵を知らない相手も接続できるが、ハンドシェイクが失敗して接続が切れる。
認証前の相手に使わせる資源を抑えるため、ハンドシェイク中の接続は同時に 16 本までとし、それを超えた接続はすぐに切る。
受け付ける Noise メッセージの長さにも上限を設けている（ハンドシェイクは 256 バイト、それ以降は 1024 バイト）。

## 既知の制限

- UAC の確認画面とロック画面には、入力が届かない。これらはセキュアデスクトップで動作し、通常のプロセスからの入力を受け付けないためである。
- 管理者権限で動くウィンドウには、受信プロセスを同じ権限で動かさない限り、入力が届かない。
- これらの場面では、Windows 側のキーボードを使う。
- Pause キーは転送できない。Pause キーは `E1` で始まる複数バイトのスキャンコードを送るが、`SendInput` のスキャンコード指定では表現できず、対応表に載せていない。電源、スリープ、復帰のキー（`KEY_POWER`、`KEY_SLEEP`、`KEY_WAKEUP`）も、誤って Windows を停止させないために対応表に載せていない。対応表にないキーは、無視される。
- 受信プロセスが強制終了されると、Windows 側で押されたままのキーが残りうる。受信プロセスは、接続が切れたときや終了の通知（`Ctrl+C`、`Ctrl+Break`、コンソールウィンドウを閉じる操作）を受けたときにキーを離すが、タスクマネージャーなどによる強制終了ではその処理が動かない。`toggle` が起動した `session` はコンソールを持たないので、終了させる手段は、もう一度 `toggle` を実行するか、緊急の解放キーを押すか、強制終了するかである。
- `toggle` が `session-started` を返しても、占有に成功したとは限らない。結果はログで確かめる。
- 占有を始める瞬間にキーが押されると、Linux 側でそのキーが押されたままになることがある。全キーが離されたことを確かめてから占有するまでの短い間に押され、占有したあとに離されたキーが該当する。送信プロセスは、この状況を検出できた場合に警告をログに出す。その場合は、キーボードを Linux に戻してから、そのキーをもう一度押して離す。
- ハンドシェイク中の接続が同時に 16 本を超えると、それ以降の接続は切られる。同じネットワーク上の誰かが認証しないまま接続を張り続けると、受信プロセスと `toggle` コマンドが接続できなくなりうる。緊急の解放キーは、この影響を受けない。
- マウスは転送しない。
- 切り替えの要求は、`toggle` コマンドだけで受ける。ホットキーでの要求は実装していない。

## 決定事項

名前、構成、通信のプロトコル、緊急の解放キーの既定値、Windows 側の運用など、実装で決めた事項は [docs/decisions.md](docs/decisions.md) にまとめている。

## ライセンス

MIT No Attribution（MIT-0）である。
著作権表示を含めずに、利用、改変、再配布できる。
全文は [LICENSE](LICENSE) にある。
