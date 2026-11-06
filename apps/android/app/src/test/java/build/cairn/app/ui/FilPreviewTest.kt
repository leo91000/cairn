package build.cairn.app.ui

import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w640dp-h360dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class FilPreviewTest : FilPreviewCases()
