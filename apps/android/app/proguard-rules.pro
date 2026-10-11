# Kotlin serialization generates serializers; no reflective model adapters are used.

# JNI reaches WebRTC classes and callbacks by their original names.
-keep class org.webrtc.** { *; }

# WebRTC's native library loads org.jni_zero.JniZero by name; the AAR ships no consumer
# rules, so R8 would otherwise strip it and abort on PeerConnection setup. JniZeroJni is
# generated per app and only reached from the optional setJniClassLoader helper.
-keep class org.jni_zero.** { *; }
-dontwarn org.jni_zero.JniZeroJni
