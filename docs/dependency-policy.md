# 의존성 검사와 예외

PR과 master push의 `Dependency policy` 워크플로는 cargo-deny 0.20.2로 모든 워크스페이스 기능과 대상의 라이선스 및 RustSec 권고를 검사한다. 루트와 독립된 `fuzz/`의 두 lockfile을 같은 정책으로 각각 검사하며, 루트 검사가 실패해도 fuzz 검사를 실행한다.

```sh
python3 scripts/check-license-exceptions.py
python3 scripts/test-license-exceptions.py
cargo deny --all-features check --warn unmaintained licenses advisories
cargo deny --manifest-path fuzz/Cargo.toml --config deny.toml --all-features check --warn unmaintained licenses advisories
```

`deny.toml`의 허용 목록에 없는 라이선스, 알려진 취약점·unsound 권고, 삭제된 버전은 실패한다. 유지보수 종료 공지는 취약점과 구분하여 경고로 표시하며 숨기지 않는다. 취약점 ignore는 현재 비어 있다. 향후 예외가 필요해도 포괄적 allow로 검사를 우회하지 말고 영향 범위와 기한을 검토해야 한다.

## 라이선스

MPL-2.0은 전역 허용하지 않는다. `serialport`와 `option-ext`만 crate별 예외이며 [정책 파일](../policy/license-exceptions.toml)에 사용 이유와 원본 소스를 기록한다. 예외 추가·변경 시 cargo-deny 목록과 정책 파일이 정확히 일치해야 한다. 이유가 없거나 임시 문구이면 CI가 실패한다.

라이브러리의 원본 라이선스와 소스 링크는 앱의 도움말 → 오픈소스 라이선스에 빌드별로 포함한다. 배포자는 MPL 구성 요소의 원본 소스와 고지를 계속 제공하고, 해당 파일을 수정하면 수정한 소스도 MPL 조건에 맞게 제공해야 한다. [Mozilla의 배포 안내](https://www.mozilla.org/en-US/MPL/2.0/FAQ/#q8-i-want-to-distribute-outside-my-organization-executable-programs-or-libraries-that-i-have-compiled-from-someone-elses-unchanged-mpl-licensed-source-code-either-standalone-or-part-of-a-larger-work-what-do-i-have-to-do)를 참고한다. 아이콘 폰트의 Apache-2.0 고지는 `assets/icons/LICENSE.txt` 및 앱의 라이선스 목록에 유지한다. 기본 egui 글꼴의 OFL-1.1·Ubuntu-font-1.0도 허용 목록에 명시한다.

퍼징 도구 `libfuzzer-sys`가 포함한 LLVM libFuzzer의 NCSA 라이선스도 허용한다. MIT/Apache-2.0과 함께 요구되는 허용적 라이선스이며 해당 소스의 저작권·허가·면책 고지를 유지한다. fuzz 실행 파일은 릴리스 패키지에 포함하지 않는다. 자체 fuzz 크레이트에는 저장소와 같은 MIT를 명시한다.

## 2026-11-06 최초 검사

삭제된 `async-executor 1.14.0`은 1.14.1로 갱신했다. 라이선스 검사는 통과하지만 [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html)이 `rsa 0.10.0-rc.18`에서 검출된다. RustSec는 이 버전을 포함해 패치 버전이 없다고 명시한다. 의존 경로에는 russh/ssh-key와 RDP의 picky/sspi/winscard가 있다. 라이브러리가 들어 있다는 사실만으로 PlayTerm의 원격 악용 가능성을 재현한 것으로 기록하지 않는다. 동시에 클라이언트이므로 무조건 영향이 없다고 판단하지도 않는다.

현재 예외는 추가하지 않았으므로 advisory 검사는 이 알려진 문제로 실패한다. RSA 지원 제거, 검증된 대체 구현, 또는 범위·기한이 있는 명시적 위험 수용을 결정하기 전에는 검사 실패를 숨기지 않는다. `pin-utils`, `ttf-parser`, 일부 zbus 의존성 등의 유지보수 종료 공지는 별도 경고로 남는다.

## 최신 변경을 합친 재검사

2026-11-06, #208–#218 및 #223의 SSH 임시 완화를 합친 #221 브랜치에서 다시 실행했다. RustSec DB는 `26fd2d43f7280a1fb925a3743ec463017afe7119`이며 루트 lockfile blob은 `1b91c78e6ea8bb0720f6194f8a766ca23dee30f9`이다.

| 검사 | 결과 |
| --- | --- |
| MPL 예외 사유 일치 검사·정책 음성 테스트 4개 | 통과 |
| 루트 라이선스 | 통과 |
| 루트 advisory | 실패(exit 1), `rsa 0.10.0-rc.18` / RUSTSEC-2023-0071 |
| 독립 fuzz 라이선스·advisory | 모두 통과(exit 0) |

`async-executor 1.14.1`로 yanked 검출은 해소됐다. [로컬 RSA 파일 서명 차단](security-rsa-223.md)은 의존성 포함을 없애지 않으므로 RSA 검출은 그대로 남는다. #207·#223 및 Draft PR #221을 계속 열어 두며, 취약점 ignore나 위험 수용을 추가하지 않았다.

## 업데이트

Dependabot은 루트와 `/fuzz`의 Cargo 및 GitHub Actions 업데이트를 매주 월요일 09:00 KST에 확인한다. 호환 가능한 Cargo minor/patch 업데이트는 묶고 major 업데이트는 별도로 검토한다. Actions는 40자리 커밋 SHA로 고정하며 Dependabot이 고정 SHA의 갱신 PR을 관리한다. SHA를 갱신할 때 해당 저장소의 태그가 가리키는 실제 커밋을 확인한다. 이 검사는 정식 코드 서명·공증이나 실장비 호환성 검사를 대체하지 않는다.
