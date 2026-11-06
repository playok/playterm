# Security policy / 보안 정책

## 지원 버전

보안 수정은 [최신 안정 릴리스](https://github.com/playok/playterm/releases/latest)와 기본 브랜치를 우선 대상으로 합니다. 이전 릴리스로의 보안 패치 역적용은 보장하지 않습니다. 보고할 때 앱 버전 또는 커밋, OS·아키텍처, 사용한 프로토콜을 알려 주세요.

## 비공개 취약점 보고

[GitHub의 비공개 취약점 보고](https://github.com/playok/playterm/security/advisories/new)를 사용해 주세요. 이 저장소에서 private vulnerability reporting을 활성화했습니다. 보고하려면 GitHub 계정 로그인이 필요합니다. 공개 이슈·PR·토론에는 아직 공개되지 않은 취약점의 재현 절차, 공격 코드, 민감한 로그를 올리지 말아 주세요.

영향과 발생 조건, 최소 재현 절차, 기대 동작과 실제 동작을 가짜 데이터로 작성해 주세요. 비밀번호, 마스터 비밀번호, 개인 키, OTP, 세션 파일, 고객사 주소·계정은 보내지 마세요. 일반 버그와 기능 요청은 공개 이슈를 사용하되 같은 민감 정보 제외 원칙을 지켜 주세요.

유지보수자는 가능한 범위에서 확인·분석하고 비공개 보고 안에서 진행 상황을 공유합니다. 정해진 응답 기한이나 수정 일정을 보장하지 않습니다. 14일 이상 답변이 없으면 같은 비공개 보고에서 후속 메시지를 남겨 주세요. 공개 시점은 유지보수자와 조율해 주세요.

정식 Windows 코드 서명·macOS Developer ID 서명·공증 준비와 미서명 배포 안내는 [#192](https://github.com/playok/playterm/issues/192)에서 추적합니다. 이 문서가 서명·공증 완료를 의미하지 않습니다. 배포 파일의 실제 상태는 해당 릴리스 설명에서 확인해 주세요.

## Supported versions

Security fixes prioritize the [latest stable release](https://github.com/playok/playterm/releases/latest) and the default branch. Backports to older releases are not guaranteed. Include the application version or commit, OS, architecture, and protocol in your report.

## Reporting a vulnerability privately

Use [GitHub private vulnerability reporting](https://github.com/playok/playterm/security/advisories/new). Private vulnerability reporting is enabled for this repository, and a GitHub account is required. Keep unpublished reproduction steps, exploit code, and sensitive logs out of public issues, pull requests, and discussions.

Describe the impact, conditions, minimal reproduction, expected result, and observed result using synthetic data. Do not send passwords, vault passwords, private keys, one-time codes, session files, or customer addresses and accounts. Use public issues for ordinary bugs and feature requests with the same care to remove sensitive information.

Maintainers will triage reports as availability permits and communicate through the private report. No response deadline or fix schedule is guaranteed. If there is no response after 14 days, follow up in the same private report. Coordinate disclosure timing with the maintainers.

Windows code signing and macOS Developer ID signing/notarization remain tracked in [#192](https://github.com/playok/playterm/issues/192). This policy does not imply they are complete. Check each release's notes for its actual signing status.
