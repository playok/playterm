# 서명 시험·퍼징·커버리지 (#217)

## 릴리스 서명 CLI

```sh
cargo test --locked -p playterm-sign
```

시험은 실제 `playterm-sign` 실행 파일을 호출해 폐기 가능한 키를 생성하고 서명한다. 정상 검증, 다른 공개 키, 손상되었지만 유효한 hex 서명, 변경된 데이터, 잘못된 hex를 구분한다. 서브프로세스에만 시험 키 경로를 지정하며 사용자의 서명 키 환경 변수는 제거한다. 실제 릴리스 개인 키는 필요 없다.

## 짧은 파서 퍼징

`fuzz/`는 별도 워크스페이스와 lockfile을 사용한다. `vt_input`은 UTF-8/CP949 디코딩, OSC 가드, VT 입력을 임의 경계로 나누어 처리한다. 고정 80×24 화면과 제한된 스크롤백을 사용하며 GUI·네트워크·로그 파일을 열지 않는다. 입력으로 생긴 터미널 이벤트를 실행하지 않는다.

Linux 또는 지원되는 macOS에서 저장소 루트 기준:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked --version 0.13.2
mkdir -p fuzz/corpus/vt_input
cp fuzz/seeds/vt_input/* fuzz/corpus/vt_input/
cargo +nightly fuzz run vt_input -- -max_total_time=10 -max_len=65536 -timeout=5 -rss_limit_mb=1024
```

[cargo-fuzz 설치 요구사항](https://rust-fuzz.github.io/book/cargo-fuzz/setup.html)에 맞는 C++ 컴파일러와 sanitizer가 필요하다. Windows는 [별도 MSVC/AddressSanitizer 준비](https://rust-fuzz.github.io/book/cargo-fuzz/windows/setup.html)를 따른다. 현재 Windows ARM64 개발 머신의 네이티브 퍼징 실행을 검증한 것은 아니며, 위 명령은 Ubuntu의 `Quality tools` CI에서 직접 실행한다. CI의 퍼징 시간은 10초로 제한하며 장시간 퍼징을 필수 검사로 두지 않는다.

ANSI, OSC/클립보드, 잘린 시퀀스, 한글, 바이너리 입력을 seed로 제공한다. 새 코퍼스와 충돌 입력은 각각 무시되는 `fuzz/corpus/`, `fuzz/artifacts/`에 생긴다. 실패하면 해당 입력으로 `cargo +nightly fuzz run vt_input fuzz/artifacts/vt_input/<input>`을 실행해 재현하고, 원인을 고친 뒤 필요한 회귀 사례만 커밋한다. 10초 통과는 장시간 안정성이나 무결점의 증명이 아니다.

## 커버리지 재생성

[cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov#usage)와 현재 Rust 툴체인의 LLVM 도구를 설치한다.

```sh
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --locked --version 0.9.1
# 빠른 도구 검증 / quick signer report
cargo llvm-cov --locked -p playterm-sign --html --output-dir target/coverage/sign
# 전체 워크스페이스 / entire workspace
cargo llvm-cov --locked --workspace --html --output-dir target/coverage/workspace
# HTML 대신 LCOV가 필요하면 / LCOV alternative
cargo llvm-cov --locked --workspace --lcov --output-path target/coverage/workspace.lcov
```

HTML은 지정한 출력 폴더의 `html/index.html`에 있다. 생성 결과는 `target/` 안에 두며 커밋하지 않는다. CI에 커버리지 수치 임계값을 강제하지 않는다. `--ignore-run-fail`로 실패를 숨기지 않으며, 실패한 검사는 원인을 확인한다.

기본 `cargo test`에서 무시되는 실제 서버·네이티브 창 시험과 해당 OS에서 컴파일되지 않는 분기는 이 결과에 포함되지 않는다. 장비별 상호운용성과 실제 RDP 세션은 #194, 장시간 soak는 #195에서 따로 추적한다.
