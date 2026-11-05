# 다음 릴리스 변경 내역

UNRELEASED — v0.5.0 이후 개발 중인 변경이다. 게시 전 후보 태그 범위를 확정하고 검증 결과와 문구를 대조한다.

## Highlights

- SecureCRT 세션을 가져올 수 있습니다. 저장된 암호는 가져오지 않으며 별도로 입력해야 합니다. [#188](https://github.com/playok/playterm/pull/188)
- Windows 다운로드에 기본 미압축 패키지와 용량이 작은 선택형 UPX 패키지를 제공합니다. x64·ARM64에 동일한 정책을 적용합니다. [#193](https://github.com/playok/playterm/issues/193)

## Fixes

- 암호화된 SSH 키의 암호 창에서 취소하거나 Esc를 누르면 전체 연결을 중단합니다. 다른 키나 저장된 비밀번호로 계속하려면 ‘이 키 건너뛰기’를 선택하세요. 재접속과 점프호스트에도 적용합니다. [#189](https://github.com/playok/playterm/issues/189)
- 공개 키 뒤 OTP를 요구하는 서버에서 저장 비밀번호를 OTP로 보내거나 OTP를 계정 비밀번호로 저장하지 않습니다. [#190](https://github.com/playok/playterm/issues/190)
- 여러 앱 창에서 비밀번호를 동시에 저장할 때 볼트 갱신을 직렬화합니다. 다른 창에서 볼트를 초기화하면 이전 창은 다시 잠금을 풀어야 합니다. [#191](https://github.com/playok/playterm/issues/191)
- SFTP 재접속 후 이전 연결의 탐색 상태를 버리고, 다운로드 대상 링크 및 업로드 대상 폴더를 보호합니다. [#182](https://github.com/playok/playterm/pull/182), [#183](https://github.com/playok/playterm/pull/183), [#186](https://github.com/playok/playterm/pull/186)
- RD Gateway를 바꾼 연결에는 이전 게이트웨이로 저장한 비밀번호를 재사용하지 않습니다. 재활성화 응답을 기다리는 RDP 연결도 취소할 수 있습니다. [#184](https://github.com/playok/playterm/pull/184), [#185](https://github.com/playok/playterm/pull/185), [#187](https://github.com/playok/playterm/pull/187)

## Compatibility and update notes

- 자동 업데이트는 기본 미압축 Windows 패키지를 선택합니다. 현재 UPX 압축판을 쓰는 경우에도 다음 자동 업데이트부터 미압축판으로 바뀝니다.
- 볼트 암호화 형식은 유지됩니다. 동시 저장용 `vault.toml.lock` 파일이 추가되므로 실행 중 삭제하지 마세요. 여러 창은 모두 새 버전으로 재시작해야 잠금 규약이 적용됩니다. 볼트 사용 중 오류가 나면 다른 창의 작업이 끝난 뒤 다시 저장하세요.
- 키 암호 취소 동작이 바뀝니다. 암호를 모르는 키를 건너뛰려면 별도 버튼을 사용하세요.
- 업데이트 적용에는 앱 재시작이 필요합니다. 재시작하면 열린 연결이 종료됩니다.

## Known limitations

- 정식 코드 서명·공증 여부는 아래 다운로드 안내의 실제 빌드 결과를 확인하세요. UPX 미사용이나 코드 서명으로 백신 오탐이 완전히 없어지지는 않습니다.
- 실제 장비 호환성, macOS 및 네이티브 Windows x64 실행, 장시간·절전 복귀 검증은 [검증 기록](https://github.com/playok/playterm/blob/master/docs/validation.md)에서 확인할 수 있습니다.
