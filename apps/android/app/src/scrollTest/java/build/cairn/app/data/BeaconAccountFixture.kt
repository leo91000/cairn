package build.cairn.app.data

/** The same account and installation envelope that the Beacon returns. */
fun beaconAccountFixture(csrf: String = "fixture"): String =
    """{"authenticated":true,"csrf":"$csrf","account":{"id":"fixture-account","email":"owner@example.test"},"installations":[{"id":"fixture","name":"Installation de test","role":"owner","online":true}]}"""

fun beaconInstallationsFixture(): String =
    """[{"id":"fixture","name":"Installation de test","role":"owner","online":true}]"""
