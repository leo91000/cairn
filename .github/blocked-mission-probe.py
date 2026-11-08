from pathlib import Path
p = Path("apps/android/app/src/test/java/build/cairn/app/ui/BlockedMissionJourneyTest.kt")
s = p.read_text()
s = s.replace("@RunWith(RobolectricTestRunner::class)", "@RunWith(org.robolectric.ParameterizedRobolectricTestRunner::class)")
s = s.replace("class BlockedMissionJourneyTest {", """class BlockedMissionJourneyTest(private val observation: Int) {
    companion object {
        @JvmStatic
        @org.robolectric.ParameterizedRobolectricTestRunner.Parameters(name = "observation={0}")
        fun observations(): List<Array<Int>> = (1..30).map { arrayOf(it) }
    }
""")
s = s.replace("private fun waitText(value: String, substring: Boolean = false) =\n        compose.waitUntil(15000) {", """private fun waitText(value: String, substring: Boolean = false) {
        println("[DEBUG-archive] observation=$observation waitText=$value clock=${compose.mainClock.currentTime}")
        try {
        compose.waitUntil(15000) {""")
s = s.replace("\n    private fun waitCall(method:", """
        } catch (failure: Throwable) {
            println(compose.onRoot(useUnmergedTree = true).printToString())
            throw AssertionError("[DEBUG-archive] waitText=$value observation=$observation", failure)
        }
    }

    private fun waitCall(method:""")
s = s.replace('        waitText("Modifications enregistrées")', '        println("[DEBUG-archive] before notice wait: busy=${vm.state.value.busy} notice=${vm.state.value.notice} error=${vm.state.value.error} archived=${vm.state.value.tasks.any { it.id == proof.id && it.archived }}")\n        waitText("Modifications enregistrées")')
p.write_text(s)
