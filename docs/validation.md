# 이슈 189부터 196까지 검증 기록

2026-11-05, Windows build 26300 ARM64에서 번호 순서로 조사하고 수정했다. 개발 기준은 master `25a9bcb40b55ceda091efdc7cdb291268d4cb415`에 이 작업의 변경을 더한 트리다. 앱 버전 문자열은 아직 0.5.0이며, 이번 개발 패키지는 공식 v0.5.0 배포 파일과 다르다. 검증 실행자는 이 작업의 Codex 로컬 세션이다.

공통 검사: `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace` 통과. 전체 테스트 로그는 로컬 `target/workspace-tests-full.log`에 있다. 첫 샌드박스 실행에서 기존 홈 경로 테스트 2개가 실패했으며, 사용자 환경에서 다시 실행한 전체 테스트는 통과했다. `python scripts/test-release-notes.py`의 태그 범위·초안 차단·패키지 안내 보존 테스트 3개도 통과했다.

## 이슈별 결과

| 이슈 | 구현과 확인한 결과 | 남은 완료 조건 |
|---|---|---|
| [189](https://github.com/playok/playterm/issues/189) | 키 암호 취소를 전체 연결 종료로 전파. 별도 ‘이 키 건너뛰기’만 후속 인증 허용. 잘못된 암호 3회 뒤도 종료. 루프백에서 에이전트·기본 키·저장 비밀번호·점프호스트가 취소 뒤 사용되지 않는지 확인. 실제 창에서 취소·Esc·재시도·키 로그인·건너뛰기 후 비밀번호 로그인·재접속 버튼·`r` 명령을 검증 | 이번 환경은 Windows ARM64. 다른 플랫폼의 GUI 확인은 호환성 표에서 별도 관리 |
| [190](https://github.com/playok/playterm/issues/190) | 공개 키 파일과 Windows·macOS 에이전트의 partial success 보존. 저장 비밀번호를 OTP로 보내지 않음, OTP는 account_password=false. 공개 키→OTP 오답·재시도·취소·새 연결과 기존 비밀번호→OTP 회귀 통과. 실제 Windows 창에서 OTP 재시도·취소·재접속·로그인 확인. 모호한 hidden 질문은 자동 채움/저장 대상에서 제외 | 실제 OpenSSH 키+PAM MFA 조합도 PR 198 CI에서 통과. 고정 테스트 인자를 사용했으며 실제 OTP 제공자는 별도 미검증 |
| [191](https://github.com/playok/playterm/issues/191) | `vault.toml.lock` OS 잠금으로 create/reset/read-modify-write 직렬화. 대기 750 ms 제한, 실패 시 메모리 변경 미반영, reset 뒤 이전 창의 자격 증명 재생성 방지. 실제 별도 Windows·macOS 프로세스로 추가/추가·추가/삭제·초기화·동시 생성·비정상 종료 후 잠금 해제 통과. 실제 Windows UI에서 잠금 오류 표시→잠금 해제 후 볼트 생성 성공 | macOS 별도 프로세스 시험도 아래 CI에서 통과. macOS 네이티브 GUI는 별도 미검증 |
| [192](https://github.com/playok/playterm/issues/192) | 공식 v0.5.0 Windows ZIP 두 개를 다운로드해 체크섬·Authenticode·Ed25519 검사. 최종 ZIP 추출 후 서명 검증 단계를 배포 스크립트에 추가. 서명 필수 모드에서 unsigned ZIP 거부 확인. 수동 배포 빌드에서 Windows 4개 ZIP과 macOS 2개 ZIP 검증 단계 성공 | Windows NotSigned, macOS ad-hoc. 정식 Windows 인증서와 Apple Developer ID/공증 시크릿 없음. 깨끗한 OS 첫 실행 검증 필요. 미완료 |
| [193](https://github.com/playok/playterm/issues/193) | 같은 소스의 x64·ARM64 빌드에서 미압축 기본 ZIP과 `-upx.zip` 네 개 생성·체크섬 검사. 기존 업데이트 이름 유지, UPX 사용자도 다음 업데이트에서 기본 미압축판 선택. 최종 ZIP의 실행 파일을 실제 Windows ARM64에서 실행해 SSH 연결·종료 확인 | 공식 다운로드 제공은 다음 태그 릴리스에서 적용. x64 시험은 ARM64 에뮬레이션이며 네이티브 x64 실행은 별도 필요 |
| [194](https://github.com/playok/playterm/issues/194) | [호환성 표](compatibility.md)에 필수 조합·절차·결과 필드·승인/보류 기준·미지원/미검증 구분 추가. 실제 OpenSSH 키 로그인·셸·한글 경로 SFTP용 하네스와 Ubuntu CI 단계 추가. OpenSSH 9.6p1과의 실제 상호운용 시험도 통과 | 배포 대상 클라이언트의 실서버 조합·네트워크 장비·RDP 대상 및 macOS 배포 앱 GUI 결과 필요. 미완료 |
| [195](https://github.com/playok/playterm/issues/195) | [부하 시험 절차](stress-testing.md), 가짜 출력 서버·자원 샘플러 추가. 실제 1/10/30탭 및 30탭 8시간 시험 통과. 시작 시 좁은 분할 화면 결함 수정. 백엔드 연결·종료 100회와 포워딩 포트 해제 확인. SFTP 응답 정지 중 취소 결함을 수정하고 실제 Windows 화면에서 업로드·다운로드 취소와 기존 파일 보존 확인 | 전체 앱 100회 반복 후 휴지 기준선, UI p95 지연, 실서버 SFTP 장애·RDP·절전 복귀는 미검증. 미완료 |
| [196](https://github.com/playok/playterm/issues/196) | [다음 릴리스 초안](releases/next.md), [릴리스 절차](release-process.md), 커밋 기반 초안 생성기 추가. 게시 설명은 태그에 들어 있는 검토 문서만 사용하며 실제 서명·패키지·체크섬 안내 보존. README 개발/배포 버전 구분. 실제 업데이트 UI에서 한글·링크 정리·스크롤·나중에 닫기 확인 | 다음 버전의 `docs/releases/vX.Y.Z.md`를 검토·태그에 포함하고 실제 게시 결과 확인 필요 |

## PR 197 CI 결과

2026-11-05, 커밋 `b88fd48743a48947a7efcbf8e60e1eed9b20abc9`의 [CI 실행 42040505752](https://github.com/playok/playterm/actions/runs/42040505752)를 확인했다. Windows·macOS·Ubuntu 작업이 모두 성공했고, 리뷰 요청이나 미해결 리뷰 스레드는 없었다.

| 실행 | 확인한 실제 결과 |
|---|---|
| [Windows](https://github.com/playok/playterm/actions/runs/42040505752/job/127289770159) | fmt, 릴리스 설명 Python 검사, clippy, 전체 워크스페이스 테스트 통과 |
| [macOS ARM64](https://github.com/playok/playterm/actions/runs/42040505752/job/127289769444) | clippy·전체 테스트·lrzsz 통과. `agent_on_unix_socket_authenticates` 및 별도 프로세스 볼트 직렬화·프로세스 종료 후 잠금 해제·잠금 제한 시간 시험 통과 |
| [Ubuntu](https://github.com/playok/playterm/actions/runs/42040505752/job/127289770093) | 네트워크·lrzsz 통과. 별도 OpenSSH 단계에서 `openssh_key_shell_and_unicode_sftp`가 ignored 없이 1개 실행되어 통과. 서버는 OpenSSH 9.6p1 Ubuntu-3ubuntu13.20, 공개 키 로그인·셸 출력·한글 경로 3 MB 왕복 확인 |

이 결과는 테스트 실행 증거이며 macOS 배포 앱의 네이티브 GUI·코드 서명·공증, OpenSSH MFA, 실장비 및 장시간 시험 결과로 확대하지 않는다.

## 후속 회귀 검증

[PR 198](https://github.com/playok/playterm/pull/198)의 [CI 실행 42045321718](https://github.com/playok/playterm/actions/runs/42045321718)은 Windows·macOS·Ubuntu 모두 통과했다. Ubuntu의 별도 OpenSSH 9.6p1 데몬에서 `publickey,keyboard-interactive`와 PAM을 조합해 자동 채움 방지·저장 대상 분류·오답 재시도·취소·새 연결을 검증했다. PAM은 고정된 테스트 비밀번호를 사용하며 실제 OTP 제공자 시험과 구분한다. OTP 질문 경로는 기존 루프백·Windows GUI 증거로 확인한다.

8시간 시험을 준비하는 예행 실행에서 Windows 측정 파일 독자가 원자적 교체를 잠시 막으면 가짜 출력 서버가 종료되는 시험 하네스 결함을 발견했다. 교체를 최대 1초 범위에서 재시도하고 스냅샷 갱신 시각을 기록하도록 수정했다. 일시적 충돌 후 성공·영구 충돌 시 기존 파일 보존과 제한 횟수 실패를 Python 회귀 검사로 확인했다. 보완된 하네스의 실제 Windows 30탭·20초 예행은 20개 샘플에서 출력 증가와 30개 연결을 유지했고, 앱은 약 0.25초에 종료하여 서버 연결 0개를 남겼다. 이 예행을 8시간 시험 통과로 기록하지 않는다.

## 2026-11-06 후속 검증

[PR 200](https://github.com/playok/playterm/pull/200)에 기록한 [30탭 8시간 결과](soak-results/20261105T143712Z/README.md)는 28,814초 동안 955개 샘플에서 30개 연결과 응답 상태를 유지했다. private 메모리 증가율은 약 0.075%, 최대 730.84 MiB였으며 종료는 약 0.269초, 종료 뒤 서버 연결은 0개였다. 이 결과는 해당 기록의 소스·실행 파일에 한정하며 아래 SFTP 수정판의 8시간 시험으로 간주하지 않는다.

SFTP 루프백에서 READ/WRITE 및 OPEN 응답을 정지시키면 기존 취소는 원격 요청 완료까지 기다렸다. 취소 신호로 커밋 전 원격 대기를 중단하고, 정지한 업로드의 임시 파일 삭제 대기는 250 ms로 제한했다. 늦게 성공한 OPEN은 별도 소유권 확인으로 핸들을 닫고, 배타적 생성이 확인된 임시 파일만 제거한다. 실패한 OPEN에서 다른 파일을 지우지 않는다. 최종 대상 교체·롤백은 파일 보존을 위해 중단하지 않는다. 서버가 영구 단절되면 원격 임시 파일이 남을 수 있다.

Windows ARM64의 실제 개발 앱에서 8 MiB 전송을 시작하고 루프백 서버의 데이터 응답 정지를 확인한 뒤 취소를 클릭했다. 서버가 계속 정지한 상태에서 [다운로드](screenshots/validation/sftp-download-cancelled.png)와 [업로드](screenshots/validation/sftp-upload-cancelled.png) 모두 `Cancelled`가 표시됐다. 로컬·원격 기존 대상의 sentinel 내용은 그대로였고 서버 재개 후 임시 파일은 남지 않았다. 첫 시도는 수동 조작 사이 30초 요청 제한 시간이 먼저 만료되어 `Failed: Timeout`이었고, 취소 검증은 시작·취소를 연속 조작해 다시 확인했다. 화면의 `/docs` 경로 오류는 가짜 셸이 돌려준 경로가 시험 서버에 없어서 발생한 별도 목록 오류다.

회귀 검사는 업로드·다운로드별 데이터/OPEN 정지 중 1초 이내 취소, 늦은 OPEN 임시 파일 정리, 배타적 OPEN 실패 시 다른 파일 보존, 전송 중 서버 종료 시 기존 파일 보존을 포함한다. 별도로 백엔드 SSH/PTY 연결·종료를 100회 반복하고 각 회차의 로컬 포워딩 포트 해제를 확인했다. 이 검사는 전체 GUI 100회 반복의 메모리 회복 측정을 대신하지 않는다. 로컬 화면·서버 제어 산출물은 `target/gui-validation/sftp-stall/`에 있다.

수정판의 로컬 `cargo clippy --locked --workspace --all-targets -- -D warnings`와 `cargo test --locked --workspace`는 모두 통과했다. SSH 루프백 34개가 통과했고 수동 서버 1개만 ignored였다. 전체 로그는 `target/workspace-tests-sftp.log`에 있다.

[수동 배포 빌드 42044743525](https://github.com/playok/playterm/actions/runs/42044743525)는 소스 `8a139475`에서 Windows·macOS 빌드 작업이 성공했고 게시 단계는 건너뛰었다. [Windows 작업](https://github.com/playok/playterm/actions/runs/42044743525/job/127304543097)은 최종 기본/UPX × ARM64/x64 ZIP 4개 모두 `NotSigned`임을 기록했고 합산 SHA-256 목록의 Ed25519 검증은 `signature OK`였다. [macOS 작업](https://github.com/playok/playterm/actions/runs/42044743525/job/127304542346)은 ARM64/x64 최종 ZIP에서 추출한 앱 모두 `valid on disk`, designated requirement 충족을 확인했지만 `Signature=adhoc`였다. 이는 Developer ID 서명·공증 또는 깨끗한 OS 실행 성공을 의미하지 않는다. 공식 릴리스는 게시하지 않았다.

## 이슈 202: 두 공개 키 연속 인증 회귀

2026-11-06, Windows ARM64의 master `82b0d1b9a63291d2d7b1c92ee04e1a6bea40fc58`에 새 루프백 통합 검사 `two_public_keys_continue_across_files_and_skip_duplicates`를 먼저 추가했다. 수정 전에는 첫 키의 부분 성공 뒤 `expected Connected, got Closed(Some("인증 실패: 남은 인증 방식(publickey)을 사용할 수 없습니다"))`로 실패했다. 이후 같은 검사에서 명시적 키→기본 파일, 기본 파일→기본 파일, 앞/중간 잘못된 키와 중복 파일을 통과했다. 이는 실제 PlayTerm 네트워크 인증 함수의 수정 전 실패/수정 후 통과 증거이며 OpenSSH 대조 실험과 구분한다.

인증 진행 상태는 누적 보존하고 다음 공개 키 탐색은 최신 서버 허용 방식으로 판단한다. 연결별 SHA-256 공개 키 지문으로 파일·에이전트 전체에서 이미 시도한 키를 제외한다. 에이전트가 서명을 거부했다면 같은 개인 키의 파일이나 다른 에이전트를 계속 사용할 수 있다. 새 연결과 점프호스트는 각각 독립된 탐색 상태를 갖는다. 키 암호 취소/건너뛰기와 후속 인증의 저장 비밀번호 보호는 그대로 검증한다.

로컬 통합 검사에서는 Windows named pipe 에이전트 내 A+B, 명시적 A→에이전트 A/B, 에이전트 A→기본 파일 A/B, 명시적 A→B 서명 거부 에이전트→파일 B, A만 있는 경우의 실패, 재연결, 점프호스트, A+B→OTP 오답·취소·새 연결을 확인했다. 같은 A를 여러 소스에 넣어도 최종 성공으로 오인하지 않는다. 상태 단위 검사는 부분 성공 뒤 일반 거부에도 진행 상태를 보존하고, publickey가 남으면 계속 시도하며 keyboard-interactive만 남으면 멈추는지 확인한다.

두 번째 키의 암호 창에서 취소하면 A 이후 어떤 자격 증명도 제출하지 않고 종료하며, 명시적으로 건너뛰면 다음 파일의 B로 연결되는 검사도 통과했다. 로컬 전체 워크스페이스 테스트와 warnings-denied clippy가 통과했고, 전체 로그는 `target/workspace-tests-202-final.log`에 있다. 이 로그 이후 추가한 두 번째 키 취소/건너뛰기 검사도 개별 실행에서 통과했다.

실제 Windows 개발 앱에서도 [명시적 A→에이전트 B 뒤 OTP 창](screenshots/validation/ssh-two-keys-otp.png), [OTP 오답 후 로그인](screenshots/validation/ssh-two-keys-connected.png), [에이전트 단독 A+B 뒤 로그인](screenshots/validation/ssh-two-agent-keys-connected.png)을 확인했다. `r` 재접속과 OTP 취소 뒤 연결 종료도 확인했다. 각 연결에서 서버는 서로 다른 공개 키 지문 두 개만 받았으며 중복 A는 다시 제출되지 않았다. 로컬 화면과 서버 요청 기록은 `target/gui-validation/202-two-keys/`에 있다. 이 GUI 서버는 russh 루프백이며 실제 OpenSSH로 표시하지 않는다.

`scripts/test-openssh.sh`는 기존 공개 키·셸·SFTP·키+PAM 검사에 실제 OpenSSH의 `publickey,publickey` 및 `publickey,publickey,keyboard-interactive` 데몬을 추가한다. 독립 OpenSSH 클라이언트의 A+B 성공/A 단독 실패 대조 후 PlayTerm에서 파일·동일 에이전트·중복·잘못된 키·A 단독 실패와 두 키 뒤 PAM 오답/취소/새 연결을 실행한다. PAM은 고정 테스트 인자로 실제 OTP 제공자가 아니다. OpenSSH 버전과 결과는 이 변경 PR의 Ubuntu CI 로그에 기록된다. Unix에서는 별도 프로세스의 `SSH_AUTH_SOCK`으로 두 에이전트 간 fallback을 검사해 테스트 프로세스의 환경을 바꾸지 않는다. Windows Pageant fallback 중단 조건도 수정했지만 실제 Pageant 프로세스 간 fallback은 실행하지 않았으며, Windows named pipe와 Unix socket 실행 결과와 구분한다.

[PR 203 첫 CI](https://github.com/playok/playterm/actions/runs/42130857959)의 Ubuntu에서 OpenSSH 9.6p1 Ubuntu-3ubuntu13.20의 위 세 통합 검사가 실제 실행되어 모두 통과했다. 첫 작업 자체는 마지막 root 소유 서버 로그를 일반 사용자로 출력하다 권한 오류가 나 실패했으므로, 로그 출력에 `sudo cat`을 사용하도록 보완했다. 같은 실행의 macOS 전체 검사는 성공했고 `two_public_keys_continue_between_unix_agents` 등 새 Unix 에이전트 회귀 검사도 통과했다. 최종 머지 판단은 보완 커밋의 모든 CI 작업 성공을 기준으로 한다.

## 공식 v0.5.0 검사

공식 태그 커밋은 `3943e8073a091a0d6aad5a79b5f83541e3f41ba7`이다. 개발 트리의 새 기능을 이 버전의 출시 기능으로 표시하지 않는다.

| 공식 Windows 파일 | SHA-256 | Authenticode |
|---|---|---|
| playterm-0.5.0-windows-arm64.zip | `0b7c00a701dcf95bbbd59d1a0ec599058270f25b7c0c12b4c681840cb8cd1e80` | NotSigned, 게시자·타임스탬프 없음 |
| playterm-0.5.0-windows-x64.zip | `af6b442a32bee216237a528e0775f3702c83803172399a7ac1a20c7aaa6eb09c` | NotSigned, 게시자·타임스탬프 없음 |

두 해시 모두 공식 체크섬과 일치했고, `playterm-sign verify`로 공식 `SHA256SUMS.txt.sig`를 앱 공개 키와 확인하여 `signature OK`를 얻었다. 저장소 시크릿 이름만 조회한 결과 `PLAYTERM_SIGNING_KEY`만 등록되어 있었으며 Windows 코드 서명 및 Apple 인증서/공증용 이름은 없었다. 시크릿 값은 조회하지 않았다. 원본·검사 JSON은 로컬 `target/release-validation/v0.5.0/`에 있다. 인증서 준비 일정은 아직 미확정이며 다음 서명 릴리스 후보 전에 담당자가 확정해야 한다.

## 실제 화면 증거

대표 캡처는 저장소에서도 확인할 수 있다: [SSH 취소](screenshots/validation/ssh-cancel.png), [공개 키 뒤 OTP 로그인](screenshots/validation/ssh-otp.png), [볼트 잠금 오류](screenshots/validation/vault-busy.png), [30탭 출력](screenshots/validation/tabs-30.png), [업데이트 변경 내역 스크롤](screenshots/validation/release-notes.png).

아래 파일은 검증 머신의 `target/`에 있는 로컬 산출물이며 GitHub에 게시된 증거가 아니다. 테스트 암호·키·서버는 모두 가짜 loopback 전용이다.

| 경로 | 내용 |
|---|---|
| `target/gui-validation/189/` | 키 암호 창, 취소, 재접속, 건너뛰기, 비밀번호·키 로그인 |
| `target/gui-validation/auth-final/r-reconnect.png`, `r-cancelled.png`, `auth-calls.txt` | 최종 릴리스 빌드의 실제 `r` 명령 재접속과 Esc 취소. 패키지 시험 전에 인증 요청 기록이 비어 있음을 확인 |
| `target/gui-validation/190/` | 공개 키 뒤 OTP 창·오답·취소·재접속 후 정상 로그인 |
| `target/gui-validation/191/` | 다른 프로세스 잠금에 따른 오류, 잠금 해제 후 볼트 생성 |
| `target/gui-validation/193-final/` | 최종 ZIP 네 개의 실제 SSH 연결 화면. x64는 Windows ARM64 에뮬레이션 |
| `target/gui-validation/195-tabs/` | 수정 후 1/10/30탭별 창 캡처, CSV, 서버 연결·전송량·입력 수와 종료 결과 |
| `target/gui-validation/196/02-notes-final.png`, `03-notes-scrolled.png` | 실제 업데이트 대화상자의 한글 변경 내역과 스크롤. 테스트 전용 99.0.0 미리보기이며 실제 업데이트를 설치하지 않음 |

## 짧은 부하 시험 측정

Windows ARM64, ASUS Vivobook S 15 S5507QA, RAM 약 32 GB, 릴리스 빌드, 기본 렌더러와 스크롤백으로 실행했다. 각 조건에서 출력과 실제 키 입력 후 20초 동안 1초 간격으로 수집했다. 다른 빌드 작업이 실행된 개발 머신이므로 전용 장비의 장시간 기준선으로 쓰지 않는다. CPU는 모든 논리 프로세서 용량 대비 앱 사용률이다.

| 탭 수 | 최대 private MiB | 최대 작업 집합 MiB | 최대 CPU % | 최대 스레드 | 최대 핸들 | 앱 종료 초 | 종료 후 서버 연결 |
|---|---:|---:|---:|---:|---:|---:|---:|
| 1 | 110.9 | 139.4 | 0.39 | 12 | 419 | 0.378 | 0 |
| 10 | 300.9 | 309.3 | 0.77 | 12 | 426 | 0.420 | 0 |
| 30 | 727.3 | 690.8 | 1.41 | 12 | 449 | 0.498 | 0 |

서버는 각 단계의 1/10/30 동시 연결을 확인했고 각 단계에서 실제 키 입력 13건을 받았다. 60개 자원 샘플의 Responding 값은 모두 true였다. 이는 입력 지연 p95나 8시간 누수 시험의 대체 결과가 아니다. 예열·휴지 뒤 메모리 회복과 출력 버퍼 상한은 장시간 시험에서 별도로 확인한다. 수정 전 분할 화면 측정은 비교 기록 `target/gui-validation/195-final/`로 남기고 위 탭 기준선에 섞지 않았다.
