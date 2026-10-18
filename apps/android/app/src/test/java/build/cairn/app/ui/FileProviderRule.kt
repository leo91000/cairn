package build.cairn.app.ui

import android.app.Application
import android.content.pm.PackageManager
import androidx.core.content.FileProvider
import androidx.test.core.app.ApplicationProvider
import org.junit.rules.ExternalResource

/** Give each Robolectric application its own provider roots, as on a fresh device process. */
internal class FileProviderRule : ExternalResource() {
    override fun before() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val info =
            checkNotNull(
                app.packageManager.resolveContentProvider(
                    "${app.packageName}.files",
                    PackageManager.GET_META_DATA,
                )
            )
        // attachInfo clears FileProvider's process-wide path cache for this authority.
        FileProvider().attachInfo(app, info)
    }
}
