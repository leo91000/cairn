# Deliverable fixtures

Synthetic local fixtures: a two-second solid-color H.264 MP4, a one-second 440 Hz WAV, a 64-pixel PNG, a looping GIF alternating red and blue every 250 ms, and a one-page PDF with review text. These contain no user data. Browser tests use the checked-in files so CI does not need a media encoder. Rust preview tests verify thumbnail generation when FFmpeg is installed and the download-preserving fallback otherwise.

The GIF is also copied into Android instrumentation assets and JVM test resources to verify real WebView animation.
