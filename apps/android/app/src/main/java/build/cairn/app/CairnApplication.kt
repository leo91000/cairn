package build.cairn.app

import android.app.Application
import build.cairn.app.data.initializeNativePush

class CairnApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        initializeNativePush(this)
    }
}
