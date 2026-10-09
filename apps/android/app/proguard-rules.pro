# Kotlin serialization generates serializers; no reflective model adapters are used.

# JNI reaches WebRTC classes and callbacks by their original names.
-keep class org.webrtc.** { *; }
