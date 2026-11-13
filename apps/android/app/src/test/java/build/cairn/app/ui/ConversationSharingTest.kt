package build.cairn.app.ui

import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
// Robolectric loops on dialog text fields with width qualifiers above 320 dp:
// https://github.com/robolectric/robolectric/issues/8460
// The native ChatJourneyDeviceTest covers this same sharing flow at the device width.
@Config(sdk = [36], qualifiers = "w320dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ConversationSharingTest : ChatJourneyCases() {
    @Test fun conversationSharingUsesChosenExpirationAndRevokesLink() = exercise(true)
}
