# kbd-handoff

Linux に接続したキーボードを、同じ LAN 上の Windows PC で使えるようにするツールである。
Windows 側で `toggle` を実行するとキーボードが Windows へ移り、もう一度実行すると Linux へ戻る。

## 仕組み

- Linux 側の `kbd-handoff-sender` が常駐し、Windows からの接続を待つ。
- Windows 側の `kbd-handoff-receiver toggle` が切り替えを要求する。Windows 側は常駐しない。
- キーボードを渡している間、Linux 側はキーボードを占有するので、Super キーを含むショートカットも Windows に届く。
- 通信は事前共有鍵で認証し、暗号化する（Noise Protocol Framework）。

## インストール

[Releases](https://github.com/neverclear86/kbd-handoff/releases) から実行ファイルを入手する。

- Linux：`kbd-handoff-sender-*-x86_64-linux-gnu.tar.gz`（musl の静的リンク版もある）
- Windows：`kbd-handoff-receiver-*-x86_64-windows.zip`

ソースからビルドする場合は、[ビルド](#ビルド)を参照する。

## Linux 側の設定

### 1. 事前共有鍵を作る

```sh
kbd-handoff-sender gen-psk
```

出力された 64 桁の値を、Linux 側と Windows 側の設定ファイルに書く。

### 2. 設定ファイルを書く

`~/.config/kbd-handoff/sender.toml` に置く（見本は `sender.example.toml`）。

```toml
listen = "0.0.0.0:47820"
psk = "<事前共有鍵>"
device_name = "keyd virtual keyboard"
```

`device_name` には、転送するキーボードの名前を書く。
名前は `kbd-handoff-sender list-devices` で調べる。
keyd を使っている場合は `keyd virtual keyboard` を指定する。

### 3. キーボードを読む権限を与える

一般ユーザーは、既定では入力デバイスを読めない。
対象のキーボードだけを読めるように、udev ルールを置く。

```sh
echo 'SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="<デバイス名>", TAG+="uaccess"' \
  | sudo tee /etc/udev/rules.d/70-kbd-handoff.rules
sudo udevadm control --reload
sudo udevadm trigger --subsystem-match=input
```

ファイル名の番号は 73 より小さくする。

### 4. 常駐させる

systemd のユーザーサービスとして登録する。

```sh
install -Dm755 kbd-handoff-sender ~/.local/bin/kbd-handoff-sender
install -Dm644 kbd-handoff-sender.service ~/.config/systemd/user/kbd-handoff-sender.service
systemctl --user daemon-reload
systemctl --user enable --now kbd-handoff-sender.service
```

`kbd-handoff-sender.service` は、リポジトリでは `contrib/systemd/` にある。
ファイアウォールを使っている場合は、47820/TCP を開ける。

## Windows 側の設定

`kbd-handoff-receiver.exe` と同じフォルダなどに `receiver.toml` を置く（見本は `receiver.example.toml`）。

```toml
server = "192.0.2.10:47820"
psk = "<事前共有鍵>"
```

`server` には Linux 側のアドレスを書く。

## 使い方

Windows 側で次のコマンドを実行する。
ランチャーやマクロデバイスに登録しておくと便利である。

```powershell
kbd-handoff-receiver.exe toggle --config C:\path\to\receiver.toml
```

1回目でキーボードが Windows へ移り、2回目で Linux へ戻る。

Windows 側が応答しなくなったときは、Linux のキーボードで **左 Ctrl + 左 Alt + 左 Shift + Backspace** を同時に押すと、Linux へ戻る。
組み合わせは `sender.toml` の `emergency_release` で変えられる。
接続が切れたときも、キーボードは自動で Linux へ戻る。

ログの場所は次のとおりである。

- Linux：`journalctl --user -u kbd-handoff-sender`
- Windows：`%LOCALAPPDATA%\kbd-handoff\receiver.log`

## 制限

- UAC の確認画面、ロック画面、管理者権限で動くウィンドウには入力が届かない。
- Pause キーと電源系のキーは転送しない。
- マウスは転送しない。

## ビルド

```sh
# Linux 側
cargo build --release -p kbd-handoff-sender

# Windows 側（Linux 上でクロスビルドする。mingw-w64 が必要）
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu -p kbd-handoff-receiver

# テスト
cargo test --workspace
```

Rust 1.88 以上が必要である。
設計と決定事項の詳細は [docs/decisions.md](docs/decisions.md) にある。

## ライセンス

[MIT-0](LICENSE)
