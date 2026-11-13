package build.cairn.app.ui

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ChatJourneyDeviceTest : ChatJourneyCases() {
    @Test fun conversationSharingUsesChosenExpirationAndRevokesLink() = exercise(true)
}
