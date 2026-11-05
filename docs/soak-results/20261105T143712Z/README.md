# Eight hour thirty tab output soak

Run 20261105T143712Z on Windows ARM64. Tested source: c6fd4b462f01aa64f83afae0fa9d2c2163c90c23. Executable SHA-256: a94aba7137c0a507ceea8720d8376d625ae0555b44a832fd50d105c626241ec6.
All samples kept 30 loopback connections and a responsive native app. The app exited within five seconds and left zero server connections. Resource limits: post-warmup private memory growth at most 20%, threads +5, handles +20; gaps over 90 seconds invalidate the run. Raw samples and native window captures accompany this report.

This covers sustained synthetic terminal output. It does not verify UI input latency, SFTP failure injection, RDP, network transitions, or sleep/resume, and does not complete issue #195.
