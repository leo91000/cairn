package dev.leo.manager

import android.app.Application
import dev.leo.manager.data.initializeNativePush

class LeoApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        initializeNativePush(this)
    }
}
