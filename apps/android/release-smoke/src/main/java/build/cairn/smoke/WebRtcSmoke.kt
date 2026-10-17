package build.cairn.smoke

import android.app.Activity
import android.app.Instrumentation
import android.content.Context
import android.content.pm.ApplicationInfo
import android.os.Bundle

/** No WebRTC dependency: every reflected class comes from the installed, minified target APK. */
class WebRtcSmoke : Instrumentation() {
    override fun onCreate(arguments: Bundle?) {
        super.onCreate(arguments)
        start()
    }

    override fun onStart() {
        val result = Bundle()
        try {
            check(targetContext.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE == 0) {
                "Release smoke requires a non-debuggable target APK"
            }

            val loader = targetContext.classLoader
            val factoryClass = loader.loadClass("org.webrtc.PeerConnectionFactory")
            val optionsClass =
                loader.loadClass("org.webrtc.PeerConnectionFactory\$InitializationOptions")
            val optionsBuilder =
                optionsClass.getMethod("builder", Context::class.java).invoke(null, targetContext)
            val options =
                optionsBuilder.javaClass
                    .getMethod("createInitializationOptions")
                    .invoke(optionsBuilder)

            // This crosses JNI and used to abort when R8 removed org.jni_zero.JniZero.
            factoryClass.getMethod("initialize", optionsClass).invoke(null, options)
            val builder = factoryClass.getMethod("builder").invoke(null)
            val factory = builder.javaClass.getMethod("createPeerConnectionFactory").invoke(builder)
            check(factory != null) { "WebRTC returned no native factory" }
            factoryClass.getMethod("dispose").invoke(factory)

            result.putString("cairnReleaseSmoke", "webrtc-initialize-factory-dispose-v1")
            finish(Activity.RESULT_OK, result)
        } catch (failure: Throwable) {
            result.putString("stream", failure.stackTraceToString())
            finish(Activity.RESULT_CANCELED, result)
        }
    }
}
