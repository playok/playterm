# 기여 안내 / Contributing

버그·기능 요청은 GitHub 이슈로, 보안 취약점은 [SECURITY.md의 비공개 보고](SECURITY.md)로 보내 주세요. 한국어와 영어 모두 사용할 수 있습니다.

Use GitHub issues for bugs and features, and [SECURITY.md](SECURITY.md) for private vulnerability reports. Korean and English are welcome.

## 빌드 / Build

Rust **1.95 이상**과 Git이 필요합니다. Windows는 Visual Studio 2022의 MSVC C++ 빌드 도구, macOS는 Xcode command line tools가 필요합니다. Linux 빌드 의존성은 다음과 같습니다.

Install Rust **1.95 or newer** and Git. Windows needs the Visual Studio 2022 MSVC C++ build tools; macOS needs Xcode command line tools. On Ubuntu:

```sh
sudo apt-get update
sudo apt-get install libasound2-dev libudev-dev libssl-dev pkg-config
git clone https://github.com/playok/playterm.git
cd playterm
cargo build --locked --workspace
cargo run --locked -p playterm
```

실행 가능한 파일은 `target/debug/`, 최적화 빌드는 `cargo build --locked --release -p playterm` 후 `target/release/`에 있습니다. Windows/MSVC와 macOS의 상세 준비는 [한국어 README](README.md) / [English README](README.en.md)를 참고하세요. GUI 시험에는 실제 데스크톱 세션이 필요합니다.

Executables are in `target/debug/`, or `target/release/` after `cargo build --locked --release -p playterm`. See the READMEs for platform setup. Native GUI checks require an interactive desktop session.

## 구성 / Layout

| 경로 / Path | 역할 / Role |
| --- | --- |
| `crates/playterm` | egui 앱·대화상자·업데이트 / GUI, dialogs, updater |
| `crates/playterm-types` | 런타임 없는 공유 설정 타입 / Shared connection settings |
| `crates/playterm-config` | 프로필·설정·암호화 볼트 / Profiles, settings, encrypted vault |
| `crates/playterm-net` | SSH·Telnet·시리얼·RDP·SFTP·포워딩·ZMODEM / Transports and transfers |
| `crates/playterm-term` | 터미널 화면·문자셋 / Terminal model and encoding |
| `crates/playterm-i18n` | 한국어·영어 문구 / Korean and English strings |
| `crates/playterm-sign` | 릴리스 체크섬 서명 / Release checksum signer |
| `scripts/` | 릴리스·상호운용·부하 시험 도구 / Release and test tools |
| `docs/releases/` | 버전별 변경 내역과 다음 릴리스 초안 / Release notes and next draft |

## 검증 / Validation

저장소 루트에서 실행합니다. / Run from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

설정만 바꿨다면 `cargo test --locked -p playterm-config`로 빠르게 확인할 수 있습니다. Linux/macOS의 ZMODEM 상호운용 시험에는 `lrzsz`가 필요합니다(`apt-get install lrzsz` / `brew install lrzsz`). CI는 이를 필수로 검사합니다. `scripts/test-openssh.sh`는 테스트 계정과 데몬을 만드는 **폐기 가능한 Linux CI 러너 전용**입니다.

For configuration work, `cargo test --locked -p playterm-config` is a quick focused check. ZMODEM interoperability tests require `lrzsz` on Linux/macOS; CI requires these tests to run. `scripts/test-openssh.sh` creates an account and daemons and is only for disposable Linux CI runners.

서명 CLI 시험, 짧은 VT 파서 퍼징, 커버리지 재생성 명령은 [품질 도구 안내](docs/quality-tools.md)에 있습니다. / See [quality tools](docs/quality-tools.md) for signing tests, bounded parser fuzzing and coverage commands.

기능 변경은 앱을 실제로 실행해 관련 흐름을 확인하고 OS·실행한 검사·결과를 PR에 적습니다. 자동 GUI 흐름 시험은 `crates/playterm/src/ui_tests.rs`에 있습니다. 시험 설정은 별도 `PLAYTERM_CONFIG_DIR`를 사용하고, 재현 로그·캡처에는 실제 비밀번호·OTP·개인 키를 넣지 않습니다. 미실행 항목과 한계는 결과와 구분해 적습니다.

For feature changes, run the actual app through the affected flow and report the OS, checks and results in the PR. Automated GUI flows live in `ui_tests.rs`. Use an isolated `PLAYTERM_CONFIG_DIR` and disposable credentials; keep real passwords, OTPs and private keys out of evidence. State which checks were not run.

## 변경과 PR / Changes and pull requests

```sh
git switch -c fix/issue-number-description
git add <changed-files>
git commit -m "fix: describe the resulting behavior"
git push -u origin HEAD
```

한 PR은 설명할 수 있는 한 문제에 집중합니다. `fix:`, `feat:`, `docs:`, `ci:` 등 변경을 나타내는 간단한 커밋 제목을 쓰고, PR에는 관련 이슈와 변경 이유·결과·검증을 적습니다. 의존성을 바꾸면 `Cargo.lock`도 함께 갱신합니다. CI 실패는 원인을 고치거나 남는 차단 사유를 설명하며, 보안 검사를 끄거나 근거 없이 예외 처리하지 않습니다.

Keep a PR focused on a coherent problem. Use concise commit subjects such as `fix:`, `feat:`, `docs:` or `ci:`. Explain the issue, resulting behavior and validation. Include `Cargo.lock` for dependency changes. Fix failing checks or document the remaining blocker; do not bypass security checks with unsupported exceptions.

사용자에게 보이는 변경은 [다음 릴리스 초안](docs/releases/next.md)에 기록합니다. 배포된 버전의 변경 내역은 [docs/releases/](docs/releases/)와 [GitHub Releases](https://github.com/playok/playterm/releases)가 기준입니다. 같은 내용을 별도 CHANGELOG에 복제하지 않습니다.

Record user-visible changes in [the next release draft](docs/releases/next.md). Published notes live in [docs/releases/](docs/releases/) and [GitHub Releases](https://github.com/playok/playterm/releases); keep one source instead of duplicating a changelog.
