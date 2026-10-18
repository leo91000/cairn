package build.cairn.app.ui

import android.Manifest
import android.app.Application
import android.content.ContentProvider
import android.content.ContentValues
import android.database.MatrixCursor
import android.net.Uri
import android.provider.MediaStore
import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivityResultRegistryOwner
import androidx.activity.result.ActivityResultRegistry
import androidx.activity.result.ActivityResultRegistryOwner
import androidx.activity.result.contract.ActivityResultContract
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.core.app.ActivityOptionsCompat
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowContentResolver

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
class AttachSheetTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    class Photos : ContentProvider() {
        @Volatile var id: Long? = null

        override fun onCreate() = true

        override fun getType(uri: Uri) = "image/png"

        override fun query(
            uri: Uri,
            projection: Array<out String>?,
            selection: String?,
            selectionArgs: Array<out String>?,
            sortOrder: String?,
        ) =
            MatrixCursor(arrayOf(MediaStore.Images.Media._ID)).apply {
                id?.let { addRow(arrayOf(it)) }
            }

        override fun insert(uri: Uri, values: ContentValues?): Uri? = null

        override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?) = 0

        override fun update(
            uri: Uri,
            values: ContentValues?,
            selection: String?,
            selectionArgs: Array<out String>?,
        ) = 0
    }

    @Test
    fun limitedPhotoReselectionRefreshesTheListAndRemovesRevokedSelections() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val photos = Robolectric.buildContentProvider(Photos::class.java).create().get()
        ShadowContentResolver.registerProviderInternal("media", photos)
        shadowOf(app)
            .denyPermissions(
                Manifest.permission.READ_MEDIA_IMAGES,
                Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED,
            )
        val registry =
            object : ActivityResultRegistry() {
                var request: Int? = null
                var launches = 0

                override fun <I, O> onLaunch(
                    requestCode: Int,
                    contract: ActivityResultContract<I, O>,
                    input: I,
                    options: ActivityOptionsCompat?,
                ) {
                    request = requestCode
                    launches++
                }
            }
        val owner =
            object : ActivityResultRegistryOwner {
                override val activityResultRegistry = registry
            }
        val attached = mutableListOf<Uri>()
        compose.setContent {
            CompositionLocalProvider(LocalActivityResultRegistryOwner provides owner) {
                CairnTheme { AttachSheet(8, attach = { uris, _ -> attached += uris }, close = {}) }
            }
        }
        fun grantPhoto(id: Long) {
            photos.id = id
            shadowOf(app).grantPermissions(Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED)
            compose.runOnIdle {
                registry.dispatchResult(
                    checkNotNull(registry.request),
                    mapOf(
                        Manifest.permission.READ_MEDIA_IMAGES to false,
                        Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED to true,
                    ),
                )
            }
        }
        compose.onNodeWithText("Photos récentes").performClick()
        grantPhoto(1)
        compose.waitUntil(10000) {
            compose.onAllNodesWithContentDescription("Photo récente").fetchSemanticsNodes().size ==
                1
        }
        compose.onNodeWithContentDescription("Photo récente").performClick()
        compose.onNodeWithText("Joindre 1 photo").assertExists()

        compose.onNodeWithText("Choisir d’autres photos").performClick()
        grantPhoto(2)
        compose.waitUntil(10000) {
            compose.onAllNodesWithText("Joindre 1 photo").fetchSemanticsNodes().isEmpty()
        }
        assertEquals(2, registry.launches)
        compose.onNodeWithContentDescription("Photo récente").performClick()
        compose.onNodeWithText("Joindre 1 photo").performClick()
        assertEquals(
            listOf(Uri.withAppendedPath(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, "2")),
            attached,
        )
    }
}
