package build.cairn.app.data

import android.content.Intent
import android.net.Uri
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class NotificationRoutingTest {
    private val id = "bafde3d2-6ef8-4327-8126-e969faea8098"

    @Test
    fun ruleBudgetIntentOpensTheRule() {
        val intent =
            Intent("build.cairn.app.OPEN_INSTALLATION")
                .setData(Uri.parse("cairn://installation?rule=$id"))
        val target = notificationTarget(intent)!!
        assertEquals(RULE_NOTIFICATION_TARGET_PREFIX + id, target)
        assertEquals("rules?rule=$id", notificationRoute(target))
    }

    @Test
    fun invalidOrMissingRuleOpensTheInstallationInbox() {
        for (uri in
            listOf("cairn://installation", "cairn://installation?rule=invalid", "mailto:opaque")) {
            val target =
                notificationTarget(
                    Intent("build.cairn.app.OPEN_INSTALLATION").setData(Uri.parse(uri))
                )!!
            assertEquals(INSTALLATION_NOTIFICATION_TARGET, target)
            assertEquals("fil", notificationRoute(target))
        }
    }

    @Test
    fun conversationAndUnrelatedIntentsKeepTheirTargets() {
        val target = notificationTarget(Intent("build.cairn.app.OPEN_CHAT").putExtra("chat", id))!!
        assertEquals("chat/$id", notificationRoute(target))
        assertEquals(
            "",
            notificationTarget(Intent("build.cairn.app.OPEN_CHAT").putExtra("chat", "invalid")),
        )
        assertNull(notificationTarget(Intent(Intent.ACTION_SEND)))
    }
}
