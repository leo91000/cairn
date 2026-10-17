package build.cairn.app.ui

import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MarkdownTablesDeviceTest {
    @get:Rule val compose = createComposeRule()

    @Test fun quotedColumns() = MarkdownTableCases(compose).readableColumns("quote")

    @Test fun listedColumns() = MarkdownTableCases(compose).readableColumns("list")

    @Test fun readableColumns() = MarkdownTableCases(compose).readableColumns()
}
